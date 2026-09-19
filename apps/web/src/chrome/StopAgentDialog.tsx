import { CONFIRM_STOP_AGENT_LABEL } from '../settings/generalPreferences';
import { GLYPH } from '../ui/glyphs';
import { SessionGlyph } from './SessionGlyph';
import { StatusDot } from './StatusDot';
import { STOP_TITLE, type PendingStop, type StopGate } from './stopAgent';

/** The heading's id, for `aria-labelledby`. There is only ever one of these open. */
const TITLE_ID = 'stop-agent-title';
const REASON_ID = 'stop-agent-reason';

/**
 * "Stop this agent?" — asked before a tab close that would cut an agent off mid-turn.
 *
 * What it keeps from the dialog it is measured against is the reasoning, not the layout:
 *
 *  - it names **which** session — the tab's own glyph, title and status dot, since Claude is
 *    the only agent (D-3) and the product name would tell two tabs apart not at all;
 *  - it says what is lost in one sentence, which comes from `workAtStake` and so is only
 *    ever the sentence that is true for the state the pane was in;
 *  - the destructive button is the right-hand one and is painted `status-failed`, which the
 *    accent picker cannot recolour (`theme/tokens.ts` never emits a status token), so it
 *    reads as destructive whatever accent the user chose;
 *  - the escape hatch is a preference, not a disappearing affordance: the checkbox says
 *    which Settings row turns the question back on, and that row renders the same label.
 *
 * A native `<dialog>` opened with `showModal()`, because modal is the point: the rest of the
 * window goes inert, so there is no second close to issue behind it and no focus to lose
 * into the strip. Escape arrives as the dialog's `cancel` event, and a press on the
 * backdrop arrives on the dialog element itself — the padding is on the inner box so that
 * only the backdrop can be that target.
 *
 * **No hooks, deliberately.** Everything it needs is in props and every button goes straight
 * to the gate, so a node-only suite (D-18) can call this as a function, press a button in the
 * element tree it returns, and look at the store. The one piece of DOM work — opening the
 * dialog and focusing Cancel — is a callback ref, which is not a hook and never runs under
 * `renderToStaticMarkup`.
 *
 * Cancel takes the initial focus: Enter on an unread dialog should do the harmless thing.
 */
export function StopAgentDialog({
  pending,
  gate,
}: {
  readonly pending: PendingStop;
  readonly gate: Pick<StopGate, 'cancel' | 'confirm' | 'setDontAskAgain'>;
}) {
  return (
    <dialog
      ref={openModal}
      aria-labelledby={TITLE_ID}
      aria-describedby={REASON_ID}
      onCancel={(event) => {
        // The gate owns whether this is open, not the browser. Letting Escape close the
        // element itself would leave a closed dialog mounted and the close still pending.
        event.preventDefault();
        gate.cancel();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) {
          gate.cancel();
        }
      }}
      className="bg-bg2 text-fg border-line2 m-auto w-[380px] max-w-[calc(100vw-2rem)] rounded-panel border p-0 shadow-popover backdrop:bg-shade"
    >
      <div className="flex flex-col gap-3 p-5">
        <h2 id={TITLE_ID} className="text-row font-semibold">
          {STOP_TITLE}
        </h2>
        <p id={REASON_ID} className="text-fg2 leading-normal">
          {pending.reason}
        </p>
        <div className="bg-bg0 border-line flex items-center gap-2 rounded-control border px-3 py-2">
          <SessionGlyph kind={pending.tab.kind} />
          <span className="min-w-0 flex-1 truncate">{pending.tab.title}</span>
          <StatusDot status={pending.status} now={pending.askedAt} />
        </div>
        <label className="text-fg2 flex cursor-pointer items-start gap-2">
          <input
            type="checkbox"
            checked={pending.dontAskAgain}
            onChange={(event) => gate.setDontAskAgain(event.currentTarget.checked)}
            className="accent-acc mt-0.5 cursor-pointer"
          />
          <span>
            Don&apos;t ask again
            <span className="text-fg3 text-chip block">
              {`Turn it back on in Settings ${GLYPH.chevron} General ${GLYPH.chevron} ${CONFIRM_STOP_AGENT_LABEL}.`}
            </span>
          </span>
        </label>
        <div className="mt-1 flex justify-end gap-2">
          <button
            type="button"
            data-stop-cancel
            onClick={() => gate.cancel()}
            className="bg-bg3 text-fg border-line2 cursor-pointer rounded-control border px-3 py-1.5 focus-visible:shadow-focus focus-visible:outline-none"
          >
            Cancel
          </button>
          {/* `text-bg0` on the red rather than `text-fg`: both themes are dark, so bg0 is
              near-black, and a light label on a mid-lightness red is the pairing that
              fails contrast. */}
          <button
            type="button"
            onClick={() => gate.confirm()}
            className="bg-status-failed text-bg0 border-status-failed cursor-pointer rounded-control border px-3 py-1.5 font-semibold focus-visible:shadow-focus focus-visible:outline-none"
          >
            Stop agent
          </button>
        </div>
      </div>
    </dialog>
  );
}

/**
 * Open the dialog modally once it is in the document, and close it on the way out.
 *
 * Closing before removal rather than just unmounting is what hands focus back: `close()` is
 * specified to return it to whatever held it before `showModal()`, whereas removing the
 * focused element from the document leaves it on `<body>` — which, for a close issued with
 * the pointer while the caret sat in the prompt, would lose the caret on Cancel.
 */
function openModal(node: HTMLDialogElement): () => void {
  if (!node.open) {
    node.showModal();
  }
  node.querySelector<HTMLElement>('[data-stop-cancel]')?.focus();
  return () => {
    if (node.open) {
      node.close();
    }
  };
}
