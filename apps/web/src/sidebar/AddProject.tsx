import type { ReactNode } from 'react';

import { addProjectNotice, isAddProjectBusy, type AddProjectState } from '../store/addProject';
import { useCommands } from '../store/hooks';
import { GLYPH } from '../ui/glyphs';

/**
 * **Add a project**: the `+` on the group header, and what happens after it.
 *
 * The dialog itself is the platform's own folder picker, opened by the Rust side — the
 * webview cannot reach a filesystem and must not (D-1/D-2), and a text field where somebody
 * types a path is a worse product that invites exactly the path-shaped mistakes the plan's
 * §5 warns about. So there is no dialog to render here. What there is instead is the
 * *answer*, and the answer is the part that had to be designed.
 *
 * Browse-a-folder only. Clone-from-URL and create-new-project are v0.4 (plan §2), and
 * Orca's *Host* selector is not coming at all while Nysia is local-only — a selector with
 * one option promises something that is not on the ladder.
 */

/** Which token paints an outcome. `tone` is a feeling; this is the only place it is a colour. */
const TONE: Readonly<Record<'added' | 'known' | 'choose' | 'refused', string>> = {
  // It worked, in the colour a healthy session already uses.
  added: 'border-status-running text-status-running',
  // Deliberately *not* a status colour. A folder that is already registered is a fact, and
  // the panel that reports it should read like the sidebar it is sitting in.
  known: 'border-line text-fg2',
  // A choice, not a fault. `many_repositories` is what happens the first time somebody
  // points Nysia at the folder all their work is in, and it is one click from done.
  choose: 'border-status-needs-input text-status-needs-input',
  refused: 'border-status-failed text-status-failed',
};

/**
 * The `+` that starts it.
 *
 * It was `disabled`, titled *"Adding a project arrives with the worktree manager in v0.4"* —
 * an honest statement of a plan that has since changed, because `Start →` creates a
 * branch-keyed worktree **in a project** and there is nothing to key against until one is
 * registered (v0.3 plan §1).
 *
 * It appears in three places — a group header, the empty state, and under a refusal — so
 * `label` and `children` are separate: the header shows a `+` that still has to be named for
 * a screen reader, and the other two show the name itself.
 */
export function AddProjectButton({
  state,
  className,
  label = 'Add a project',
  children,
}: {
  readonly state: AddProjectState;
  readonly className: string;
  readonly label?: string;
  readonly children?: ReactNode;
}) {
  const commands = useCommands();
  const busy = isAddProjectBusy(state);
  return (
    <button
      type="button"
      disabled={busy}
      aria-label={label}
      // The only thing a title should ever say is what the button does. It is *not*
      // disabled-with-an-excuse any more; while a picker is open it is disabled because a
      // second one would be a second registration racing the first.
      title={busy ? 'Waiting for the folder picker' : label}
      onClick={() => commands.addProject()}
      className={className}
    >
      {children ?? GLYPH.add}
    </button>
  );
}

/**
 * What the daemon said, or nothing at all.
 *
 * Five outcomes and four treatments, which is the requirement stated as a shape: the three
 * refusals each get their own heading, a registration that found the folder already there
 * gets a heading that does not read as a failure, and a fresh one is the only thing painted
 * as a success.
 *
 * The daemon's own `message` and `nextSteps` are rendered verbatim underneath. They are
 * where the detail lives — the names of the repositories it found, the `git init` that would
 * fix it — and neither is reachable from this side. Neither ever names the caller's path:
 * `RegisterRefusal` has no field to carry one and reduces the names it does carry to their
 * final component, because an error envelope is the thing most likely to end up in a log
 * (traps register #13).
 */
export function AddProjectResult({ state }: { readonly state: AddProjectState }) {
  const commands = useCommands();
  const notice = addProjectNotice(state);
  if (notice === null) {
    return null;
  }

  return (
    <div
      role="status"
      data-outcome={notice.tone}
      className={`bg-bg2 mx-2.5 mt-1 mb-2 flex flex-col gap-1.5 rounded-panel border p-2.5 text-xs ${TONE[notice.tone]}`}
    >
      <div className="flex items-start gap-2">
        <span className="min-w-0 flex-1 font-semibold wrap-anywhere">{notice.heading}</span>
        <button
          type="button"
          aria-label="Dismiss"
          onClick={() => commands.dismissAddProject()}
          className="text-fg3 hover:text-fg cursor-pointer border-0 bg-transparent p-0 leading-none focus-visible:shadow-focus focus-visible:outline-none"
        >
          {GLYPH.close}
        </button>
      </div>
      {state.phase === 'refused' ? (
        <>
          {/* `wrap-anywhere` for `CommandErrors`' reason: a folder name has no spaces to
              break on, and a box that only breaks between words runs its text past the
              border. */}
          <p className="text-fg2 leading-normal wrap-anywhere">{state.message}</p>
          <ul className="text-fg2 flex list-none flex-col gap-1 p-0 leading-normal">
            {state.nextSteps.map((step) => (
              <li key={step} className="wrap-anywhere">
                {step}
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {notice.canBrowseAgain ? (
        <AddProjectButton
          state={state}
          label="Choose another folder"
          className="text-fg hover:bg-bg3 mt-0.5 cursor-pointer self-start rounded-chip border-0 bg-transparent px-2 py-1 focus-visible:shadow-focus focus-visible:outline-none"
        >
          Choose another folder
        </AddProjectButton>
      ) : null}
    </div>
  );
}

/**
 * What the sidebar shows when it is holding no projects.
 *
 * Three things it can be, and they are not the same thing: the window has not finished
 * connecting, the daemon answered and has none, or the daemon would not answer. The third
 * is every daemon today — the verbs are served in v0.3 wave C1 — so it carries the daemon's
 * own sentence rather than a shrug. Ten invented project names is what used to be here, and
 * a name on screen that cannot be told from a real one is the one thing a person cannot
 * check.
 */
export function NoProjects({
  connecting,
  unavailable,
  state,
}: {
  readonly connecting: boolean;
  readonly unavailable: string | null;
  readonly state: AddProjectState;
}) {
  return (
    <div className="px-2.5 py-3 text-xs">
      <p className="text-fg2 leading-normal wrap-anywhere">
        {connecting
          ? 'Looking for the daemon…'
          : (unavailable ?? 'No projects yet. Point Nysia at a folder you already have.')}
      </p>
      {connecting ? null : (
        <AddProjectButton
          state={state}
          label="Add a project"
          className="text-fg hover:bg-bg2 border-line mt-2 cursor-pointer rounded-chip border bg-transparent px-2.5 py-1.5 focus-visible:shadow-focus focus-visible:outline-none"
        >
          Add a project
        </AddProjectButton>
      )}
    </div>
  );
}
