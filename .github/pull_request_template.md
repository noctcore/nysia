## What changed

<!-- The change itself, in a few lines. Not a file list — the diff already is one. -->

## Why

<!-- What this is for: the defect it fixes, the decision it implements, the gap it
     closes. Link the design doc or the issue if there is one. -->

## How it was verified

<!-- The gates in CLAUDE.md section 7 all pass before a pull request opens, so saying
     so adds nothing. What is worth writing down is what you did beyond them: the case
     you reproduced first, the fix you reverted to watch a new proof go red, the thing
     you checked by hand because no gate covers it. -->

---

<!-- Before opening, per CLAUDE.md section 1:

     - Labels and an assignee, passed to `gh pr create` at creation:
       `--assignee Shironex --label "<type>,<priority>,<area>"`.
       Exactly one type, exactly one priority, at least one area.
     - Areas and the extras (`dependencies`, `gate`, `design-system`) are also applied
       automatically from the paths you touched, and an unassigned pull request is
       assigned automatically. Both are backstops for a pull request opened from the
       web UI — they are not a substitute for passing them at creation, and nothing
       infers a type or a priority for you. pr-facets fails the pull request without
       them.
     - No attribution trailers anywhere: not in a commit message, not in the title,
       not in this body.

     Delete these comments or leave them; they do not render either way. -->
