import type { ErrorCode } from '../generated/ErrorCode';

/**
 * Where **Add a project** has got to, and how each ending is put to the user.
 *
 * The plan is blunt about why this is a module and not four strings inside a component
 * (v0.3 §3.2): *"could not register" is not an answer a user can act on.* A path can turn
 * out to be four things and the daemon says which, with its own code and its own next
 * steps; throwing that away at the last step — after proto wrote it, after the wire carried
 * it, after Rust passed the code along — is how a user ends up with one generic failure.
 *
 * So the reading of an outcome lives here, apart from the rendering, and is asserted
 * without a DOM (D-18). What the component still owns is colour: every colour is a token
 * and this module names none.
 */

/**
 * The three refusals §3.2 requires a caller to tell apart.
 *
 * Derived from the generated [`ErrorCode`] rather than written out, so a code renamed in
 * `nysia-proto` fails the typecheck here instead of silently falling through to *refused*
 * with no heading of its own. `Extract` is what does that: the open `(string & {})` tail
 * does not extend a literal, so it drops out, and a literal that no longer exists in
 * `ErrorCode` leaves `never` behind — which the table below can no longer be assigned to.
 */
export type RegisterRefusalCode = Extract<
  ErrorCode,
  'not_a_repository' | 'many_repositories' | 'path_unreadable'
>;

/**
 * The codes this window has a distinct answer for, as a lookup rather than a `switch`.
 *
 * A `switch` over `ErrorCode` narrows nothing — `wireConstants.ts` records why at length:
 * the `(string & {})` tail survives every arm, so each case keeps the whole union and an
 * exhaustiveness check never clears. A table sidesteps the question: the key is whatever
 * arrived, the value is a literal, and a miss is "not one of ours".
 *
 * **A `Map`, not an object literal**, and that is not a preference. The key is a string off
 * the wire, and an object literal answers for every name on `Object.prototype` — so
 * `REFUSALS['toString']` is a function, and a daemon (or anything between) sending
 * `kind: "constructor"` would have been rendered as a refusal with a heading about a folder
 * nobody had a problem with. A `Map` holds only what was put in it.
 */
const REFUSALS = new Map<string, RegisterRefusalCode>([
  ['not_a_repository', 'not_a_repository'],
  ['many_repositories', 'many_repositories'],
  ['path_unreadable', 'path_unreadable'],
]);

/**
 * Whether a failed registration was one of §3.2's refusals, and which.
 *
 * `null` for everything else — a dropped socket, a daemon that does not serve the verb — and
 * that distinction is the one the provider branches on. A refusal is an *answer* and belongs
 * in the dialog; anything else is a command that failed and belongs in the notice list.
 */
export function registerRefusal(kind: string | null): RegisterRefusalCode | null {
  if (kind === null) {
    return null;
  }
  return REFUSALS.get(kind) ?? null;
}

/** Where **Add a project** has got to. */
export type AddProjectState =
  /** Nothing is happening, and nothing is on screen. */
  | { readonly phase: 'idle' }
  /**
   * The native folder picker is open.
   *
   * A modal the user leaves open is a state this window is in for as long as they take, so
   * the `+` has to know about it: two pickers at once is two registrations racing.
   */
  | { readonly phase: 'browsing' }
  /**
   * A folder was picked and the daemon is deciding what it is.
   *
   * Its own phase rather than part of `browsing`, because it is the one that can take a
   * while for a reason the user cannot see: the daemon canonicalises before anything else,
   * and that blocks for the OS's own timeout on a disconnected network share.
   */
  | { readonly phase: 'registering' }
  /**
   * It worked.
   *
   * `alreadyRegistered` is the daemon's answer to a different question than "did this
   * request succeed" — it says the *path* was already a project, possibly from another
   * client days ago. Registering something twice is not an error and must not be shown as
   * one.
   */
  | { readonly phase: 'added'; readonly name: string; readonly alreadyRegistered: boolean }
  /** The daemon looked and said what it found instead of a repository. */
  | {
      readonly phase: 'refused';
      readonly code: RegisterRefusalCode;
      /** The daemon's sentence, verbatim. It never names the path (trap 13). */
      readonly message: string;
      /** The daemon's next steps, verbatim. Never empty. */
      readonly nextSteps: readonly string[];
    };

/**
 * How an outcome should read, for the panel that renders it.
 *
 * `tone` is a name for the feeling, not a colour: the component maps it to a token, because
 * a hardcoded colour is a pixel that stops following the theme switcher. There are four,
 * and the one that matters is that `known` is not `refused` — a folder that is already in
 * the sidebar has to look like a fact, not a failure.
 */
export interface AddProjectNotice {
  /** The line in bold. Distinct for every outcome, which is the point of the module. */
  readonly heading: string;
  readonly tone: 'added' | 'known' | 'choose' | 'refused';
  /**
   * Whether the panel offers another go at the picker.
   *
   * True for every refusal, because in each case the fix is *a different folder* and the
   * user is already standing in the dialog. It is emphatically true for
   * `many_repositories`, where the daemon has just listed the folders to choose between:
   * that is the common shape of a `Projekty/` directory, and making someone reopen a menu
   * to act on advice they are currently reading is the difference between an error and a
   * step.
   */
  readonly canBrowseAgain: boolean;
}

/**
 * What to show for an outcome, or `null` while nothing has finished.
 *
 * The headings say the same thing the daemon's message says, in the four words that fit in
 * bold above it — they do not replace it. The daemon's own sentence and next steps are
 * rendered underneath, verbatim, because they are the part that names the five repositories
 * it found or the `git init` that would fix it.
 */
export function addProjectNotice(state: AddProjectState): AddProjectNotice | null {
  switch (state.phase) {
    case 'idle':
    case 'browsing':
    case 'registering':
      return null;
    case 'added':
      return state.alreadyRegistered
        ? {
            heading: `${state.name} is already in your sidebar`,
            tone: 'known',
            canBrowseAgain: false,
          }
        : { heading: `Added ${state.name}`, tone: 'added', canBrowseAgain: false };
    case 'refused':
      return REFUSAL_NOTICES[state.code];
  }
}

/**
 * One heading per refusal, which is the whole of "three different things".
 *
 * `many_repositories` is a *choice*, and is toned as one. It is not an edge case — it is
 * what happens the first time somebody points Nysia at the folder all their work is in —
 * and a user who meets it should read it as "which one?", not as "that failed".
 */
const REFUSAL_NOTICES: Readonly<Record<RegisterRefusalCode, AddProjectNotice>> = {
  not_a_repository: {
    heading: 'That folder is not a git repository',
    tone: 'refused',
    canBrowseAgain: true,
  },
  many_repositories: {
    heading: 'That folder holds several repositories',
    tone: 'choose',
    canBrowseAgain: true,
  },
  path_unreadable: {
    heading: 'That folder could not be read',
    tone: 'refused',
    canBrowseAgain: true,
  },
};

/** Whether the picker or the daemon is busy, so the `+` cannot start a second one. */
export function isAddProjectBusy(state: AddProjectState): boolean {
  return state.phase === 'browsing' || state.phase === 'registering';
}
