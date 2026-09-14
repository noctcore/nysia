import { describe, expect, it } from 'vitest';

import type { SessionSummary as WireSession } from '../generated/SessionSummary';
import { CREDIT_WINDOW_DEFAULT } from '../generated/wireConstants';
import { StoreCommandError } from '../store/errors';
import { describeStoreContract } from '../store/storeContract';
import type { CommandFailure, DaemonBridge } from './bridge';
import { DaemonStore } from './DaemonStore';
import type { StreamId } from './frames';
import type { TerminalFactory, XtermLike } from './surface/XtermSurface';
import { TerminalRouter } from './terminals';
import { encodeFrame, encodeHeaderOnly } from './testFrames';

/**
 * A daemon that answers over an in-process bridge.
 *
 * W4's daemon is not in this tree yet, so the alternative was to ship the provider untested
 * and find out on integration. What it fakes is deliberately only the *socket*: the decoder,
 * the credit ledger, the surfaces and the store are all the real ones.
 *
 * ## It enforces the Rust side's preconditions
 *
 * This is the part an earlier version got wrong, and the cost was the worst defect in the
 * branch. `attachChannel` simply stored a callback and refused nothing, so a connect
 * sequence that asked the Rust side for something it could not yet give passed every test
 * here and could never reach a real daemon. A fake that cannot fail the way the real thing
 * fails proves that the code runs, not that it works — so each of these mirrors a refusal
 * `state.rs` actually makes:
 *
 *  - `terminal_attach` before `daemon_connect` → there is no connection to attach to;
 *  - `stream_attach` before `terminal_attach` → there is no stream connection to route on;
 *  - `stream_attach` for a session the daemon does not hold → unknown session.
 */
class FakeDaemon implements DaemonBridge {
  sessions: WireSession[] = [];
  /** Commands seen, in order, for the tests that assert a round trip happened. */
  readonly calls: { command: string; args?: Record<string, unknown> }[] = [];
  /** Set to make the next matching command reject. */
  failures = new Map<string, CommandFailure | string>();
  /**
   * Set to make the next `times` matching commands reject, and the ones after them work.
   *
   * What a daemon that is *coming up* looks like, which {@link failures} cannot express: one
   * refusal followed by success is indistinguishable from a loop that gave up and got lucky,
   * and the thing worth asserting is that the window is still asking on the fourth attempt.
   */
  readonly refusals = new Map<string, { failure: CommandFailure | string; times: number }>();
  /** Resolved to drop the connection, which is what `daemon_watch` returning means. */
  #dropped: (() => void) | null = null;
  #delivery: ((bytes: Uint8Array) => void) | null = null;
  connects = 0;
  attaches = 0;
  /** Whether a control connection exists, as `Client::connect` decides. */
  #connected = false;
  /** Ids handed out on this connection. Spent, never reissued — as proto requires. */
  #nextStream = 1;
  readonly streams = new Map<string, StreamId>();

  readonly window = {
    minimize: async (): Promise<void> => {},
    toggleMaximize: async (): Promise<void> => {},
    close: async (): Promise<void> => {},
  };

  async invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    this.calls.push(args === undefined ? { command } : { command, args });

    const failure = this.failures.get(command);
    if (failure !== undefined) {
      this.failures.delete(command);
      throw failure;
    }

    const refusal = this.refusals.get(command);
    if (refusal !== undefined && refusal.times > 0) {
      refusal.times -= 1;
      throw refusal.failure;
    }

