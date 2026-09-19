//! The shells a session can be.
//!
//! Claude is the only agent in v1 and shells are the other session type (D-3): PowerShell
//! 7, Command Prompt, Git Bash and WSL. WSL is a plain shell — `wsl.exe -d <distro>` in a
//! ConPTY, with no path translation, no worktrees and no agents inside it (D-17) — which
//! covers the design's menu entry honestly at near-zero cost.
//!
//! [`ShellProfile::Posix`] is the fifth, and is not in the design's menu: it is what a
//! macOS session is, and what a Unix CI runner has to spawn for the session tests to mean
//! anything.
//!
//! Every profile resolves through [`super::resolve`] rather than handing a bare name to
//! the spawn, and Git Bash in particular is resolved from `git.exe` rather than from a
//! bare `bash`: on Windows `bash` on `PATH` is `System32\bash.exe`, the WSL launcher, and
//! a "Git Bash" session that silently opened WSL would be a confusing bug rather than a
//! convenience.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

use super::resolve::{ResolveError, resolve};

/// Which shell a session runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellProfile {
    /// PowerShell 7 (`pwsh`), the modern cross-platform one, not Windows PowerShell.
    PowerShell7,
    /// The Windows Command Prompt (`cmd.exe`).
    CommandPrompt,
    /// Git Bash, resolved from the Git for Windows installation that owns `git.exe`.
    GitBash,
    /// WSL as a plain shell, optionally naming a distribution.
    Wsl {
        /// The distribution to launch, or `None` for the user's default.
        distro: Option<String>,
    },
    /// The user's login shell on Unix, falling back to `/bin/sh`.
    Posix,
}

/// Why a profile could not be turned into something spawnable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The profile's program could not be resolved.
    #[error("{profile} is unavailable: {source}")]
    Unavailable {
        /// Which profile failed.
        profile: &'static str,
        /// What resolution said about it.
        #[source]
        source: ResolveError,
    },
    /// The profile does not exist on this platform at all.
    #[error("{profile} is not available on this platform")]
    WrongPlatform {
        /// Which profile was asked for.
        profile: &'static str,
    },
    /// A WSL distribution name carried a character that would let it be read as another
    /// argument.
    #[error("{0:?} is not a usable WSL distribution name")]
    BadDistro(String),
}

impl ShellProfile {
    /// The profile a new session gets when the caller expresses no preference.
    #[must_use]
    pub fn platform_default() -> Self {
        if cfg!(windows) {
            // PowerShell 7 is the design's default, but it is an optional install; the
            // Command Prompt is the one shell a Windows machine always has.
            if resolve("pwsh").is_ok() {
                Self::PowerShell7
            } else {
                Self::CommandPrompt
            }
        } else {
            Self::Posix
        }
    }

    /// A stable identifier for this profile, used in logs and on the wire.
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::PowerShell7 => "pwsh",
            Self::CommandPrompt => "cmd",
            Self::GitBash => "git-bash",
            Self::Wsl { .. } => "wsl",
            Self::Posix => "posix",
        }
    }

    /// A human-facing label for this profile.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::PowerShell7 => "PowerShell 7".to_owned(),
            Self::CommandPrompt => "Command Prompt".to_owned(),
            Self::GitBash => "Git Bash".to_owned(),
            Self::Wsl { distro: None } => "WSL".to_owned(),
            Self::Wsl {
                distro: Some(distro),
            } => format!("WSL ({distro})"),
            Self::Posix => "Shell".to_owned(),
        }
    }

    /// Build the command for this profile, with the program already resolved and validated
    /// and the environment already scrubbed.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError`] when the shell is not installed, not available on this
    /// platform, or named with an argument-like distribution.
    pub fn command(&self) -> Result<CommandBuilder, ProfileError> {
        let mut command = CommandBuilder::from_argv(self.argv()?);
        super::env::sanitize(&mut command);
        Ok(command)
    }

    /// Whether this profile would build a command right now, and why not when it would not.
    ///
    /// **The same question [`Self::command`] asks before a spawn**, answered by the same code
    /// and stopping short of the command itself: the program is resolved on this process's
    /// `PATH` at the moment of the call, and validated the way a spawn would validate it. That
    /// is the `PATH` this process started with; a directory added to the system's since is not
    /// on it. That is what makes it an answer about *this machine* rather than about the
    /// platform — a `cfg!(windows)` list would offer PowerShell 7 to everybody on Windows,
    /// which is the menu entry that led a person to a refusal.
    ///
    /// It is a snapshot, not a promise. A shell can be uninstalled between this answer and a
    /// launch, which is why [`Self::command`] still refuses on its own.
    ///
    /// # Cost
    ///
    /// Filesystem probes and nothing else — no process is started. Each profile walks `PATH`
    /// with every `PATHEXT` extension tried in every directory, and Git Bash first resolves
    /// `git` and then stats its candidates. Cheap per call, but unbounded by anything this
    /// module controls: a long `PATH` on a slow or network drive makes it slow. Callers keep
    /// it off hot paths and out from under any lock.
    ///
    /// # Errors
    ///
    /// Returns the [`ProfileError`] [`Self::command`] would have returned.
    pub fn launchable(&self) -> Result<(), ProfileError> {
        self.argv().map(|_| ())
    }

    /// The argument vector this profile launches, argv\[0\] resolved and validated.
    fn argv(&self) -> Result<Vec<OsString>, ProfileError> {
        let (program, args) = match self {
            Self::PowerShell7 => (
                locate("pwsh", self.id())?,
                // `-NoLogo` because the banner is noise in a pane that is already labelled.
                vec![OsString::from("-NoLogo")],
            ),
            Self::CommandPrompt => {
                if !cfg!(windows) {
                    return Err(ProfileError::WrongPlatform { profile: self.id() });
                }
                (locate("cmd.exe", self.id())?, Vec::new())
            }
            Self::GitBash => (
                git_bash()?,
                vec![OsString::from("-i"), OsString::from("-l")],
            ),
            Self::Wsl { distro } => {
                if !cfg!(windows) {
                    return Err(ProfileError::WrongPlatform { profile: self.id() });
                }
                let mut args = Vec::new();
                if let Some(distro) = distro {
                    check_distro(distro)?;
                    args.push(OsString::from("-d"));
                    args.push(OsString::from(distro));
                }
                (locate("wsl.exe", self.id())?, args)
            }
            Self::Posix => {
                if cfg!(windows) {
                    return Err(ProfileError::WrongPlatform { profile: self.id() });
                }
                (posix_shell()?, vec![OsString::from("-l")])
            }
        };

        program
            .argv(args)
            .map_err(|source| ProfileError::Unavailable {
                profile: self.id(),
                source,
            })
    }
}

