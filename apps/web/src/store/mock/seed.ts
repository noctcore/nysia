import type { AgentState } from '../../generated/AgentState';
import type { AgentStatus } from '../../generated/AgentStatus';
import type { AgentStatusRow } from '../../generated/AgentStatusRow';
import type { PaneKey } from '../../generated/PaneKey';
import type { SessionHandle } from '../../generated/SessionHandle';
import type { Issue } from '../../tasks/issue';
import { emptySnapshot, type LauncherGroup, type Project, type StoreSnapshot, type Tab } from '../types';

/**
 * The seed data from the design mock, transcribed from the `renderVals()` block in
 * `docs/design/Nysia-ADE.dc.html`.
 *
 * It lives behind `MockStore` and is never imported by a component — that is the rule the
 * whole store boundary exists to enforce. Wave 2 deletes nothing here; it just stops
 * constructing the mock.
 *
 * Session ages are offsets from the moment the store is built rather than the mock's
 * pre-formatted `21h`. Two reasons: the daemon sends timestamps, so the sidebar has to do
 * the arithmetic either way; and a fixed epoch would make the seeded ages drift further from
 * the design every day the repository sits there.
 *
 * The session rows are `nysia-proto`'s `SessionSummary` now rather than a hand-written
 * lookalike, so `exitStatus: null` — still running — stands where a five-state `status` used
 * to be guessed. A fixture whose shape is the wire's is the only kind that proves anything
 * about what a component will be handed.
 */

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

function pane(tab: number, leaf: number): PaneKey {
  return `tab_${tab}:leaf_${leaf}`;
}

function handle(id: string): SessionHandle {
  return `sess_${id}`;
}

const KIREI_PANE = pane(1, 1);
const PWSH_PANE = pane(2, 1);
const CODEX_PANE = pane(3, 1);
const WSL_PANE = pane(4, 1);

const KIREI_HANDLE = handle('9f1c0a4e-0f2b-4d21-9a70-1d2b3c4d5e6f');
const PWSH_HANDLE = handle('2b7d81c3-5e44-4f0a-8c19-7a6b5c4d3e2f');
const CODEX_HANDLE = handle('c4e5f607-1829-4a3b-b5c6-d7e8f90a1b2c');
const WSL_HANDLE = handle('7d6e5f40-3b2a-4190-8e7d-6c5b4a392817');

export const SEED_ACTIVE_PROJECT = 'D:/dev/shiroani';

/** Project names in sidebar order, which is the order the mock lists them in. */
export const SEED_PROJECT_NAMES: readonly string[] = [
  'Settly',
  'nightcore',
  'shiranami',
  'shiroani',
  'omniscribe',
  'portfolio',
  'vr-chat-invite-desktop',
  'deskmate',
  'szlak',
  'mat-majka',
];

/**
 * The sidebar's `Dev` group.
 *
 * The mock lists four projects, expands the fourth into its worktree block, then lists six
 * more below it — so `shiroani` is the active project and every other row is collapsed.
 * Only the active project carries worktrees here, which is exactly what a daemon would
 * send: enumerating branches and sessions for ten repositories nobody is looking at is
 * work for nothing.
 */
function seedProjects(now: number): readonly Project[] {
  return SEED_PROJECT_NAMES.map((name) => {
    const id = `D:/dev/${name}`;
    if (id !== SEED_ACTIVE_PROJECT) {
      return { id, name, group: 'Dev', worktrees: [] };
    }
    return {
      id,
      name,
      group: 'Dev',
      worktrees: [
        {
          branch: 'master',
          isPrimary: true,
          sessions: [
            {
              paneKey: KIREI_PANE,
              handle: KIREI_HANDLE,
              kind: 'agent' as const,
              title: 'Kirei deps but we already did…',
              createdAtMs: now - 21 * HOUR,
              exitStatus: null,
            },
            {
              paneKey: PWSH_PANE,
              handle: PWSH_HANDLE,
              kind: 'shell' as const,
              title: 'pwsh',
              createdAtMs: now - 3 * MINUTE,
              exitStatus: null,
            },
          ],
        },
      ],
    };
  });
}

/**
 * The four tabs in the mock's strip.
 *
 * The mock draws the Codex tab with its own glyph; v1 has one agent and no provider trait
 * (D-3, D-4), so `SessionKind` is the only discriminator and every agent tab gets the
 * accent asterisk. The Codex row survives here as a title, which is all it ever was.
 */
