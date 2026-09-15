import { describe, expect, it } from 'vitest';

import type { DaemonBridge } from './bridge';
import { createTransportLog, SILENT_TRANSPORT_LOG, type TransportEvent } from './log';

/** A bridge that records what was invoked and can be told to reject. */
function fakeBridge(options?: { readonly reject?: boolean }): {
  readonly bridge: DaemonBridge;
  readonly calls: { command: string; args?: Record<string, unknown> }[];
} {
  const calls: { command: string; args?: Record<string, unknown> }[] = [];
  const bridge: DaemonBridge = {
    invoke: <T,>(command: string, args?: Record<string, unknown>): Promise<T> => {
      calls.push({ command, ...(args === undefined ? {} : { args }) });
      return options?.reject === true
        ? Promise.reject(new Error('the socket has gone'))
        : (Promise.resolve(undefined) as Promise<T>);
    },
    attachChannel: async () => {},
    window: {
      minimize: async () => {},
      toggleMaximize: async () => {},
      close: async () => {},
    },
  };
  return { bridge, calls };
}

describe('the transport log', () => {
  it('sends a name and two numbers, and nothing else', () => {
    // The shape is the whole security property. There is no message parameter, so there is
    // no argument at which scrollback, a keystroke or a tool's input could be substituted
    // (trap 13) — and this asserts on the *exact* argument object rather than on the fields
    // it cares about, so adding a free-text one fails here rather than passing quietly.
    const { bridge, calls } = fakeBridge();
    createTransportLog(bridge).record('replay_timeout', { stream: 7, count: 42 });

    expect(calls).toEqual([
      {
        command: 'client_log',
        args: { event: 'replay_timeout', stream: 7, count: 42 },
      },
    ]);
  });

  it('sends an explicit null for a detail it was not given', () => {
    // The Rust side takes `Option<u32>` and `Option<u64>`. serde reads an explicit null as
    // `None` wherever it runs; a missing key depends on how the arguments were serialised,
    // which is not a thing to be depending on across an IPC boundary.
    const { bridge, calls } = fakeBridge();
    createTransportLog(bridge).record('channel_unreadable');

    expect(calls[0]?.args).toEqual({
      event: 'channel_unreadable',
      stream: null,
      count: null,
    });
  });

  it('never rejects, and never reports its own failure', async () => {
    // The failures worth logging are the ones where the transport is already broken, which
    // is exactly when this call is most likely to fail. A logger that threw there would turn
    // a diagnostic into a second failure on a path that has nowhere to put one — `record` is
    // called from a Channel callback, a timeout and a render path.
    const { bridge, calls } = fakeBridge({ reject: true });
    const log = createTransportLog(bridge);

    expect(() => {
      log.record('dropped_while_hidden', { stream: 1, count: 8192 });
    }).not.toThrow();

    // The rejection is settled and swallowed rather than left to the runtime — and nothing
    // was logged about the logging, which is the recursion this is one line away from.
    await Promise.resolve();
    expect(calls).toHaveLength(1);
  });

  it('writes nowhere when it is the silent one', () => {
    // The default for anything constructed without a logger, so a test fake does not have to
    // grow a bridge method for a dependency it does not care about.
    expect(() => {
      SILENT_TRANSPORT_LOG.record('replay_timeout', { stream: 1, count: 1 });
    }).not.toThrow();
  });

  it('names only events the Rust allowlist accepts', () => {
    // The union here is a convenience; `WEBVIEW_EVENTS` in `commands.rs` is the gate, and a
    // name that is not on it is dropped there. This is the reminder that the two are meant
    // to say the same thing — and that widening this one alone changes nothing, because the
    // check lives in the other process on purpose (CLAUDE.md §6).
    const named: TransportEvent[] = [
      'channel_unreadable',
      'replay_timeout',
      'dropped_while_hidden',
    ];
    expect(named).toHaveLength(3);
  });
});
