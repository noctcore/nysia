//! Turning a daemon failure into the envelope a caller can act on.
//!
//! §6.2 makes next steps non-optional, and `nysia-proto` makes them non-optional in the type
//! system: [`NextSteps`] cannot be built empty and [`ErrorEnvelope::new`] will not take
//! anything else. The reason is worth restating, because it is the difference between a
//! failed command and a failed session: an agent told "invalid argument" and nothing else
//! does not stop. It guesses, the guess is a plausible flag that does not exist, and the cost
//! is a loop rather than one error.
//!
//! Everything in this module exists so that no error path in the daemon can skip that.

use nysia_proto::{ErrorCode, ErrorEnvelope, NextSteps};

/// The step used when a caller hands in a blank one.
///
/// [`NextSteps::new`] refuses a blank step, and this is the one place in the daemon where
/// that refusal would have nowhere to go — an error envelope that cannot be built is an error
/// the caller never sees at all. So a blank first step is replaced rather than refused.
const LAST_RESORT: &str = "run `nysia --help` to see the verb's flags";

/// Build the next steps for an error, guaranteeing a non-empty list.
///
/// `first` is replaced by [`LAST_RESORT`] when it is blank, which is the only input
/// [`NextSteps::new`] rejects — so by the time the constructor runs, it cannot fail.
#[must_use]
pub(crate) fn steps(first: &str, rest: &[&str]) -> NextSteps {
    let first = if first.trim().is_empty() {
        LAST_RESORT
    } else {
        first
    };
    let mut steps = match NextSteps::new(first) {
        Ok(steps) => steps,
        // Unreachable: `first` is non-blank by the three lines above, and blank is the whole
        // of what `NextSteps::new` checks. Spelled out rather than `expect`ed so that the
        // proof lives next to the code it is about.
        Err(_) => unreachable!("a non-blank first step is the only thing NextSteps::new asks"),
    };
    for step in rest {
        steps = steps.and(*step);
    }
    steps
}

/// An error envelope, with its steps.
#[must_use]
pub(crate) fn envelope(
    code: ErrorCode,
    message: impl Into<String>,
    first: &str,
    rest: &[&str],
) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, steps(first, rest))
}

/// What a daemon-side failure knows about itself.
///
/// Implemented by every error type that can reach a caller, so the dispatcher never has to
/// ask "what code is this one" — the error answers for itself, next to its own definition,
/// where the answer is obvious and stays right when a variant is added.
pub(crate) trait IntoEnvelope {
    /// The envelope this failure answers with.
    fn into_envelope(self) -> ErrorEnvelope;
}

/// A store failure, as the caller of a status verb meets it.
///
/// This impl lives here rather than beside [`crate::store::StoreError`] — which is where the
/// others live, and where this one belongs — because `store/**` is another worker's file. The
/// trait is this crate's, so the orphan rule permits it; the reason it is not next to its
/// type is ownership, not design, and it should move when the two are in one hand.
///
/// **Nothing in the message carries a row.** `StoreError`'s own variants name an action and a
/// path and never a payload, and a `waiting` row's question is a secret the same way
/// scrollback is (trap 13).
impl IntoEnvelope for crate::store::StoreError {
    fn into_envelope(self) -> ErrorEnvelope {
        envelope(
            ErrorCode::Internal,
            self.to_string(),
            "retry the verb; a hook that cannot be recorded is spooled and drained at the \
             next daemon start",
            &[
                "check that the daemon's runtime directory is writable and not full",
                "see docs/plans/v0.2-delivery-plan.md §2.3 for what the spool guarantees",
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_failure_says_what_happens_to_the_status_that_was_lost() {
        // §6.2 at the layer a hook meets: a caller told only "it failed" cannot tell whether
        // the status is gone or merely late, and those need opposite reactions.
        let envelope = crate::store::StoreError::Poisoned.into_envelope();
        assert_eq!(envelope.code(), &ErrorCode::Internal);
        assert!(
            envelope
                .next_steps()
                .iter()
                .any(|step| step.contains("spooled")),
            "got {:?}",
            envelope.next_steps()
        );
    }

    #[test]
    fn every_envelope_carries_at_least_one_step() {
        let built = envelope(
            ErrorCode::Internal,
            "something went wrong",
            "try again",
            &[],
        );
        assert_eq!(built.next_steps(), ["try again"]);
    }

    #[test]
    fn a_blank_first_step_is_replaced_rather_than_losing_the_error() {
        // The one input `NextSteps::new` refuses. Losing the whole envelope over it would
        // mean the caller learns nothing at all, which is strictly worse than a generic step.
        for blank in ["", "   ", "\t\n"] {
            let built = envelope(ErrorCode::Internal, "boom", blank, &["and then this"]);
            assert_eq!(built.next_steps(), [LAST_RESORT, "and then this"]);
        }
    }

    #[test]
    fn blank_later_steps_are_dropped_without_dropping_the_list() {
        let built = envelope(ErrorCode::Internal, "boom", "first", &["", "second", "  "]);
        assert_eq!(built.next_steps(), ["first", "second"]);
    }
}
