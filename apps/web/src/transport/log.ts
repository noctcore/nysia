import type { DaemonBridge } from './bridge';
import type { StreamId } from './frames';

/**
 * The three things only the webview can see, on their way to the window's log file.
 *
 * ## Why this is three events and not thirty
 *
 * The Rust side of the window logs its own failures from typed values, in a process the
 * webview cannot reach into: `commands.rs` records which verb was in flight and how it ended,
 * and `daemon/stream.rs` records a corrupt framing, a credit frame that would not parse, an
 * incoherent grant and a frame discarded for a detached stream.
 *
 * So this module is deliberately the **residue** — what is left once the failures Rust can
 * see are recorded where Rust can see them. Each of these three happens after the last byte
 * Rust is standing near:
 *
 * - `channel_unreadable` — Rust wrote well-formed frames and the decoder on this side could
 *   not resynchronise on what arrived. Whatever happened, happened in the delivery.
 * - `replay_timeout` — the daemon's `replay_end` marker never arrived, so a pane opened its
 *   input gate on the deadline and the keystrokes typed in the meantime are gone.
 * - `dropped_while_hidden` — a background pane's ring overflowed and its screen was reset.
 *
 * ## The names are not this module's to choose
 *
 * `client_log` in `nysia-desktop` holds the allowlist, and a name that is not on it is
 * dropped. That list is on the Rust side on purpose: a webview that could name its own event
 * could put anything in one, and a check that lives in the same process as the caller is a
 * suggestion rather than a gate (CLAUDE.md §6). The union below is the convenience — the
 * refusal is what enforces it.
 *
 * ## What may be said about an event
 *
 * A name from that list, a stream id and a count. **There is no message field and there will
 * not be one.** Free text on a log path is where a payload lands, and scrollback can carry
 * secrets — as can a `question`, which is a tool's input verbatim (trap 13). Every event so
 * far has been expressible as a name plus two numbers; one that is not should be argued about
 * before it is added, not accommodated by widening the type.
 */

/** One of the three events `client_log` will accept. */
export type TransportEvent =
  | 'channel_unreadable'
  | 'replay_timeout'
  | 'dropped_while_hidden';

/** What may accompany an event: a stream id, and a count of whatever was lost. */
export interface TransportEventDetail {
  readonly stream?: StreamId;
  readonly count?: number;
}

/** Somewhere to record a transport event. */
export interface TransportLog {
  /**
   * Record that `event` happened.
   *
   * Returns nothing and never throws. A logger a caller has to handle failures from is a
   * logger that turns a diagnostic into a second failure, and every call site here is
   * already on a path where something has gone wrong.
   */
  record(event: TransportEvent, detail?: TransportEventDetail): void;
}

/**
 * A log that writes nowhere.
 *
 * The default for anything constructed without one, so the store and the router take a
 * logger as an option rather than as a requirement: every existing test fake would otherwise
 * have to grow a method to keep compiling, for a dependency most of them do not care about.
 */
export const SILENT_TRANSPORT_LOG: TransportLog = {
  record: () => {},
};

/**
 * The real log: `client_log` on the Rust side, and from there the window's log file.
 *
 * **Fire and forget.** The promise is dropped and its rejection swallowed, and neither is an
 * oversight. `record` is called from a Channel callback, a timeout and a render path — none
 * of which has anywhere to put a rejection — and the failures worth logging are the ones
 * where the transport is already broken, which is exactly when this call is most likely to
 * fail. A rejection here is also never itself logged: that is the recursion this module would
 * otherwise be one line away from.
 */
export function createTransportLog(bridge: DaemonBridge): TransportLog {
  return {
    record: (event, detail) => {
      // `null` rather than omitted: the Rust side takes `Option<u32>` and `Option<u64>`, and
      // serde reads an explicit null as `None` on every platform, where a missing key depends
      // on how the arguments were serialised.
      void bridge
        .invoke('client_log', {
          event,
          stream: detail?.stream ?? null,
          count: detail?.count ?? null,
        })
        .catch(() => {
          // Deliberately empty. See the doc comment: there is nowhere for this to go, and
          // reporting it would put a notice in front of the user about the logging rather
          // than about the thing that was being logged.
        });
    },
  };
}
