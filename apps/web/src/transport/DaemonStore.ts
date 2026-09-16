import type { PaneKey } from '../generated/PaneKey';
import type { Project } from '../generated/Project';
import type { ProjectRegistered } from '../generated/ProjectRegistered';
import type { ProjectStart } from '../generated/ProjectStart';
import type { ProjectStarted } from '../generated/ProjectStarted';
import type { SessionHandle } from '../generated/SessionHandle';
import type { SessionKind } from '../generated/SessionKind';
import type { SessionSummary as WireSession } from '../generated/SessionSummary';
import type { ShellProfile } from '../generated/ShellProfile';
import { branchForIssue } from '../tasks/branchName';
import type { Issue } from '../tasks/issue';
import {
  isTasksBusy,
  unavailableReason,
  type TaskStartState,
  type TasksState,
} from '../tasks/tasks';
import { isAddProjectBusy, registerRefusal, type AddProjectState } from '../store/addProject';
import { StoreCommandError, type StoreCommandName, type StoreError } from '../store/errors';
import {
  emptySnapshot,
  type LauncherGroup,
  type NavSection,
  type ProjectId,
  type Store,
  type StoreSnapshot,
  type Tab,
  type WindowControls,
} from '../store/types';
import {
  asCommandFailure,
  describeFailure,
  isRetryable,
  type DaemonBridge,
} from './bridge';
import type { StreamId } from './frames';
import { SILENT_TRANSPORT_LOG, type TransportLog } from './log';
import { readIssues, readStarted } from './tasks';
import { REPLAY_BOUNDARY_DEADLINE_MS } from './surface/TerminalSurface';
import type { TerminalRouter } from './terminals';

/**
 * The daemon-backed provider, behind W3's `Store` interface.
 *
 * Swapping this in is one line in `main.tsx`, and not one component file changes — which is
 * what the interface was built for. The rules it has to honour are stated in
 * `store/types.ts` and asserted in `store/storeContract.ts`; this file runs that same suite.
 *
 * ## What comes from the daemon, and what does not
 *
 * **Sessions and tabs are real.** `session_list` is the authority, and it is what makes D-1
 * observable: a window that has just started, attaching to a daemon that has been running
 * for hours, gets back the sessions that outlived the last window.
 *
 * **Projects are real too, now.** They used to be W3's seed — ten plausible names held
 * client-side, marked as such in a comment nobody reading the sidebar could see. The verbs
 * are on the wire (v0.3 wave A), so `project_list` is the authority and the seed is gone.
 *
 * What it is replaced by is sometimes *nothing*, and that is the point: a daemon on a fresh
 * machine has no projects, and one too old to serve the verb refuses it — so the sidebar is
 * empty and says why, in the daemon's own words. An empty sidebar that explains itself is
 * worth more than ten names that cannot be told from real ones — and the sessions grafted
 * onto the seeded project went with it for the same reason. Which worktree a session belongs
 * to is something only the daemon knows; deriving it from a `cwd` invents a domain object out
 * of an unrelated signal, which is precisely what D-5 deleted the local task model to stop.
 *
 * **A failed `project_list` never fails the connection.** It is fetched on every connect
 * without anyone asking for it, so a refusal goes into `snapshot.projectsUnavailable` for
 * the sidebar to show, not into the notice list and not into the reconnect loop's verdict.
 * The first arrangement did the opposite: the fetch sat inside the connect sequence, so a
 * daemon that does not serve the verb yet — which is *every* daemon today — dropped the
 * window into `reconnecting` and retried forever against something no amount of waiting
 * fixes.
 *
 * **Launchers are derived**, from `ShellProfile`'s four variants. Which shells exist on the
 * machine is properly the daemon's answer, but the four *kinds* are on the wire already,
 * and deriving a menu from a closed enum is reading the protocol rather than inventing.
 *
 * ## Selection lives here
 *
 * Which tab is focused and which rail destination is showing are properties of *this
 * window*, not of the daemon — two windows on one daemon do not share a cursor. So
 * `selectNav`, `selectTab` and `selectProject` never leave the process, and inventing
 * daemon verbs for them would have made the daemon wrong about something it cannot know.
 *
 * ## Every failure reaches the user
 *
 * `routeCommands` treats a `StoreCommandError` as an expected outcome the provider has
 * already written into `snapshot.errors`, and anything else as a provider bug it reports
 * through `unexpectedFailures`. A socket closing mid-command is the likeliest real failure
 * in this whole file and is emphatically not a bug, so every path out of it — including the
 * reconnect loop — goes through {@link DaemonStore.fail}, which records *and* rejects with
 * the right type.
 */

/**
 * What one connection attempt leaves the reconnect loop to do.
 *
 * Three outcomes rather than a boolean, because "it did not work" hides the distinction the
 * loop has to act on: a daemon that is not up yet and a runtime that is not installed both
 * fail the same call, and only one of them is worth another twenty seconds.
 */
type ConnectOutcome = 'connected' | 'retry' | 'stopped';

/**
 * What the Tasks panel offers when the failure carried no advice of its own.
 *
 * Deliberately one sentence naming the control that is on screen next to it, rather than
 * something about daemons and logs: whoever is reading this is looking at a table with
 * nothing in it, and the question they have is what to press.
 */
const RETRY_STEP = 'Press ↻ to ask again.';

export class DaemonStore implements Store {
  #snapshot: StoreSnapshot = emptySnapshot('connecting');
  readonly #listeners = new Set<() => void>();
  readonly #bridge: DaemonBridge;
  readonly #router: TerminalRouter;
  readonly #retryDelaysMs: readonly number[];
  /**
   * Where the three things only this side can see are recorded.
   *
   * Optional, defaulting to a log that writes nowhere, so no existing test fake had to grow
   * a method for a dependency it does not care about. See `log.ts` for why the list is
   * three long and why the Rust side holds it.
   */
  readonly #log: TransportLog;

