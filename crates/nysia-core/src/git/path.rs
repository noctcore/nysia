//! Path identity: one folder, one string.
//!
//! A project's id is derived from its canonical path (v0.3 §3.1), so whatever this module
//! produces is what makes a project keep its identity across a daemon restart. Registering
//! the same folder twice has to be one project and not two, and the same folder arrives
//! spelled differently every time: `C:\Users\kacpe\Projekty` and `c:\users\kacpe\projekty`
//! and `C:\Users\kacpe\Projekty\` and a symlink pointing at all three.
//!
//! # What this normalises
//!
//! - **Symbolic links and junctions**, by resolving them. This is [`std::fs::canonicalize`],
//!   which is `GetFinalPathNameByHandleW` on Windows and `realpath(3)` elsewhere.
//! - **Case, on both platforms**, wherever the volume is case-insensitive. On Windows
//!   `GetFinalPathNameByHandleW` reports the spelling the filesystem actually holds, so
//!   `c:\users\kacpe` comes back as `C:\Users\kacpe` whatever the caller typed — the case
//!   the plan's trap names. macOS turned out to do the same on APFS: its `realpath(3)`
//!   reports the on-disk spelling rather than the one it was handed. That was measured on
//!   the CI runner rather than assumed, because the manual page does not promise it. On a
//!   case-sensitive volume nothing is corrected and nothing needs to be — the two spellings
//!   are genuinely two directories there.
//! - **8.3 short names, on Windows.** `C:\Users\RUNNER~1\AppData` expands to the long form.
//!   It is not a curiosity: GitHub's Windows runner sets `TEMP` to a short-name path, so a
//!   test comparing a raw `temp_dir()` join against a canonicalised path fails there and
//!   nowhere else.
//! - **Trailing separators**, `.` and `..` components, and `/` as a separator on Windows.
//! - **The `\\?\` verbatim prefix**, which `GetFinalPathNameByHandleW` always returns and
//!   almost nothing wants to see. See [`strip_verbatim`] for when it is safe to remove.
//!
//! # What this deliberately does not normalise
//!
//! Stated rather than discovered, because each of these is a folder that will register
//! twice and look like a bug:
//!
//! - **A path at or over `MAX_PATH`**, which keeps its `\\?\\` prefix because removing it
//!   would change what the path resolves to — and `CreateProcess` will not accept a verbatim
//!   `lpCurrentDirectory`. So [`crate::git::inspect_folder`] on such a folder fails with a
//!   spawn error carrying Windows error 267, rather than answering about it. It fails at the
//!   spawn and no project id is ever derived from it, so there is no silently wrong answer:
//!   it is a folder Nysia cannot register, not one it registers twice. Making it registrable
//!   means giving the spawn a short path of its own, which is a change to the spawn and not
//!   to this type.
//! - **`subst` drives and mapped network drives.** `GetFinalPathNameByHandleW` reports the
//!   drive the handle was opened through, so a folder reached through `subst X: C:\Projekty`
//!   canonicalises under `X:\` and does not merge with its target.
//! - **Hard links and bind mounts**, which are the same directory entry by no definition
//!   this module can see.
//!
//! # Why a newtype
//!
//! Every git spawn in this module takes a [`CanonicalPath`] as its working directory and
//! never a bare [`Path`]. That is the confinement check the module doc promises, expressed
//! so that it cannot be skipped: a path that has not been resolved and proven to be a
//! directory has no way to reach a spawn. It also settles the flag-shaped-folder attack —
//! a folder named `--upload-pack=calc` is a working directory, never an argument, so there
//! is nothing for git's option parser to find.

use std::fmt;
use std::path::{Path, PathBuf};

use super::error::PathError;

/// The Windows verbatim prefix, which turns off all path parsing in the Win32 layer.
#[cfg(windows)]
const VERBATIM: &str = r"\\?\";

/// The UNC form of the verbatim prefix: `\\?\UNC\server\share` is `\\server\share`.
#[cfg(windows)]
const VERBATIM_UNC: &str = r"\\?\UNC\";

/// `MAX_PATH`. A path at or over this length needs the verbatim prefix to be usable by the
/// ANSI-era Win32 entry points, so stripping it there would produce a path that does not
/// work.
#[cfg(windows)]
const MAX_PATH: usize = 260;

