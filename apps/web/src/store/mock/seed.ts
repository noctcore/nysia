import type { PaneKey } from '../../generated/PaneKey';
import type { SessionHandle } from '../../generated/SessionHandle';
import type {
  LauncherGroup,
  Project,
  SessionSummary,
  StoreSnapshot,
  Tab,
} from '../types';

/**
 * The seed data from the design mock, transcribed from the `renderVals()` block in
 * `docs/design/Nysia-ADE.dc.html`.
 *
 * It lives behind `MockStore` and is never imported by a component — that is the rule the
 * whole store boundary exists to enforce. Wave 2 deletes nothing here; it just stops
 * constructing the mock.
 *
 * Ages are stored as offsets from a fixed reference instant rather than as the mock's
 * pre-formatted `21h`, because the daemon will send timestamps and the sidebar has to do
 * the arithmetic either way.
 */

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

/** Midday on the day the design was pulled, so the seeded ages are stable to read. */
export const SEED_NOW = Date.parse('2026-09-13T12:16:00Z');

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

const SHIROANI_SESSIONS: readonly SessionSummary[] = [
  {
    paneKey: KIREI_PANE,
    handle: handle('9f1c0a4e-0f2b-4d21-9a70-1d2b3c4d5e6f'),
    kind: 'agent',
    title: 'Kirei deps but we already did…',
    status: 'running',
    startedAt: SEED_NOW - 21 * HOUR,
  },
  {
    paneKey: PWSH_PANE,
    handle: handle('2b7d81c3-5e44-4f0a-8c19-7a6b5c4d3e2f'),
    kind: 'shell',
    title: 'pwsh',
    status: 'idle',
    startedAt: SEED_NOW - 3 * MINUTE,
  },
];

/**
 * The sidebar's `Dev` group.
 *
 * The mock lists four projects, expands the fourth into its worktree block, then lists six
 * more below it — so `shiroani` is the active project and every other row is collapsed.
 */
export const SEED_PROJECTS: readonly Project[] = [
  { id: 'D:/dev/Settly', name: 'Settly', group: 'Dev', worktrees: [] },
  { id: 'D:/dev/nightcore', name: 'nightcore', group: 'Dev', worktrees: [] },
  { id: 'D:/dev/shiranami', name: 'shiranami', group: 'Dev', worktrees: [] },
  {
    id: 'D:/dev/shiroani',
    name: 'shiroani',
    group: 'Dev',
    worktrees: [{ branch: 'master', isPrimary: true, sessions: SHIROANI_SESSIONS }],
  },
  { id: 'D:/dev/omniscribe', name: 'omniscribe', group: 'Dev', worktrees: [] },
  { id: 'D:/dev/portfolio', name: 'portfolio', group: 'Dev', worktrees: [] },
  {
    id: 'D:/dev/vr-chat-invite-desktop',
    name: 'vr-chat-invite-desktop',
    group: 'Dev',
    worktrees: [],
  },
  { id: 'D:/dev/deskmate', name: 'deskmate', group: 'Dev', worktrees: [] },
  { id: 'D:/dev/szlak', name: 'szlak', group: 'Dev', worktrees: [] },
  { id: 'D:/dev/mat-majka', name: 'mat-majka', group: 'Dev', worktrees: [] },
];

export const SEED_ACTIVE_PROJECT = 'D:/dev/shiroani';

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
    handle: handle('9f1c0a4e-0f2b-4d21-9a70-1d2b3c4d5e6f'),
    kind: 'agent',
    title: 'Kirei deps but we already did…',
  },
  {
    paneKey: PWSH_PANE,
    handle: handle('2b7d81c3-5e44-4f0a-8c19-7a6b5c4d3e2f'),
    kind: 'shell',
    title: 'pwsh · shiroani',
  },
  {
    paneKey: CODEX_PANE,
    handle: handle('c4e5f607-1829-4a3b-b5c6-d7e8f90a1b2c'),
    kind: 'agent',
    title: 'codex · deskmate',
  },
  {
    paneKey: WSL_PANE,
    handle: handle('7d6e5f40-3b2a-4190-8e7d-6c5b4a392817'),
    kind: 'shell',
    title: 'wsl · szlak',
  },
];

export const SEED_ACTIVE_TAB = KIREI_PANE;

/**
 * The `+` menu, grouped AGENTS then TERMINALS.
 *
 * Which shells exist on the machine is something only the daemon can answer, which is why
 * this is store data and not a constant in the menu component.
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
      { id: 'shell.wsl', label: 'WSL · Ubuntu', hint: 'bash', kind: 'shell' },
      { id: 'shell.gitbash', label: 'Git Bash', hint: '', kind: 'shell' },
    ],
  },
];

export const SEED_SNAPSHOT: StoreSnapshot = {
  nav: 'session',
  projects: SEED_PROJECTS,
  activeProjectId: SEED_ACTIVE_PROJECT,
  tabs: SEED_TABS,
  activeTab: SEED_ACTIVE_TAB,
  launchers: SEED_LAUNCHERS,
  daemon: {
    connected: true,
    memoryBytes: 4_294_967_296,
    terminalCount: 9,
    worktreeCount: 3,
  },
  usage: [
    { label: '5h', percentLeft: 100 },
    { label: '6d', percentLeft: 97 },
    { label: 'Fable', percentLeft: 99 },
  ],
};