  /**
   * Which stream id the daemon assigned each session, from its `stream_attach` answer.
   *
   * Learned, never derived. Both sides used to compute the id from the order sessions
   * appeared in `session_list` — two implementations of one convention, which is the
   * arrangement that rots. The daemon picks the id now and says so, because it is the only
   * party that knows which ids are already in use on this connection.
   */
  readonly #streams = new Map<SessionHandle, StreamId>();

  /**
   * Which stream connection the ids in {@link #streams} were learned over.
   *
   * A `StreamId` does not identify a surface on its own, which is the part that is easy to
   * miss: a daemon whose id counter restarts hands the first session id 1 again, so a
   * reconnect can leave a pane's id *unchanged* while `resetStreams` has disposed the
   * surface behind it. A pane keyed on the id alone then never remounts — it keeps a
   * disposed surface, the output goes to a fresh one the delivery path built lazily and
   * nobody has shown, and that one buffers as hidden until it overflows and resets. The
   * status bar says ready the whole time.
   *
   * Counting connections is enough to tell those apart, and it is a number rather than
   * anything richer because the only question ever asked of it is whether it changed.
   */
  #streamEpoch = 0;
  #nextErrorId = 1;
  /**
   * How many task queries this store has started, so a late one can be told from the live one.
   *
   * **The project is not enough to identify a query, and the gap it leaves is reachable
   * rather than theoretical.** `withActiveProject` puts `tasks` back to `idle` whenever the
   * selection moves, which un-holds the `↻` that {@link isTasksBusy} was holding — so
   * `refreshTasks(A)`, `selectProject(B)`, `refreshTasks(B)`, `selectProject(A)`,
   * `refreshTasks(A)` leaves two queries for A outstanding at once, and an ownership test on
   * the id alone passes for both.
   *
   * What lands then is whichever answered last, which can be the *first* — and the harm is
   * not that the rows would be a little old. The fresh query can load A's issues and the
   * stale one replace them with a refusal: the table empties and the screen reads *"GitHub
   * CLI is not signed in"* over a repository whose list had just arrived, recoverable only
   * by pressing `↻`.
   *
   * A counter closes it because it is *monotonic*: every query gets a number no later query
   * can wear, so "am I still the live one" is a question with one answer. {@link #settleStart}
   * does the stronger version of the same thing with object identity, which is available to
   * it because a start writes a state object it can hold on to; a query writes nothing until
   * it answers, so it carries a number instead.
   */
  #taskQuery = 0;
  #disposed = false;