export const SEED_TABS: readonly Tab[] = [
  {
    paneKey: KIREI_PANE,
    handle: KIREI_HANDLE,
    kind: 'agent',
    title: 'Kirei deps but we already did…',
  },
  { paneKey: PWSH_PANE, handle: PWSH_HANDLE, kind: 'shell', title: 'pwsh · shiroani' },
  { paneKey: CODEX_PANE, handle: CODEX_HANDLE, kind: 'agent', title: 'codex · deskmate' },
  { paneKey: WSL_PANE, handle: WSL_HANDLE, kind: 'shell', title: 'wsl · szlak' },
];

export const SEED_ACTIVE_TAB = KIREI_PANE;

/**
 * The `+` menu, grouped AGENTS then TERMINALS.
 *
 * Which shells exist on the machine is something only the daemon can answer, which is why
 * this is store data and not a constant in the menu component.
 *
 * The four shell ids are the ones `DaemonStore.profileFor` switches on, spelled exactly as
 * it spells them, in the order it lists them. They had drifted two ways: `shell.gitbash`
 * against the daemon's `shell.git_bash`, and a WSL row hinting `bash` where the daemon
 * hints `wsl`.
 *
 * The id was invisible — nothing rendered it until the `+` menu began keying its glyph off
 * it. The hint was not: `NewTabButton` renders `item.hint` and always has, so the mock's
 * WSL row sat on screen reading `bash`, which is Git Bash's answer, in the menu whose whole
 * job is telling the two apart.
 *
 * Underneath both is the cost that does not depend on anything rendering: a fixture whose
 * identifiers disagree with the thing it stands in for makes every test that passes against
 * it prove something slightly false.
 */
export const SEED_LAUNCHERS: readonly LauncherGroup[] = [
  {
    label: 'Agents',
    items: [
      { id: 'agent.claude', label: 'Claude', hint: 'default', kind: 'agent' },
      { id: 'agent.codex', label: 'Codex', hint: '', kind: 'agent' },
      { id: 'agent.gemini', label: 'Gemini', hint: '', kind: 'agent' },
      { id: 'agent.opencode', label: 'OpenCode', hint: '', kind: 'agent' },
    ],
  },
  {
    label: 'Terminals',
    items: [
      { id: 'shell.pwsh', label: 'PowerShell 7', hint: 'pwsh', kind: 'shell' },
      { id: 'shell.cmd', label: 'Command Prompt', hint: 'cmd', kind: 'shell' },
      { id: 'shell.git_bash', label: 'Git Bash', hint: 'bash', kind: 'shell' },
      { id: 'shell.wsl', label: 'WSL', hint: 'wsl', kind: 'shell' },
    ],
  },
];

/**
 * One status row, with the six fields a seeded row does not vary spelled once.
 *
 * `question` is `null` rather than a transcribed payload: it is `unknown` on the wire
 * because Claude's `tool_input` is arbitrary JSON, and nothing in v0.2 renders it. A seed
 * that invented a shape for it would be the first hand-written opinion about a field D-13
 * gives Rust.
 */
function statusRow(
  pane: PaneKey,
  state: AgentState,
  observedAt: number,
): AgentStatusRow {
  return {
    pane,
    state,
    question: null,
    isInterrupt: state === 'interrupted',
    sessionBoundary: false,
    agentId: null,
    observedAt,
    restoredUnconfirmed: false,
  };
}

/**
 * Status for the two agent panes the design mock shows.
 *
 * Only the agents: a shell has no agent and therefore no row, which is the case the dot
 * paints as *unknown* rather than as idle, and the seed should exercise it rather than
 * paper over it.
 *
 * `observedAt` is an offset from the moment the store is built, for `createdAtMs`'s reason
 * and one more: staleness is a comparison against the clock, so a fixed epoch would make
 * every seeded dot decay to *active* the first time anyone opened the repository a day
 * later.
 *
 * **What this proves and what it does not.** A teal dot here proves the mapping and the
 * rendering. It proves nothing about the wire — there is no daemon ingest and no status RPC
 * until v0.2 wave C, so the daemon-backed provider reports an empty list and the window
 * paints the accent.
 */
