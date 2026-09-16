import { describe, expect, it } from 'vitest';

import {
  addProjectNotice,
  isAddProjectBusy,
  registerRefusal,
  type AddProjectState,
  type RegisterRefusalCode,
} from './addProject';

/*
 * v0.3 plan §3.2, as an executable claim: *"could not register" is not an answer a user can
 * act on.*
 *
 * A path can turn out to be four things and the daemon says which, with its own code, its
 * own sentence and its own next steps. Everything from proto's `RegisterRefusal` to Rust's
 * `CommandFailure.kind` exists to carry that distinction to the window, and this is the last
 * place it can be thrown away — so what is asserted here is that five outcomes reach the
 * user as five different notices, and that the one which is not a failure does not look like
 * one.
 *
 * Node-only (D-18). None of this needs a DOM, which is the reason the reading lives apart
 * from the rendering.
 */

const REFUSAL_CODES: readonly RegisterRefusalCode[] = [
  'not_a_repository',
  'many_repositories',
  'path_unreadable',
];

function refused(code: RegisterRefusalCode): AddProjectState {
  return { phase: 'refused', code, message: `the daemon's sentence for ${code}`, nextSteps: ['x'] };
}

const OUTCOMES: readonly AddProjectState[] = [
  { phase: 'added', name: 'nysia', alreadyRegistered: false },
  { phase: 'added', name: 'nysia', alreadyRegistered: true },
  ...REFUSAL_CODES.map(refused),
];

describe('what a registration says to the user', () => {
  it('gives each of the five outcomes its own heading', () => {
    // The requirement in one line. Two outcomes sharing a heading is two different things
    // the user cannot tell apart, whatever colour they are painted.
    const headings = OUTCOMES.map((state) => addProjectNotice(state)?.heading);
    expect(headings.every((heading) => heading !== undefined)).toBe(true);
    expect(new Set(headings).size).toBe(OUTCOMES.length);
  });

  it('does not make an already-registered folder look like a failure', () => {
    // §3.2: registering a path twice is one project, and the answer says which happened.
    // Showing that as an error teaches a user that Nysia breaks when they repeat themselves.
    const known = addProjectNotice({
      phase: 'added',
      name: 'nysia',
      alreadyRegistered: true,
    });
    expect(known?.tone).toBe('known');
    expect(known?.tone).not.toBe('refused');
    // Nor does it offer to browse again: nothing went wrong and there is nothing to redo.
    expect(known?.canBrowseAgain).toBe(false);
  });

  it('treats a folder of repositories as a choice rather than a fault', () => {
    // The common shape of a `Projekty/` directory, not an edge case. The daemon has just
    // listed the folders to choose between, so the panel offers the picker rather than
    // making someone find the menu again.
    const many = addProjectNotice(refused('many_repositories'));
    expect(many?.tone).toBe('choose');
    expect(many?.canBrowseAgain).toBe(true);

    const notRepo = addProjectNotice(refused('not_a_repository'));
    expect(notRepo?.tone).toBe('refused');
    expect(notRepo?.tone).not.toBe(many?.tone);
  });

  it('offers another folder after every refusal, because that is the fix', () => {
    for (const code of REFUSAL_CODES) {
      expect(addProjectNotice(refused(code))?.canBrowseAgain, code).toBe(true);
    }
  });

  it('shows nothing at all while nothing has finished', () => {
    // A panel that appeared the moment the `+` was pressed would sit under an open modal
    // saying nothing, and the user would meet it again once they had picked.
    for (const phase of ['idle', 'browsing', 'registering'] as const) {
      expect(addProjectNotice({ phase }), phase).toBeNull();
    }
  });
});

describe('which failures are refusals', () => {
  it('recognises the three §3.2 codes and nothing else', () => {
    for (const code of REFUSAL_CODES) {
      expect(registerRefusal(code)).toBe(code);
    }
    // Everything here is a failed command rather than an answer about a folder. `unsupported`
    // is the one that matters today — it is what every daemon says until wave C1 — and
    // routing it into the dialog would put "that folder is not a repository" in front of
    // someone whose folder is fine.
    for (const other of ['unsupported', 'invalid_request', 'internal', 'disconnected', 'other']) {
      expect(registerRefusal(other), other).toBeNull();
    }
    expect(registerRefusal(null)).toBeNull();
  });

  it('does not treat a lookalike as one of them', () => {
    // The table is keyed on the exact code, so a prefix or a near miss from a newer daemon
    // falls through to "a command failed" rather than being rendered as a refusal whose
    // heading would then be wrong about what happened.
    expect(registerRefusal('not_a_repository_maybe')).toBeNull();
    expect(registerRefusal('NOT_A_REPOSITORY')).toBeNull();
    expect(registerRefusal('')).toBeNull();
  });

  it('is not fooled by a name every object has', () => {
    // A plain object literal inherits `toString`, `constructor` and the rest, so a lookup
    // that answered from the prototype chain would report `constructor` as a refusal — and
    // the daemon's `kind` is a string from the wire, which is exactly where that class of
    // input comes from.
    for (const inherited of ['toString', 'constructor', '__proto__', 'hasOwnProperty']) {
      expect(registerRefusal(inherited), inherited).toBeNull();
    }
  });
});

describe('whether the + is busy', () => {
  it('is busy only while the picker or the daemon has it', () => {
    // Two pickers at once is two registrations racing, and the second lands on a snapshot
    // the first has already replaced.
    expect(isAddProjectBusy({ phase: 'browsing' })).toBe(true);
    expect(isAddProjectBusy({ phase: 'registering' })).toBe(true);
    expect(isAddProjectBusy({ phase: 'idle' })).toBe(false);
    // Not while an outcome is on screen: the panel is something to read, not something to
    // wait for, and a `+` held shut until it is dismissed would be a dialog with no way out.
    expect(isAddProjectBusy({ phase: 'added', name: 'n', alreadyRegistered: false })).toBe(false);
    expect(isAddProjectBusy(refused('many_repositories'))).toBe(false);
  });
});