  constructor(options: {
    readonly bridge: DaemonBridge;
    readonly router: TerminalRouter;
    /**
     * Backoff between reconnect attempts, in milliseconds.
     *
     * A list rather than a formula so a test can pass `[0]` and a real window can back off
     * without either of them needing a clock they can control.
     */
    readonly retryDelaysMs?: readonly number[];
    /** Where transport events go. Defaults to nowhere. */
    readonly log?: TransportLog;
  }) {
    this.#bridge = options.bridge;
    this.#router = options.router;
    this.#retryDelaysMs = options.retryDelaysMs ?? [250, 500, 1000, 2000, 5000];
    this.#log = options.log ?? SILENT_TRANSPORT_LOG;
    // The one place the replay gate becomes visible to a person. A surface holds input shut
    // until the daemon's replay boundary has been parsed; if that frame never arrives the
    // deadline opens it anyway, and the keystrokes typed in the meantime are gone. Opening in
    // silence would leave "the first few seconds of typing did nothing after a relaunch" with
    // nothing to read anywhere, which is exactly how the defect this gate closes was lived
    // with for a wave before anybody wrote it down.
    this.#router.on({
      replayTimeout: (stream, dropped) => {
        // The notice tells the user; this tells whoever reads the log afterwards *which*
        // pane, which is the question the notice cannot answer. `stream` is the daemon's own
        // id, so it joins up with the `stream_attach` line Rust wrote when the pane opened.
        this.#log.record('replay_timeout', { stream, count: dropped });
        this.#recordOnce(
          'selectTab',
          `The daemon never marked the end of its replay for stream ${stream}, so this pane ` +
            `ignored ${dropped} characters of input for ${REPLAY_BOUNDARY_DEADLINE_MS / 1000}s ` +
            `before accepting any. Typing works now; anything typed in that window was lost.`,
        );
      },
    });
  }

  getSnapshot = (): StoreSnapshot => this.#snapshot;

  subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  selectNav = async (section: NavSection): Promise<void> => {
    this.#update((current) =>
      current.nav === section ? current : { ...current, nav: section },
    );
  };

  selectProject = async (id: ProjectId): Promise<void> => {
    if (!this.#snapshot.projects.some((project) => project.id === id)) {
      throw this.fail('selectProject', `No project ${id} is open.`);
    }
    this.#update((current) => withActiveProject(current, id));
  };

  selectTab = async (paneKey: PaneKey): Promise<void> => {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      throw this.fail('selectTab', `Session ${paneKey} is no longer open.`);
    }
    this.#update((current) =>
      current.activeTab === paneKey ? current : { ...current, activeTab: paneKey },
    );
  };

  closeTab = async (paneKey: PaneKey): Promise<void> => {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      throw this.fail('closeTab', `Session ${paneKey} is no longer open.`);
    }

    // The refresh is inside the try on purpose. It is a `session_list` round trip, and a
    // socket closing mid-command is the likeliest real failure in this whole file — outside
    // the try it would reject with a raw failure rather than a `StoreCommandError`, land in
    // the unexpected-failure path, and the user would be shown nothing at all.
    try {
      await this.#bridge.invoke('session_close', { handle: tab.handle });

      const stream = this.#streams.get(tab.handle);
      if (stream !== undefined) {
        this.#router.close(stream);
        this.#streams.delete(tab.handle);
      }
      await this.#refresh();
    } catch (cause) {
      throw this.fail('closeTab', describeFailure(cause));
    }
  };

  openTab = async (launcher: string): Promise<void> => {
    const item = this.#snapshot.launchers
      .flatMap((group) => group.items)
      .find((candidate) => candidate.id === launcher);
    if (!item) {
      throw this.fail('openTab', `No launcher ${launcher} is available.`);
    }

    let handle: SessionHandle;
    try {
      handle = await this.#bridge.invoke<SessionHandle>('session_create', {
        request: {
          kind: item.kind,
          paneKey: null,
          profile: profileFor(launcher),
          cwd: null,
          envOverrides: {},
          cols: 80,
          rows: 24,
        },
      });
      // Inside the try for the same reason as `closeTab`: this is a round trip, and a
      // failure here has to reach the user as a `StoreCommandError` or it reaches nobody.
      await this.#refresh();
    } catch (cause) {
      // The daemon's own message and next step, verbatim — "pwsh is not on PATH" and what
      // to do about it, rather than a menu that closed and a tab that never appeared.
      throw this.fail('openTab', describeFailure(cause));
    }

    const opened = this.#snapshot.tabs.find((tab) => tab.handle === handle);
    if (opened) {
      this.#update((current) => ({ ...current, activeTab: opened.paneKey }));
    }
  };

  /**
   * Browse for a folder and register it.
   *
   * Two round trips and five endings, and **only one of them is a failure**. The picker
   * answers with a path or with nothing; the daemon answers with a project, a project it
   * already had, or one of §3.2's three refusals. All five resolve, writing what happened
   * into `snapshot.addProject`. What rejects is a transport failure — a dropped socket, a
   * daemon that does not serve the verb — because that is the only ending the notice list is
   * the right place for.
   *
   * Getting that boundary wrong is visible to a user rather than merely untidy: a refusal
   * routed through `runCommand` paints *"addProject failed"* in the corner, and §3.2 says in
   * as many words that registering a folder twice is not an error. A folder holding several
   * repositories is not one either — it is the ordinary shape of a `Projekty/` directory and
   * a question about which one.
   *
   * The `+` is held shut while a picker is open. Two pickers is two registrations racing,
   * and the second would land on a snapshot the first has already replaced.
   */
  addProject = async (): Promise<void> => {
    if (isAddProjectBusy(this.#snapshot.addProject)) {
      return;
    }
    this.#setAddProject({ phase: 'browsing' });

    let picked: string | null;
    try {
      picked = await this.#bridge.invoke<string | null>('project_pick_folder');
    } catch (cause) {
      this.#setAddProject({ phase: 'idle' });
      throw this.fail('addProject', describeFailure(cause));
    }

    // A cancelled dialog is an outcome, and the quietest one. Anything that is not a path is
    // treated as one: the Rust side answers `null`, and a bridge that hands back `undefined`
    // for a command it does not know would otherwise be registered as the folder `undefined`.
    if (typeof picked !== 'string' || picked === '') {
      this.#setAddProject({ phase: 'idle' });
      return;
    }

    this.#setAddProject({ phase: 'registering' });
    let registered: ProjectRegistered;
    try {
      registered = await this.#bridge.invoke<ProjectRegistered>('project_register', {
        path: picked,
      });
    } catch (cause) {
      const failure = asCommandFailure(cause);
      const code = registerRefusal(failure?.kind ?? null);
      if (failure !== null && code !== null) {
        // The daemon's own sentence and its own steps, kept apart rather than joined: the
        // panel puts the heading, the sentence and the steps in three different places, and
        // for `many_repositories` the steps are the list of folders to choose between.
        this.#setAddProject({
          phase: 'refused',
          code,
          message: failure.message,
          nextSteps: failure.nextSteps,
        });
        return;
      }
      this.#setAddProject({ phase: 'idle' });
      throw this.fail('addProject', describeFailure(cause));
    }

    this.#setAddProject({
      phase: 'added',
      name: registered.project.name,
      alreadyRegistered: registered.alreadyRegistered,
    });
    // The list is re-read rather than having the new project spliced into it: `project_list`
    // decides the order, and a client that inserted its own would disagree with the daemon
    // the first time anything else registered one.
    await this.#refreshProjects();
    // Selected either way. A folder that was already registered is still the one the user
    // just went looking for, and leaving the sidebar where it was would answer a deliberate
    // act with nothing moving.
    //
    // Only if the re-read found it, though. When `project_list` failed the sidebar is still
    // drawing the list it had, and pointing `activeProjectId` at something absent from it
    // would leave a selection nothing renders and a `selectProject` that rejects.
    this.#update((current) =>
      current.projects.some((project) => project.id === registered.project.id)
        ? withActiveProject(current, registered.project.id)
        : current,
    );
  };

  dismissAddProject = async (): Promise<void> => {
    this.#setAddProject({ phase: 'idle' });
  };

  /**
   * Ask the daemon for the active project's issues, and never throw.
   *
   * **Every ending is an answer.** D-5 queries GitHub live, so the query can fail three ways
   * a user must be able to tell apart — `gh` absent, `gh` unauthenticated, the query itself
   * refused — and wave C's contract says an empty list for any of them is a lie. So all three
   * land in `snapshot.tasks` as the screen's own content, with the daemon's sentence and next
   * steps verbatim, rather than in the notice list over a table that looks like a repository
   * with no work in it.
   *
   * That includes the answer a daemon older than this window gives: `tasks_list` landed in
   * v0.3 wave C1 and one that predates it refuses the verb as `unsupported`, which is a real
   * answer the screen shows in the daemon's own words. The runtime outlives the window (D-1),
   * so that pairing is ordinary rather than exotic — and it is the same arrangement
   * `#refreshProjects` was built with one wave ago, for the same reason: a window that treated
   * "not served here" as a transport failure would reconnect forever against something no
   * amount of waiting fixes.
   *
   * With no active project it does nothing at all. There is nothing to ask GitHub about, and
   * the screen says that for itself rather than being handed a refusal nobody sent.
   */
  refreshTasks = async (): Promise<void> => {
    const project = this.#snapshot.activeProjectId;
    if (project === null || isTasksBusy(this.#snapshot.tasks)) {
      return;
    }
    // Taken before the round trip and carried through both endings — see {@link #taskQuery}.
    this.#taskQuery += 1;
    const query = this.#taskQuery;
    // The confirmation from the last start goes with the list it was about — unless a start
    // is still in flight, which is reachable by pressing `↻` while one runs. Clearing that
    // would un-busy the row mid-worktree and then flip it back when the answer landed, so a
    // start in progress outlives the list it was started from.
    this.#update((current) => ({
      ...current,
      tasks: { phase: 'loading' },
      taskStart:
        current.taskStart.phase === 'starting' ? current.taskStart : { phase: 'idle' },
    }));

    let issues: readonly Issue[];
    try {
      issues = readIssues(await this.#bridge.invoke<unknown>('tasks_list', { project }));
    } catch (cause) {
      const failure = asCommandFailure(cause);
      this.#settle(project, query, {
        phase: 'unavailable',
        reason: unavailableReason(failure?.kind ?? null),
        // `describeFailure` is deliberately not used: it joins the sentence and the first
        // step into one string, and this panel puts them in two different places — the
        // steps are the part that says `gh auth login`.
        message: failure?.message ?? describeFailure(cause),
        // The daemon's envelope promises at least one step, so this fallback is for a failure
        // that never became one — a Tauri-level error before the command ran, or an answer
        // this window could not parse. Tested on `length` rather than on nullishness: an
        // envelope carrying an *empty* list is not nullish, and `??` would pass it straight
        // through to a panel with a heading, a sentence and no way out of it.
        nextSteps:
          failure !== null && failure.nextSteps.length > 0 ? failure.nextSteps : [RETRY_STEP],
      });
      return;
    }

    this.#settle(project, query, { phase: 'loaded', issues });
  };

  /**
   * Write a task answer, unless the screen has moved on since it was asked for.
   *
   * The round trip is not instant and two things can move under it, so there are two tests
   * and neither implies the other.
   *
   * **The project**, because switching while a query is in flight leaves the answer belonging
   * to a repository that is no longer showing. Without this, project A's issues would sit
   * under project B's name — the same ending {@link withActiveProject} exists to prevent,
   * reached by a different road. The id is still checked on its own because the selection can
   * move without a second query being asked for at all: `selectProject` does not fetch, the
   * Tasks screen's effect does, and it only runs while that screen is mounted.
   *
   * **The query number**, because the project is the same in the case that actually loses
   * data. Leave A, come back to A, and both queries are A's — so an id comparison cannot tell
   * the live answer from the abandoned one, and the abandoned one can arrive second and win.
   * {@link #taskQuery} carries the whole of why that is worse than stale rows.
   *
   * Discarding is right rather than merely safe: a newer query is already in flight, so the
   * screen is about to be correct without this answer, and `loading` is what it shows in the
   * meantime.
   */
  #settle(project: ProjectId, query: number, tasks: TasksState): void {
    if (this.#snapshot.activeProjectId !== project || this.#taskQuery !== query) {
      return;
    }
    this.#update((current) => ({ ...current, tasks }));
  }

  /**
   * Hand an issue to an agent: a branch-keyed worktree, a session in it, and a tab.
   *
   * **The branch is derived here and the issue number never leaves this method.** D-6 is made
   * unrepresentable rather than merely forbidden — the request carries a project and a
   * branch, and there is no field an issue id could travel in — so `tasks/branchName.ts` runs
   * before the request exists and the daemon never learns which issue this was.
   *
   * Unlike {@link refreshTasks} this rejects and records. Somebody pressed a button, so a
   * branch that will not check out or a repository that moved has to reach them with the
   * daemon's own next steps, wherever they are looking by then.
   *
   * The session list is re-read rather than having a tab spliced in, for `openTab`'s reason:
   * `session_list` decides what exists, and a client that invented a row would disagree with
   * the daemon the first time anything else opened one.
   *
   * Its two late writes go through {@link #settleStart}, for {@link #settle}'s reason reached
   * by the other road.
   */
  startTask = async (issue: Issue): Promise<void> => {
    const project = this.#snapshot.activeProjectId;
    if (project === null) {
      throw this.fail('startTask', 'No project is selected, so there is nowhere to start it.');
    }
    // The row has to be one of the rows currently loaded, by **identity**, and that closes a
    // frame-long window rather than a hypothetical one. A React tree renders the issues from
    // one snapshot and re-renders on the next; between those two, the list underneath can
    // already have been thrown away — `selectProject` does it synchronously — so a click that
    // has been dispatched hands over a row belonging to a repository that is no longer the
    // active one. The branch is derived from that row, and the request carries the *new*
    // project: a worktree for one repository's issue, made in another, which is the exact
    // hazard three comments in this file cite as the reason the list is reset at all.
    //
    // Identity rather than an issue number, for `#settleStart`'s reason: a refresh replaces
    // every row object, and a number that survives a refresh says nothing about whether the
    // title the branch is derived from did.
    const listed = this.#snapshot.tasks;
    if (listed.phase !== 'loaded' || !listed.issues.includes(issue)) {
      throw this.fail(
        'startTask',
        'That issue is not in the list on screen any more. Refresh it and start it again.',
      );
    }
    // A refusal rather than a resolve. `store/storeContract.ts` requires this verb to start or
    // to reject, and resolving here left `taskStart` on `starting` — the one shape it forbids,
    // because a caller that saw a resolved promise and a spinning row would have no way to
    // tell the two apart. The UI disables every other `Start →` while one runs, so this is
    // reachable only by a caller that is not the table, which is precisely who needs telling.
    if (this.#snapshot.taskStart.phase === 'starting') {
      throw this.fail(
        'startTask',
        'A task is already starting. Wait for its worktree before starting another.',
      );
    }

    const branch = branchForIssue(issue);
    // Held by reference, and that reference is this start's claim on the phase — see
    // {@link #settleStart}. Every other write in this file spreads the snapshot, so the
    // object survives a `↻` and a session refresh unchanged, and is replaced by exactly the
    // things that end this start: a project move, a failure, or its own answer.
    const pending: TaskStartState = { phase: 'starting', issue: issue.number };
    this.#update((current) => ({ ...current, taskStart: pending }));

    // Typed as the wire's own request rather than as an object literal, so a field the daemon
    // renames fails the typecheck here instead of being refused at run time by a daemon that
    // cannot see what was meant. There is deliberately no field an issue number could travel
    // in — D-6 made unrepresentable, and `ProjectStart`'s own comment is where that is argued.
    const request: ProjectStart = {
      project,
      branch,
      // One agent in v1 and no provider trait (D-3, D-4), and the design is explicit that
      // `Start →` hands the issue to an agent. This is sent even where the daemon refuses it
      // — an agent session is `unsupported` until the verb that serves one lands — because a
      // window that quietly asked for a shell instead would open the wrong thing and say it
      // had done what was asked. The refusal is rendered like any other, and the screen starts
      // working the moment the daemon serves it, with nothing to change here.
      kind: 'agent',
      // `profile` is for a shell, so it is null here rather than absent: the field is on the
      // wire either way, and `ShellProfile | null` is what the generated type spells.
      profile: null,
    };

    let started: ProjectStarted;
    try {
      started = readStarted(
        await this.#bridge.invoke<unknown>('project_start', { request }),
      );
      // Inside the try, like `openTab`: this is a round trip, and a failure here has to reach
      // the user as a `StoreCommandError` or it reaches nobody.
      await this.#refresh();
    } catch (cause) {
      // The state write is conditional, the refusal never is. Somebody pressed a button and
      // has to hear that it failed wherever they are looking by now — but putting the button
      // back is about *this* start, and there may be a newer one in flight that owns it.
      this.#settleStart(pending, { phase: 'idle' });
      throw this.fail('startTask', describeFailure(cause));
    }

    this.#settleStart(pending, {
      phase: 'started',
      issue: issue.number,
      // The daemon's branch, not the one asked for. They should agree, and if they ever do
      // not it is the daemon that knows which worktree it actually opened.
      branch: started.branch,
      paneKey: started.paneKey,
      adopted: started.adopted,
    });
  };

  /**
   * Write a `Start →` answer, unless the screen has moved on since the button was pressed.
   *
   * {@link #settle}'s hazard, reached by the other road and one notch worse. `project_start`
   * creates a worktree and can take seconds, and `withActiveProject` clears `taskStart` the
   * moment the project moves — so a late answer would repaint *"#7 started in a new worktree
   * on issue/7-…"* under whichever repository is showing by then, a confirmation naming an
   * issue that does not exist there. The list above it is already correct, which is what
   * makes the line worse than useless: it is the only thing on screen that is lying.
   *
   * **The test is object identity, not the project, and not the issue number.** Both of the
   * weaker tests pass in a case this one catches: switch away and back — which clears the
   * phase — then press `Start →` on the same row again, and a comparison on either would let
   * the first answer land on the second start's `starting`, un-busying a row whose worktree
   * is still being made and reporting `adopted` for the wrong one of the two. The state this
   * start wrote is its claim on the phase, and only the start still holding it may write.
   *
   * A `↻` during a start deliberately does not disturb that claim: `refreshTasks` carries a
   * `starting` phase across untouched rather than rebuilding it, so the answer still finds
   * its own start when it lands.
   *
   * Dropping the confirmation loses nothing durable. The worktree was made, the session
   * exists, and `#refresh` has already put its tab in the strip — what is discarded is one
   * line of prose belonging to a screen the user has left.
   */
  #settleStart(pending: TaskStartState, taskStart: TaskStartState): void {
    if (this.#snapshot.taskStart !== pending) {
      return;
    }
    this.#update((snapshot) => ({ ...snapshot, taskStart }));
  }

  dismissError = async (id: string): Promise<void> => {
    this.#update((current) => {
      const errors = current.errors.filter((error) => error.id !== id);
      return errors.length === current.errors.length ? current : { ...current, errors };
    });
  };

  get window(): WindowControls {
    return this.#bridge.window;
  }

  /**
   * Which stream connection {@link surfaceStream} is currently answering for.
   *
   * Read together with the id by anything that mounts a surface: the pair identifies a
   * surface, where the id alone does not. See {@link #streamEpoch}.
   */
  get streamEpoch(): number {
    return this.#streamEpoch;
  }

  /** The surface a pane draws into, or `null` before the daemon has named the session. */
  surfaceStream(paneKey: PaneKey): StreamId | null {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    return tab ? (this.#streams.get(tab.handle) ?? null) : null;
  }

  /**
   * Tell the user that output was thrown away while a pane was hidden.
   *
   * The drop is how memory stays bounded for a background pane (see
   * `surface/TerminalSurface.ts`), and it is not a fault — but a terminal that silently
   * skips a stretch of its own output is worse than one that says so, because the reader
   * takes what is left as following on from what came before.
   *
   * Takes the stream as well as the count so the log line names the pane. The notice on
   * screen cannot — it is one sentence in a list — and "a background session" is exactly the
   * detail somebody reading the log afterwards needs and cannot recover.
   */
  reportDroppedOutput(stream: StreamId, bytes: number): void {
    this.#log.record('dropped_while_hidden', { stream, count: bytes });
    this.#recordOnce(
      'selectTab',
      `A background session produced more output than Nysia holds for a hidden pane, so ${bytes} bytes were dropped and its screen was reset.`,
    );
  }

  /** The router, for the pane component that mounts surfaces. */
  get terminals(): TerminalRouter {
    return this.#router;
  }

  /**
   * Forward what the user typed to the session in `paneKey`.
   *
   * Not a `Store` command, and deliberately so: `StoreCommandName` enumerates the commands
   * a *notice* can name, and a keystroke is not one of them. One notice per character typed
   * while the daemon is down would bury every other message in the list, and the disconnect
   * that caused it already has a notice of its own.
   *
   * A failure is **recorded once** rather than thrown away. One notice per character typed
   * would bury the list, so it is deduplicated by message the way the reconnect loop's is —
   * but typing into a dead session and seeing nothing anywhere is how a user concludes the
   * whole window is broken.
   */
  async sendInput(paneKey: PaneKey, text: string): Promise<void> {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      this.#recordOnce('selectTab', `Session ${paneKey} is no longer open.`);
      return;
    }
    try {
      await this.#bridge.invoke('terminal_send', {
        request: { handle: tab.handle, text, enter: false, interrupt: false },
      });
    } catch (cause) {
      this.#recordOnce('selectTab', describeFailure(cause));
    }
  }

  /**
   * Tell the daemon a pane changed size, in cells.
   *
   * Recorded once on failure, like {@link sendInput}: a pane wrapping at the wrong width
   * with nothing said about why is the kind of fault users work around for months.
   */
  async resize(paneKey: PaneKey, cols: number, rows: number): Promise<void> {
    const tab = this.#snapshot.tabs.find((candidate) => candidate.paneKey === paneKey);
    if (!tab) {
      return;
    }
    // The surface has already sized itself — it is what measured the host — so this only
    // carries the answer to the daemon, which owns the PTY.
    try {
      await this.#bridge.invoke('terminal_resize', {
        request: { handle: tab.handle, cols, rows },
      });
    } catch (cause) {
      this.#recordOnce('selectTab', describeFailure(cause));
    }
  }

  /**
   * Connect, load, and keep reconnecting for as long as waiting could still help.
   *
   * Started once, from {@link createDaemonStore}. It never throws: a store whose connect
   * loop rejected would leave the chrome rendering `connecting` forever with nothing said
   * about why, which is the state this whole file exists to avoid.
   *
   * **It stops on a failure the daemon side called permanent**, and that is not a detail of
   * the backoff. `daemon_connect` starts a daemon when it cannot find one — it takes the
   * spawn lock, runs the runtime and waits twenty seconds for it to answer — so a loop that
   * retried a `retryable: false` answer would start one process every twenty-odd seconds for
   * the life of the window, appending to the daemon log each time, against a runtime that is
   * missing or will not run. Nothing about that improves by repeating it, the notice is
   * already on screen with what to do about it, and the status is `failed` rather than
   * `reconnecting` precisely because the window is no longer waiting for anything.
   *
   * The way back is a restart, which is what those next steps say: reinstall, or start a
   * daemon by hand, then reopen Nysia.
   */
  async run(): Promise<void> {
    let attempt = 0;
    while (!this.#disposed) {
      const outcome = await this.#connectOnce();
      if (outcome === 'stopped') {
        return;
      }
      if (outcome === 'connected') {
        attempt = 0;
        // Resolves when the socket drops — a blocking command rather than an `emit`,
        // because tauri#12724 leaks on sustained emits and a second mechanism for one
        // edge is not worth the leak.
        await this.#bridge.invoke('daemon_watch').catch(() => undefined);
        if (this.#disposed) {
          return;
        }
        this.#update((current) => ({ ...current, status: 'reconnecting' }));
      } else {
        attempt += 1;
      }

      const delay =
        this.#retryDelaysMs[Math.min(attempt, this.#retryDelaysMs.length - 1)] ?? 1000;
      await sleep(delay);
    }
  }

  /** Stop reconnecting and release every surface. */
  dispose(): void {
    this.#disposed = true;
    this.#router.dispose();
  }

  /**
   * Record a failure and produce the rejection that goes with it.
   *
   * Both, in that order, and before the promise settles — the contract in `store/errors.ts`
   * is explicit that rejecting without recording loses the message and recording without
   * rejecting leaves a caller unable to sequence.
   */
  fail(command: StoreCommandName, message: string): StoreCommandError {
    const id = `err_${this.#nextErrorId}`;
    this.#nextErrorId += 1;
    const error: StoreError = { id, command, message, at: Date.now() };
    this.#update((current) => ({ ...current, errors: [...current.errors, error] }));
    return new StoreCommandError(command, message, id);
  }

  /**
   * One connection attempt.
   *
   * `connected` once the first snapshot is on screen, `retry` for a failure another attempt
   * could get past, and `stopped` for one it could not — which {@link run} obeys by ending.
   */
  async #connectOnce(): Promise<ConnectOutcome> {
    try {
      await this.#bridge.invoke('daemon_connect');
    } catch (cause) {
      // `failed` rather than `reconnecting` once the daemon side has said something that
      // waiting will not change — a protocol mismatch never resolves itself, a runtime that
      // is not installed does not appear — and a window that kept saying "reconnecting"
      // would be lying about what it is waiting for.
      if (!isRetryable(cause)) {
        this.#update((current) => ({ ...current, status: 'failed' }));
        this.#recordOnce('openTab', describeFailure(cause));
        return 'stopped';
      }
      this.#update((current) => ({ ...current, status: 'reconnecting' }));
      this.#recordOnce('openTab', describeFailure(cause));
      return 'retry';
    }

    try {
      // The channel first, and deliberately before anything that needs a session to exist.
      // Opening the output connection is not a per-session act — it is the connection every
      // session's frames will ride — so it has to work against a daemon holding none. An
      // earlier order asked for a session here and fetched the session list afterwards,
      // which meant the attach could never succeed against any daemon: it ran before the
      // only call that could have satisfied it, failed, set `reconnecting`, and retried in
      // the same order forever.
      await this.#bridge.attachChannel((delivery) => {
        const unreadable = this.#router.deliver(delivery);
        if (unreadable) {
          // There is no delimiter to resynchronise on, so the channel is finished. Dropping
          // the connection is what gets a fresh one.
          //
          // The one failure in this file that Rust cannot see for itself: it wrote
          // well-formed frames, and whatever happened to them happened in the delivery. The
          // decoder's message is *not* passed along — it is built from the bytes that would
          // not parse, and those bytes are terminal output.
          this.#log.record('channel_unreadable', {
            count: this.#router.bufferedBytes,
          });
          this.#recordOnce('openTab', unreadable.message);
        }
      });

      // **Everything learned over the previous connection is void.** A `StreamId` is scoped
      // to one stream connection — proto is explicit that an id "means nothing on the
      // other's connection" — so both the map and the surfaces keyed by it have to go.
      //
      // Keeping them was how any disconnect left every existing pane silent for the rest of
      // the process: `#refresh` skipped every handle already in the map, so no session was
      // ever re-attached, while the chrome said `ready`. Keeping the surfaces was the other
      // half — a daemon whose counter restarted would hand the first new session id 1, and
      // its output would land in whichever pane held id 1 before.
      this.#streams.clear();
      this.#router.resetStreams();
      // Bumped in the same breath as the reset that invalidates them. A pane on screen is
      // holding a surface this call just disposed, and the id it is keyed on may come back
      // unchanged, so this is the only thing that tells it to mount the new one.
      this.#streamEpoch += 1;

      await this.#refresh();
    } catch (cause) {
      this.#update((current) => ({ ...current, status: 'reconnecting' }));
      this.#recordOnce('openTab', describeFailure(cause));
      return 'retry';
    }

    // Deliberately after the try, and deliberately unable to throw. The sessions are what
    // this connection is *for*; the project list is something the sidebar would like. A
    // daemon that does not serve the verb yet is every daemon until wave C1, and putting
    // this inside the sequence above turned that into an endless reconnect against a
    // refusal that said, in the envelope, that waiting would not help.
    await this.#refreshProjects();

    this.#update((current) => ({ ...current, status: 'ready' }));
    return 'connected';
  }

  /**
   * Re-read the project list, and never throw.
   *
   * A refusal lands in `projectsUnavailable` as the daemon's own sentence, which the sidebar
   * shows where the projects would have been. Not a notice: nobody asked for this list and
   * it is fetched on every connect, so a notice would be a red box on every launch for
   * something the user did not do — which is how a notice list stops being read.
   *
   * The projects already on screen are left alone on a failure rather than cleared. They
   * were true when the daemon said them and this call says nothing about whether they still
   * are; blanking the sidebar on a dropped socket would be inventing an answer in the other
   * direction.
   */
  async #refreshProjects(): Promise<void> {
    let projects: readonly Project[];
    try {
      projects = await this.#bridge.invoke<Project[]>('project_list');
    } catch (cause) {
      const message = describeFailure(cause);
      this.#update((current) =>
        current.projectsUnavailable === message
          ? current
          : { ...current, projectsUnavailable: message },
      );
      return;
    }

    this.#update((current) => {
      // Same rule as `activeTab`: keep what is selected if it still exists, else the first
      // row, else nothing. A stale id survives `project_list` no longer naming it otherwise,
      // and `selectProject` rejects an id it cannot find — so the sidebar's next click would
      // have failed against a project the sidebar is no longer drawing.
      const activeProjectId =
        projects.find((project) => project.id === current.activeProjectId)?.id ??
        projects[0]?.id ??
        null;
      // Through the setter, because this is the *third* place the active project moves and
      // it moves on every connect and reconnect: a daemon that has forgotten a project falls
      // back to the first row here, silently, while the Tasks screen is open.
      return {
        ...withActiveProject(current, activeProjectId),
        projects,
        projectsUnavailable: null,
        daemon: {
          ...current.daemon,
          // The status bar's third figure, which had nothing behind it until now.
          worktreeCount: projects.reduce((total, project) => total + project.worktrees.length, 0),
        },
      };
    });
  }

  #setAddProject(addProject: AddProjectState): void {
    this.#update((current) => ({ ...current, addProject }));
  }

  /** Re-read the session list and rebuild everything derived from it. */
  async #refresh(): Promise<void> {
    const sessions = await this.#bridge.invoke<WireSession[]>('session_list');

    // Ask the daemon to route each session this window does not already hold an id for. It
    // answers with the id, so nothing here guesses one — and a session already attached is
    // skipped rather than attached twice, because an id is spent for the life of the
    // connection and a second would route frames nothing reads.
    for (const session of sessions) {
      if (this.#streams.has(session.handle)) {
        continue;
      }
      const stream = await this.#bridge.invoke<StreamId>('stream_attach', {
        handle: session.handle,
      });
      this.#streams.set(session.handle, stream);
    }

    // Drop ids for sessions the daemon no longer holds. Without this a handle closed in
    // another window keeps its entry forever, and the map is what decides whether a session
    // still needs attaching — so a stale entry is a pane that never gets reattached.
    const live = new Set(sessions.map((session) => session.handle));
    for (const handle of [...this.#streams.keys()]) {
      if (!live.has(handle)) {
        const stream = this.#streams.get(handle);
        if (stream !== undefined) {
          this.#router.close(stream);
        }
        this.#streams.delete(handle);
      }
    }

    const tabs = sessions.map(toTab);
    this.#update((current) => {
      const activeTab =
        tabs.find((tab) => tab.paneKey === current.activeTab)?.paneKey ??
        tabs[0]?.paneKey ??
        null;
      return {
        ...current,
        tabs,
        activeTab,
        launchers: LAUNCHERS,
        daemon: { ...current.daemon, terminalCount: sessions.length },
      };
    });
  }

  /** Record a connection failure, but not the same one over and over. */
  #recordOnce(command: StoreCommandName, message: string): void {
    // A reconnect loop that appended a notice per attempt would bury everything else in the
    // list within a minute of the daemon being down.
    if (this.#snapshot.errors.some((error) => error.message === message)) {
      return;
    }
    this.fail(command, message);
  }

  #update(next: (current: StoreSnapshot) => StoreSnapshot): void {
    const updated = next(this.#snapshot);
    if (updated === this.#snapshot) {
      return;
    }
    this.#snapshot = updated;
    for (const listener of [...this.#listeners]) {
      listener();
    }
  }
}

