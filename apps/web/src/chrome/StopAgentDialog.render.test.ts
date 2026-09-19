import { createElement, isValidElement, type ReactElement, type ReactNode } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { GeneralPane } from '../settings/GeneralPane';
import { CONFIRM_STOP_AGENT_LABEL, loadGeneral } from '../settings/generalPreferences';
import { STOP_TITLE, type PendingStop, type StopGate } from './stopAgent';
import { StopAgentDialog } from './StopAgentDialog';
import { AGENT, settled, stopScene, type Scene } from './stopAgentFixture';

/*
 * The stop dialog, rendered and pressed — still node-only (D-18).
 *
 * Two kinds of case. The markup cases render with `renderToStaticMarkup` and assert what the
 * dialog itself draws: the question, the session it names, the order and paint of its two
 * buttons. The pressing cases call the component as a function — it has no hooks for exactly
 * this reason — find a control in the element tree it returns, invoke that control's own
 * handler, and then look at the **store**. That is as close to a click as a DOM-less suite
 * gets, and it keeps the whole path from the button to `closeTab` under test: a Cancel that
 * called through would close the session, and these cases would see it gone.
 *
 * What neither can see is the browser's half — `showModal()`, focus moving to Cancel, Escape
 * becoming a `cancel` event. Those are the platform's, and the handler each one reaches is
 * pressed here directly.
 */

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

/** The dialog the scene's pending close would render, as an element tree. */
function dialog(scene: Scene): ReactElement<Props> {
  const pending = scene.gate.getSnapshot();
  expect(pending, 'the scene is not asking anything').not.toBe(null);
  return StopAgentDialog({ pending: pending as PendingStop, gate: scene.gate });
}

function button(root: ReactElement<Props>, label: string): ReactElement<Props> {
  const found = elements(root).filter(
    (element) => element.type === 'button' && text(element.props.children) === label,
  );
  expect(found, `the dialog has no single ${label} button`).toHaveLength(1);
  return found[0] as ReactElement<Props>;
}

/** Invoke one of an element's own event handlers, with whatever event it reads. */
function fire(element: ReactElement<Props>, handler: string, event: object = {}): void {
  const listener = element.props[handler];
  expect(typeof listener, `the element has no ${handler}`).toBe('function');
  (listener as (event: object) => void)(event);
}

/** A scene with a working agent and the question already asked about it. */
function asking(): Scene {
  const scene = stopScene([[AGENT.paneKey, 'working']]);
  scene.gate.request(AGENT);
  return scene;
}

function markup(scene: Scene): string {
  const pending = scene.gate.getSnapshot();
  if (pending === null) {
    // Thrown rather than asserted: this runs while the suite is collected, outside a case.
    throw new Error('the scene is not asking anything');
  }
  const gate: StopGate = scene.gate;
  return renderToStaticMarkup(createElement(StopAgentDialog, { pending, gate }));
}

