//! The log beside the endpoint, and the only thing that keeps it from growing for ever.
//!
//! A spawned daemon's stdout and stderr are redirected into `<runtime dir>/<stem>.log` by
//! [`crate::rpc::discovery`], and under D-1 that process lives for **days**. Nothing used to
//! trim the file, so a long-lived session filled the user's disk one `tracing` line at a
//! time. This module is the cap.
//!
//! # The policy, in two numbers
//!
//! [`MAX_LOG_BYTES`] per file — 8 MiB — and [`KEPT_ROTATIONS`] older copies kept beside it —
//! 3 — for a ceiling of [`MAX_TOTAL_LOG_BYTES`], which is 32 MiB and is *derived* from the
//! other two rather than written down a second time. `the_ceiling_is_the_product_of_the_two`
//! holds all three to the literals in this paragraph, because a constant in code beside a
//! different number in prose is a defect this project has corrected in six separate files.
//!
//! The cap is measured on a schedule, not on every write. So a file can be over it when it
//! is measured, and the copy taken aside is that whole file — cap plus whatever was written
//! since the last check. [`MAX_TOTAL_LOG_BYTES`] is therefore **the ceiling when the trim
//! keeps up**, not a hard limit, and the honest statement of what a trim guarantees is the
//! one `a_trim_empties_the_live_log_and_keeps_only_the_policy` makes: the live file is left
//! empty, and never more than [`KEPT_ROTATIONS`] copies survive. Claiming a hard ceiling
//! would be a claim no check in this repo can make.
//!
//! # Why the live file is truncated and never renamed
//!
//! This is the whole design, and the obvious implementation is the wrong one.
//!
//! The daemon does not open its own log. Its stdout and stderr are an **inherited handle**
//! onto a file the spawner opened, and a handle names the file, not the path. Rename the
//! live log to `<stem>.log.1` and the rename succeeds — and the daemon goes on writing into
//! `<stem>.log.1` for the rest of its life while the fresh `<stem>.log` stays empty for ever.
//! The log would appear to stop the first time it was rotated. Windows adds a second way for
//! a rename to be wrong, refusing it outright against an open handle whose share mode did
//! not permit deletion, but the fatal problem is the one that happens when the rename
//! *works*.
//!
//! Truncating in place is the one operation that survives a writer holding the file open, on
//! both platforms. An append handle — `O_APPEND` on Unix, `FILE_APPEND_DATA` on Windows —
//! resolves the write offset to the current end of file at every write, so after a
//! `set_len(0)` the next line the daemon writes lands at offset 0 of the same file it was
//! already writing to. It never learns that anything happened.
//! `rotation_survives_a_writer_holding_the_file_open` is that claim as a test, and it is the
//! test a rename-based implementation fails.
//!
//! **What it costs.** Lines written between the copy and the truncate are lost — the classic
//! `copytruncate` trade. It is bounded by how long the copy takes, it happens once per 8 MiB
//! of output, and the alternative silently loses every line written after the first rotation
//! instead of a handful during it.
//!
//! Only the live file needs this. The rotations have no writer, so they are moved with an
//! ordinary [`std::fs::rename`].
//!
//! # What a Nysia log never contains
//!
//! Trap 13: scrollback can carry secrets, and so can a `question`, which is a tool's input
//! verbatim. `nysia-proto` confines `tool_input` to `waiting` events in both directions for
//! exactly that reason, the store logs nothing at all, and no error variant echoes a payload.
//! A log file that undid any of that would turn debugging into a liability, so the rule for
//! everything that writes into one of these files is:
//!
//! - **no PTY output**, in either direction — not a payload byte, not a scrollback line, not
//!   a rendered frame's contents;
//! - **no keystrokes** — `TerminalSend.text` is what the user typed and may be a password;
//! - **no `tool_input`** and no `question`, which is the same field by another name;
//! - **no session environment or working directory** — `SessionCreate.envOverrides` is where
//!   a token reaches a shell, and a session's `cwd` names what a person is working on. Nysia's
//!   *own* runtime paths are exempt and are logged: the log says where it is, and a reader who
//!   has the file already has the directory it is in. The exemption is that narrow on purpose
//!   — it covers the endpoint, the runtime directory and the log, and nothing a session chose;
//! - **no request or response bodies at all.** A verb's *name*, a handle, a pane key, a
//!   stream id, a byte count and a boolean are the whole vocabulary.
//!
//! Nothing in Nysia has to remember that. The window's log path takes a closed set of typed
//! fields and no free-text string, so there is nowhere for a payload to be put — see
//! `nysia-desktop`'s `commands` module, where `a_verbs_name_is_logged_and_its_payload_is_not`
//! holds it.
//!
//! ## The rule binds the crates Nysia links, not only the code it writes
//!
//! A rule that covered only our own `tracing::` calls would have been a convention with a
//! hole under it, and the hole was real. Nysia links a VT and a pty, and each of them prints
//! what passed through it:
//!
//! - `vte-0.15.0/src/ansi.rs:1341` — `debug!("[unhandled osc_dispatch]: [{}] …")`, which is
//!   every byte of an OSC parameter the parser did not handle, verbatim.
//! - `alacritty_terminal-0.26.0/src/term/mod.rs:2222` — `trace!("Setting title to '{title:?}'")`,
//!   and a shell's window title is routinely the working directory.
//! - `portable-pty@2afb836/pty/src/win/pseudocon.rs:160` —
//!   `error!("CreateProcessW `{:?}` in cwd `{:?}` failed: {}")`: the command line and the
//!   working directory of a spawn that failed. Both reached the daemon over the socket in a
//!   `SessionCreate`, which is the class the rule above is about.
//! - `portable-pty@2afb836/pty/src/cmdbuilder.rs:566` — `warn!("$SHELL -> {shell:?} which is
//!   not executable …")`, which prints whatever a `SessionCreate` set `SHELL` to. `SHELL` is
//!   not one of `crate::pty::SCRUBBED_VARS`, so an `envOverrides` entry reaches it intact.
//! - `portable-pty@2afb836/pty/src/cmdbuilder.rs:147` and `:181` —
//!   `trace!("adding SYS env: {:?} {:?}")` and the same line for `USER`: the whole
//!   environment block the session is about to inherit, name and value, one line each.
//!
//! All of them use the `log` crate, which reaches the subscriber through `tracing-log`. The
//! two VT lines are silent at `info`, so the shipped default never leaked them — but they are
//! loud at `debug` and `trace`, which are the levels a person sets *because* they are
//! debugging and are about to send somebody the file. That is the worst possible way round.
//!
//! The pty is the worse case and was the later discovery. `pseudocon.rs:160` is an `ERROR`
//! and `cmdbuilder.rs:566` a `WARN`, so both clear the shipped `info` default with no
//! `NYSIA_LOG` set at all — and they are on opposite legs, the first `cfg(windows)` and the
//! second `cfg(unix)`, so either one on its own looks like a platform quirk rather than a
//! rule with a crate-shaped hole in it. Measured on both: see
//! `a_failed_spawn_writes_a_caller_offered_path_without_the_confinement` in `nysia`'s `log`
//! module, which plants a spawn that cannot succeed and reads the line back out.
//!
//! [`CONFINED_TARGETS`] is the answer, and it is why the sentence above is a rule rather than
//! an aspiration.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// Targets whose own logging carries terminal bytes, and the level each is held to.
///
/// `tracing_subscriber::EnvFilter` directives, folded in by every binary that installs a
/// subscriber *after* whatever `NYSIA_LOG` asked for — and folding them in is only half of
/// it, because on its own it does not hold. `NYSIA_LOG` is put through
/// [`screen_directives`] first and a directive that could reach one of these targets is
/// refused rather than honoured. The two halves together are the guarantee; the next section
/// is why neither is enough alone.
///
/// That is CLAUDE.md §6: *a security default that an ordinary caller can undo is not a
/// default, it is a suggestion.* Before the fold, `NYSIA_LOG` replaced the filter wholesale,
/// so raising the level to debug something silently switched off a confinement the person did
/// not know was there. Before the screen, naming a module did the same thing — which is
/// worse, because the string that does it is the one somebody reaches for while debugging the
/// very subsystem whose bytes leak.
///
/// # Why the fold is not enough on its own
///
/// `EnvFilter` resolves a callsite against its **most specific** matching directive, and
/// specific means *long*: in `tracing-subscriber` 0.3.23, `StaticDirective::cares_about_target`
/// matches with a plain `metadata.target().starts_with(directive_target)` and `Ord for
/// StaticDirective` orders on `target.len()` before anything else. Every entry here names a
/// crate, so any longer string that begins with that crate's name out-specifies it and wins:
///
/// ```text
/// NYSIA_LOG=vte::ansi=trace                     every unhandled OSC parameter, verbatim
/// NYSIA_LOG=portable_pty::win::pseudocon=trace  a failed spawn's command line and its cwd
/// NYSIA_LOG=vte:=trace                          the same as the first, and not even a path
/// ```
///
/// The third is what decides the shape of the screen. `vte:` is four characters where `vte`
/// is three, and `"vte::ansi".starts_with("vte:")` is true, so a target that is not a Rust
/// module path at all still out-specifies this list. A `::`-boundary test would have let it
/// through. So the screen refuses any target that *begins with* a confined crate's name,
/// which is deliberately a little wider than the set that can actually reach one.
///
/// A span or field directive is refused whatever target it names, and it is a different hole
/// rather than the same one. `EnvFilter::enabled` consults its *dynamic* directives first and
/// returns `true` straight out of the span scope before a single target directive is looked
/// at, so `NYSIA_LOG=[x]=trace` enables every event raised inside a span called `x`, on every
/// target, however this list is spelled.
///
/// # The way out, which is named rather than stumbled into
///
/// [`UNCONFINED_ENV`] lifts all of it, and is the only thing that does. A developer who needs
/// to watch the parser sets that as well, on purpose, and the binaries say so at startup. See
/// its docs for why the escape hatch is a second variable and not a level.
///
/// # Why these three, and why at the level each names
///
/// See the module docs for the call sites and what each of them prints. `vte` is `off` rather
/// than `warn` because every line it emits at any level is parser diagnostics about bytes —
/// there is nothing in it worth keeping. `alacritty_terminal` is `warn` rather than `off`
/// because its errors are worth having and it is only `trace` that prints the title.
///
/// `portable_pty` is `off`, and copying `alacritty_terminal`'s `warn` would have confined
/// nothing: `warn` permits `ERROR`, and the Windows leak *is* an `ERROR` while the Unix one is
/// a `WARN`. Naming the leaking module rather than the crate — `portable_pty::win::pseudocon`
/// is a target in its own right — was the other candidate, and it was measured rather than
/// argued: it silences the Windows `ERROR` and leaves both the Unix `WARN` and the Windows
/// environment dump exactly where they were, because those are a different module of the same
/// crate.
///
/// ## What `off` costs
///
/// A default that hides a real failure is not a safe default either, so this was measured at
/// the pinned rev rather than reasoned about, and it costs less than it looks:
///
/// - `pseudocon.rs:160` logs the string that the `bail!` on the *next line* puts into
///   [`crate::pty::SpawnError::Spawn`]'s `reason`, which `crate::rpc::session`'s
///   `spawn_log_line` already writes. The diagnostic is not lost; it is re-routed through the
///   one writer that applies this module's rule to it before it reaches the file.
/// - Traps 6 and 11 are ConPTY teardown, and that is the failure someone would most want a
///   line for — but the crate makes no `log::` call around `ClosePseudoConsole`, the reader
///   drain or EOF, so there is no teardown line to give up. `rg 'log::' pty/src` at that rev
///   is how that was established: every call site it returns is either one this module
///   already names or one in `serial.rs`, which Nysia never reaches because it opens ptys
///   through `native_pty_system` and never a serial port. How many there were is not
///   quoted, because nothing in this repo runs that `rg` — and an unchecked count is what
///   the rest of this module is careful not to write down.
/// - What is genuinely given up is `cmdbuilder.rs`'s Unix shell resolution warnings: the
///   `$SHELL` one, and the passwd-lookup ones its `cfg(unix)` `get_shell` writes beside
///   it. Those are the leak.
///
/// # Adding to this list
///
/// A crate belongs here when *its own* logging can print bytes that came off a PTY, or a value
/// that reached the runtime from a caller. That is a question about the dependency's source,
/// not about how Nysia calls it, so the entry should arrive with a file and a line the way the
/// ones above did — and with a plant that shows the line no longer reaches the file.
/// `every_confined_directive_parses` cannot stand in for that plant: `portable-pty=off` parses
/// perfectly and confines nothing, because an `EnvFilter` target is a Rust path and this
/// crate's is spelled with an underscore.
pub const CONFINED_TARGETS: &[&str] = &["vte=off", "alacritty_terminal=warn", "portable_pty=off"];