/// The MS-DOS device names, which are reserved in every directory at every depth.
///
/// A file called `NUL` cannot exist without the verbatim prefix, so a path containing one
/// as a component keeps it.
#[cfg(windows)]
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// An existing directory, resolved to the one spelling this build will use for it.
///
/// Construct with [`CanonicalPath::of`]. There is no way to make one from a path that does
/// not exist, is not a directory, or could not be read, which is what lets every caller
/// downstream treat it as a folder that was there when it was resolved.
///
/// One caveat, and it is listed under *What this deliberately does not normalise*: a path
/// long enough to keep its `\\?\\` prefix is not usable as a working directory, so a
/// spawn against one fails. It fails loudly and at the spawn, which is why the guarantee is
/// worded as "a folder that existed" rather than "a folder you can run a command in".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalPath(PathBuf);

impl CanonicalPath {
    /// Resolve `path` to the canonical spelling of an existing directory.
    ///
    /// # Errors
    ///
    /// - [`PathError::Missing`] when nothing is there.
    /// - [`PathError::Unreadable`] when something is there and the OS would not resolve it,
    ///   which on both platforms is usually a permission on a parent directory.
    /// - [`PathError::NotADirectory`] when the path names a file. A file is its own shape
    ///   rather than "not a repository": answering `NoRepository` for one would send a
    ///   caller looking for a `.git` beside it.
    pub fn of(path: impl AsRef<Path>) -> Result<Self, PathError> {
        let path = path.as_ref();
        let resolved = std::fs::canonicalize(path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                PathError::Missing {
                    path: path.to_path_buf(),
                }
            } else {
                PathError::Unreadable {
                    path: path.to_path_buf(),
                    source,
                }
            }
        })?;

        // `canonicalize` resolves the link, so this reports what the link points at rather
        // than the link itself — which is the question a caller is asking.
        let metadata = std::fs::metadata(&resolved).map_err(|source| PathError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(PathError::NotADirectory {
                path: strip_verbatim(resolved),
            });
        }

        Ok(Self(strip_verbatim(resolved)))
    }

    /// Resolve a path that git itself printed.
    ///
    /// Git reports paths with forward slashes on Windows (`C:/Users/kacpe/repo`) and without
    /// resolving short names, so its output has to go through the same normalisation as a
    /// caller's input or the two will not compare equal. A worktree whose directory has been
    /// deleted cannot be canonicalised at all, which is why this returns an [`Option`]
    /// rather than an error: see [`crate::worktree::Worktree`].
    pub(crate) fn of_git_output(path: &Path) -> Option<Self> {
        Self::of(path).ok()
    }

    /// The path, for a spawn or a `Display`.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The path's own final component, which is a project's default name (v0.3 §3.1).
    ///
    /// `None` for a filesystem root, which has no name of its own.
    #[must_use]
    pub fn folder_name(&self) -> Option<&str> {
        self.0.file_name().and_then(std::ffi::OsStr::to_str)
    }

    /// Whether `self` is `other` or is inside it, comparing whole components.
    ///
    /// Component-wise rather than by string prefix, which is the difference between
    /// `C:\a\b` containing `C:\a\bc` — it does not — and appearing to.
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        other.0.starts_with(&self.0)
    }
}

impl AsRef<Path> for CanonicalPath {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for CanonicalPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

/// Remove the `\\?\` prefix when the path underneath it still means the same thing.
///
/// `GetFinalPathNameByHandleW` returns the verbatim form for everything, and a project id
/// derived from `\\?\C:\Users\kacpe\Projekty\nysia` is a string a person will read in the
/// sidebar and in the store for as long as the project exists. Removing it is not only
/// cosmetic: `CreateProcess` does not accept a verbatim `lpCurrentDirectory`, and git prints
/// the plain form, so the two would never compare equal.
///
/// The prefix is kept whenever removing it would change what the path resolves to: a path
/// at or over `MAX_PATH`, a component that is a reserved device name, and a component ending
/// in a dot or a space, all of which the Win32 parsing layer rewrites or refuses when the
/// prefix is absent.
#[cfg(windows)]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    let stripped = if let Some(rest) = text.strip_prefix(VERBATIM_UNC) {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(VERBATIM) {
        rest.to_owned()
    } else {
        return path;
    };

    if stripped.len() >= MAX_PATH || !is_plainly_spellable(Path::new(&stripped)) {
        return path;
    }
    PathBuf::from(stripped)
}