    switch (command) {
      case 'daemon_connect':
        this.connects += 1;
        this.#connected = true;
        return undefined as T;
      case 'stream_attach': {
        this.attachCalls += 1;
        // Both refusals are real: `attach_session` needs a stream connection, and the daemon
        // cannot route a session it does not hold.
        if (this.attaches === 0) {
          throw disconnected('no stream connection is open');
        }
        const handle = args?.handle;
        if (!this.sessions.some((candidate) => candidate.handle === handle)) {
          throw disconnected(`no session ${String(handle)}`);
        }
        const existing = this.streams.get(String(handle));
        if (existing !== undefined) {
          return existing as T;
        }
        const assigned = this.#nextStream;
        this.#nextStream += 1;
        this.streams.set(String(handle), assigned);
        return assigned as T;
      }
      case 'daemon_watch':
        return new Promise<T>((resolve) => {
          this.#dropped = () => resolve(undefined as T);
        });
      case 'host_platform':
        return 'windows' as T;
      case 'session_list':
        return this.sessions.map((session) => ({ ...session })) as T;
      case 'session_create': {
        const request = args?.request as { kind: 'agent' | 'shell' } | undefined;
        // Monotonic, never `sessions.length + 1`: a daemon does not reuse a pane key after a
        // session closes, and a fake that did would hide a stale entry behind a new session
        // that happened to land on the same key.
        this.#nextSession += 1;
        const created = session(this.#nextSession, request?.kind ?? 'shell');
        this.sessions.push(created);
        return created.handle as T;
      }
      case 'session_close': {
        const handle = args?.handle;
        this.sessions = this.sessions.filter((candidate) => candidate.handle !== handle);
        return undefined as T;
      }
      default:
        return undefined as T;
    }
  }

  async attachChannel(onDelivery: (bytes: Uint8Array) => void): Promise<void> {
    // `Client::attach_channel` refuses outright when no daemon is attached. Modelling that
    // is the whole point: without it, a connect sequence that attached before it connected
    // looked perfectly healthy here and could never work anywhere else.
    if (!this.#connected) {
      throw disconnected('the window is not connected to a daemon');
    }
    this.attaches += 1;
    this.#delivery = onDelivery;
  }

  /** Start from a set of sessions, without letting their numbers be handed out again. */
  seed(sessions: WireSession[]): void {
    this.sessions = sessions;
    this.#nextSession = sessions.length;
  }

  /** Push a delivery the way the one Channel would. */
  send(bytes: Uint8Array): void {
    this.#delivery?.(bytes);
  }

  /** How many `stream_attach` round trips this daemon has served, across all connections. */
  attachCalls = 0;
  /** Sessions ever created, so a pane key is never handed out twice. */
  #nextSession = 0;

  /** Pretend the socket dropped. */
  drop(): void {
    this.#connected = false;
    this.attaches = 0;
    // Both are properties of the *stream connection*, and it is gone. Proto scopes a
    // `StreamId` to one connection, and a fresh connection counts from the first id again —
    // modelling that is what makes a client which reuses ids across a reconnect fail here
    // rather than in front of a user.
    this.streams.clear();
    this.#nextStream = 1;
    const resolve = this.#dropped;
    this.#dropped = null;
    resolve?.();
  }
}

/** What the Rust side returns when a precondition is not met. */
function disconnected(message: string): CommandFailure {
  return { message, nextSteps: ['Start the Nysia daemon, then try again.'], retryable: true };
}

/** A wire session, numbered so a fixture reads clearly. */
function session(n: number, kind: 'agent' | 'shell' = 'shell'): WireSession {
  return {
    handle: `sess_0e2fa1f4-4f3e-4c5f-9f2a-1b2c3d4e5f${String(n).padStart(2, '0')}`,
    paneKey: `tab_${n}:leaf_1`,
    kind,
    title: kind === 'agent' ? `claude ${n}` : `pwsh ${n}`,
    createdAtMs: 1_757_721_600_000,
    exitStatus: null,
  };
}

/** A terminal that records, so no DOM is needed (D-18). */
function stubTerminals(): TerminalFactory {
  return (): XtermLike => ({
    element: undefined,
    open: () => {},
    write: (_data, done) => done?.(),
    resize: () => {},
    fit: () => undefined,
    focus: () => {},
    dispose: () => {},
    onData: () => ({ dispose: () => {} }),
  });
}

function build(seed = 2): { store: DaemonStore; daemon: FakeDaemon } {
  const daemon = new FakeDaemon();
  daemon.seed(
    Array.from({ length: seed }, (_, index) => session(index + 1, index === 0 ? 'agent' : 'shell')),
  );
  const store = new DaemonStore({
    bridge: daemon,
    router: new TerminalRouter({
      bridge: daemon,
      createTerminal: stubTerminals(),
      platform: 'windows',
    }),
    // No waiting in tests; the backoff itself is asserted separately.
    retryDelaysMs: [0],
  });
  void store.run();
  return { store, daemon };
}

