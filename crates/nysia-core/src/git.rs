//! The single git chokepoint.
//!
//! Every `git` spawn in Nysia goes through this module (D-15). One place to harden means
//! one place to audit: argument construction, the confinement check on the working
//! directory, timeouts, and the credential environment.
//!
//! The daemon also watches each repository and *pushes* status and diff to clients rather
//! than answering polls, so that the UI never blocks on a git invocation.
//!
//! Owned by wave 2 (W4).