/// Resolve a profile's program, attributing the failure to the profile rather than to a
/// bare program name the caller never typed.
fn locate(
    program: &str,
    profile: &'static str,
) -> Result<super::resolve::ResolvedProgram, ProfileError> {
    resolve(program).map_err(|source| ProfileError::Unavailable { profile, source })
}

/// Find the Git Bash that belongs to the installed Git for Windows.
///
/// `git.exe` lives at `<root>\cmd\git.exe` or `<root>\bin\git.exe`, and the shell to launch
/// is `<root>\bin\bash.exe` — the MSYS wrapper that sets up the environment — never
/// `<root>\usr\bin\bash.exe`, which is the raw binary and starts without one.
fn git_bash() -> Result<super::resolve::ResolvedProgram, ProfileError> {
    if !cfg!(windows) {
        return Err(ProfileError::WrongPlatform {
            profile: "git-bash",
        });
    }
    for candidate in git_bash_candidates() {
        if let Ok(resolved) = resolve(&candidate) {
            return Ok(resolved);
        }
    }
    Err(ProfileError::Unavailable {
        profile: "git-bash",
        source: ResolveError::NotFound {
            program: "bash.exe (Git for Windows)".to_owned(),
        },
    })
}

/// Every place a Git for Windows `bash.exe` is worth looking for, best first.
fn git_bash_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(git) = resolve("git") {
        // `<root>\cmd\git.exe` and `<root>\bin\git.exe` both put the root two levels up.
        if let Some(root) = git.program.parent().and_then(Path::parent) {
            candidates.push(root.join("bin").join("bash.exe"));
        }
    }
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(dir) = std::env::var_os(var) {
            candidates.push(Path::new(&dir).join("Git").join("bin").join("bash.exe"));
        }
    }
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            Path::new(&dir)
                .join("Programs")
                .join("Git")
                .join("bin")
                .join("bash.exe"),
        );
    }
    candidates
}

/// The user's login shell, or `/bin/sh` when `SHELL` names nothing launchable.
fn posix_shell() -> Result<super::resolve::ResolvedProgram, ProfileError> {
    if let Some(shell) = std::env::var_os("SHELL")
        && let Ok(resolved) = resolve(&shell)
    {
        return Ok(resolved);
    }
    locate("/bin/sh", "posix")
}

