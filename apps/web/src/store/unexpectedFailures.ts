import type { StoreCommandName, StoreError } from './errors';

/**
 * Where a rejection that is *not* a `StoreCommandError` goes.
 *
 * `errors.ts` promises that a dropped connection mid-request reaches the user, and the
 * contract can hold a provider to that only for the failures it wrapped. A provider that
 * lets a raw `TypeError`, or an unwrapped transport error, out of a command has broken the
 * promise — and until this existed the only trace was a `console.error` nobody has open.
 *
 * It is a second, tiny external store rather than a new field on `StoreSnapshot`, because
 * the snapshot belongs to the provider and this is a statement *about* the provider. W5
 * cannot be asked to record its own unwrapped failures: if it could reliably do that, they
 * would have been wrapped.
 *
 * The default instance is module-level, which is what a sink for "something got out" has
 * to be — every routed command in the window feeds one list and `CommandErrors` renders
 * it. The factory exists so a test gets its own.
 */
export interface FailureSink {
  getSnapshot(): readonly StoreError[];
  subscribe(listener: () => void): () => void;
  report(command: StoreCommandName, cause: unknown): void;
  dismiss(id: string): void;
  /** Test-only: drop everything, so one case cannot leak into the next. */
  clear(): void;
}

export function createFailureSink(): FailureSink {
  let failures: readonly StoreError[] = [];
  let next = 1;
  const listeners = new Set<() => void>();

  function emit(updated: readonly StoreError[]): void {
    failures = updated;
    for (const listener of [...listeners]) {
      listener();
    }
  }

  return {
    // Referentially stable between changes, like the store's own snapshot, because this is
    // read through `useSyncExternalStore` too.
    getSnapshot: () => failures,

    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },

    report(command, cause) {
      // The console line stays: this is a bug in a provider, and a developer with the
      // devtools open should get the stack, not just the sentence the user gets.
      console.error(`Store command ${command} failed unexpectedly`, cause);
      const id = `unexpected_${next}`;
      next += 1;
      emit([...failures, { id, command, message: describeCause(cause), at: Date.now() }]);
    },

    dismiss(id) {
      const remaining = failures.filter((failure) => failure.id !== id);
      if (remaining.length !== failures.length) {
        emit(remaining);
      }
    },

    clear() {
      if (failures.length > 0) {
        emit([]);
      }
    },
  };
}

export const unexpectedFailures = createFailureSink();

/** Convenience for the routing layer, so it does not name the instance at every call. */
export function reportUnexpectedFailure(command: StoreCommandName, cause: unknown): void {
  unexpectedFailures.report(command, cause);
}

/**
 * A sentence for the notice.
 *
 * It says the connection may have dropped, deliberately: that is the likely cause of an
 * unwrapped rejection reaching here and it is the part the user can act on. The underlying
 * message is appended when there is one, because a daemon error usually reads better than
 * anything invented in its place.
 */
export function describeCause(cause: unknown): string {
  const detail =
    cause instanceof Error ? cause.message : typeof cause === 'string' ? cause : '';
  const prefix = 'Nysia lost track of that request; the daemon connection may have dropped.';
  return detail === '' ? prefix : `${prefix} ${detail}`;
}