/// Nothing to strip anywhere else: only Windows has a verbatim prefix.
#[cfg(not(windows))]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// Whether every component survives Win32's path parsing unchanged.
#[cfg(windows)]
fn is_plainly_spellable(path: &Path) -> bool {
    use std::path::Component;

    path.components().all(|component| match component {
        Component::Normal(name) => {
            let name = name.to_string_lossy();
            // A trailing dot or space is silently trimmed by the parsing layer, so a folder
            // that has one is only reachable through the verbatim prefix.
            let trimmed = !name.ends_with('.') && !name.ends_with(' ');
            // A reserved name is a device wherever it appears, with or without an extension.
            let stem = name.split('.').next().unwrap_or(&name).to_ascii_uppercase();
            trimmed && !RESERVED_NAMES.contains(&stem.as_str())
        }
        // A prefix, a root, or a `.`/`..` that canonicalize should already have removed.
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A uniquely named temporary directory, shell-safe. See
    /// [`crate::git::testing::unique_name`] for both halves of why.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(crate::git::testing::unique_name(&format!("path-{tag}")));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn a_missing_path_is_missing_rather_than_not_a_repository() {
        let dir = temp_dir("missing");
        let err = CanonicalPath::of(dir.join("no-such-folder")).unwrap_err();
        assert!(matches!(err, PathError::Missing { .. }), "{err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_is_its_own_shape_and_not_a_folder() {
        let dir = temp_dir("file");
        let file = dir.join("README.md");
        std::fs::write(&file, "not a folder").expect("write");
        let err = CanonicalPath::of(&file).unwrap_err();
        assert!(matches!(err, PathError::NotADirectory { .. }), "{err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_trailing_separator_and_a_dot_component_are_the_same_folder() {
        let dir = temp_dir("trailing");
        let plain = CanonicalPath::of(&dir).expect("plain");
        let trailing = CanonicalPath::of(format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR))
            .expect("trailing separator");
        let dotted = CanonicalPath::of(dir.join(".")).expect("dot component");
        let round_trip = CanonicalPath::of(
            dir.join("..")
                .join(dir.file_name().expect("the temp dir has a name")),
        )
        .expect("parent then back");

        assert_eq!(plain, trailing);
        assert_eq!(plain, dotted);
        assert_eq!(plain, round_trip);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_verbatim_prefix_does_not_reach_a_caller() {
        // The prefix `GetFinalPathNameByHandleW` always returns, and which a project id
        // would otherwise carry for the life of the project.
        let dir = temp_dir("verbatim");
        let canonical = CanonicalPath::of(&dir).expect("canonical");
        assert!(
            !canonical.to_string().starts_with(r"\\?\"),
            "{canonical} still carries the verbatim prefix"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn containment_compares_components_rather_than_string_prefixes() {
        let dir = temp_dir("contains");
        let inner = dir.join("b");
        let sibling = dir.join("bc");
        std::fs::create_dir_all(&inner).expect("inner");
        std::fs::create_dir_all(&sibling).expect("sibling");

        let inner = CanonicalPath::of(&inner).expect("inner");
        let sibling = CanonicalPath::of(&sibling).expect("sibling");
        let parent = CanonicalPath::of(&dir).expect("parent");

        assert!(parent.contains(&inner));
        assert!(parent.contains(&parent), "a folder contains itself");
        assert!(
            !inner.contains(&sibling),
            "a string prefix is not a path prefix: {inner} must not contain {sibling}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_name_is_the_default_project_name() {
        let dir = temp_dir("name");
        let canonical = CanonicalPath::of(&dir).expect("canonical");
        assert_eq!(
            canonical.folder_name(),
            dir.file_name().and_then(std::ffi::OsStr::to_str)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn case_is_normalised_to_what_the_filesystem_holds() {
        // The plan's trap, exactly: `C:\Users\...` and `c:\users\...` are one folder and
        // two strings, and registration idempotency is a comparison of those strings.
        let dir = temp_dir("case");
        let shouted = PathBuf::from(dir.to_string_lossy().to_uppercase());
        let whispered = PathBuf::from(dir.to_string_lossy().to_lowercase());

        let a = CanonicalPath::of(&shouted).expect("upper case resolves");
        let b = CanonicalPath::of(&whispered).expect("lower case resolves");
        let c = CanonicalPath::of(&dir).expect("as created");

        assert_eq!(a, b, "two casings of one folder must be one canonical path");
        assert_eq!(a, c);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn a_forward_slash_separator_is_the_same_folder() {
        let dir = temp_dir("slashes");
        let inner = dir.join("nested");
        std::fs::create_dir_all(&inner).expect("nested");
        let forward = PathBuf::from(inner.to_string_lossy().replace('\\', "/"));

        assert_eq!(
            CanonicalPath::of(&forward).expect("forward slashes"),
            CanonicalPath::of(&inner).expect("backslashes"),
            "git prints C:/... and a caller types C:\\..."
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn a_short_name_expands_to_the_long_one() {
        // GitHub's Windows runner sets TEMP to a path with an 8.3 component
        // (`C:\Users\RUNNER~1\...`). Canonicalising expands it, which is why a test must
        // never compare a raw `temp_dir()` join against a canonical path.
        //
        // Where this actually exercises: on that runner, and nowhere else. A developer's TEMP
        // has no short component, so the assertion below passes without the expansion ever
        // happening — a vacuous pass, kept rather than deleted because the runner is where the
        // behaviour has to hold. Manufacturing an 8.3 name locally needs `GetShortPathNameW`,
        // which is not in this crate's `windows` feature set, so the test says which kind of
        // pass it was instead of pretending they are the same.
        let dir = temp_dir("shortname");
        let long = dir.join("a directory with a long name");
        std::fs::create_dir_all(&long).expect("long name");

        let exercised = long.to_string_lossy().contains('~');
        let canonical = CanonicalPath::of(&long).expect("canonical");
        assert!(
            !canonical.to_string().contains('~'),
            "{canonical} still holds a short name component"
        );
        if !exercised {
            eprintln!(
                "a_short_name_expands_to_the_long_one: no 8.3 component in {}, so this run \
                 asserted nothing; the expansion is exercised on GitHub's Windows runner",
                long.display()
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(windows)]
    fn the_verbatim_prefix_is_kept_where_removing_it_would_change_the_path() {
        // Purely a check on the decision function: these are paths Win32 rewrites or
        // refuses without the prefix, so the readable form is not available for them.
        assert!(!is_plainly_spellable(Path::new(r"C:\work\NUL")));
        assert!(!is_plainly_spellable(Path::new(r"C:\work\nul.txt")));
        assert!(!is_plainly_spellable(Path::new(r"C:\work\trailing.")));
        assert!(!is_plainly_spellable(Path::new(r"C:\work\trailing ")));
        assert!(is_plainly_spellable(Path::new(r"C:\work\ordinary")));
        assert!(is_plainly_spellable(Path::new(r"C:\work\console")));

        let long = format!(r"C:\{}", "a".repeat(MAX_PATH));
        let kept = strip_verbatim(PathBuf::from(format!(r"\\?\{long}")));
        assert!(
            kept.to_string_lossy().starts_with(VERBATIM),
            "a path at MAX_PATH needs the prefix to stay usable"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn case_is_normalised_on_a_case_insensitive_volume() {
        // Measured on the runner rather than assumed. `realpath(3)`'s manual page does not
        // promise to report the spelling held on disk, and this shipped asserting that macOS
        // did *not* correct case — the CI leg said otherwise, so the doc comment and this test
        // were turned around rather than the behaviour being worked around.
        //
        // The consequence is the one the plan cares about: registering `/Users/k/Projekty` and
        // `/Users/k/projekty` is one project on macOS as well as on Windows.
        let dir = temp_dir("macos-case");
        let mixed = dir.join("MixedCase");
        std::fs::create_dir_all(&mixed).expect("mixed case");

        let Ok(shouted) = CanonicalPath::of(dir.join("MIXEDCASE")) else {
            // A case-sensitive volume, where the two really are different directories and one
            // of them does not exist. Nothing to correct, and nothing to assert.
            std::fs::remove_dir_all(&dir).ok();
            return;
        };
        let mixed = CanonicalPath::of(&mixed).expect("mixed case resolves");
        assert_eq!(
            mixed, shouted,
            "two casings of one folder must be one canonical path"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
