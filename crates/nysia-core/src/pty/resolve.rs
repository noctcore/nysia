//! `which`-style program resolution, and the pre-validation that has to go with it.
//!
//! Neither platform reports a bad program in a usable way once the spawn is under way:
//!
//! - On Windows, `CreateProcess` launches real executables only. `npm` installs its
//!   binaries as `.cmd` and `.ps1` shims, so `Command::new("claude")` fails with
//!   `NotFound` even though `claude` is plainly on `PATH` (traps register #8). A shim has
//!   to be recognised and launched through its interpreter.
//! - On Unix, a failed `execve` inside a pty child returns `Ok` from the spawn and the
//!   child then dies silently, because `portable-pty`'s fd cleanup closes the pipe Rust
//!   uses to report the exec error (wezterm#7893). By the time anything notices, the only
//!   evidence is an empty pty.
//!
//! Both of those are only avoidable by resolving and validating the path *before* the
//! spawn, which is what this module is for.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Why a program could not be resolved to something launchable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// Nothing on `PATH` matched, or the explicit path does not exist.
    #[error("{program} was not found on PATH")]
    NotFound {
        /// The program as the caller spelled it.
        program: String,
    },
    /// A path was found, but it names a directory rather than a file.
    #[error("{path} is not a file")]
    NotAFile {
        /// The offending path.
        path: String,
    },
    /// A Unix path was found, but no execute bit is set for anyone.
    #[error("{path} is not executable")]
    NotExecutable {
        /// The offending path.
        path: String,
    },
    /// A Windows path was found whose extension is not something `CreateProcess` or a
    /// known interpreter can launch.
    #[error("{path} has no launchable extension")]
    UnknownExtension {
        /// The offending path.
        path: String,
    },
    /// The interpreter a shim needs is itself missing.
    #[error("{path} needs {interpreter}, which was not found")]
    MissingInterpreter {
        /// The shim that needed an interpreter.
        path: String,
        /// The interpreter that could not be resolved.
        interpreter: String,
    },
}

/// A program that has been resolved to something the OS will actually launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProgram {
    /// The real executable to spawn.
    pub program: PathBuf,
    /// Arguments that must precede the caller's own — how a shim's interpreter is told
    /// which script to run. Empty for a program launched directly.
    pub leading_args: Vec<OsString>,
}

impl ResolvedProgram {
    /// A program launched directly, with no interpreter in front of it.
    fn direct(program: PathBuf) -> Self {
        Self {
            program,
            leading_args: Vec::new(),
        }
    }

    /// The full argument vector for `program` plus the caller's `args`, argv\[0\] first.
    #[must_use]
    pub fn argv<I, S>(&self, args: I) -> Vec<OsString>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut argv = Vec::with_capacity(1 + self.leading_args.len());
        argv.push(self.program.clone().into_os_string());
        argv.extend(self.leading_args.iter().cloned());
        argv.extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        argv
    }
}

/// Resolve `program` to something launchable, searching `PATH` when it has no separator.
///
/// # Errors
///
/// Returns a [`ResolveError`] describing what was wrong with the candidate rather than
/// letting the spawn fail opaquely later.
pub fn resolve(program: impl AsRef<OsStr>) -> Result<ResolvedProgram, ResolveError> {
    let program = program.as_ref();
    let candidate = if has_separator(program) {
        locate_explicit(Path::new(program))
    } else {
        search_path(program)
    };
    let path = candidate.ok_or_else(|| ResolveError::NotFound {
        program: program.to_string_lossy().into_owned(),
    })?;
    validate(&path)
}

/// Whether the caller spelled a path rather than a bare program name.
fn has_separator(program: &OsStr) -> bool {
    let text = program.to_string_lossy();
    text.contains('/') || (cfg!(windows) && text.contains('\\'))
}

/// Find an explicitly spelled path, preferring a Windows shim sitting beside it.
fn locate_explicit(path: &Path) -> Option<PathBuf> {
    for ext in &candidate_extensions() {
        let candidate = with_appended_extension(path, ext);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    path.is_file().then(|| path.to_path_buf())
}

/// Walk `PATH`, and on Windows every `PATHEXT` extension within each entry.
fn search_path(program: &OsStr) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect();
    search_in(program, &dirs, &candidate_extensions())
}

