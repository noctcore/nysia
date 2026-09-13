import { useRef, type KeyboardEvent, type RefObject } from 'react';

import { useCommands, useSnapshot } from '../store/hooks';
import type { Tab } from '../store/types';
import { GLYPH } from '../ui/glyphs';
import { isArrowKey, nextOption, tabbableIndex } from '../ui/roving';
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
 *
 * The roles are the APG tabs pattern, properly this time. The strip used to be a
 * `role="tablist"` that also contained a `+` button and, inside each tab, a separately
 * focusable close button — so the list was full of things that were not tabs, and there
 * was no arrow-key movement at all, which is the behaviour the role promises a screen
 * reader user. Now the tablist holds nothing but tabs, `+` is its sibling, and:
 *
 *  - the tab itself is the focusable element, with one tab stop for the whole strip;
 *  - Left and Right move focus and select, which is what every terminal and editor does
 *    and what APG calls automatic activation;
 *  - the close button is `tabIndex={-1}` — still clickable, still reachable in a screen
 *    reader's browse mode — and Delete or Backspace on the focused tab closes it, which is
 *    APG's pattern for a deletable tab.
 */
export function TabStrip() {
  const { tabs, activeTab } = useSnapshot();
  const commands = useCommands();
  const paneKeys = tabs.map((tab) => tab.paneKey);
  const tabbable = tabbableIndex(paneKeys, activeTab ?? '');

  // A ref map rather than a query: a `PaneKey` is `<tabId>:<leafId>`, and a colon is a
  // combinator in a CSS selector. Holding the nodes avoids having to escape data at all.
  const tabNodes = useRef(new Map<string, HTMLDivElement>());

  function focusTab(paneKey: string): void {
    tabNodes.current.get(paneKey)?.focus();
  }

  /**
   * Move DOM focus to whatever the store decided is active, once it has decided.
   *
   * Not to a neighbour computed here: which tab a provider activates after a close is its
   * own decision — a daemon may well pick the most recently used — and guessing would let
   * DOM focus and `activeTab` disagree. The surviving tabs are keyed by `PaneKey` and were
   * never unmounted, so this is a plain `focus()` and needs no effect.
   *
   * `hadFocus` is why it takes an argument at all. Closing a tab with the pointer, while
   * the caret sits somewhere else entirely — the prompt, the sidebar, a settings field —
   * used to yank focus into the strip. Focus is only ours to move when we are the ones who
   * destroyed the node holding it.
   */
  function focusAfterClose(hadFocus: boolean): void {
    if (!hadFocus) {
      return;
    }
    const next = commands.getSnapshot().activeTab;
    if (next === null) {
      document.querySelector<HTMLElement>('[data-new-session]')?.focus();
      return;
    }
    focusTab(next);
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>, tab: Tab) {
    if (isArrowKey(event.key)) {
      event.preventDefault();
      const next = nextOption(paneKeys, tab.paneKey, event.key);
      if (next !== undefined) {
        focusTab(next);
        commands.selectTab(next);
      }
      return;
    }
    if (event.key === 'Delete' || event.key === 'Backspace') {
      event.preventDefault();
      // The key arrived on the tab, so the caret is inside it by definition — but the node
      // is gone by the time the callback runs, so the answer is captured now.
      commands.closeTab(tab.paneKey, () => focusAfterClose(true));
    }
  }

  return (
    <div data-tauri-drag-region className="relative flex h-titlebar flex-1 items-end gap-1">
      <div role="tablist" aria-label="Sessions" className="flex items-end gap-1">
        {tabs.map((tab, index) => (
          <TabButton
            key={tab.paneKey}
            tab={tab}
            active={tab.paneKey === activeTab}
            tabbable={index === tabbable}
            nodes={tabNodes}
            onKeyDown={onKeyDown}
            onClose={focusAfterClose}
          />
        ))}
      </div>
      <NewTabButton />
    </div>
  );
}

function TabButton({
  tab,
  active,
  tabbable,
  nodes,
  onKeyDown,
  onClose,
}: {
  readonly tab: Tab;
  readonly active: boolean;
  readonly tabbable: boolean;
  readonly nodes: RefObject<Map<string, HTMLDivElement>>;
  readonly onKeyDown: (event: KeyboardEvent<HTMLDivElement>, tab: Tab) => void;
  readonly onClose: (hadFocus: boolean) => void;
}) {
  const commands = useCommands();

  return (
    // The tab is the focusable element, so the whole 34px chip takes the focus ring rather
    // than a word inside it.
    <div
      role="tab"
      ref={(node) => {
        if (node) {
          nodes.current.set(tab.paneKey, node);
        } else {
          nodes.current.delete(tab.paneKey);
        }
      }}
      aria-selected={active}
      tabIndex={tabbable ? 0 : -1}
      onClick={() => commands.selectTab(tab.paneKey)}
      onKeyDown={(event) => onKeyDown(event, tab)}
      className={`flex h-tab cursor-pointer items-center gap-2 whitespace-nowrap px-3.5 focus-visible:shadow-focus focus-visible:outline-none ${
        active
          ? 'border-line bg-bg1 text-fg -mb-px rounded-t-control border border-b-bg1'
          : 'text-fg2'
      }`}
    >
      <SessionGlyph kind={tab.kind} />
      <span className="max-w-[240px] truncate">{tab.title}</span>
      <button
        type="button"
        tabIndex={-1}
        aria-label={`Close ${tab.title}`}
        onMouseDown={(event) => {
          /*
           * Without this the focus test below is a tautology.
           *
           * `tabIndex={-1}` keeps the button out of the tab order, but in Chromium and
           * WebView2 it stays *click*-focusable: mousedown moves `document.activeElement`
           * onto the button before `onClick` runs, so `contains(activeElement)` is true on
           * every pointer close, whatever had focus a moment earlier. Suppressing the
           * default leaves the caret where the user put it, which is both the behaviour
           * this control should have and the only way the test below can answer honestly.
           *
           * This is invisible to a synthetic `element.click()`, which dispatches no
           * mousedown and moves no focus — so a probe driving the UI that way will keep
           * reporting the bug fixed. It has to be a real pointer event.
           */
          event.preventDefault();
        }}
        onClick={(event) => {
          // Otherwise the click bubbles to the tab and selects what it is about to close.
          event.stopPropagation();
          // Captured before the command, because the node is unmounted by the time the
          // callback runs. A pointer close from elsewhere in the window leaves the caret
          // where it was.
          const hadFocus = event.currentTarget.closest('[role="tab"]')?.contains(
            document.activeElement,
          );
          commands.closeTab(tab.paneKey, () => onClose(hadFocus === true));
        }}
        className="text-fg3 hover:text-fg ml-1.5 cursor-pointer border-0 bg-transparent p-0"
      >
        {GLYPH.close}
      </button>
    </div>
  );
}