/// Refuse a distribution name that could be read as another `wsl.exe` argument, or that
/// carries whitespace or control characters.
fn check_distro(distro: &str) -> Result<(), ProfileError> {
    let bad = distro.is_empty()
        || distro.starts_with('-')
        || distro
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"');
    if bad {
        return Err(ProfileError::BadDistro(distro.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_has_a_stable_id_and_a_label() {
        let profiles = [
            ShellProfile::PowerShell7,
            ShellProfile::CommandPrompt,
            ShellProfile::GitBash,
            ShellProfile::Wsl { distro: None },
            ShellProfile::Wsl {
                distro: Some("Ubuntu".to_owned()),
            },
            ShellProfile::Posix,
        ];
        for profile in &profiles {
            assert!(!profile.id().is_empty());
            assert!(!profile.label().is_empty());
        }
        assert_eq!(
            ShellProfile::Wsl {
                distro: Some("Ubuntu".to_owned())
            }
            .label(),
            "WSL (Ubuntu)"
        );
    }

    #[test]
    fn whether_a_profile_is_launchable_is_what_building_its_command_would_say() {
        // One question with two callers: the `+` menu asks it ahead of time and a spawn asks
        // it at the last moment. They share the code, and this holds the two answers equal —
        // including for a refusal every platform makes, a distribution that reads as a flag,
        // so an answer that said "yes" to everything fails here on both legs.
        for profile in [
            ShellProfile::PowerShell7,
            ShellProfile::CommandPrompt,
            ShellProfile::GitBash,
            ShellProfile::Wsl { distro: None },
            ShellProfile::Wsl {
                distro: Some("-e".to_owned()),
            },
            ShellProfile::Posix,
        ] {
            assert_eq!(
                profile.launchable().is_ok(),
                profile.command().is_ok(),
                "{profile:?}"
            );
        }
        assert!(
            ShellProfile::Wsl {
                distro: Some("-e".to_owned())
            }
            .launchable()
            .is_err()
        );
    }

    #[test]
    fn the_platform_default_is_spawnable_here() {
        let profile = ShellProfile::platform_default();
        let command = profile.command().expect("the default shell must build");
        assert!(!command.get_argv().is_empty());
    }

    #[test]
    fn the_environment_is_scrubbed_on_the_way_out() {
        let command = ShellProfile::platform_default()
            .command()
            .expect("the default shell must build");
        for name in super::super::env::SCRUBBED_VARS {
            assert!(command.get_env(name).is_none(), "{name} leaked");
        }
        assert_eq!(
            command.get_env("TERM"),
            Some(super::super::env::FORCED_TERM.as_ref())
        );
    }

    #[test]
    fn a_distribution_name_that_looks_like_a_flag_is_refused() {
        assert!(check_distro("Ubuntu-22.04").is_ok());
        assert!(check_distro("").is_err());
        assert!(check_distro("-e").is_err());
        assert!(check_distro("Ubuntu 22").is_err());
        assert!(check_distro("Ubuntu\"").is_err());
        assert!(check_distro("Ubuntu\n").is_err());
    }

    #[test]
    #[cfg(windows)]
    fn the_windows_only_profiles_refuse_to_build_elsewhere_and_build_here() {
        let command = ShellProfile::CommandPrompt
            .command()
            .expect("cmd.exe is always present on Windows");
        let argv = command.get_argv();
        assert!(
            Path::new(&argv[0])
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe"))
        );
    }

    #[test]
    #[cfg(windows)]
    fn wsl_passes_the_distribution_through_as_a_separate_argument() {
        // D-17: WSL is a plain shell, so the whole integration is these two argv entries.
        let Ok(command) = (ShellProfile::Wsl {
            distro: Some("Ubuntu".to_owned()),
        })
        .command() else {
            // `wsl.exe` is absent on a stripped Windows image; the argument shape is still
            // covered by the profile's construction below.
            return;
        };
        let argv = command.get_argv();
        assert_eq!(argv[1], OsString::from("-d"));
        assert_eq!(argv[2], OsString::from("Ubuntu"));
    }

    #[test]
    #[cfg(windows)]
    fn git_bash_is_taken_from_git_not_from_bash_on_path() {
        // `bash` on PATH is `System32\bash.exe`, the WSL launcher. A Git Bash session that
        // opened WSL instead would be a silent, very confusing bug.
        for candidate in git_bash_candidates() {
            assert!(
                !candidate.starts_with(
                    Path::new(&std::env::var_os("SystemRoot").unwrap_or_default()).join("System32")
                ),
                "{} is the WSL launcher, not Git Bash",
                candidate.display()
            );
            assert!(candidate.ends_with(Path::new("bin").join("bash.exe")));
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_windows_profiles_report_the_wrong_platform_rather_than_a_missing_program() {
        for profile in [
            ShellProfile::CommandPrompt,
            ShellProfile::GitBash,
            ShellProfile::Wsl { distro: None },
        ] {
            assert!(matches!(
                profile.command(),
                Err(ProfileError::WrongPlatform { .. })
            ));
        }
    }

    #[test]
    #[cfg(unix)]
    fn the_posix_profile_falls_back_to_bin_sh() {
        // Whatever `SHELL` says, something must be spawnable.
        assert!(posix_shell().is_ok());
        assert!(ShellProfile::Posix.command().is_ok());
    }
}