/// The environment variable every Nysia binary takes its filter from.
///
/// Named here rather than spelled out in each binary because [`screen_directives`] is what
/// refuses a directive and the caller is what reports the refusal, and a message naming a
/// different variable than the one the person set would be worse than no message at all.
pub const LOG_ENV: &str = "NYSIA_LOG";

/// The separately named way to ask for a log that [`CONFINED_TARGETS`] does not hold.
///
/// Set it to anything non-empty and the confinement is not applied at all: `NYSIA_LOG` is
/// honoured verbatim, `vte::ansi=trace` included, and the resulting file can contain terminal
/// output, a window title, an environment block and the working directory of a spawn — all
/// of which the module docs above say a Nysia log never contains. It is not a file to attach
/// to an issue.
///
/// CLAUDE.md §6 asks for this shape: *where a deliberate override is genuinely needed, put it
/// behind a separately named entry point whose name says so, so the call site is obvious in
/// review and greppable — never on the path everyone already uses.* A second variable is what
/// makes the two cases different in kind rather than in degree. Somebody debugging a spawn
/// that failed raises the level and gets no terminal bytes; somebody who has decided they
/// need the parser's own output types a word that says `UNCONFINED` and is told what that
/// means. Neither can happen to the other by accident, and `rg NYSIA_LOG_UNCONFINED` finds
/// every place it is honoured.
pub const UNCONFINED_ENV: &str = "NYSIA_LOG_UNCONFINED";