/**
 * The contract every provider owes the chrome, run against this one.
 *
 * The same suite `MockStore` and `AsyncProbeStore` run. It reaching `ready` here is the
 * whole claim of deliverable 8: the chrome cannot tell which provider it is talking to.
 */
describeStoreContract('DaemonStore', () => build().store);

describe('what the daemon is authoritative for', () => {
  it('shows the sessions that outlived the last window', async () => {
    // D-1 made observable. The daemon has been running; this window has just started, and
    // everything in the strip came from the socket rather than from anything held here.
    const { store, daemon } = build(3);
    await ready(store);

    expect(store.getSnapshot().tabs).toHaveLength(3);
    expect(store.getSnapshot().tabs.map((tab) => tab.handle)).toEqual(
      daemon.sessions.map((s) => s.handle),
    );
    expect(store.getSnapshot().daemon.terminalCount).toBe(3);
  });

  it('opens a session through the daemon rather than inventing one', async () => {
    const { store, daemon } = build();
    await ready(store);
    const before = daemon.sessions.length;

    await store.openTab('shell.pwsh');
    expect(daemon.sessions).toHaveLength(before + 1);
    expect(
      daemon.calls.some(
        (call) =>
          call.command === 'session_create' &&
          (call.args?.request as { profile?: { shell?: string } })?.profile?.shell === 'pwsh',
      ),
      'the launcher must map to the wire profile',
    ).toBe(true);
  });

  it('closes a session through the daemon', async () => {
    const { store, daemon } = build(3);
    await ready(store);
    const closing = store.getSnapshot().tabs[0]?.paneKey ?? '';

    await store.closeTab(closing);
    expect(daemon.calls.some((call) => call.command === 'session_close')).toBe(true);
    expect(store.getSnapshot().tabs.some((tab) => tab.paneKey === closing)).toBe(false);
  });

  it('shows the daemon’s own sessions against the project, never seeded ones', async () => {
    // Showing four invented sessions beside a sidebar that claims to reflect a live daemon
    // is how a user learns to distrust everything else on the screen.
    const { store, daemon } = build(2);
    await ready(store);

    const active = store.getSnapshot().projects.find(
      (project) => project.id === store.getSnapshot().activeProjectId,
    );
    const shown = active?.worktrees[0]?.sessions ?? [];
    expect(shown.map((s) => s.handle)).toEqual(daemon.sessions.map((s) => s.handle));
  });

  it('keeps a session’s stream id stable when another one closes', async () => {
    // Renumbering on every refresh would route a surviving pane's output into the terminal
    // of whichever session happened to take its place.
    const { store } = build(3);
    await ready(store);
    const [, second, third] = store.getSnapshot().tabs;
    const before = store.surfaceStream(third?.paneKey ?? '');

    await store.closeTab(second?.paneKey ?? '');
    expect(store.surfaceStream(third?.paneKey ?? '')).toBe(before);
  });
});

describe('a command that cannot be satisfied', () => {
  it('carries the daemon’s next step to the user, not just its message', async () => {
    // The whole reason the error envelope has `nextSteps`. A window that showed only
    // "spawn failed" would have thrown away the one part a person can act on.
    const { store, daemon } = build();
    await ready(store);
    daemon.failures.set('session_create', {
      message: 'pwsh is not on PATH',
      nextSteps: ['Install PowerShell 7, or pick cmd from the + menu.'],
      retryable: false,
    });

    const rejection = await store.openTab('shell.pwsh').catch((cause: unknown) => cause);
    expect(rejection).toBeInstanceOf(StoreCommandError);

    const recorded = store.getSnapshot().errors.at(-1);
    expect(recorded?.message).toContain('pwsh is not on PATH');
    expect(recorded?.message).toContain('Install PowerShell 7');
    expect(recorded?.id).toBe(
      rejection instanceof StoreCommandError ? rejection.errorId : undefined,
    );
  });

  it('records a bare string rejection rather than showing nothing', async () => {
    // `invoke` can reject with a plain string when the failure is Tauri's own, before the
    // command ran. Losing that would leave a menu that closed and a tab that never appeared.
    const { store, daemon } = build();
    await ready(store);
    daemon.failures.set('session_close', 'the window has no permission for that command');

    await store.closeTab(store.getSnapshot().tabs[0]?.paneKey ?? '').catch(() => {});
    expect(store.getSnapshot().errors.at(-1)?.message).toContain('no permission');
  });
});