/// Look for `program` in each of `dirs`, trying `extensions` **before** the bare name.
///
/// The order is the whole point. `npm`'s `cmd-shim` writes three files side by side —
/// `claude` (a `sh` script, for Git Bash), `claude.cmd` and `claude.ps1` — and checking the
/// bare name first finds the `sh` script, which `CreateProcess` cannot launch and which
/// this module would then reject as having no launchable extension. Trying `PATHEXT` first
/// finds `claude.cmd`, which is the file that actually runs.
fn search_in(program: &OsStr, dirs: &[PathBuf], extensions: &[String]) -> Option<PathBuf> {
    for dir in dirs {
        let bare = dir.join(program);
        for ext in extensions {
            let candidate = with_appended_extension(&bare, ext);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if bare.is_file() {
            return Some(bare);
        }
    }
    None
}

/// The extensions to try ahead of a bare name: `PATHEXT` on Windows, none anywhere else.
fn candidate_extensions() -> Vec<String> {
    if cfg!(windows) {
        windows_extensions()
    } else {
        Vec::new()
    }
}

/// `PATHEXT`, lower-cased and normalised to a leading dot, with a sane default when the
/// variable is missing or empty.
fn windows_extensions() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_default();
    let parsed: Vec<String> = raw
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let lower = entry.to_ascii_lowercase();
            if lower.starts_with('.') {
                lower
            } else {
                format!(".{lower}")
            }
        })
        .collect();
    if parsed.is_empty() {
        [".com", ".exe", ".bat", ".cmd", ".ps1"]
            .iter()
            .map(|ext| (*ext).to_owned())
            .collect()
    } else {
        parsed
    }
}

/// Append an extension without replacing one the path already has.
///
/// `Path::with_extension` would turn `git.exe` into `git.cmd`, which is how a search ends
/// up validating a file the caller never asked for.
fn with_appended_extension(path: &Path, ext: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(ext);
    PathBuf::from(name)
}

/// Confirm a located path is launchable, and work out whether it needs an interpreter.
fn validate(path: &Path) -> Result<ResolvedProgram, ResolveError> {
    if !path.is_file() {
        return Err(ResolveError::NotAFile {
            path: path.display().to_string(),
        });
    }
    #[cfg(windows)]
    {
        validate_windows(path)
    }
    #[cfg(unix)]
    {
        validate_unix(path)
    }
}

/// Classify a Windows path by extension: a real executable, a batch shim that needs
/// `cmd.exe`, or a PowerShell shim that needs `powershell.exe`.
#[cfg(windows)]
fn validate_windows(path: &Path) -> Result<ResolvedProgram, ResolveError> {
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "exe" | "com" => Ok(ResolvedProgram::direct(path.to_path_buf())),
        // `cmd.exe /c` is how a `.cmd` or `.bat` shim gets launched at all: it is a script,
        // and `CreateProcess` refuses scripts.
        "cmd" | "bat" => {
            let shell = interpreter("cmd.exe", path)?;
            Ok(ResolvedProgram {
                program: shell,
                leading_args: vec![OsString::from("/c"), path.as_os_str().to_owned()],
            })
        }
        // `-File` rather than `-Command`, so the shim's own path is not re-parsed as
        // PowerShell source. `-NoProfile` keeps a user's profile out of a shim launch.
        "ps1" => {
            let shell = interpreter("powershell.exe", path)?;
            Ok(ResolvedProgram {
                program: shell,
                leading_args: vec![
                    OsString::from("-NoProfile"),
                    OsString::from("-ExecutionPolicy"),
                    OsString::from("Bypass"),
                    OsString::from("-File"),
                    path.as_os_str().to_owned(),
                ],
            })
        }
        _ => Err(ResolveError::UnknownExtension {
            path: path.display().to_string(),
        }),
    }
}

/// Resolve the interpreter a shim needs, reporting the shim in the error so the message
/// says which launch failed rather than only naming `cmd.exe`.
#[cfg(windows)]
fn interpreter(name: &str, shim: &Path) -> Result<PathBuf, ResolveError> {
    // A system interpreter lives beside the rest of Windows; prefer that to whatever is
    // first on a user's `PATH`.
    if let Some(root) = std::env::var_os("SystemRoot") {
        let system32 = Path::new(&root).join("System32").join(name);
        if system32.is_file() {
            return Ok(system32);
        }
    }
    search_path(OsStr::new(name)).ok_or_else(|| ResolveError::MissingInterpreter {
        path: shim.display().to_string(),
        interpreter: name.to_owned(),
    })
}

