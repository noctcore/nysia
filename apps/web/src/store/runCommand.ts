import { StoreCommandError } from './errors';

/**
 * The only way a component issues a store command.
 *
 * `void store.openTab(id)` was the previous shape, and it had one failure mode that the
 * design of the store now makes routine: the daemon refuses to launch because the shell
 * binary is not on PATH, the promise rejects, the rejection is unhandled, and the menu
 * closes as though it had worked. Nothing appears on screen and nothing appears in a log.
 *
 * A `StoreCommandError` is an expected outcome that the provider has *already* written
 * into `snapshot.errors`, so there is a notice on screen and there is nothing left to do
 * here. Anything else coming out of a command is a bug in the provider, and is reported
 * rather than swallowed — the whole point of this helper is that failures stop being
 * silent, so it must not become a new way to be silent.
 *
 * `onSettled` runs after either outcome, for the caller that has to move DOM focus once
 * the store has converged.
 */
export function runCommand(command: Promise<void>, onSettled?: () => void): void {
  command
    .catch((cause: unknown) => {
      if (cause instanceof StoreCommandError) {
        return;
      }
      console.error('Store command threw something other than a StoreCommandError', cause);
    })
    .finally(() => onSettled?.());
}