/**
 * Move the active project, taking everything that belonged to the old one with it.
 *
 * **The one place `activeProjectId` is written**, and it is a function rather than a rule
 * because the rule was already broken when it was only a rule. `store/types.ts` says the task
 * list is reset whenever the active project moves; the project moves in *three* places — a
 * click in the sidebar, the selection after **Add a project**, and the fallback to the first
 * row in `#refreshProjects`, which runs on every connect and every reconnect — and only the
 * first of them did it.
 *
 * What that cost is not cosmetic and not a race. Register a folder while the Tasks screen is
 * open and the active project becomes the new one, while `tasks` still holds the *previous*
 * repository's issues; the screen's refetch only fires on `idle`, so those rows stay on
 * screen under the new project's name. `Start →` on one of them then asks for a branch
 * derived from the old repository's issue, in the new repository — a worktree and an agent
 * session in the wrong project, one click away, which is the exact hazard three comments in
 * this file cite as the reason the reset exists.
 *
 * Returns `current` unchanged when the id has not moved, so it is safe on the paths that call
 * it every connect.
 */
function withActiveProject(
  current: StoreSnapshot,
  activeProjectId: ProjectId | null,
): StoreSnapshot {
  if (current.activeProjectId === activeProjectId) {
    return current;
  }
  return {
    ...current,
    activeProjectId,
    tasks: { phase: 'idle' },
    taskStart: { phase: 'idle' },
  };
}

