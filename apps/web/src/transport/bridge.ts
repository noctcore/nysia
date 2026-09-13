import { Channel, invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

import type { WindowControls } from '../store/types';

/**
 * The only place `apps/web` touches Tauri.
 *
 * `src/transport` is the one directory the ESLint ban set and `tools/lint-meta` allow to
 * import `@tauri-apps` (D-1, D-2), and within it this module is the whole surface: an
 * `invoke`, a `Channel`, and the three window verbs. Everything else — the decoder, the
 * credit ledger, the surfaces, the store — takes a {@link DaemonBridge} and therefore runs
 * under node-only vitest against a fake (D-18).
 *
 * That is not a testing convenience so much as the architecture's own claim, made
 * checkable: the window is a client with no privileged path, and a module list this short
 * is what lets anyone verify it in a minute.
 */

/** What the store needs of the Rust side. */
export interface DaemonBridge {
  /** Invoke a Tauri command. Rejects with whatever the command returned as its error. */
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  /**
   * Hand the Rust side the one multiplexed Channel and start receiving deliveries.
   *
   * Called once per connection. Rust replaces any channel already attached, which is what
   * a webview reload produces.
   */
  attachChannel(onDelivery: (bytes: Uint8Array) => void): Promise<void>;
  /** The three custom window controls. */
  readonly window: WindowControls;
}

/** How a Tauri command reports a failure, mirroring `daemon::CommandFailure` in Rust. */
export interface CommandFailure {
  readonly message: string;
  readonly nextSteps: readonly string[];
  readonly retryable: boolean;
}

/**
 * A sentence to show the user, from whatever a rejected `invoke` produced.
 *
 * The daemon's error envelope carries `nextSteps` precisely so that a failure reaches a
 * person as something they can act on rather than a status code, and this is the last place
 * that can throw it away. `invoke` can also reject with a plain string — a Tauri-level
 * failure before the command ran — so both shapes are handled rather than one assumed.
 */
export function describeFailure(cause: unknown): string {
  if (isCommandFailure(cause)) {
    const step = cause.nextSteps[0];
    return step === undefined ? cause.message : `${cause.message} — ${step}`;
  }
  if (typeof cause === 'string' && cause.length > 0) {
    return cause;
  }
  if (cause instanceof Error) {
    return cause.message;
  }
  return 'The daemon could not be reached.';
}

/** Whether another attempt could plausibly work. */
export function isRetryable(cause: unknown): boolean {
  // A failure of an unrecognised shape is treated as retryable, deliberately: giving up on
  // something this code does not understand strands the window with no way back, while
  // retrying costs one round trip on a backoff.
  return isCommandFailure(cause) ? cause.retryable : true;
}

function isCommandFailure(cause: unknown): cause is CommandFailure {
  if (typeof cause !== 'object' || cause === null) {
    return false;
  }
  const candidate = cause as Partial<CommandFailure>;
  return (
    typeof candidate.message === 'string' &&
    Array.isArray(candidate.nextSteps) &&
    typeof candidate.retryable === 'boolean'
  );
}

/**
 * The real bridge.
 *
 * The window controls go through `@tauri-apps/api/window` rather than through commands of
 * our own: `core:window:allow-minimize`, `allow-toggle-maximize` and `allow-close` are
 * already granted in `capabilities/default.json`, and adding Rust commands that forward to
 * the same three calls would be a second code path with nothing to say for itself.
 *
 * The design uses custom chrome on every platform (decorations are off in
 * `tauri.conf.json`), so these three are the *only* way to minimise, maximise or close.
 * They are on the store rather than in the titlebar because no component outside this
 * directory may import Tauri.
 */
export function createTauriBridge(): DaemonBridge {
  return {
    invoke: <T,>(command: string, args?: Record<string, unknown>): Promise<T> =>
      invoke<T>(command, args),

    attachChannel: async (onDelivery) => {
      const channel = new Channel<ArrayBuffer | Uint8Array | number[]>();
      channel.onmessage = (delivery) => {
        onDelivery(asBytes(delivery));
      };
      await invoke('terminal_attach', { channel });
    },

    window: {
      minimize: () => getCurrentWindow().minimize(),
      toggleMaximize: () => getCurrentWindow().toggleMaximize(),
      close: () => getCurrentWindow().close(),
    },
  };
}

/**
 * Normalise whatever the Channel handed over into bytes.
 *
 * Tauri delivers a raw response body as an `ArrayBuffer` on the fetch path and can hand
 * back a plain number array on the `eval` path — which is exactly the path §7.3's
 * coalescing exists to avoid, but a small frame after a quiet 16 ms still takes it. A
 * decoder that assumed one shape would work perfectly on a busy session and fail on an idle
 * one, which is the worst way round.
 */
function asBytes(delivery: ArrayBuffer | Uint8Array | number[]): Uint8Array {
  if (delivery instanceof Uint8Array) {
    return delivery;
  }
  if (delivery instanceof ArrayBuffer) {
    return new Uint8Array(delivery);
  }
  return Uint8Array.from(delivery);
}