/// Confirm a Unix path has an execute bit. A path without one spawns "successfully" and
/// then dies silently, which is the failure this check exists to turn into an error.
#[cfg(unix)]
fn validate_unix(path: &Path) -> Result<ResolvedProgram, ResolveError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = path
        .metadata()
        .map_err(|_| ResolveError::NotAFile {
            path: path.display().to_string(),
        })?
        .permissions()
        .mode();
    if mode & 0o111 == 0 {
        return Err(ResolveError::NotExecutable {
            path: path.display().to_string(),
        });
    }
    Ok(ResolvedProgram::direct(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_program_that_is_certainly_on_path() {
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        let resolved = resolve(name).expect("the platform shell must resolve");
        assert!(resolved.program.is_file());
        assert!(resolved.program.is_absolute());
    }

    #[test]
    fn reports_a_missing_program_rather_than_letting_the_spawn_fail() {
        let err = resolve("nysia-no-such-program-exists").unwrap_err();
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn an_explicit_path_that_does_not_exist_is_not_found() {
        let err = resolve("./nysia-no-such-program-exists").unwrap_err();
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn a_directory_is_not_a_program() {
        let dir = std::env::temp_dir();
        let err = resolve(&dir).unwrap_err();
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn argv_puts_the_interpreter_first_and_the_callers_args_last() {
        let resolved = ResolvedProgram {
            program: PathBuf::from("cmd.exe"),
            leading_args: vec![OsString::from("/c"), OsString::from("claude.cmd")],
        };
        let argv = resolved.argv(["--help"]);
        assert_eq!(
            argv,
            vec![
                OsString::from("cmd.exe"),
                OsString::from("/c"),
                OsString::from("claude.cmd"),
                OsString::from("--help"),
            ]
        );
    }

    #[test]
    fn appending_an_extension_does_not_replace_an_existing_one() {
        assert_eq!(
            with_appended_extension(Path::new("git.exe"), ".cmd"),
            PathBuf::from("git.exe.cmd")
        );
    }

    #[test]
    #[cfg(windows)]
    fn pathext_is_normalised_to_lower_case_dotted_entries() {
        let extensions = windows_extensions();
        assert!(extensions.iter().all(|ext| ext.starts_with('.')));
        assert!(
            extensions
                .iter()
                .all(|ext| ext == &ext.to_ascii_lowercase())
        );
        assert!(extensions.iter().any(|ext| ext == ".exe"));
    }

    #[test]
    fn a_shim_beside_a_bare_name_wins_the_search() {
        // `npm`'s `cmd-shim` writes `claude`, `claude.cmd` and `claude.ps1` side by side.
        // Resolving to the extensionless `sh` script is the failure this ordering exists to
        // prevent: swap the two loops in `search_in` and this trips.
        let dir = std::env::temp_dir().join("nysia-resolve-shim-order");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(
            dir.join("pretend-agent"),
            "#!/bin/sh
",
        )
        .expect("write sh shim");
        std::fs::write(
            dir.join("pretend-agent.cmd"),
            "@echo off
",
        )
        .expect("write cmd shim");

        let dirs = vec![dir.clone()];
        let found = search_in(
            OsStr::new("pretend-agent"),
            &dirs,
            &[".cmd".to_owned(), ".exe".to_owned()],
        )
        .expect("something must be found");
        assert_eq!(found, dir.join("pretend-agent.cmd"));

        // With no extensions to try — the Unix case — the bare name is what is found.
        let bare = search_in(OsStr::new("pretend-agent"), &dirs, &[]).expect("bare");
        assert_eq!(bare, dir.join("pretend-agent"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn a_cmd_shim_is_launched_through_cmd_exe() {
        // The trap: `CreateProcess` cannot launch a `.cmd`, so an npm shim has to go
        // through its interpreter or the spawn fails with `NotFound`.
        let dir = std::env::temp_dir().join("nysia-resolve-cmd-shim");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let shim = dir.join("pretend-agent.cmd");
        std::fs::write(&shim, "@echo off\r\n").expect("write shim");

        let resolved = resolve(&shim).expect("a .cmd shim must resolve");
        assert!(
            resolved
                .program
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe"))
        );
        assert_eq!(resolved.leading_args[0], OsString::from("/c"));
        assert_eq!(resolved.leading_args[1], shim.as_os_str());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn an_explicit_extensionless_path_resolves_to_its_shim() {
        let dir = std::env::temp_dir().join("nysia-resolve-explicit-shim");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(
            dir.join("pretend-agent"),
            "#!/bin/sh
",
        )
        .expect("write sh shim");
        std::fs::write(
            dir.join("pretend-agent.cmd"),
            "@echo off
",
        )
        .expect("write cmd shim");

        let resolved = resolve(dir.join("pretend-agent")).expect("the shim must resolve");
        assert!(
            resolved
                .program
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe"))
        );
        assert_eq!(
            resolved.leading_args.last(),
            Some(&dir.join("pretend-agent.cmd").into_os_string())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn a_ps1_shim_is_launched_with_file_not_command() {
        let dir = std::env::temp_dir().join("nysia-resolve-ps1-shim");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let shim = dir.join("pretend-agent.ps1");
        std::fs::write(&shim, "exit 0\r\n").expect("write shim");

        let resolved = resolve(&shim).expect("a .ps1 shim must resolve");
        assert!(resolved.leading_args.contains(&OsString::from("-File")));
        assert_eq!(
            resolved.leading_args.last(),
            Some(&shim.as_os_str().to_owned())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn an_extensionless_file_is_refused_rather_than_spawned() {
        let dir = std::env::temp_dir().join("nysia-resolve-unknown-ext");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("readme.txt");
        std::fs::write(&file, "not a program").expect("write file");

        let err = resolve(&file).unwrap_err();
        assert!(matches!(err, ResolveError::UnknownExtension { .. }));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn a_file_without_an_execute_bit_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join("nysia-resolve-noexec");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("not-executable");
        std::fs::write(&file, "#!/bin/sh\n").expect("write file");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        // Without this check the spawn returns `Ok` and the child dies silently
        // (wezterm#7893).
        let err = resolve(&file).unwrap_err();
        assert!(matches!(err, ResolveError::NotExecutable { .. }));

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(resolve(&file).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }
}
