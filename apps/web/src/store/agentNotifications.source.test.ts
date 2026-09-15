import { describe, expect, it } from 'vitest';

/*
 * A tripwire on "the window reads `notify` and never re-derives it", read out of the source.
 *
 * `agentNotifications.test.ts` proves the behaviour on the two frames that tell the
 * implementations apart. This proves the *shape*, which is a weaker claim and a more
 * durable one: an implementation that consulted `sessionBoundary` in addition to the arm
 * would keep passing every behavioural case as long as the arm still vetoed first, and
 * would then be one refactor away from being the only thing consulted.
 *
 * The check is therefore mechanical and blunt. The two field names do not appear in that
 * file at all — not in code, not in a comment — so "does the notification path look at the
 * flags" is a grep with a yes/no answer rather than a reading of a hundred lines. The prose
 * there says session *boundary* and *restored from the spool* in English for exactly this
 * reason; if a future comment wants to name the fields, it can name them here instead.
 *
 * `TabStrip.source.test.ts` and `colourGuard.test.ts` already read raw source this way, so
 * the mechanism is not new. `import.meta.glob` rather than `node:fs`: `apps/web` is a
 * browser bundle and ESLint bans node builtins across the package, tests included.
 */
const source = String(
  Object.values(
    import.meta.glob('./agentNotifications.ts', {
      query: '?raw',
      import: 'default',
      eager: true,
    }),
  )[0],
);

describe('the notification sink', () => {
  it('is read from a file that exists and holds the sink', () => {
    // Without this the cases below pass vacuously against an empty string if the glob ever
    // stops resolving.
    expect(source.length).toBeGreaterThan(1000);
    expect(source).toContain('createAgentNotificationSink');
  });

  it('names the two suppression flags nowhere', () => {
    // The failure this forbids: `if (row.sessionBoundary) return;` — which is correct today,
    // passes every behavioural test, and is a second copy of a rule `nysia-proto` put on the
    // wire precisely so there would be one.
    expect(source, 'the sink is reading a row flag instead of the notify arm').not.toContain(
      'sessionBoundary',
    );
    expect(source, 'the sink is reading a row flag instead of the notify arm').not.toContain(
      'restoredUnconfirmed',
    );
  });

  it('branches on the notify arm, naming both cases', () => {
    // `Notify` is an enum rather than a boolean so that a caller has to say which arm it is
    // in. A truthiness check would compile and would lose that.
    expect(source).toContain('change.notify.decision');
    expect(source).toContain("case 'suppressed':");
    expect(source).toContain("case 'permitted':");
  });

  it('vetoes on the arm before it looks at the state', () => {
    // Order is the whole claim. A window narrowing that ran first would be re-deriving the
    // rule from the state, which is what the arm exists to stop.
    const arm = source.indexOf('change.notify.decision');
    const state = source.indexOf('NOTIFIABLE[row.state]');
    expect(arm).toBeGreaterThan(-1);
    expect(state).toBeGreaterThan(-1);
    expect(arm, 'the state is consulted before the notify arm').toBeLessThan(state);
  });
});
