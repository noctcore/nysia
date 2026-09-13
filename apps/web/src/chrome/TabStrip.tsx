import { useSnapshot, useStore } from '../store/useStore';
import type { Tab } from '../store/types';
import { GLYPH } from '../ui/glyphs';
import { NewTabButton } from './NewTabButton';
import { SessionGlyph } from './SessionGlyph';

/**
 * The tab strip (design-spec.md §2).
 *
 * A tab is a session and a session is an agent or a shell — one uniform surface, no
 * separate terminal panel and chat panel. The tabs are bottom-aligned in the 40px titlebar
 * and 34px tall, so the active one reaches the body: it fills `bg1`, takes a 1px `line`
 * border rounded `8px 8px 0 0`, and then paints its *bottom* border `bg1` as well and
 * drops a pixel, which is what makes it merge into the pane instead of sitting on a seam.
 */
export function TabStrip() {
  const { tabs, activeTab } = useSnapshot();

  return (
    <div
      className="relative flex h-titlebar flex-1 items-end gap-1"
      role="tablist"
      aria-label="Sessions"
    >
      {tabs.map((tab) => (
        <TabButton key={tab.paneKey} tab={tab} active={tab.paneKey === activeTab} />
      ))}
      <NewTabButton />
    </div>
  );
}

function TabButton({ tab, active }: { readonly tab: Tab; readonly active: boolean }) {
  const store = useStore();

  return (
    <div
      className={`flex h-tab items-center gap-2 whitespace-nowrap px-3.5 ${
        active
          ? 'border-line bg-bg1 text-fg -mb-px rounded-t-control border border-b-bg1'
          : 'text-fg2'
      }`}
    >
      <SessionGlyph kind={tab.kind} />
      <button
        type="button"
        role="tab"
        aria-selected={active}
        onClick={() => void store.selectTab(tab.paneKey)}
        className="max-w-[240px] cursor-pointer truncate border-0 bg-transparent p-0 focus-visible:shadow-focus focus-visible:outline-none"
      >
        {tab.title}
      </button>
      <button
        type="button"
        aria-label={`Close ${tab.title}`}
        onClick={() => void store.closeTab(tab.paneKey)}
        className="text-fg3 hover:text-fg ml-1.5 cursor-pointer border-0 bg-transparent p-0 focus-visible:shadow-focus focus-visible:outline-none"
      >
        {GLYPH.close}
      </button>
    </div>
  );
}
