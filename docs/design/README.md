# Design sources

| File | What it is |
|---|---|
| `design-spec.md` | The written extraction — tokens, chrome, and the three screens. Start here. |
| `Nysia-ADE.dc.html` | The original design mock, pulled verbatim from Claude Design. |
| `2026-09-13-nysia-architecture.md` | Architecture and the locked founding decisions. |

## Provenance of `Nysia-ADE.dc.html`

Pulled 2026-09-13 from the Claude Design project **"Custom Agentic Development
Environment"** (`57f72c65-b055-4125-b08e-7ce6de7e2a2a`), file `Nysia ADE.dc.html`,
turn 2 "Ember, refined". Three artboards at 1440x900: Session, Tasks, Settings > Agents.

It **does not render standalone.** The document depends on the Claude Design runtime
(`support.js`, the `<x-dc>` / `<sc-for>` / `<sc-if>` custom elements and the trailing
`DCLogic` script), none of which are vendored here. Read it as a specification of exact
DOM structure, inline styles, glyphs, and copy — not as a page to open in a browser.

The bottom `<script type="text/x-dc">` block is the useful part for implementers: it holds
the literal theme tables for Ember and Graphite, the accent presets, the status palette,
and the seed data for every list in the mock.

To refresh it, re-pull the same project and path with the Claude Design integration.
