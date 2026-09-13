import type { SessionKind } from './generated/SessionKind';

/**
 * How a session kind is spelled in the UI.
 *
 * The argument type is generated from `nysia-proto`, so adding a third session kind in
 * Rust breaks this switch at `pnpm typecheck` rather than at runtime. That is the whole
 * point of D-13 and the reason this function exists in the scaffold at all.
 */
export function sessionLabel(kind: SessionKind): string {
  switch (kind) {
    case 'shell':
      return 'Shell';
    case 'agent':
      return 'Agent';
  }
}