describe('the connection', () => {
  it('hands over exactly one channel per connection', async () => {
    // One multiplexed Channel for every session (§7.3). A second attach per session would
    // be the shape tauri#12724 punishes.
    const { store, daemon } = build();
    await ready(store);
    await store.openTab('shell.pwsh');

    expect(daemon.attaches).toBe(1);
  });

  it('reconnects when the socket drops, and says so while it is trying', async () => {
    const { store, daemon } = build();
    await ready(store);
    expect(daemon.connects).toBe(1);

    daemon.drop();
    await until(() => daemon.connects > 1);
    expect(daemon.connects).toBeGreaterThan(1);
    await until(() => store.getSnapshot().status === 'ready');
  });

  it('re-attaches every session after a reconnect, and routes their output', async () => {
    // `ready` is not the claim worth making, and asserting only that was how this suite
    // missed the defect entirely: the store reached `ready` perfectly while every pane had
    // been left silent for the rest of the process. A `StreamId` belongs to one stream
    // connection, so ids learned before the drop describe nothing after it — the sessions
    // have to be attached again, and their output has to arrive.
    const { store, daemon } = build(2);
    await ready(store);
    expect(daemon.attachCalls).toBe(2);

    const before = store.getSnapshot().tabs.map((tab) => store.surfaceStream(tab.paneKey));
    expect(before.every((stream) => stream !== null)).toBe(true);

    daemon.drop();
    await until(() => store.getSnapshot().status === 'ready' && daemon.connects > 1);
    await until(() => daemon.attachCalls >= 4);

    expect(
      daemon.attachCalls,
      'every session must be attached again on the new connection',
    ).toBe(4);

    // And the ids the store now holds are the ones this connection assigned.
    for (const tab of store.getSnapshot().tabs) {
      const stream = store.surfaceStream(tab.paneKey);
      expect(stream).not.toBeNull();
      expect(stream).toBe(daemon.streams.get(tab.handle));
    }

    // Output on a reissued id reaches the pane that owns it now, not the one that held the
    // same number before the drop.
    const [first] = store.getSnapshot().tabs;
    const stream = store.surfaceStream(first?.paneKey ?? '') ?? 0;
    store.terminals.surface(stream).show({} as HTMLElement);
    expect(() =>
      daemon.send(encodeFrame('output', stream, new Uint8Array(64))),
    ).not.toThrow();
  });

  it('gives the pane a new connection token when a reconnect reissues its id', async () => {
    // The pair `TerminalView` keys its effect on, and the id alone is not enough. A stream
    // connection carries its own id counter, so a single-session window is handed id 1 again
    // after a reconnect: the pane's id is unchanged while `resetStreams` has disposed the
    // surface behind it. Keyed on the id alone the effect never reran — the pane kept a dead
    // surface, the output went to a fresh one the delivery path built lazily and nothing had
    // ever shown, and it buffered as hidden while the status bar said ready.
    const { store, daemon } = build(1);
    await ready(store);

    const pane = store.getSnapshot().tabs[0]?.paneKey ?? '';
    const before = store.surfaceStream(pane);
    const connection = store.streamEpoch;
    expect(before).not.toBeNull();

    // The surface the mounted pane is holding right now.
    const mounted = store.terminals.surface(before ?? 0);

    daemon.drop();
    await until(() => store.getSnapshot().status === 'ready' && daemon.connects > 1);
    await until(() => daemon.attachCalls >= 2);

    expect(
      store.surfaceStream(pane),
      'the daemon must reissue the same id here, or this test proves nothing',
    ).toBe(before);
    expect(
      store.streamEpoch,
      'a pane whose id came back unchanged has nothing else to remount on',
    ).not.toBe(connection);
    expect(
      store.terminals.surface(before ?? 0),
      'the surface behind that id was disposed, so the pane must be handed a new one',
    ).not.toBe(mounted);
  });

  it('forgets a session the daemon no longer holds', async () => {
    // The map is what decides whether a session still needs attaching, so an entry for a
    // handle closed in another window is a pane that never gets reattached.
    const { store, daemon } = build(2);
    await ready(store);
    const [, second] = store.getSnapshot().tabs;
    const gone = second?.paneKey ?? '';

    daemon.sessions = daemon.sessions.slice(0, 1);
    await store.selectNav('tasks');
    await store.openTab('shell.pwsh');

    expect(store.surfaceStream(gone)).toBeNull();
  });

  it('gives up rather than looping when waiting cannot help', async () => {
    // A protocol mismatch never resolves itself. A window that kept saying "reconnecting"
    // would be lying about what it is waiting for.
    //
    // **And the loop has to actually end.** `daemon_connect` starts a daemon when it finds
    // none — the spawn lock, the process, twenty seconds of waiting for it to answer — so a
    // loop that retried this answer would start one process every twenty-odd seconds for the
    // life of the window. Asserting the status alone passed against exactly that code: the
    // snapshot said `failed` while the loop went on connecting underneath it. Awaiting `run`
    // is what makes the difference observable — it never resolves unless the loop returns.
    const daemon = new FakeDaemon();
    daemon.failures.set('daemon_connect', {
      message: 'this build speaks protocol v1, which is outside v2..=v2',
      nextSteps: ['Restart the daemon so both are the same version.'],
      retryable: false,
    });
    const store = new DaemonStore({
      bridge: daemon,
      router: new TerminalRouter({
        bridge: daemon,
        createTerminal: stubTerminals(),
        platform: 'windows',
      }),
      retryDelaysMs: [0],
    });
    const running = store.run();

    const dials = (): number =>
      daemon.calls.filter((call) => call.command === 'daemon_connect').length;

    await running;
    expect(store.getSnapshot().status).toBe('failed');
    expect(store.getSnapshot().errors[0]?.message).toContain('protocol');
    expect(dials()).toBe(1);

    // Nothing dials again afterwards, which is the part a user would have seen as a daemon
    // log growing by a line every twenty seconds. The fixture's refusal is one-shot, so a
    // loop that retried would not merely try again — it would *succeed*, and the window
    // would end up ready on a build it had just refused to speak to.
    await until(() => dials() > 1, 20);
    expect(dials()).toBe(1);
    store.dispose();
  });

  it('keeps probing while the runtime is still starting, and attaches when it answers', async () => {
    // Twenty seconds is a bound on one call, not a verdict on the daemon. Defender scanning
    // a binary it has never seen is exactly a first launch, so a daemon that binds at second
    // twenty-five used to sit beside a window that had already told the user to reopen the
    // app — and reopening worked, which made a timeout look like a flake.
    //
    // **A pin rather than the proof.** What changed is in Rust: the readiness timeout became
    // retryable, and the window stopped starting a second runtime beside the one already on
    // its way. This loop already retried anything retryable, so it would pass either side of
    // that. What it holds in place is the half that lives here — while the window is still
    // probing it says `reconnecting`, never `failed`, never stops, and the notice on screen
    // does not tell the user to do something about a failure nobody has established.
    const daemon = new FakeDaemon();
    daemon.refusals.set('daemon_connect', {
      failure: {
        message: 'Nysia started its runtime and it has not answered within 20 seconds',
        nextSteps: ['Nysia is still trying. If it never connects, the runtime wrote why to X.'],
        retryable: true,
      },
      times: 3,
    });
    const store = new DaemonStore({
      bridge: daemon,
      router: new TerminalRouter({
        bridge: daemon,
        createTerminal: stubTerminals(),
        platform: 'windows',
      }),
      retryDelaysMs: [0],
    });

    const seen: string[] = [];
    store.subscribe(() => seen.push(store.getSnapshot().status));
    let ended = false;
    void store.run().then(() => {
      ended = true;
    });

    await until(() => store.getSnapshot().errors.length > 0);
    expect(store.getSnapshot().status).toBe('reconnecting');
    const notice = store.getSnapshot().errors[0]?.message ?? '';
    expect(notice).toContain('has not answered');
    expect(notice).not.toContain('reopen');

    // And then the daemon answers, with nobody having touched anything.
    await until(() => store.getSnapshot().status === 'ready');
    expect(store.getSnapshot().status).toBe('ready');
    expect(
      daemon.calls.filter((call) => call.command === 'daemon_connect').length,
    ).toBeGreaterThan(3);
    expect(seen).not.toContain('failed');
    expect(ended, 'the loop gave up on a daemon that was still starting').toBe(false);
    store.dispose();
  });

  it('does not bury the notice list while the daemon is down', async () => {
    // A reconnect loop appending a notice per attempt fills the list within a minute.
    const daemon = new FakeDaemon();
    daemon.failures.set('daemon_connect', 'no daemon is listening');
    const store = new DaemonStore({
      bridge: daemon,
      router: new TerminalRouter({
        bridge: daemon,
        createTerminal: stubTerminals(),
        platform: 'windows',
      }),
      retryDelaysMs: [0],
    });
    void store.run();

    await until(() => store.getSnapshot().errors.length > 0);
    const after = store.getSnapshot().errors.length;
    await until(() => daemon.connects >= 1);
    expect(store.getSnapshot().errors.length).toBe(after);
    store.dispose();
  });
});