export function seedAgentStatus(now: number): readonly AgentStatus[] {
  return [
    // Mid-turn, and recent enough to be fresh: the design mock's live session.
    { lead: statusRow(KIREI_PANE, 'working', now - 2 * MINUTE), subagents: [] },
    // Blocked on a question, which is the transition the notification rule exists for.
    { lead: statusRow(CODEX_PANE, 'waiting', now - 40 * 1000), subagents: [] },
  ];
}

/**
 * The mock's GitHub issues, transcribed from the `issues` array in the design HTML.
 *
 * Same rule as the project names and the session ages: what the mock shows comes from the
 * design file rather than from somebody's imagination, so the screen this store drives is
 * the screen the design draws. The mock's rows are pre-formatted — a hash-prefixed id string
 * and `updated: '7 days ago'` — because it is a drawing; these are the fields `gh` sends, with
 * the age as an offset from `now` so that `updatedPhrase` prints the mock's own words
 * however long this repository sits there.
 *
 * Three fields the mock's array does not have, because the mock hardcodes them in its
 * markup: the author and repository on every row's sub-line — `Shironex` and `Settly` —
 * and the URL, which is where {@link issue.repositoryOf} recovers the repository from since
 * `gh issue list --json` has no field for it. The URLs are built from the same pair.
 *
 * **Not a task model** (D-5). This is a fixture behind the mock provider; nothing persists
 * it, and `MockStore.refreshTasks` hands it over as the answer to a query.
 */
export function createSeedIssues(now: number = Date.now()): readonly Issue[] {
  return SEED_ISSUES.map(([number, title, labels, days]) => ({
    number,
    title,
    state: 'open' as const,
    updatedAt: new Date(now - days * DAY).toISOString(),
    url: `https://github.com/Shironex/Settly/issues/${number}`,
    author: 'Shironex',
    // Copied rather than shared. `Issue.labels` is generated as `Array<string>` — ts-rs emits
    // a `Vec` that way and nothing on this side may edit the output (D-13) — so the row's own
    // list has to be a mutable one, and handing out the frozen literal below would make every
    // caller's row alias the same array.
    labels: [...labels],
  }));
}

/** The mock's rows: number, title, labels, and how many days ago it was updated. */
const SEED_ISSUES: readonly [number, string, readonly string[], number][] = [
  [200, 'soft-deletable-tables-require-deleted-at: two unguarded singular-selector sites it cannot see', ['enhancement'], 7],
  [199, 'data-slot="agenda-show-more" ships with no consumer: the pager lookup it was added for was replaced', ['bug'], 7],
  [198, 'Sidebar biuro identity renders nothing while loading and disappears silently on error', ['bug', 'design-adoption'], 7],
  [197, 'PortalAccountRepository.findAccountsForFirma outlived the offboarding path it fronted', ['bug'], 7],
  [196, 'Contract.wymiarEtatu is unvalidated free text, so no working-time arithmetic can pro-rate a part-timer', ['enhancement'], 9],
  [114, 'Wayfinder map: attendance and working time (Czas pracy)', ['wayfinder:map'], 9],
  [161, 'HR export for PIP/ZUS inspections does not exist (part 1, the RODO account export, has shipped)', ['enhancement'], 9],
  [154, 'SME: odpowiedzi rodzicow na 52 pytania (ankiety Google Forms) — watek zbiorczy', ['wayfinder:task'], 9],
  [192, 'No vendor shell: nothing in the product can see across tenants', ['enhancement'], 15],
  [93, 'Wayfinder map: leave and absence module (urlopy i nieobecnosci)', ['wayfinder:map'], 49],
  [131, 'T17: SPEC-attendance assembly and sign-off', ['wayfinder:grilling'], 49],
];

export function createSeedSnapshot(now: number = Date.now()): StoreSnapshot {
  return {
    ...emptySnapshot('ready'),
    nav: 'session',
    projects: seedProjects(now),
    activeProjectId: SEED_ACTIVE_PROJECT,
    tabs: SEED_TABS,
    activeTab: SEED_ACTIVE_TAB,
    projectsUnavailable: null,
    launchers: SEED_LAUNCHERS,
    daemon: {
      memoryBytes: 4 * 1024 ** 3,
      terminalCount: 9,
      worktreeCount: 3,
    },
    usage: [
      { label: '5h', percentLeft: 100 },
      { label: '6d', percentLeft: 97 },
      { label: 'Fable', percentLeft: 99 },
    ],
    agentStatus: seedAgentStatus(now),
  };
}