/// Whether this process was asked, by [`UNCONFINED_ENV`]'s own name, to drop the confinement.
///
/// Empty counts as unset. `NYSIA_LOG_UNCONFINED=` is what a shell is left holding when
/// somebody clears it, and reading that as *yes* would make the override outlive the
/// intention behind it.
#[must_use]
pub fn confinement_lifted() -> bool {
    std::env::var_os(UNCONFINED_ENV).is_some_and(|value| !value.is_empty())
}

/// A `NYSIA_LOG` value, split in two by [`screen_directives`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screened<'a> {
    /// The directives that cannot reach a [`CONFINED_TARGETS`] target, in the order written.
    pub honoured: Vec<&'a str>,
    /// The directives that could have, and were dropped instead.
    ///
    /// Empty for every `NYSIA_LOG` anybody ordinarily sets. A caller is expected to say when
    /// it is not, because a directive that is silently ignored is indistinguishable from one
    /// that did not do anything, and the person is owed the name of the way round it.
    pub refused: Vec<&'a str>,
}

/// Split a `NYSIA_LOG` value into the directives that can be honoured and the ones that would
/// undo [`CONFINED_TARGETS`].
///
/// Splitting on `,` and dropping the empty pieces is exactly what `EnvFilter` does with the
/// same string — `Builder::parse` in `tracing-subscriber` 0.3.23 — so a directive that
/// survives this reaches the filter spelled the way it was written, and nothing here can
/// change what a legitimate one means.
///
/// A directive is refused when either of these is true:
///
/// - its target begins with a confined crate's name, so it can out-specify the entry holding
///   that crate down. Why that is `starts_with` and not a module-path test is above.
/// - it carries a span or field selector, which is to say it contains a `[`. Those are
///   resolved out of the span scope before any target directive is consulted, so one of them
///   enables every target at its level. The harmless spelling — a field *name* with no value,
///   which `EnvFilter` keeps among its static directives — is refused with the rest, because
///   telling the two apart means reimplementing that crate's grammar to decide a safety
///   question, and wider is the right direction to be wrong in here.
///
/// Everything else is honoured untouched, a bare level included: `NYSIA_LOG=trace` names no
/// target, and a directive with no target is the least specific thing `EnvFilter` has, so
/// every entry in [`CONFINED_TARGETS`] already beats it.
///
/// This screens rather than rewrites. A refused directive is dropped whole and never edited
/// down to something safe, because a filter the person did not write is a filter they cannot
/// reason about — and the caller can then name the string it would not honour.
#[must_use]
pub fn screen_directives(asked: &str) -> Screened<'_> {
    let (refused, honoured): (Vec<&str>, Vec<&str>) = asked
        .split(',')
        .filter(|directive| !directive.is_empty())
        .partition(|directive| reaches_a_confined_target(directive));
    Screened { honoured, refused }
}

