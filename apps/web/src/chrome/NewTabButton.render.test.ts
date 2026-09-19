import { createElement, isValidElement, type ReactElement, type ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';

import type { Launcher, LauncherGroup, LauncherId } from '../store/types';
import { NewSessionMenu, type NewSessionMenuProps } from './NewTabButton';

/*
 * The `+` menu, rendered and pressed — node-only (D-18), the way `StopAgentDialog`'s tests do
 * it. The markup cases render with `renderToStaticMarkup` and read what the menu draws. The
 * pressing cases call `NewSessionMenu` as a function — it has no hooks for exactly this
 * reason — find a control in the element tree it returns, and invoke that control's own
 * handler. What neither can see is the browser refusing a click on a `disabled` button; the
 * markup cases hold that the attribute is there, and the pressing cases hold that the
 * handler refuses too.
 */

const NO_PWSH = 'the pwsh profile is unavailable: pwsh was not found on PATH';

function launcher(id: LauncherId, label: string, unavailable: string | null): Launcher {
  return {
    id,
    label,
    hint: id.split('.').at(-1) ?? '',
    kind: id.startsWith('agent.') ? 'agent' : 'shell',
    unavailable,
  };
}

/** The menu a Windows machine without PowerShell 7 gets from its daemon. */
const WITHOUT_PWSH: readonly LauncherGroup[] = [
  { label: 'AGENTS', items: [launcher('agent.claude', 'Claude', null)] },
  {
    label: 'TERMINALS',
    items: [
      launcher('shell.pwsh', 'PowerShell 7', NO_PWSH),
      launcher('shell.cmd', 'Command Prompt', null),
      launcher('shell.git_bash', 'Git Bash', null),
      launcher('shell.wsl', 'WSL', null),
    ],
  },
];

/** What the menu asked the store to do, and where it left itself. */
interface Pressed {
  readonly opened: LauncherId[];
  refreshes: number;
  readonly openSet: boolean[];
}

function props(open: boolean, pressed: Pressed): NewSessionMenuProps {
  return {
    open,
    setOpen: (next) => pressed.openSet.push(next),
    launchers: WITHOUT_PWSH,
    commands: {
      openTab: (id) => pressed.opened.push(id),
      refreshLaunchers: () => {
        pressed.refreshes += 1;
      },
    },
  };
}

function nothingPressed(): Pressed {
  return { opened: [], refreshes: 0, openSet: [] };
}

type Props = Record<string, unknown> & { readonly children?: ReactNode };

/** Every element in a tree, parents before children. Components are not expanded. */
function elements(node: ReactNode): ReactElement<Props>[] {
  if (Array.isArray(node)) {
    return node.flatMap((child: ReactNode) => elements(child));
  }
  if (!isValidElement<Props>(node)) {
    return [];
  }
  return [node, ...elements(node.props.children)];
}

/** An element's text, as a reader would get it. */
function text(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') {
    return String(node);
  }
  if (Array.isArray(node)) {
    return node.map((child: ReactNode) => text(child)).join('');
  }
  return isValidElement<Props>(node) ? text(node.props.children) : '';
}

/** The one button whose text contains `label`. */
function button(root: ReactElement<Props>, label: string): ReactElement<Props> {
  const found = elements(root).filter(
    (element) => element.type === 'button' && text(element.props.children).includes(label),
  );
  expect(found, `the menu has no single ${label} button`).toHaveLength(1);
  return found[0] as ReactElement<Props>;
}

/** The `+` itself, which is labelled rather than worded. */
function plus(root: ReactElement<Props>): ReactElement<Props> {
  const found = elements(root).filter((element) => element.props['data-new-session'] === true);
  expect(found, 'the menu has no single + button').toHaveLength(1);
  return found[0] as ReactElement<Props>;
}

function press(element: ReactElement<Props>): void {
  const listener = element.props.onClick;
  expect(typeof listener, 'the control has no onClick').toBe('function');
  (listener as () => void)();
}

/** The whole opening tag of the button whose text contains `label`, as rendered. */
function tagOf(markup: string, label: string): string {
  const buttons = [...markup.matchAll(/<button[^>]*>(?:(?!<\/button>).)*<\/button>/gs)].map(
    (match) => match[0],
  );
  const found = buttons.filter((whole) => whole.includes(label));
  expect(found, `the markup has no single ${label} button`).toHaveLength(1);
  const whole = found[0] ?? '';
  return whole.slice(0, whole.indexOf('>') + 1);
}

describe('a shell the daemon cannot launch', () => {
  const shown = renderToStaticMarkup(createElement(NewSessionMenu, props(true, nothingPressed())));

  it('is drawn, not hidden, with the daemon’s own reason', () => {
    // A person who wants PowerShell 7 has to learn that it is missing and why. A menu that
    // left the row out would tell them nothing, which is what they had before the daemon
    // could be asked.
    expect(shown).toContain('PowerShell 7');
    expect(shown).toContain(NO_PWSH);
    // Under its own name, not somewhere else in the menu.
    expect(tagOf(shown, 'PowerShell 7')).toBeTruthy();
    const row = shown.slice(shown.indexOf('PowerShell 7'));
    expect(row.indexOf(NO_PWSH)).toBeLessThan(row.indexOf('</button>'));
  });

  it('is disabled, so it does not look live and then fail', () => {
    // `App.render.test.ts`'s standing rule: no affordance that looks live and does nothing.
    // A row that stays clickable and answers with a notice one round trip later is that.
    expect(tagOf(shown, 'PowerShell 7')).toMatch(/\sdisabled=""/);
    for (const live of ['Claude', 'Command Prompt', 'Git Bash', 'WSL']) {
      expect(tagOf(shown, live), `${live} was drawn as unavailable`).not.toContain('disabled');
    }
  });

  it('wears no hover and no pointer, and reads dimmer than a live row', () => {
    const refused = tagOf(shown, 'PowerShell 7');
    expect(refused).not.toContain('hover:bg-bg3');
    expect(refused).not.toContain('cursor-pointer');
    expect(refused).toContain('text-fg3');
    const live = tagOf(shown, 'Command Prompt');
    expect(live).toContain('hover:bg-bg3');
    expect(live).toContain('text-fg ');
  });

  it('paints no colour the theme switcher cannot reach', () => {
    expect([...shown.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
  });
});

describe('pressing the menu', () => {
  it('opens nothing from a row the daemon refused, and stays open', () => {
    // Held by the handler as well as by `disabled`: the attribute is the browser's promise,
    // and this is the menu's own.
    const pressed = nothingPressed();
    press(button(NewSessionMenu(props(true, pressed)), 'PowerShell 7'));
    expect(pressed.opened).toEqual([]);
    expect(pressed.openSet).toEqual([]);
  });

  it('opens a row the daemon has not refused, and closes', () => {
    const pressed = nothingPressed();
    press(button(NewSessionMenu(props(true, pressed)), 'Command Prompt'));
    expect(pressed.opened).toEqual(['shell.cmd']);
    expect(pressed.openSet).toEqual([false]);
  });

  it('asks the daemon again as it opens, and not as it closes', () => {
    // The answer is computed when it is asked, so asking on open is what offers a shell
    // installed since the last answer. Closing is not a question.
    const opening = nothingPressed();
    press(plus(NewSessionMenu(props(false, opening))));
    expect(opening.refreshes).toBe(1);
    expect(opening.openSet).toEqual([true]);

    const closing = nothingPressed();
    press(plus(NewSessionMenu(props(true, closing))));
    expect(closing.refreshes).toBe(0);
    expect(closing.openSet).toEqual([false]);
  });
});
