# lint-meta fixtures

Two miniature repositories. `trips/` breaks both architecture rules and `clean/` satisfies
them while exercising every carve-out the rules allow.

`scripts/prove-lint-meta.ts` runs the rules against both and fails unless `trips/` reports
exactly the expected violations and `clean/` reports none. That is the proof that the rules
still trip (traps register #13) — without it, `pnpm lint` passing would mean nothing.

The repo-wide scan skips this directory, which is why the proof points the runner at it
explicitly.