/// What to tell the reader about a directive [`screen_directives`] refused.
///
/// One wording, so that the daemon and the window cannot come to explain the same refusal
/// differently — which is how a reader decides that one of them must have meant something
/// else. Where each process says it is each process's own business: `filter` in `nysia`'s
/// `log` module writes it to stderr, which is that process's log; `install` in
/// `nysia-desktop`'s writes it through `tracing` once there is a subscriber, because a
/// packaged window has no stderr to write it to.
///
/// It has to carry [`UNCONFINED_ENV`]'s name. A refusal with no way out in it leaves somebody
/// holding a directive that silently does nothing, which is the state the screen is supposed
/// to improve on rather than to create.
#[must_use]
pub fn refusal_note(directive: &str) -> String {
    format!(
        "{LOG_ENV}: `{directive}` was not applied. It could switch off the rule that keeps \
         terminal bytes and a caller's paths out of this log. Set {UNCONFINED_ENV}=1 as well \
         to have it anyway, and then do not share the file."
    )
}

/// What to tell the reader when [`UNCONFINED_ENV`] is set.
///
/// Said into the log it is about, before anything else is written there, so the file states
/// what it is rather than leaving that to whoever finds it later.
#[must_use]
pub fn unconfined_note() -> String {
    format!(
        "{UNCONFINED_ENV} is set. {LOG_ENV} is being honoured in full, so this log can carry \
         terminal output, a window title, an environment block and a caller's paths. It is \
         not a file to attach to an issue."
    )
}

/// Whether `directive` could enable a callsite [`CONFINED_TARGETS`] is holding down.
///
/// Trimmed first, and that is not cosmetic: the check has to see at least as much of the
/// target as `EnvFilter` will, and leading space is the one way a directive could otherwise
/// look like it names something else.
fn reaches_a_confined_target(directive: &str) -> bool {
    let directive = directive.trim();
    if directive.contains('[') {
        return true;
    }
    let target = directive
        .split_once('=')
        .map_or(directive, |(target, _level)| target)
        .trim();
    confined_crates().any(|krate| target.starts_with(krate))
}

/// The crate each [`CONFINED_TARGETS`] entry names.
///
/// Derived from the list rather than written down beside it, so a fourth entry is screened on
/// the day it is added and there is no second roster to keep in step with the first.
/// `the_screen_refuses_a_directive_under_every_confined_crate` is what holds that.
fn confined_crates() -> impl Iterator<Item = &'static str> {
    CONFINED_TARGETS.iter().copied().map(|directive| {
        directive
            .split_once('=')
            .map_or(directive, |(krate, _)| krate)
    })
}

/// How large one log file may get before it is rotated: 8 MiB.
///
/// Large enough that an ordinary day of `info` never rotates at all, small enough that a
/// person can open the file in an editor when it has.
pub const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// How many rotated copies are kept beside the live log: 3.
///
/// Named `<stem>.log.1` through `<stem>.log.3`, oldest last. `.3` is dropped when a fourth
/// rotation arrives.
pub const KEPT_ROTATIONS: usize = 3;

/// Every log file for one endpoint, at the cap: 32 MiB.
///
/// Derived, never written down. A reader who wants to know what Nysia costs on disk should be
/// able to get the answer without multiplying two numbers that might have drifted apart.
///
/// **A working figure, not a hard limit.** The cap is measured on the caller's schedule, so a
/// file that was written to hard between two checks is rotated at whatever size it had
/// reached, and the copy kept beside it is that size. The real disk cost is this plus one
/// check interval's output per file, and it is quoted here as the figure that holds whenever
/// the trim keeps up — which, against 8 MiB and half a minute, it does.
pub const MAX_TOTAL_LOG_BYTES: u64 = MAX_LOG_BYTES * (KEPT_ROTATIONS as u64 + 1);

/// At least one rotation, or a trim would discard the 8 MiB it was called to preserve.
const _: () = assert!(KEPT_ROTATIONS >= 1);

/// What a [`trim`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trimmed {
    /// The log was within the cap, or there was no log. Nothing moved.
    Untouched,
    /// The log was over the cap: its contents were moved into the first rotation and the
    /// live file was truncated in place.
    Rotated {
        /// How many bytes were moved out of the live log.
        bytes: u64,
    },
}