describe('output arriving on the channel', () => {
  it('reaches the session it belongs to, and only that one', async () => {
    // The demultiplexing claim, end to end: two panes, one Channel, and output for the
    // second must not appear in the first.
    const { store, daemon } = build(2);
    await ready(store);

    const [first, second] = store.getSnapshot().tabs;
    const target = store.surfaceStream(second?.paneKey ?? '');
    const other = store.surfaceStream(first?.paneKey ?? '');
    expect(target).not.toBeNull();
    expect(target).not.toBe(other);

    const written: number[] = [];
    const surface = store.terminals.surface(target ?? 0);
    const host = {} as HTMLElement;
    surface.show(host);

    daemon.send(encodeFrame('output', target ?? 0, new Uint8Array(2048)));
    written.push(2048);

    // Under the ack batch: held back on purpose, because a grant per chunk would spend a
    // quarter of the channel on acknowledgements.
    expect(daemon.calls.some((call) => call.command === 'terminal_ack')).toBe(false);
  });

  it('acknowledges upstream once the batch fills', async () => {
    // The other half, and the one that matters for liveness: an ack that never went would
    // stall the stream at the window's ceiling and the shell would block for good.
    const { store, daemon } = build(2);
    await ready(store);

    const stream = store.surfaceStream(store.getSnapshot().tabs[0]?.paneKey ?? '') ?? 0;
    store.terminals.surface(stream).show({} as HTMLElement);

    const chunk = 48 * 1024;
    for (let sent = 0; sent <= CREDIT_WINDOW_DEFAULT.ackBatch; sent += chunk) {
      daemon.send(encodeFrame('output', stream, new Uint8Array(chunk)));
    }

    await until(() => daemon.calls.some((call) => call.command === 'terminal_ack'));
    const ack = daemon.calls.find((call) => call.command === 'terminal_ack');
    expect(ack?.args?.stream).toBe(stream);
    expect(ack?.args?.bytes).toBeGreaterThanOrEqual(CREDIT_WINDOW_DEFAULT.ackBatch);
  });

  it('survives a delivery it cannot read without throwing into the channel', async () => {
    // This runs inside the Channel's `onmessage`, where a throw goes nowhere anyone can see.
    const { store, daemon } = build();
    await ready(store);

    const garbage = encodeHeaderOnly(0, 0);
    expect(() => daemon.send(garbage)).not.toThrow();
    expect(store.getSnapshot().errors.length).toBeGreaterThan(0);
  });
});



async function ready(store: DaemonStore): Promise<void> {
  await until(() => store.getSnapshot().status === 'ready');
}

/** Poll until `condition` holds, or give up. */
async function until(condition: () => boolean, ticks = 200): Promise<void> {
  for (let tick = 0; tick < ticks; tick += 1) {
    if (condition()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
}
