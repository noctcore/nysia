import { sessionLabel } from './sessionLabel';
import type { PaneKey } from './generated/PaneKey';
import type { SessionKind } from './generated/SessionKind';

const KINDS: readonly SessionKind[] = ['shell', 'agent'];
const EXAMPLE_PANE: PaneKey = 'tab_1:leaf_1';

/**
 * The wave-0 placeholder. It exists to prove one thing: a type generated from Rust by
 * ts-rs is importable, typechecked and usable here. The real chrome — the 40/1fr/30 rows,
 * the 48/222/1fr body, the tab strip and the projects sidebar — lands in wave 1 (W3).
 */
export function App() {
  return (
    <main className="flex h-full flex-col items-center justify-center gap-4 bg-bg1 px-6 text-fg">
      <h1 className="text-2xl font-medium">Nysia</h1>
      <p className="text-fg2 text-sm">
        Wave 0 scaffold. The daemon, the PTYs and the chrome are not built yet.
      </p>
      <dl className="border-line bg-bg0 grid grid-cols-[auto_1fr] gap-x-6 gap-y-2 rounded-lg border p-5 text-sm">
        <dt className="text-fg3">PaneKey</dt>
        <dd className="font-mono">{EXAMPLE_PANE}</dd>
        {KINDS.map((kind) => (
          <div key={kind} className="contents">
            <dt className="text-fg3">SessionKind</dt>
            <dd>
              <span className="font-mono">{kind}</span>
              <span className="text-fg2"> → {sessionLabel(kind)}</span>
            </dd>
          </div>
        ))}
      </dl>
      <p className="text-fg3 text-xs">
        Those types are generated from <code>crates/nysia-proto</code> by ts-rs.
      </p>
    </main>
  );
}