describe('what the dialog says', () => {
  const shown = markup(asking());

  it('asks the question and names the session it would stop', () => {
    expect(shown).toContain(STOP_TITLE);
    expect(shown).toContain(AGENT.title);
    expect(shown).toContain('current work');
    // The heading is the dialog's name and the sentence its description, for a reader that
    // announces the dialog rather than walking it.
    expect(shown).toMatch(/<dialog[^>]*aria-labelledby="stop-agent-title"/);
    expect(shown).toContain('id="stop-agent-title"');
    expect(shown).toMatch(/<dialog[^>]*aria-describedby="stop-agent-reason"/);
  });

  it('shows the same dot the tab does', () => {
    expect(shown).toContain('data-status="running"');
  });

  it('puts the destructive button on the right, painted as destructive', () => {
    const buttons = [...shown.matchAll(/<button[^>]*>([^<]*)<\/button>/g)];
    expect(buttons.map((match) => match[1])).toEqual(['Cancel', 'Stop agent']);
    const [cancel, stop] = buttons.map((match) => match[0]);
    // `status-failed`, which no accent can recolour — not `acc`, which every accent does.
    expect(stop).toContain('bg-status-failed');
    expect(stop).not.toMatch(/\bbg-acc/);
    expect(cancel).not.toContain('status-failed');
  });

  it('starts with the box unticked', () => {
    const box = shown.match(/<input[^>]*type="checkbox"[^>]*>/)?.[0];
    expect(box, 'the dialog has no checkbox').toBeDefined();
    expect(box).not.toMatch(/\bchecked\b/);
  });

  it('says which Settings row turns the question back on', () => {
    expect(shown).toContain(`Settings › General › ${CONFIRM_STOP_AGENT_LABEL}`);
  });

  it('paints no colour the theme switcher cannot reach', () => {
    expect([...shown.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((match) => match[0])).toEqual([]);
  });
});

describe('what the dialog does', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('Cancel leaves the session alive', async () => {
    const scene = asking();
    fire(button(dialog(scene), 'Cancel'), 'onClick');
    await settled();
    expect(scene.isOpen(AGENT)).toBe(true);
    expect(scene.gate.getSnapshot(), 'the question is still up').toBe(null);
  });

  it('Escape leaves the session alive too', async () => {
    const scene = asking();
    let prevented = false;
    fire(dialog(scene), 'onCancel', {
      preventDefault: () => {
        prevented = true;
      },
    });
    await settled();
    expect(scene.isOpen(AGENT)).toBe(true);
    expect(scene.gate.getSnapshot()).toBe(null);
    // The gate decides when the dialog goes, not the browser closing the element itself.
    expect(prevented).toBe(true);
  });

  it('cancels on a press on the backdrop, and not on a press inside the box', async () => {
    const scene = asking();
    const root = dialog(scene);
    fire(root, 'onClick', { target: {}, currentTarget: {} });
    expect(scene.gate.getSnapshot(), 'a press inside the box dismissed it').not.toBe(null);

    const backdrop = {};
    fire(root, 'onClick', { target: backdrop, currentTarget: backdrop });
    await settled();
    expect(scene.gate.getSnapshot()).toBe(null);
    expect(scene.isOpen(AGENT)).toBe(true);
  });

  it('Stop agent ends the session', async () => {
    const scene = asking();
    fire(button(dialog(scene), 'Stop agent'), 'onClick');
    await settled();
    expect(scene.isOpen(AGENT)).toBe(false);
  });

  it("counts Don't ask again only when the agent is stopped", () => {
    const cancelled = asking();
    const box = (scene: Scene) =>
      elements(dialog(scene)).find((element) => element.type === 'input');
    const cancelledBox = box(cancelled);
    expect(cancelledBox, 'the dialog has no checkbox').toBeDefined();
    fire(cancelledBox as ReactElement<Props>, 'onChange', { currentTarget: { checked: true } });
    fire(button(dialog(cancelled), 'Cancel'), 'onClick');
    expect(loadGeneral(cancelled.storage).confirmStopAgent).toBe(true);

    const stopped = asking();
    fire(box(stopped) as ReactElement<Props>, 'onChange', { currentTarget: { checked: true } });
    fire(button(dialog(stopped), 'Stop agent'), 'onClick');
    expect(loadGeneral(stopped.storage).confirmStopAgent).toBe(false);
  });

  it('leaves the Settings row it names showing the choice, so it can be undone', () => {
    const scene = asking();
    const box = elements(dialog(scene)).find((element) => element.type === 'input');
    fire(box as ReactElement<Props>, 'onChange', { currentTarget: { checked: true } });
    fire(button(dialog(scene), 'Stop agent'), 'onClick');

    // The same storage the pane reads through `settingsStorage()`.
    vi.stubGlobal('localStorage', scene.storage);
    const pane = renderToStaticMarkup(createElement(GeneralPane));
    const row = pane.match(
      new RegExp(`<button[^>]*aria-label="${CONFIRM_STOP_AGENT_LABEL}"[^>]*>`),
    )?.[0];
    expect(row, `Settings › General has no ${CONFIRM_STOP_AGENT_LABEL} switch`).toBeDefined();
    expect(row).toContain('role="switch"');
    expect(row).toContain('aria-checked="false"');
  });
});