/** One wire session as the chrome sees it. */
function toTab(session: WireSession): Tab {
  return {
    paneKey: session.paneKey,
    handle: session.handle,
    kind: session.kind,
    title: session.title,
  };
}

/**
 * The `+` menu, derived from `ShellProfile`'s four variants.
 *
 * Which shells are actually installed is the daemon's answer and will replace this; the
 * four kinds, though, are on the wire already, so deriving the menu from a closed enum is
 * reading the protocol rather than inventing a second one.
 *
 * The agent row is Claude alone. There is one agent in v1 and no provider trait (D-3, D-4),
 * and listing providers that cannot be started would be an affordance that looks live and
 * does nothing.
 */
const LAUNCHERS: readonly LauncherGroup[] = [
  {
    label: 'AGENTS',
    items: [
      { id: 'agent.claude', label: 'Claude', hint: 'default', kind: 'agent' as SessionKind },
    ],
  },
  {
    label: 'TERMINALS',
    items: [
      { id: 'shell.pwsh', label: 'PowerShell 7', hint: 'pwsh', kind: 'shell' as SessionKind },
      { id: 'shell.cmd', label: 'Command Prompt', hint: 'cmd', kind: 'shell' as SessionKind },
      { id: 'shell.git_bash', label: 'Git Bash', hint: 'bash', kind: 'shell' as SessionKind },
      { id: 'shell.wsl', label: 'WSL', hint: 'wsl', kind: 'shell' as SessionKind },
    ],
  },
];

/** The wire profile a launcher asks for, or `null` for the default agent. */
function profileFor(launcher: string): ShellProfile | null {
  switch (launcher) {
    case 'shell.pwsh':
      return { shell: 'pwsh' };
    case 'shell.cmd':
      return { shell: 'cmd' };
    case 'shell.git_bash':
      return { shell: 'git_bash' };
    case 'shell.wsl':
      return { shell: 'wsl', distro: null };
    default:
      return null;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
