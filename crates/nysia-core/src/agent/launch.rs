//! Turning "start the agent" into something the OS will actually launch.
//!
//! The type and the error live here, in the neutral half of `agent/`; which program gets
//! resolved, and what it is called, is [`super::claude`]'s business. Types flow up and
//! behaviour stays down, so nothing in this file has to be re-exported out of the Claude
//! module — see the boundary note in [`super`].

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::pty::ResolveError;

/// Why an agent could not be turned into something launchable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    /// The agent's program could not be resolved to a launchable file.
    #[error("the {agent} CLI is unavailable: {source}")]
    Unavailable {
        /// Which agent failed, for a message a user can act on.
        agent: &'static str,
        /// What resolution said about it.
        #[source]
        source: ResolveError,
    },
}

/// An agent CLI, resolved and pre-validated, ready to be spawned.
///
/// "Pre-validated" is the load-bearing word and it is traps register #8 and #11 together:
/// neither platform reports a bad program usefully once the spawn is under way, so the check
/// has to happen before it. [`crate::pty::resolve`] does that work; this type is what comes
/// out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLaunch {
    /// Which agent this is, for error messages.
    agent: &'static str,
    /// What to call it in front of a person, which is what a tab is labelled with.
    label: &'static str,
    /// The real executable to spawn — `cmd.exe` when the CLI is an npm batch shim.
    program: PathBuf,
    /// The full argument vector, argv\[0\] first.
    argv: Vec<OsString>,
}

impl AgentLaunch {
    /// Build one from an already-resolved program.
    pub(super) fn new(
        agent: &'static str,
        label: &'static str,
        program: PathBuf,
        argv: Vec<OsString>,
    ) -> Self {
        Self {
            agent,
            label,
            program,
            argv,
        }
    }

    /// Which agent this launches.
    pub fn agent(&self) -> &'static str {
        self.agent
    }

    /// What to call a session running it.
    ///
    /// Two strings rather than one, and the second is not a capitalisation of the first:
    /// [`Self::agent`] identifies the agent in an error a person reads next to the command
    /// they typed, and this is a **tab label**. The alternative was labelling the tab with
    /// [`Self::program`], and that is a path — which is somebody's disk, on screen, in every
    /// screenshot of the app (traps register #13).
    ///
    /// Both come from the agent's own module, because what an agent is called is exactly the
    /// kind of thing a second agent changes (D-4).
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// The real executable.
    ///
    /// On Windows this is `cmd.exe` or `powershell.exe` whenever the CLI turned out to be an
    /// npm shim, because `CreateProcess` launches real executables and nothing else.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The full argument vector, argv\[0\] first.
    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }

    /// A [`Command`] that runs it.
    ///
    /// This is the one-shot form — `nysia agent doctor` asking the CLI its version, and the
    /// proof in this module's tests. A session is not spawned this way: it needs a pty, and
    /// [`crate::pty`] owns that.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(self.argv.iter().skip(1));
        command
    }
}

/// Resolve the agent CLI, passing `args` through to it.
///
/// # Errors
///
/// Returns [`LaunchError::Unavailable`] when the CLI is not on `PATH`, is not a file, or is
/// not something this platform can launch.
pub fn launch<I, S>(args: I) -> Result<AgentLaunch, LaunchError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    super::claude::launch(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_repeats_the_arguments_after_argv_zero() {
        let launch = AgentLaunch::new(
            "claude",
            "Claude",
            PathBuf::from("cmd.exe"),
            vec![
                OsString::from("cmd.exe"),
                OsString::from("/c"),
                OsString::from("call"),
                OsString::from("claude.cmd"),
                OsString::from("--version"),
            ],
        );
        let command = launch.command();
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, vec!["/c", "call", "claude.cmd", "--version"]);
        assert_eq!(command.get_program(), OsStr::new("cmd.exe"));
    }
}
