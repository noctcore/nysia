import { describe, expect, it } from 'vitest';

import type { Issue } from '../generated/Issue';
import type { ProfileAvailability } from '../generated/ProfileAvailability';
import type { Project as WireProject } from '../generated/Project';
import type { SessionCreate } from '../generated/SessionCreate';
import type { SessionSummary as WireSession } from '../generated/SessionSummary';
import { CREDIT_WINDOW_DEFAULT } from '../generated/wireConstants';
import { StoreCommandError } from '../store/errors';
import { describeStoreContract } from '../store/storeContract';
import type { CommandFailure, DaemonBridge } from './bridge';
import { DaemonStore } from './DaemonStore';
import type { StreamId } from './frames';
import type { TerminalFactory, XtermLike } from './surface/XtermSurface';
import type { TransportEvent, TransportEventDetail, TransportLog } from './log';
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
  /**
   * What `project_list` answers with, and what `project_register` appends to.
   *
   * Seeded with two, because the contract's fixture needs two projects to exercise
   * selection. That is a requirement on the *fixture* and not on a provider: a daemon on a
   * fresh machine has none, which is the case `no projects` below covers.
   */
  projects: WireProject[] = [wireProject('nysia'), wireProject('orca')];
  /** What the folder picker answers with. `null` is a cancelled dialog. */
  picks: string | null = 'D:/dev/valve';
  /**
   * What `profile_list` answers with: every shell the menu offers, launchable or not.
   *
   * All four launchable by default, so a test about something else is not also a test about
   * which shells exist. The ones about the menu say what the machine has.
   */
  profiles: ProfileAvailability[] = [
    { profile: { shell: 'pwsh' }, unavailable: null },
    { profile: { shell: 'cmd' }, unavailable: null },
    { profile: { shell: 'git_bash' }, unavailable: null },
    { profile: { shell: 'wsl', distro: null }, unavailable: null },
  ];
  /**
   * The launch nonce `daemon_connect` reports, which is how the window tells one daemon from
   * the next. Changing it is a daemon that restarted.
   */
  launchNonce = 'nonce_first';
  /**
   * What `tasks_list` answers with, in the shape the **daemon** sends it.
   *
   * Lowercase `state` and flat label names, which is what changed when v0.3 wave C1 landed:
   * these rows used to carry `gh`'s own `OPEN` and label objects, because the window was
   * doing the conversion. The daemon does it now
   * (`crates/nysia-core/src/rpc/tasks.rs`), and a fake still sending gh's spellings would be
   * pinning a tolerance the window has deliberately dropped — it would keep passing while the
   * reader accepted a shape no daemon produces, which is the opposite of what a fake is for.
   *
   * Typed `unknown[]` rather than `Issue[]` on purpose: a test that hands this an answer no
   * daemon should send is testing the reader, and a fixture the compiler polices could not
   * carry one.
   */
  issues: unknown[] = [
    {
      number: 200,
      title: 'Add the Tasks screen',
      state: 'open',
      updatedAt: '2026-09-09T12:00:00Z',
      url: 'https://github.com/noctcore/nysia/issues/200',
      author: 'Shironex',
      labels: ['area:web'],
    },
  ];
  /**
   * Branches this daemon already has a worktree for, so `project_start` adopts rather than
   * creates.
   */
  readonly adoptedBranches = new Set<string>();
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
        return {
          pid: 4242,
          startedAtMs: 1_757_721_600_000,
          launchNonce: this.launchNonce,
          appVersion: '0.3.0',
        } as T;
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
        // `Client::wait_for_disconnect` returns at once when nothing is connected. A
        // connection that dropped *during* the connect sequence is already gone by the time
        // the window starts watching, and a fake that waited for a second drop would hide
        // the reconnect that follows.
        if (!this.#connected) {
          return undefined as T;
        }
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
      case 'project_list':
        return this.projects.map((project) => ({ ...project })) as T;
      case 'profile_list':
        return this.profiles.map((row) => ({ ...row })) as T;
      case 'tasks_list':
        return [...this.issues] as T;
      case 'project_start': {
        // The daemon's own verb — `project_start`, not `task_start`: the request carries a
        // branch and **no issue number**, and the answer says whether a worktree was adopted
        // or made. A session is created too, because the window re-reads `session_list`
        // afterwards and a fake that answered without one would let a client through that
        // never opens a tab.
        const request = args?.request as { branch?: string } | undefined;
        const branch = request?.branch ?? '';
        this.#nextSession += 1;
        const created = session(this.#nextSession, 'agent');
        this.sessions.push(created);
        const adopted = this.adoptedBranches.has(branch);
        return {
          branch,
          handle: created.handle,
          paneKey: created.paneKey,
          adopted,
        } as T;
      }
      case 'project_pick_folder':
        return this.picks as T;
      case 'project_register': {
        // Idempotent, because the daemon's is: the id is derived from the canonical path,
        // so registering twice is one project and the answer says which happened. A fake
        // that minted a second project would let a client through that showed a duplicate
        // row on every re-register.
        const path = String(args?.path);
        const existing = this.projects.find((project) => project.id === projectId(path));
        if (existing) {
          return { project: existing, alreadyRegistered: true } as T;
        }
        const registered = wireProject(lastSegment(path));
        this.projects.push(registered);
        return { project: registered, alreadyRegistered: false } as T;
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
  return {
    kind: 'disconnected',
    message,
    nextSteps: ['Start the Nysia daemon, then try again.'],
    retryable: true,
  };
}

/** A project as `project_list` reports it, with the id the daemon would derive. */
function wireProject(name: string): WireProject {
  return {
    id: projectId(`D:/dev/${name}`),
    name,
    group: 'Dev',
    worktrees: [{ branch: 'master', isPrimary: true, sessions: [] }],
  };
}

/**
 * A stand-in for the daemon's `proj_<32 hex>`, derived from the path and nothing else.
 *
 * It is not the daemon's hash and does not pretend to be. What matters here is the property
 * the daemon's id has and a random one would not: the same path reaches the same id, which
 * is what makes registration idempotent rather than merely tidy.
 */
function projectId(path: string): string {
  let hash = 0;
  for (const code of path.toLowerCase()) {
    hash = (hash * 31 + (code.codePointAt(0) ?? 0)) % 0xffff_ffff;
  }
  return `proj_${hash.toString(16).padStart(32, '0')}`;
}

function lastSegment(path: string): string {
  const segments = path.split(/[\\/]/).filter((segment) => segment !== '');
  return segments.at(-1) ?? path;
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

/** Everything the store asked to have recorded, in order. */
class RecordingLog implements TransportLog {
  readonly entries: { event: TransportEvent; detail?: TransportEventDetail }[] = [];

  record(event: TransportEvent, detail?: TransportEventDetail): void {
    this.entries.push({ event, ...(detail === undefined ? {} : { detail }) });
  }
}

function build(seed = 2): { store: DaemonStore; daemon: FakeDaemon; log: RecordingLog } {
  const daemon = new FakeDaemon();
  daemon.seed(
    Array.from({ length: seed }, (_, index) => session(index + 1, index === 0 ? 'agent' : 'shell')),
  );
  const log = new RecordingLog();
  const store = new DaemonStore({
    bridge: daemon,
    router: new TerminalRouter({
      bridge: daemon,
      createTerminal: stubTerminals(),
      platform: 'windows',
    }),
    // No waiting in tests; the backoff itself is asserted separately.
    retryDelaysMs: [0],
    log,
  });
  void store.run();
  return { store, daemon, log };
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

  it('shows the daemon’s projects, and invents no session rows against them', async () => {
    // Both halves of "the sidebar stops pretending". The names come off `project_list` —
    // they used to be ten transcribed from the design mock, held client-side and marked as
    // such in a comment nobody reading the sidebar could see. And the sessions under a
    // worktree are the daemon's too: the old provider grafted every live session onto the
    // active project's `master`, which put four real sessions under a project that did not
    // exist and made the true part indistinguishable from the invented one.
    const { store, daemon } = build(2);
    await ready(store);

    const snapshot = store.getSnapshot();
    expect(snapshot.projects.map((project) => project.name)).toEqual(
      daemon.projects.map((project) => project.name),
    );
    expect(snapshot.projectsUnavailable).toBeNull();
    // The daemon holds two sessions and reports no worktree sessions, so the sidebar shows
    // none — rather than borrowing the ones in the tab strip.
    expect(daemon.sessions).toHaveLength(2);
    expect(snapshot.projects.flatMap((project) => project.worktrees.flatMap((w) => w.sessions))).toEqual(
      [],
    );
    // And the status bar's worktree count is a fact about that list rather than a seeded 3.
    expect(snapshot.daemon.worktreeCount).toBe(2);
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
      kind: 'spawn_failed',
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

describe('adding a project', () => {
  it('registers the folder the picker answered with, and selects it', async () => {
    const { store, daemon } = build();
    await ready(store);
    daemon.picks = 'D:/dev/valve';

    await store.addProject();

    const snapshot = store.getSnapshot();
    expect(snapshot.addProject).toEqual({
      phase: 'added',
      name: 'valve',
      alreadyRegistered: false,
    });
    // The list is re-read rather than spliced, so the sidebar's order is the daemon's.
    expect(snapshot.projects.map((project) => project.name)).toEqual(
      daemon.projects.map((project) => project.name),
    );
    expect(snapshot.activeProjectId).toBe(daemon.projects.at(-1)?.id);
    expect(snapshot.errors).toEqual([]);
  });

  it('says a folder is already there instead of pretending to have added it', async () => {
    // §3.2: registering the same path twice is one project. `alreadyRegistered` is the
    // daemon's answer to that, and it is not an error — it resolves, records nothing, and
    // reaches the panel as its own outcome.
    const { store, daemon } = build();
    await ready(store);
    daemon.picks = 'D:/dev/nysia';

    const before = daemon.projects.length;
    await expect(store.addProject()).resolves.toBeUndefined();

    expect(store.getSnapshot().addProject).toEqual({
      phase: 'added',
      name: 'nysia',
      alreadyRegistered: true,
    });
    expect(daemon.projects).toHaveLength(before);
    expect(store.getSnapshot().errors, 'an idempotent register is not a failure').toEqual([]);
  });

  it('says nothing at all when the picker is cancelled', async () => {
    const { store, daemon } = build();
    await ready(store);
    daemon.picks = null;

    await store.addProject();
    expect(store.getSnapshot().addProject).toEqual({ phase: 'idle' });
    expect(store.getSnapshot().errors).toEqual([]);
    expect(daemon.calls.some((call) => call.command === 'project_register')).toBe(false);
  });

  it('carries each of the three refusals to the dialog, not to the notice list', async () => {
    // The claim this whole path exists for. A refusal is an *answer*: it resolves, so
    // `runCommand` never routes it, and it reaches the panel with the daemon's own code,
    // sentence and steps rather than as "addProject failed" in the corner.
    const refusals = [
      ['not_a_repository', 'that folder is not a git repository'],
      ['many_repositories', 'but 3 of the folders in it are'],
      ['path_unreadable', 'that path could not be read'],
    ] as const;

    for (const [kind, message] of refusals) {
      const { store, daemon } = build();
      await ready(store);
      daemon.picks = 'D:/dev/whatever';
      daemon.failures.set('project_register', {
        kind,
        message,
        nextSteps: [`what to do about ${kind}`],
        retryable: false,
      });

      await expect(store.addProject(), kind).resolves.toBeUndefined();
      expect(store.getSnapshot().addProject, kind).toEqual({
        phase: 'refused',
        code: kind,
        message,
        nextSteps: [`what to do about ${kind}`],
      });
      expect(store.getSnapshot().errors, kind).toEqual([]);
      store.dispose();
    }
  });

  it('treats anything that is not a refusal as a command that failed', async () => {
    // The other side of the same boundary. A dropped socket mid-register is not an answer
    // about the folder, so it rejects and records — which is what puts it in front of the
    // user, because the dialog has nothing to say about it.
    const { store, daemon } = build();
    await ready(store);
    daemon.picks = 'D:/dev/whatever';
    daemon.failures.set('project_register', disconnected('the connection to the daemon failed'));

    const rejection = await store.addProject().catch((cause: unknown) => cause);
    expect(rejection).toBeInstanceOf(StoreCommandError);
    expect(store.getSnapshot().errors.at(-1)?.command).toBe('addProject');
    // And the dialog goes back to idle rather than sitting on `registering` for ever.
    expect(store.getSnapshot().addProject).toEqual({ phase: 'idle' });
  });

  it('opens one picker at a time', async () => {
    // Two pickers is two registrations racing, and the second would land on a snapshot the
    // first has already replaced.
    const { store, daemon } = build();
    await ready(store);
    daemon.picks = 'D:/dev/valve';

    await Promise.all([store.addProject(), store.addProject()]);

    expect(daemon.calls.filter((call) => call.command === 'project_pick_folder')).toHaveLength(1);
  });

  it('clears the panel when it is dismissed', async () => {
    const { store, daemon } = build();
    await ready(store);
    daemon.picks = 'D:/dev/valve';

    await store.addProject();
    expect(store.getSnapshot().addProject.phase).toBe('added');
    await store.dismissAddProject();
    expect(store.getSnapshot().addProject).toEqual({ phase: 'idle' });
  });
});

describe('a daemon that does not serve the project verbs', () => {
  const unsupported = {
    kind: 'unsupported',
    message: 'this daemon does not serve the project verbs yet',
    nextSteps: ['Compare the two: `nysia --version` reports the client.'],
    retryable: false,
  } as const;

  it('shows an empty sidebar carrying the daemon’s own sentence', async () => {
    // What every daemon says until v0.3 wave C1, and therefore what this window does today.
    // An empty sidebar that explains itself beats ten project names transcribed from a
    // design mock, because a name on screen cannot be told from a real one.
    const { store, daemon } = build();
    daemon.failures.set('project_list', { ...unsupported, nextSteps: [...unsupported.nextSteps] });
    await ready(store);

    const snapshot = store.getSnapshot();
    expect(snapshot.projects).toEqual([]);
    expect(snapshot.projectsUnavailable).toContain('does not serve the project verbs');
    expect(snapshot.projectsUnavailable).toContain('nysia --version');
  });

  it('does not put it in the notice list', async () => {
    // Nobody asked for this list — it is fetched on every connect — so a red notice would be
    // on screen at every launch for something the user did not do. That is how a notice list
    // stops being read, and the sessions that *did* fail are what it is for.
    const { store, daemon } = build();
    daemon.failures.set('project_list', { ...unsupported, nextSteps: [...unsupported.nextSteps] });
    await ready(store);

    expect(store.getSnapshot().errors).toEqual([]);
  });

  it('still reaches ready, and stays there', async () => {
    // The defect this arrangement exists to avoid, and it would have been every launch. The
    // fetch used to sit inside the connect sequence, so a refusal dropped the window into
    // `reconnecting` and retried for ever against an answer that says, in the envelope, that
    // waiting will not help.
    const daemon = new FakeDaemon();
    daemon.seed([session(1)]);
    // Not one-shot: this daemon refuses the verb every time, as a daemon from before wave C1
    // does. A one-shot fixture would go green on a loop that simply got lucky on its second
    // attempt.
    daemon.refusals.set('project_list', {
      failure: { ...unsupported, nextSteps: [...unsupported.nextSteps] },
      times: Number.MAX_SAFE_INTEGER,
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
    void store.run();
    await ready(store);

    const dials = daemon.connects;
    await until(() => daemon.connects > dials, 20);
    expect(store.getSnapshot().status).toBe('ready');
    expect(daemon.connects, 'a refused project list is not a reason to reconnect').toBe(dials);
    store.dispose();
  });

  it('keeps the projects it has when a later refresh cannot reach the daemon', async () => {
    // They were true when the daemon said them, and a failed re-read says nothing about
    // whether they still are. Blanking the sidebar would be inventing an answer in the other
    // direction.
    const { store, daemon } = build();
    await ready(store);
    expect(store.getSnapshot().projects.length).toBeGreaterThan(0);

    daemon.picks = 'D:/dev/valve';
    daemon.failures.set('project_list', disconnected('the connection to the daemon failed'));
    await store.addProject();

    expect(store.getSnapshot().projects.length).toBeGreaterThan(0);
    expect(store.getSnapshot().projectsUnavailable).toContain('connection to the daemon failed');
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
      kind: 'refused',
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
        kind: 'starting',
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

  it('records an unreadable delivery, which is the one failure Rust cannot see', async () => {
    // Rust wrote well-formed frames; whatever happened to them happened in the delivery,
    // where only this side is standing. Before #75 the notice was the whole record, and a
    // notice says nothing about what the transport was doing when it gave up.
    const { store, daemon, log } = build();
    await ready(store);

    daemon.send(encodeHeaderOnly(0, 0));

    const recorded = log.entries.filter((entry) => entry.event === 'channel_unreadable');
    expect(recorded).toHaveLength(1);
    // The decoder's message is deliberately *not* passed along: it is built from the bytes
    // that would not parse, and those bytes are terminal output (trap 13).
    expect(Object.keys(recorded[0]?.detail ?? {})).toEqual(['count']);
  });

  it('names the pane whose hidden output was dropped', async () => {
    // The notice on screen says "a background session", which is the one thing a reader of
    // the log afterwards cannot recover. The stream id is the daemon's own, so this line
    // joins up with the `stream_attach` line Rust wrote when the pane opened — which is what
    // #75 asked for and what nothing was doing with the ids the protocol already carries.
    const { store, log } = build();
    await ready(store);

    const stream = store.surfaceStream(store.getSnapshot().tabs[0]?.paneKey ?? '') ?? 0;
    store.reportDroppedOutput(stream, 65_536);

    expect(log.entries).toContainEqual({
      event: 'dropped_while_hidden',
      detail: { stream, count: 65_536 },
    });
  });
});



describe('the task list, which is the screen’s whole subject', () => {
  /** A refusal shaped like the daemon's envelope, with a code the window branches on. */
  function refusal(kind: string, message: string, step: string): CommandFailure {
    return { kind, message, nextSteps: [step], retryable: false };
  }

  it('reads the daemon’s answer rather than what the design mock draws', async () => {
    const { store, daemon } = build();
    await ready(store);
    await store.refreshTasks();

    const { tasks } = store.getSnapshot();
    expect(tasks.phase).toBe('loaded');
    expect(tasks.phase === 'loaded' ? tasks.issues : []).toEqual([
      {
        number: 200,
        title: 'Add the Tasks screen',
        // Lowercase and colourless because the *daemon* made it so — v0.3 wave C1 moved both
        // conversions there, so the window reads what it is given and refuses anything else.
        state: 'open',
        updatedAt: '2026-09-09T12:00:00Z',
        url: 'https://github.com/noctcore/nysia/issues/200',
        author: 'Shironex',
        labels: ['area:web'],
      },
    ]);
    expect(daemon.calls.some((call) => call.command === 'tasks_list')).toBe(true);
  });

  it('asks about the active project and nothing else', async () => {
    // The verb takes a project. Not a path — `Project` deliberately carries none (traps
    // register #13/#14) — and emphatically not an issue number.
    const { store, daemon } = build();
    await ready(store);
    await store.refreshTasks();

    const asked = daemon.calls.find((call) => call.command === 'tasks_list');
    expect(asked?.args).toEqual({ project: store.getSnapshot().activeProjectId });
  });

  it('tells the three refusals apart, and never rejects for any of them', async () => {
    // The requirement in one test. "An empty list for any of those is a lie": a user with no
    // issues and a user whose token expired must not see the same screen, so each code has to
    // survive the round trip into a reason the panel can render differently.
    const cases = [
      ['gh_missing', 'gh_missing'],
      ['gh_unauthenticated', 'gh_unauthenticated'],
      ['query_failed', 'query_failed'],
      // A code this build does not know degrades to the widest of the three, carrying the
      // daemon's own sentence — never to a blank table. `ErrorCode` is an open union for
      // exactly this reason, and the three above are now members of it rather than spellings
      // this window proposed.
      ['something_a_newer_daemon_added', 'query_failed'],
      // What a daemon older than wave C1 answers, because it does not serve the verb at all.
      ['unsupported', 'query_failed'],
    ] as const;

    for (const [kind, reason] of cases) {
      const { store, daemon } = build();
      await ready(store);
      daemon.failures.set('tasks_list', refusal(kind, `the daemon said ${kind}`, 'Do this.'));

      // Resolves. A refusal routed through `runCommand` would paint a notice over a table
      // that looks like a repository with no work in it.
      await expect(store.refreshTasks()).resolves.toBeUndefined();

      const { tasks, errors } = store.getSnapshot();
      expect(tasks.phase, kind).toBe('unavailable');
      if (tasks.phase === 'unavailable') {
        expect(tasks.reason, kind).toBe(reason);
        // The daemon's sentence and its steps, kept apart rather than joined: the panel puts
        // them in two different places and the steps are what say `gh auth login`.
        expect(tasks.message, kind).toBe(`the daemon said ${kind}`);
        expect(tasks.nextSteps, kind).toEqual(['Do this.']);
      }
      expect(errors, kind).toEqual([]);
    }
  });

  it('offers a step even when the failure carried none', async () => {
    // A Tauri-level error never becomes an envelope, so there is no advice attached — and a
    // panel with no next step is a dead end on the one screen whose whole subject is what to
    // do next.
    const { store, daemon } = build();
    await ready(store);
    daemon.failures.set('tasks_list', 'the command never ran');

    await store.refreshTasks();
    const { tasks } = store.getSnapshot();
    expect(tasks.phase === 'unavailable' ? tasks.nextSteps.length : 0).toBeGreaterThan(0);
  });

  it('forgets one project’s issues when another becomes active', async () => {
    const { store } = build();
    await ready(store);
    await store.refreshTasks();
    expect(store.getSnapshot().tasks.phase).toBe('loaded');

    const other = store.getSnapshot().projects[1];
    await store.selectProject(other?.id ?? '');
    expect(store.getSnapshot().tasks).toEqual({ phase: 'idle' });
  });

  it('forgets them when registering a project moves the selection', async () => {
    // The second of three places the active project moves, and the one that shipped broken:
    // only `selectProject` reset the list, so registering a folder with the Tasks screen open
    // left the *previous* repository's issues on screen under the new project's name. The
    // screen's refetch only fires on `idle`, so they stayed — and `Start →` on one of them
    // would have made a worktree in the new repository for the old one's issue.
    const { store } = build();
    await ready(store);
    await store.refreshTasks();
    expect(store.getSnapshot().tasks.phase).toBe('loaded');

    await store.addProject();
    expect(store.getSnapshot().activeProjectId).not.toBeNull();
    expect(store.getSnapshot().tasks).toEqual({ phase: 'idle' });
  });

  it('forgets them when a reconnect falls back to a different project', async () => {
    // The third place, and the quietest: `#refreshProjects` keeps the selection if the daemon
    // still names it and otherwise takes the first row. That runs on every connect and every
    // reconnect, so a daemon that has forgotten a project moves the selection with nobody
    // touching anything.
    const { store, daemon } = build();
    await ready(store);
    await store.refreshTasks();
    const wasActive = store.getSnapshot().activeProjectId;
    expect(store.getSnapshot().tasks.phase).toBe('loaded');

    daemon.projects = daemon.projects.filter((project) => project.id !== wasActive);
    daemon.drop();
    await until(() => store.getSnapshot().activeProjectId !== wasActive);

    expect(store.getSnapshot().tasks).toEqual({ phase: 'idle' });
  });

  it('discards an answer that arrives after the project changed', async () => {
    // The same ending by a different road. A round trip is not instant, so switching projects
    // mid-query leaves two answers outstanding — and whichever lands last wins. Without an
    // ownership check on the write, that can be the *first*, and project A's issues settle
    // under project B.
    const { store, daemon } = build();
    await ready(store);

    let release = (): void => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const realInvoke = daemon.invoke.bind(daemon);
    daemon.invoke = async <T,>(command: string, args?: Record<string, unknown>): Promise<T> => {
      if (command === 'tasks_list') {
        await held;
      }
      return realInvoke<T>(command, args);
    };

    const inFlight = store.refreshTasks();
    const other = store.getSnapshot().projects[1];
    await store.selectProject(other?.id ?? '');
    release();
    await inFlight;

    // The stale answer is dropped, not painted. `idle` is what `selectProject` left, and the
    // screen's own effect is what asks again for the project now showing.
    expect(store.getSnapshot().tasks).toEqual({ phase: 'idle' });
  });

  it('does not let an abandoned query replace the live one’s list with a refusal', async () => {
    // The gap an id comparison leaves, and the reason it is not benign. Leave project A, come
    // back to it, and both queries are A's — so `activeProjectId` matches for both and the one
    // that lands last wins, which can be the one nobody is waiting for.
    //
    // Stale *rows* under the right name would be a small lie. This is the bigger one: the live
    // query loads A's issues, the abandoned query then answers with a refusal, and the table
    // empties under the heading `GitHub CLI is not signed in` over a repository whose list had
    // just arrived. Nothing on screen contradicts it and only `↻` clears it.
    const { store, daemon } = build();
    await ready(store);
    const held = hold(daemon, 'tasks_list');
    const first = store.getSnapshot().projects[0];
    const other = store.getSnapshot().projects[1];

    const abandoned = store.refreshTasks();
    await store.selectProject(other?.id ?? '');
    const elsewhere = store.refreshTasks();
    await store.selectProject(first?.id ?? '');
    const live = store.refreshTasks();

    // The live query answers first and puts A's issues on screen.
    held.release(2);
    await live;
    expect(store.getSnapshot().tasks.phase).toBe('loaded');

    // Then the abandoned one answers — with the failure that makes this worth a guard.
    daemon.failures.set('tasks_list', refusal('gh_unauthenticated', 'not signed in', 'Sign in.'));
    held.release(0);
    await abandoned;

    expect(
      store.getSnapshot().tasks.phase,
      'a query nobody was waiting for emptied the table',
    ).toBe('loaded');

    held.release(1);
    await elsewhere;
  });

  it('keeps the refresh held when a late answer for the same project lands', async () => {
    // The second effect of the same root. `selectProject` resets `tasks` to `idle`, which
    // un-holds the `↻` that `isTasksBusy` was holding — so a second query for the same project
    // can be started while the first is still outstanding. When the first then answers, it used
    // to flip `tasks` to `loaded` and un-hold the button *again*, letting a user start a third
    // against a query that has not come back.
    const { store, daemon } = build();
    await ready(store);
    const held = hold(daemon, 'tasks_list');
    const first = store.getSnapshot().projects[0];
    const other = store.getSnapshot().projects[1];

    const abandoned = store.refreshTasks();
    await store.selectProject(other?.id ?? '');
    await store.selectProject(first?.id ?? '');
    const live = store.refreshTasks();

    held.release(0);
    await abandoned;
    expect(
      store.getSnapshot().tasks,
      'an answer to an abandoned query un-held the refresh',
    ).toEqual({ phase: 'loading' });

    held.release(1);
    await live;
    expect(store.getSnapshot().tasks.phase).toBe('loaded');
  });
});

describe('starting an issue', () => {
  it('asks for a branch derived here, and never for an issue number', async () => {
    // D-6 made unrepresentable rather than forbidden. The request cannot carry a task id, so
    // the branch is chosen before it exists — and the daemon never learns which issue it was.
    const { store, daemon } = build();
    await ready(store);
    await store.startTask(await aLoadedRow(store));

    const request = daemon.calls.find((call) => call.command === 'project_start')?.args?.request;
    expect(request).toEqual({
      project: store.getSnapshot().activeProjectId,
      branch: 'issue/200-add-the-tasks-screen',
      kind: 'agent',
      profile: null,
    });
    expect(JSON.stringify(request)).not.toContain('"issue"');
  });

  it('asks for an agent even where the daemon refuses one', async () => {
    // Deliberately not special-cased. An agent session is `unsupported` on a daemon that does
    // not serve one yet, and the tempting fix — quietly asking for a shell instead — opens the
    // wrong thing and reports it as the thing that was asked for. Sending what the design says
    // and rendering the refusal means this screen starts working the moment the daemon serves
    // it, with nothing to change here.
    const { store, daemon } = build();
    await ready(store);
    const row = await aLoadedRow(store);
    daemon.failures.set('project_start', {
      kind: 'unsupported',
      message: 'this daemon cannot start an agent session yet',
      nextSteps: ['Update the daemon, then start the issue again.'],
      retryable: false,
    });

    await expect(store.startTask(row)).rejects.toBeInstanceOf(StoreCommandError);
    const request = daemon.calls.find((call) => call.command === 'project_start')?.args?.request;
    expect((request as { kind?: string } | undefined)?.kind).toBe('agent');
    expect(store.getSnapshot().errors.at(-1)?.message).toContain('cannot start an agent');
  });

  it('says whether a worktree was adopted or created', async () => {
    for (const adopted of [false, true]) {
      const { store, daemon } = build();
      await ready(store);
      if (adopted) {
        daemon.adoptedBranches.add('issue/200-add-the-tasks-screen');
      }

      await store.startTask(await aLoadedRow(store));
      const { taskStart } = store.getSnapshot();
      expect(taskStart.phase).toBe('started');
      expect(taskStart.phase === 'started' ? taskStart.adopted : null).toBe(adopted);
    }
  });

  it('opens a tab for the session the daemon created', async () => {
    // Re-read rather than spliced in: `session_list` decides what exists, and the pane key
    // the answer names has to be one the strip is actually holding or `Open the session`
    // points at nothing.
    const { store } = build();
    await ready(store);
    const before = store.getSnapshot().tabs.length;

    await store.startTask(await aLoadedRow(store));
    const { tabs, taskStart } = store.getSnapshot();
    expect(tabs).toHaveLength(before + 1);
    expect(tabs.some((tab) => tab.paneKey === (taskStart.phase === 'started' ? taskStart.paneKey : ''))).toBe(true);
  });

  it('records a failure as a notice and puts the button back', async () => {
    // The other half of the split: nobody asked for the list, so its refusals are content;
    // somebody pressed this, so its failure is a notice with the daemon's own next steps.
    const { store, daemon } = build();
    await ready(store);
    const row = await aLoadedRow(store);
    daemon.failures.set('project_start', {
      kind: 'invalid_request',
      message: 'that branch is checked out in another worktree',
      nextSteps: ['Close the other worktree, or start on a different branch.'],
      retryable: false,
    });

    await expect(store.startTask(row)).rejects.toBeInstanceOf(StoreCommandError);

    const { taskStart, errors } = store.getSnapshot();
    expect(taskStart).toEqual({ phase: 'idle' });
    expect(errors.at(-1)?.command).toBe('startTask');
    expect(errors.at(-1)?.message).toContain('Close the other worktree');
  });

  it('refuses a row the list on screen no longer holds', async () => {
    // A one-frame React race, and the reason it is worth a guard rather than a comment: a tree
    // renders from one snapshot and re-renders on the next, and `selectProject` throws the list
    // away *synchronously* in between. A click already dispatched then hands over a row from a
    // repository that is no longer active — and the branch derived from it would be created in
    // the new one, which is the exact hazard the reset exists to prevent, walked around.
    const { store, daemon } = build();
    await ready(store);
    const row = await aLoadedRow(store);
    const other = store.getSnapshot().projects[1];
    await store.selectProject(other?.id ?? '');

    await expect(store.startTask(row)).rejects.toBeInstanceOf(StoreCommandError);
    // Nothing was asked of the daemon, which is the point: refusing after the worktree exists
    // would be an apology rather than a guard.
    expect(daemon.calls.some((call) => call.command === 'project_start')).toBe(false);
    expect(store.getSnapshot().taskStart).toEqual({ phase: 'idle' });
    expect(store.getSnapshot().errors.at(-1)?.message).toContain('not in the list on screen');
  });

  it('holds the list by identity, so a row a refresh replaced is refused too', async () => {
    // The same guard, narrowed to why it is identity and not an issue number. These two rows
    // are equal in every field; only one of them is in the list the screen is drawing. A
    // number would accept both — and a refresh is exactly when a title changes, which is what
    // the branch name is derived from.
    const { store } = build();
    await ready(store);
    const stale = await aLoadedRow(store);
    const fresh = await aLoadedRow(store);
    expect(fresh).toEqual(stale);
    expect(fresh).not.toBe(stale);

    await expect(store.startTask(stale)).rejects.toBeInstanceOf(StoreCommandError);
    await expect(store.startTask(fresh)).resolves.toBeUndefined();
  });

  it('refuses a second start rather than resolving with the row still spinning', async () => {
    // `store/storeContract.ts` requires this verb to start or to reject. Resolving while
    // `taskStart` stayed on `starting` satisfied neither: the caller saw a promise settle and
    // the screen said a worktree was being made, and nothing distinguished that from the
    // start that really was running. The table disables every other `Start →` while one is in
    // flight, so the only caller who can reach this is one that is not the table — which is
    // exactly the caller with no other way to find out.
    const { store, daemon } = build();
    await ready(store);
    const row = await aLoadedRow(store);
    const held = hold(daemon, 'project_start');

    const first = store.startTask(row);
    await until(() => store.getSnapshot().taskStart.phase === 'starting');
    await expect(store.startTask(row)).rejects.toBeInstanceOf(StoreCommandError);
    expect(store.getSnapshot().errors.at(-1)?.message).toContain('already starting');

    held.release(0);
    await first;
    expect(store.getSnapshot().taskStart.phase).toBe('started');
  });

  it('leaves a start able to finish after a refresh landed on top of it', async () => {
    // The claim `refreshTasks` makes in a comment, made checkable. Pressing `↻` while a
    // worktree is being made is ordinary, and what keeps the start alive across it is that the
    // refresh carries the `starting` state over **by reference** — `#settleStart` claims the
    // phase by object identity, so a spread would orphan the start that is still running.
    //
    // That is load-bearing and invisible, and nothing in the tree exercised it: no test called
    // `refreshTasks()` while a start was in flight. Writing `taskStart: { ...current.taskStart }`
    // keeps every by-value assertion passing and breaks every start that had `↻` pressed during
    // it — the row spins for ever, the confirmation never appears, and the worktree is made.
    // The last line below is the one that catches it.
    const { store, daemon } = build();
    await ready(store);
    const row = await aLoadedRow(store);
    const held = hold(daemon, 'project_start');

    const starting = store.startTask(row);
    await until(() => store.getSnapshot().taskStart.phase === 'starting');

    await store.refreshTasks();
    expect(
      store.getSnapshot().taskStart,
      'the refresh un-busied a row whose worktree was still being made',
    ).toEqual({ phase: 'starting', issue: 200 });
    expect(store.getSnapshot().tasks.phase, 'the refresh did not land').toBe('loaded');

    held.release(0);
    await starting;
    expect(
      store.getSnapshot().taskStart.phase,
      'the start could not find its own claim after the refresh rebuilt it',
    ).toBe('started');
  });

  it('drops a confirmation the screen has already moved past', async () => {
    // `discards an answer that arrives after the project changed`, for the other verb, and
    // one notch worse. A worktree takes seconds, so switching projects mid-start is ordinary
    // — and the line this writes names an issue by number. Landing it late puts *"#200
    // started in a new worktree on issue/200-…"* under a repository where issue 200 is
    // somebody else's, with a correct table above it and nothing to contradict it.
    const { store, daemon } = build();
    await ready(store);
    const row = await aLoadedRow(store);
    const held = hold(daemon, 'project_start');

    const starting = store.startTask(row);
    await until(() => store.getSnapshot().taskStart.phase === 'starting');
    const other = store.getSnapshot().projects[1];
    await store.selectProject(other?.id ?? '');

    held.release(0);
    await starting;
    // The worktree was still made and its tab is in the strip — what is dropped is a line of
    // prose about a screen the user has left.
    expect(store.getSnapshot().taskStart).toEqual({ phase: 'idle' });
    expect(store.getSnapshot().tabs.length).toBeGreaterThan(2);
  });

  it('does not let an abandoned start un-busy the one that replaced it', async () => {
    // The case a check on the project — or on the issue number — lets through, which is why
    // the write is claimed by identity instead. Switch away and back and the phase is `idle`
    // again, so the *same row* can be pressed a second time while the first request is still
    // outstanding. Both are `#200`, both are in project A; only one of them owns the phase.
    //
    // Letting the first answer land here would say a worktree exists while the second is
    // still being made, un-disable every `Start →` in the table, and report `adopted` for the
    // wrong one of the two requests.
    const { store, daemon } = build();
    await ready(store);
    const held = hold(daemon, 'project_start');

    const abandoned = store.startTask(await aLoadedRow(store));
    await until(() => store.getSnapshot().taskStart.phase === 'starting');
    const other = store.getSnapshot().projects[1];
    const first = store.getSnapshot().projects[0];
    await store.selectProject(other?.id ?? '');
    await store.selectProject(first?.id ?? '');

    // The list went with the project and has to come back before the row can be pressed again
    // — which is the guard above doing its job, not a detour around it.
    const current = store.startTask(await aLoadedRow(store));
    await until(() => store.getSnapshot().taskStart.phase === 'starting');

    // Only the abandoned one. The live start stays outstanding, which is the whole point:
    // the question is what an answer nobody is waiting for does to a row that is still busy.
    held.release(0);
    await abandoned;
    expect(
      store.getSnapshot().taskStart,
      'the abandoned start wrote over the live one',
    ).toEqual({ phase: 'starting', issue: 200 });

    held.release(1);
    await current;
    expect(store.getSnapshot().taskStart.phase).toBe('started');
  });
});

describe('opening a session in the active project', () => {
  /** Every `session_create` request the daemon was sent, in order. */
  function creates(daemon: FakeDaemon): SessionCreate[] {
    return daemon.calls
      .filter((call) => call.command === 'session_create')
      .map((call) => call.args?.request as SessionCreate);
  }

  it('names the active project, and never a folder', async () => {
    // The bug, from the window's side. `Project` carries no path, so the only `cwd` this
    // window could send was `null` — and the daemon opened the session in its own directory,
    // the user's home, with a project selected. It names the project now and the daemon
    // resolves the folder; `crates/nysia-core/src/rpc/interop.rs` asserts where the shell
    // then actually is.
    const { store, daemon } = build();
    await ready(store);
    const active = store.getSnapshot().activeProjectId;
    expect(active, 'the fixture selects a project').not.toBeNull();

    await store.openTab('agent.claude');
    await store.openTab('shell.cmd');

    for (const request of creates(daemon)) {
      expect(request.cwd).toEqual({ from: 'project', project: active });
    }
    expect(creates(daemon)).toHaveLength(2);
  });

  it('opens where the daemon runs when no project is selected', async () => {
    // A fresh machine has no projects, and a session must still open. `null` is the one
    // honest answer when there is no project to name.
    const daemon = new FakeDaemon();
    daemon.projects = [];
    daemon.seed([session(1)]);
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
    await ready(store);
    expect(store.getSnapshot().activeProjectId).toBeNull();

    await store.openTab('shell.cmd');
    expect(creates(daemon).at(-1)?.cwd).toBeNull();
    store.dispose();
  });

  it('stops drawing a project the daemon says it no longer holds', async () => {
    // Forgotten by another client since this window listed it. The daemon refuses the
    // create rather than opening it somewhere else, and the sidebar would otherwise go on
    // offering a project whose every session is refused the same way.
    const { store, daemon } = build();
    await ready(store);
    const gone = store.getSnapshot().activeProjectId;
    daemon.projects = daemon.projects.filter((project) => project.id !== gone);
    daemon.failures.set('session_create', {
      kind: 'unknown_project',
      message: `no project is registered under ${String(gone)}`,
      nextSteps: ['run `nysia project list` to see the projects this daemon holds'],
      retryable: false,
    });

    const rejection = await store.openTab('agent.claude').catch((cause: unknown) => cause);
    expect(rejection).toBeInstanceOf(StoreCommandError);
    expect(store.getSnapshot().errors.at(-1)?.message).toContain('no project is registered');
    expect(store.getSnapshot().projects.some((project) => project.id === gone)).toBe(false);
  });
});

describe('the + menu', () => {
  const NO_PWSH = 'the pwsh profile is unavailable: pwsh was not found on PATH';

  /** The terminal rows, as the chrome reads them. */
  function shells(store: DaemonStore): { id: string; unavailable: string | null }[] {
    return (
      store
        .getSnapshot()
        .launchers.find((group) => group.label === 'TERMINALS')
        ?.items.map(({ id, unavailable }) => ({ id, unavailable })) ?? []
    );
  }

  function withoutPwsh(daemon: FakeDaemon): void {
    daemon.profiles = daemon.profiles.map((row) =>
      row.profile.shell === 'pwsh' ? { ...row, unavailable: NO_PWSH } : row,
    );
  }

  it('says which shells the daemon cannot launch, in the daemon’s own words', async () => {
    // The report: the menu offered PowerShell 7 on a machine without it, and the person
    // learned so from a toast after picking it. The daemon knows — it resolves each shell on
    // its own PATH — so the menu is drawn from its answer rather than from the enum.
    const daemon = new FakeDaemon();
    withoutPwsh(daemon);
    daemon.seed([session(1)]);
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
    await ready(store);

    expect(shells(store)).toEqual([
      { id: 'shell.pwsh', unavailable: NO_PWSH },
      { id: 'shell.cmd', unavailable: null },
      { id: 'shell.git_bash', unavailable: null },
      { id: 'shell.wsl', unavailable: null },
    ]);
    // The agent row is not the daemon's to answer for here, and is not marked.
    expect(store.getSnapshot().launchers[0]?.items[0]).toMatchObject({
      id: 'agent.claude',
      unavailable: null,
    });
    store.dispose();
  });

  it('asks again when the menu is refreshed, so a shell installed since is offered', async () => {
    // The answer is computed by the daemon when it is asked, so asking is the whole of what
    // keeps it true — installing PowerShell 7 while the window is open must not leave the
    // menu saying it is missing until the next launch.
    const { store, daemon } = build();
    withoutPwsh(daemon);
    await ready(store);
    await store.refreshLaunchers();
    expect(shells(store)[0]).toEqual({ id: 'shell.pwsh', unavailable: NO_PWSH });

    daemon.profiles = daemon.profiles.map((row) => ({ ...row, unavailable: null }));
    await store.refreshLaunchers();
    expect(shells(store)[0]).toEqual({ id: 'shell.pwsh', unavailable: null });
  });

  it('asks again after a launch the daemon refused, before the notice lands', async () => {
    // Uninstalled while the window was open: the menu still offered it, the daemon refused —
    // the refusal is the backstop and stays — and the menu is corrected by the same click.
    const { store, daemon } = build();
    await ready(store);
    expect(shells(store)[0]?.unavailable).toBeNull();

    withoutPwsh(daemon);
    daemon.failures.set('session_create', {
      kind: 'spawn_failed',
      message: `could not start the session: ${NO_PWSH}`,
      nextSteps: ['check the shell is installed and on PATH'],
      retryable: false,
    });
    await store.openTab('shell.pwsh').catch(() => {});

    expect(store.getSnapshot().errors.at(-1)?.message).toContain('check the shell is installed');
    expect(shells(store)[0]).toEqual({ id: 'shell.pwsh', unavailable: NO_PWSH });
  });

  it('costs one reconnect against a daemon too old to be asked, not a loop', async () => {
    // A daemon built before the verb cannot read the request, so it closes the connection
    // rather than answering `unsupported` — and asking again on the reconnect would close it
    // again for ever. The daemon that did it is remembered by its launch nonce.
    const daemon = new FakeDaemon();
    daemon.seed([session(1)]);
    const realInvoke = daemon.invoke.bind(daemon);
    daemon.invoke = async <T,>(name: string, args?: Record<string, unknown>): Promise<T> => {
      if (name === 'profile_list') {
        daemon.calls.push({ command: name });
        daemon.drop();
        throw disconnected('the daemon closed the connection');
      }
      return realInvoke<T>(name, args);
    };
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
    await until(() => daemon.connects >= 2 && store.getSnapshot().status === 'ready');
    const settled = daemon.connects;
    await until(() => daemon.connects > settled, 50);

    expect(store.getSnapshot().status).toBe('ready');
    expect(daemon.connects, 'the second connect must not ask again').toBe(2);
    expect(daemon.calls.filter((call) => call.command === 'profile_list')).toHaveLength(1);
    // And the menu is the one from before the verb existed, not an empty one.
    expect(shells(store).map((row) => row)).toEqual([
      { id: 'shell.pwsh', unavailable: null },
      { id: 'shell.cmd', unavailable: null },
      { id: 'shell.git_bash', unavailable: null },
      { id: 'shell.wsl', unavailable: null },
    ]);

    // A daemon that restarted is a different daemon, and is asked.
    daemon.launchNonce = 'nonce_second';
    daemon.invoke = realInvoke;
    withoutPwsh(daemon);
    daemon.drop();
    await until(() => shells(store)[0]?.unavailable === NO_PWSH);
    expect(shells(store)[0]).toEqual({ id: 'shell.pwsh', unavailable: NO_PWSH });
    store.dispose();
  });

  it('does not write a daemon off for a menu opened while reconnecting', async () => {
    // Nothing is connected to answer, so a failure then says nothing about the daemon. Taken
    // for an older daemon refusing the verb, it would cost the menu its availability until
    // that daemon restarted.
    const { store, daemon } = build();
    await ready(store);
    daemon.refusals.set('daemon_connect', {
      failure: disconnected('the daemon is not answering'),
      times: Number.MAX_SAFE_INTEGER,
    });
    daemon.drop();
    await until(() => store.getSnapshot().status === 'reconnecting');

    daemon.failures.set('profile_list', disconnected('no control connection is open'));
    await store.refreshLaunchers();

    withoutPwsh(daemon);
    daemon.failures.delete('profile_list');
    daemon.refusals.delete('daemon_connect');
    await until(() => shells(store)[0]?.unavailable === NO_PWSH);
    expect(shells(store)[0]).toEqual({ id: 'shell.pwsh', unavailable: NO_PWSH });
  });

  it('draws a shell the daemon cannot name as nothing, rather than failing the connection', async () => {
    // A newer daemon may answer for a shell this build has never heard of, or answer in a
    // shape it cannot read. Neither is a reason to fail a connect; the rows it can read are
    // drawn and the rest are left out.
    const { store, daemon } = build();
    daemon.profiles = [
      { profile: { shell: 'pwsh' }, unavailable: NO_PWSH },
      { profile: { shell: 'nushell' }, unavailable: null } as unknown as ProfileAvailability,
    ];
    await ready(store);
    expect(shells(store)).toEqual([{ id: 'shell.pwsh', unavailable: NO_PWSH }]);
  });
});

/**
 * Hold one command open, and release its calls **one at a time, in the order they arrived**.
 *
 * {@link FakeDaemon} answers in a microtask, which models a round trip but not two
 * overlapping ones — and overlapping is the only state in which any of the ownership checks
 * above can be observed at all.
 *
 * Releasing individually is what makes the last test say anything. Freeing both at once let
 * the second answer land while the first was still in its `session_list` refresh, so the
 * snapshot reached `started` either way and the assertion passed against a store with no
 * ownership check in it — a test that watched the right screen and proved nothing.
 */
function hold(daemon: FakeDaemon, command: string): { release: (call: number) => void } {
  const waiting: (() => void)[] = [];
  const realInvoke = daemon.invoke.bind(daemon);
  daemon.invoke = async <T,>(name: string, args?: Record<string, unknown>): Promise<T> => {
    if (name === command) {
      await new Promise<void>((resolve) => waiting.push(resolve));
    }
    return realInvoke<T>(name, args);
  };
  return {
    release: (call: number) => {
      const resume = waiting[call];
      expect(resume, `no call ${call} of ${command} is waiting`).toBeDefined();
      resume?.();
    },
  };
}

/**
 * The row every start test presses, whose branch is `issue/200-add-the-tasks-screen`.
 *
 * **Fetched rather than written out**, which the start tests did not used to do. `startTask`
 * now requires the row to be one the loaded list is holding, by identity, so a literal
 * declared here is refused before the daemon is reached — and a test that handed one over
 * would be exercising the guard instead of the verb. Going through `refreshTasks` also means
 * every start test runs the real reader against the fake's real answer, so a row shape the
 * window could not parse fails these as well as `tasks.test.ts`.
 *
 * Throws rather than returning a fallback: a fixture that silently substituted a row of its
 * own is how the assertions above would go on passing against a list that never loaded.
 */
async function aLoadedRow(store: DaemonStore): Promise<Issue> {
  await store.refreshTasks();
  const { tasks } = store.getSnapshot();
  const row = tasks.phase === 'loaded' ? tasks.issues[0] : undefined;
  if (row === undefined) {
    throw new Error(`the fake daemon's issue list did not load: ${tasks.phase}`);
  }
  return row;
}

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