/// Where the `index`th rotation of `live` lives: `<live>.1`, `<live>.2`, and so on.
///
/// An appended component rather than an inserted one, so a rotation can never collide with
/// another runtime file: `nysiad-v1.log.1` is unmistakably a rotation of `nysiad-v1.log`,
/// where `nysiad-v1.1.log` sorts next to the lease and the lock and reads like a second
/// endpoint.
#[must_use]
pub fn rotation_path(live: &Path, index: usize) -> PathBuf {
    let mut name = live.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// Rotate `live` if it has grown past [`MAX_LOG_BYTES`], leaving it empty and still open.
///
/// Safe to call against a file a live process is appending to — that is the whole point, and
/// the reason the live file is truncated rather than renamed. See the module docs.
///
/// A missing log is [`Trimmed::Untouched`] rather than an error: a daemon started by hand has
/// its stderr on a terminal and may never create one.
///
/// # A caller may not be the writer
///
/// This trims *the log beside the endpoint*, whoever is writing it. A daemon started from a
/// shell is logging to that shell, not to the file, and will still trim a stale file left by
/// an earlier spawned daemon. That is harmless — the file is nobody's — but it is surprising
/// enough to be worth saying.
///
/// # Errors
///
/// Whatever the filesystem said about reading the live log's size, copying it aside or
/// truncating it. A failed rotation is not fatal to a caller: the log keeps growing, which is
/// the state this function exists to leave, not one it makes worse.
pub fn trim(live: &Path) -> io::Result<Trimmed> {
    trim_to(live, MAX_LOG_BYTES, KEPT_ROTATIONS)
}

/// [`trim`], with the policy passed in so a test does not have to write 8 MiB to see it work.
///
/// Private because there is one policy. A caller able to choose its own cap is a caller able
/// to choose `u64::MAX`, and a retention default an ordinary call site can undo is not a
/// default (CLAUDE.md §6).
fn trim_to(live: &Path, cap: u64, kept: usize) -> io::Result<Trimmed> {
    let bytes = match std::fs::metadata(live) {
        Ok(metadata) => metadata.len(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Trimmed::Untouched),
        Err(err) => return Err(err),
    };
    if bytes <= cap {
        return Ok(Trimmed::Untouched);
    }

    // Oldest first, so nothing is overwritten before it has been moved. `rename` replaces an
    // existing destination on both platforms, which is what drops what falls off the end.
    for index in (1..kept).rev() {
        let from = rotation_path(live, index);
        if from.exists() {
            std::fs::rename(&from, rotation_path(live, index + 1))?;
        }
    }

    // `io::copy` between handles this code opened itself, never `std::fs::copy`: on Windows
    // that is `CopyFileExW`, which opens the source with a share mode this caller cannot
    // choose — against a file a daemon is holding open for append.
    let first = rotation_path(live, 1);
    let mut source = File::open(live)?;
    let mut destination = File::create(&first)?;
    io::copy(&mut source, &mut destination)?;
    destination.sync_all()?;
    drop(destination);
    restrict_to_owner(&first);

    // The truncate, and the reason a rename would have been wrong. The daemon's inherited
    // append handle resolves its offset at every write, so its next line lands at offset 0 of
    // this same file.
    OpenOptions::new().write(true).open(live)?.set_len(0)?;
    Ok(Trimmed::Rotated { bytes })
}

/// Open `live` for appending, rotating it first if a previous run left it over the cap.
///
/// What [`crate::rpc::discovery`] hands a spawned daemon as its stdout and stderr, and what
/// the window opens for its own log. Append mode is load-bearing twice over: two runtimes
/// racing cannot truncate each other's output, and it is what makes [`trim`] work against a
/// process that is still writing.
///
/// # Errors
///
/// Whatever the filesystem said about opening the file. A failed *rotation* is not an error
/// here — the log is opened anyway, oversized, because refusing to start a daemon over a
/// large log file would turn a disk-space nuisance into an outage.
pub fn open_for_append(live: &Path) -> io::Result<File> {
    if let Err(err) = trim(live) {
        tracing::warn!(
            path = %live.display(),
            %err,
            "could not rotate the log before opening it; continuing with the one that is there"
        );
    }
    let file = OpenOptions::new().create(true).append(true).open(live)?;
    restrict_to_owner(live);
    Ok(file)
}

/// Put owner-only permissions on a log file.
///
/// Best effort, and a second layer rather than the only one: the runtime directory is already
/// `0700`, so a log inside it is unreachable by anyone else whatever its own mode says. This
/// is here because trap 13 is about what happens when one of those layers is wrong, and a
/// file that carries its own confinement survives being copied out of the directory that was
/// carrying it.
fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        // Windows inherits the runtime directory's ACL, which is already owner-only for
        // everything under `%LOCALAPPDATA%`.
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    /// A scratch directory of this module's own, named for one test.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nysia-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// The targets `screen_directives` is there to keep reachable-but-quiet, one per entry.
    ///
    /// Built from [`CONFINED_TARGETS`] rather than typed out, so this roster cannot fall
    /// behind the list the way a hand-written one would.
    fn a_module_under_each_confined_crate() -> Vec<String> {
        confined_crates()
            .map(|krate| format!("{krate}::somewhere=trace"))
            .collect()
    }

    #[test]
    fn the_screen_refuses_a_directive_under_every_confined_crate() {
        // The completeness check, and the reason no test below writes `vte` three times. A
        // fourth entry added to `CONFINED_TARGETS` is screened on the day it arrives, and if
        // `confined_crates` ever stopped deriving the crate name from the entry — by being
        // handed `vte=off` whole, say — every one of these would redden at once.
        let under = a_module_under_each_confined_crate();
        assert!(!under.is_empty(), "there is nothing to confine");

        for directive in &under {
            let screened = screen_directives(directive);
            assert_eq!(
                screened.refused,
                vec![directive.as_str()],
                "a module of a confined crate was honoured"
            );
            assert!(screened.honoured.is_empty(), "{directive} survived");
        }
    }

    #[test]
    fn a_target_that_only_extends_a_confined_crate_is_refused() {
        // The case that decides `starts_with` over a `::`-boundary test, and the one a
        // boundary test would let through. `vte:` is not a module path and names nothing,
        // but `"vte::ansi".starts_with("vte:")` is true and four characters out-specify
        // three, so `EnvFilter` would resolve `vte::ansi`'s callsite against it and hand
        // over every byte of an unhandled OSC.
        //
        // `a_module_qualified_nysia_log_does_not_reach_the_log` in `nysia`'s `log` module is
        // the end of this one: it sets exactly this string and reads the file back.
        for directive in ["vte:=trace", "vte::=trace", "portable_pty:=trace"] {
            assert_eq!(
                screen_directives(directive).refused,
                vec![directive],
                "{directive} out-specifies the entry that holds that crate down"
            );
        }
    }

    #[test]
    fn a_span_or_field_directive_is_refused_whatever_target_it_names() {
        // A different hole from the one above, not a wider version of it. `EnvFilter::enabled`
        // asks its dynamic directives first and returns `true` out of the span scope before a
        // single target directive is consulted — so one of these enables `vte::ansi` at trace
        // no matter how `CONFINED_TARGETS` is spelled, and even when it names a target that
        // has nothing to do with a confined crate.
        //
        // `a_span_directive_does_not_reach_the_log` in `nysia`'s `log` module is this one
        // measured: it enters the span and reads the log back, with and without the screen.
        for directive in [
            "[x]=trace",
            "nysia_core[x]=trace",
            "[{message}]=trace",
            "[serve{verb=session_create}]=trace",
        ] {
            assert_eq!(
                screen_directives(directive).refused,
                vec![directive],
                "{directive} reaches every target through the span scope"
            );
        }
    }

    #[test]
    fn a_bare_level_and_an_unrelated_target_are_honoured() {
        // The other way a confinement goes wrong: refusing so much that `NYSIA_LOG` stops
        // being worth setting. None of these can reach a confined callsite — a directive with
        // no target is the least specific thing `EnvFilter` has, and `vt` is shorter than
        // `vte`, so `vte=off` out-specifies both — and all of them are ordinary things to want.
        for directive in [
            "trace",
            "debug",
            "off",
            "nysia_core=debug",
            "nysia_core::rpc::server=trace",
            "vt=trace",
        ] {
            let screened = screen_directives(directive);
            assert_eq!(
                screened.honoured,
                vec![directive],
                "{directive} was refused and did not need to be"
            );
            assert!(screened.refused.is_empty(), "{directive} was refused");
        }
    }

    #[test]
    fn the_rest_of_a_value_survives_one_refusal() {
        // A refusal is not a reason to throw the whole variable away. Somebody debugging the
        // daemon wrote three directives and one of them cannot be honoured; the other two are
        // what they were actually trying to do, and they reach the filter spelled exactly as
        // they were written — `screen_directives` screens, it never rewrites.
        let screened = screen_directives("nysia_core=debug,vte::ansi=trace,nysia=trace");

        assert_eq!(screened.honoured, vec!["nysia_core=debug", "nysia=trace"]);
        assert_eq!(screened.refused, vec!["vte::ansi=trace"]);
    }

    #[test]
    fn an_empty_value_asks_for_nothing_and_is_refused_nothing() {
        // The shipped default: no `NYSIA_LOG` at all. Worth pinning because the callers turn
        // an empty `honoured` into `info`, and "empty because nothing was asked for" and
        // "empty because everything was refused" have to arrive at the same place.
        for asked in ["", ",", ",,"] {
            let screened = screen_directives(asked);
            assert!(screened.honoured.is_empty(), "{asked:?}");
            assert!(screened.refused.is_empty(), "{asked:?}");
        }
    }

    #[test]
    fn what_is_said_about_a_refusal_names_the_directive_and_the_way_round_it() {
        // The half of a refusal that is not the refusal. Somebody set `NYSIA_LOG` and part of
        // it did not happen; if the line that says so does not quote what was dropped and name
        // `NYSIA_LOG_UNCONFINED`, they are left where they would have been with no line at all
        // — except now convinced the log is broken.
        let note = refusal_note("vte::ansi=trace");
        assert!(note.contains("vte::ansi=trace"), "{note}");
        assert!(note.contains(LOG_ENV), "{note}");
        assert!(note.contains(UNCONFINED_ENV), "{note}");

        let lifted = unconfined_note();
        assert!(lifted.contains(UNCONFINED_ENV), "{lifted}");
        assert!(lifted.contains(LOG_ENV), "{lifted}");
    }

    #[test]
    fn the_confinement_is_not_lifted_unless_its_own_variable_is_set() {
        // The escape hatch is off by default, which is the whole of what makes it a hatch
        // rather than a second door. If this fails on your machine, `NYSIA_LOG_UNCONFINED` is
        // exported in the shell that ran it — which is exactly what it claims to do, and the
        // logs that process writes are not ones to hand anybody.
        assert!(
            !confinement_lifted(),
            "{UNCONFINED_ENV} is set in this environment"
        );
    }

    #[test]
    fn the_ceiling_is_the_product_of_the_two() {
        // The numbers the module docs quote. Pinned to literals rather than to each other,
        // because "the doc says 8 MiB and the constant says 80 MiB" is precisely the drift a
        // test written as `assert_eq!(MAX_LOG_BYTES, MAX_LOG_BYTES)` would not catch.
        assert_eq!(MAX_LOG_BYTES, 8 * 1024 * 1024);
        assert_eq!(KEPT_ROTATIONS, 3);
        assert_eq!(MAX_TOTAL_LOG_BYTES, 32 * 1024 * 1024);
        assert_eq!(
            MAX_TOTAL_LOG_BYTES,
            MAX_LOG_BYTES * (KEPT_ROTATIONS as u64 + 1)
        );
    }

    #[test]
    fn a_log_within_the_cap_is_left_alone() {
        let dir = scratch("under");
        let live = dir.join("nysiad.log");
        std::fs::write(&live, "one line\n").expect("a log");

        assert_eq!(trim_to(&live, 64, 3).expect("a trim"), Trimmed::Untouched);
        assert_eq!(read(&live), "one line\n");
        assert!(!rotation_path(&live, 1).exists());
    }

    #[test]
    fn a_log_exactly_at_the_cap_is_left_alone() {
        // The boundary is `>`, not `>=`: a file that is exactly the size it is allowed to be
        // has not broken the rule, and rotating it would spend a copy for nothing.
        let dir = scratch("boundary");
        let live = dir.join("nysiad.log");
        std::fs::write(&live, vec![b'x'; 64]).expect("a log");

        assert_eq!(trim_to(&live, 64, 3).expect("a trim"), Trimmed::Untouched);
        assert_eq!(
            trim_to(&live, 63, 3).expect("a trim"),
            Trimmed::Rotated { bytes: 64 }
        );
    }

    #[test]
    fn a_missing_log_is_not_an_error() {
        // A daemon started by hand logs to the terminal it was started from and may never
        // create this file. The tick that trims it runs regardless.
        let dir = scratch("absent");
        assert_eq!(
            trim(&dir.join("never-written.log")).expect("a trim"),
            Trimmed::Untouched
        );
    }

    #[test]
    fn rotation_survives_a_writer_holding_the_file_open() {
        // **The test the whole design exists to pass.** The daemon's stdout and stderr are an
        // inherited handle onto this file, so the rotation happens underneath a process that
        // is still appending — and a handle names the file, not the path.
        //
        // A rename-based rotation fails here in the way that matters: the rename succeeds,
        // `writer` goes on writing into the *rotated* file because that is the file its handle
        // names, and the live log stays empty for the rest of the daemon's life. The log would
        // appear to stop the first time it was ever rotated.
        let dir = scratch("openhandle");
        let live = dir.join("nysiad.log");

        let mut writer = open_for_append(&live).expect("an append handle");
        writer.write_all(b"before the rotation\n").expect("a write");
        writer.flush().expect("a flush");

        let trimmed = trim_to(&live, 8, KEPT_ROTATIONS).expect("a trim");
        assert_eq!(trimmed, Trimmed::Rotated { bytes: 20 });

        // The same handle, never reopened — exactly what a running daemon holds.
        writer.write_all(b"after the rotation\n").expect("a write");
        writer.flush().expect("a flush");

        assert_eq!(read(&live), "after the rotation\n");
        assert_eq!(read(&rotation_path(&live, 1)), "before the rotation\n");
    }

    #[test]
    fn the_oldest_rotation_is_dropped() {
        let dir = scratch("oldest");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");

        // One rotation per generation, so each rotated file holds a line naming itself.
        for generation in 0..(KEPT_ROTATIONS + 2) {
            writer
                .write_all(format!("generation {generation}\n").as_bytes())
                .expect("a write");
            writer.flush().expect("a flush");
            assert!(matches!(
                trim_to(&live, 4, KEPT_ROTATIONS).expect("a trim"),
                Trimmed::Rotated { .. }
            ));
        }

        // The newest rotation holds the last generation written, the oldest kept holds the
        // one `KEPT_ROTATIONS - 1` before it, and nothing exists past the cap.
        let newest = KEPT_ROTATIONS + 1;
        for index in 1..=KEPT_ROTATIONS {
            assert_eq!(
                read(&rotation_path(&live, index)),
                format!("generation {}\n", newest - (index - 1))
            );
        }
        assert!(
            !rotation_path(&live, KEPT_ROTATIONS + 1).exists(),
            "a {}th rotation was kept when the policy keeps {KEPT_ROTATIONS}",
            KEPT_ROTATIONS + 1
        );
    }

    #[test]
    fn a_trim_empties_the_live_log_and_keeps_only_the_policy() {
        // What a trim actually guarantees, stated as the two things that are true however
        // hard the writer was writing: the live file is left empty, and no more than
        // `KEPT_ROTATIONS` copies survive.
        //
        // What it deliberately does *not* assert is that every file is inside the cap. A
        // rotation is a copy of the live file at the moment it was measured, so a burst
        // between two checks is carried across whole — the alternative would be truncating a
        // rotation mid-line, which throws away log to satisfy an arithmetic claim nobody
        // needed. The overshoot is named in `MAX_TOTAL_LOG_BYTES` rather than hidden here.
        let dir = scratch("ceiling");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");

        let cap = 16;
        let burst = 24;
        for _ in 0..(KEPT_ROTATIONS + 3) {
            writer.write_all(&vec![b'x'; burst]).expect("a write");
            writer.flush().expect("a flush");
            trim_to(&live, cap, KEPT_ROTATIONS).expect("a trim");

            assert_eq!(
                std::fs::metadata(&live).expect("a live log").len(),
                0,
                "a trim left bytes in the live log"
            );
        }

        let kept: Vec<usize> = (1..=KEPT_ROTATIONS + 4)
            .filter(|index| rotation_path(&live, *index).exists())
            .collect();
        assert_eq!(
            kept,
            (1..=KEPT_ROTATIONS).collect::<Vec<_>>(),
            "the rotations on disk are not the {KEPT_ROTATIONS} the policy keeps"
        );

        // The bound that does hold: each file carries one measurement's worth, so the total
        // is the policy's ceiling plus the overshoot the schedule allowed.
        let total: u64 = kept
            .iter()
            .filter_map(|index| std::fs::metadata(rotation_path(&live, *index)).ok())
            .map(|metadata| metadata.len())
            .sum();
        let ceiling = (cap + burst as u64) * KEPT_ROTATIONS as u64;
        assert!(
            total <= ceiling,
            "every log together is {total} bytes against {ceiling}"
        );
    }

    #[test]
    fn the_real_cap_is_the_one_trim_applies() {
        // `trim` against the shipped constants, so the small-cap tests above cannot all pass
        // while `trim` itself is wired to the wrong number. Sparse: `set_len` allocates no
        // blocks on NTFS or APFS, so this costs a copy and not a write of 8 MiB.
        let dir = scratch("realcap");
        let live = dir.join("nysiad.log");

        File::create(&live)
            .expect("a log")
            .set_len(MAX_LOG_BYTES)
            .expect("a size");
        assert_eq!(trim(&live).expect("a trim"), Trimmed::Untouched);

        OpenOptions::new()
            .append(true)
            .open(&live)
            .expect("the log")
            .write_all(b"the byte that crosses the cap")
            .expect("a write");
        assert!(matches!(
            trim(&live).expect("a trim"),
            Trimmed::Rotated { .. }
        ));
        assert_eq!(
            std::fs::metadata(&live).expect("the live log").len(),
            0,
            "the live log was not truncated"
        );
    }

    #[test]
    fn opening_for_append_rotates_what_a_previous_run_left_behind() {
        // The other half of the cap. A daemon that died over the cap leaves a file the next
        // spawn would otherwise append to for ever.
        let dir = scratch("reopen");
        let live = dir.join("nysiad.log");
        File::create(&live)
            .expect("a log")
            .set_len(MAX_LOG_BYTES + 1)
            .expect("a size");

        let mut opened = open_for_append(&live).expect("an append handle");
        opened.write_all(b"this run\n").expect("a write");
        opened.flush().expect("a flush");

        assert_eq!(read(&live), "this run\n");
        assert_eq!(
            std::fs::metadata(rotation_path(&live, 1))
                .expect("the rotation")
                .len(),
            MAX_LOG_BYTES + 1
        );
    }

    #[test]
    fn a_rotation_is_named_beside_the_log_it_came_from() {
        let live = Path::new("/tmp/runtime/nysiad-v1.log");
        assert_eq!(
            rotation_path(live, 2),
            Path::new("/tmp/runtime/nysiad-v1.log.2")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_log_and_its_rotations_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("modes");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");
        writer.write_all(b"secret-adjacent\n").expect("a write");
        writer.flush().expect("a flush");
        trim_to(&live, 4, KEPT_ROTATIONS).expect("a trim");

        for path in [live.clone(), rotation_path(&live, 1)] {
            let mode = std::fs::metadata(&path)
                .expect("a log")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "{} is mode {:o}, which is not owner-only",
                path.display(),
                mode & 0o777
            );
        }
    }

    #[test]
    fn a_partial_read_of_the_live_log_is_still_whole_lines() {
        // Not a property the implementation guarantees, and this test says which half it
        // does: the *rotation* is a byte-for-byte copy of what the live log held, so a reader
        // can concatenate `.1` and the live file and get the stream back in order.
        let dir = scratch("order");
        let live = dir.join("nysiad.log");
        let mut writer = open_for_append(&live).expect("an append handle");
        writer.write_all(b"first\nsecond\n").expect("a write");
        writer.flush().expect("a flush");
        trim_to(&live, 4, KEPT_ROTATIONS).expect("a trim");
        writer.write_all(b"third\n").expect("a write");
        writer.flush().expect("a flush");

        let mut rotated = String::new();
        File::open(rotation_path(&live, 1))
            .expect("the rotation")
            .read_to_string(&mut rotated)
            .expect("its bytes");
        assert_eq!(rotated + &read(&live), "first\nsecond\nthird\n");
    }
}
