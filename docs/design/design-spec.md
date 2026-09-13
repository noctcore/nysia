# Nysia ADE — design spec (extracted from `Nysia ADE.dc.html`, turn 2 "Ember, refined")

Source: claude.ai/design project `57f72c65-b055-4125-b08e-7ce6de7e2a2a`.
Three artboards, all 1440×900: **2a Session**, **2b Tasks**, **2c Settings › Agents**.

## 1. Tokens

### Themes (two, user-switchable — Tweaks → Appearance)

| Token | Ember (default) | Graphite |
|---|---|---|
| `--bg0` | `#090b10` | `#0f0f11` |
| `--bg1` | `#0c0f15` | `#141416` |
| `--bg2` | `#11151d` | `#18181b` |
| `--bg3` | `#161b25` | `#1c1c20` |
| `--line` | `#1f2532` | `#26262a` |
| `--line2` | `#2a3140` | `#2e2e33` |
| `--fg` | `#dfe3ea` | `#e6e4e0` |
| `--fg2` | `#8b94a6` | `#a8a8b0` |
| `--fg3` | `#5c6577` | `#7c7c84` |

Ember = cool blue-black. Graphite = neutral warm-grey. `bg0` is chrome (titlebar, rail, status bar, cards); `bg1` is the app body; `bg2`/`bg3` are raised surfaces.

### Accent — user-changeable, single hue drives everything

Default `#f2b35b` (amber). Presets: `#6fd6c8` teal, `#b79cf2` violet, `#ef8fa2` pink.

Derived: `--acc14` = accent @ `24` alpha (active-nav bg, focus ring), `--acc35` = accent @ `59` alpha (pill borders).

### Status palette (agent lifecycle)

| State | Colour |
|---|---|
| Running | `oklch(78% 0.12 180)` teal |
| Needs input | `oklch(80% 0.15 70)` amber |
| Queued | `#7c7c84` grey |
| Failed | `oklch(70% 0.15 25)` red |

> Note: status colours are **independent of the accent** — deliberately. Accent is identity; status is semantics. Do not let the accent picker recolour these.

### Type

- UI: **Space Grotesk** 400/500/600
- Mono / terminal / metrics: **Fira Code** 400/500
- Also loaded (alternates): IBM Plex Sans, JetBrains Mono, Manrope, DM Mono
- Base size 13px; terminal 12.5px/1.65; chips + metrics 11.5px; section labels 10.5px uppercase `.06–.1em` tracking, `--fg3`

### Geometry

- Radii: 6px (rail items, chips) · 7–8px (buttons, inputs, cards) · 10–14px (popovers, panels) · 99px (pills)
- Borders: 1px `--line`; raised/interactive `--line2`
- Shadows: popover `0 20px 50px rgba(0,0,0,.6)`, flyout `0 24px 60px rgba(0,0,0,.6)`, artboard `0 20px 60px`
- Focus: `box-shadow: 0 0 0 3px var(--acc14)` (the prompt input uses this)

## 2. App chrome (all screens)

Row grid: **40px titlebar / 1fr body / 30px status bar**.

### Titlebar (40px, `--bg0`)
- Wordmark `nysia` + accent-coloured `.` — 600/15px, in a fixed 270px slot (aligns with rail+sidebar = 48+222)
- **Tab strip** — tabs are *terminals or agents*, bottom-aligned, 34px tall. Active tab: `--bg1` fill, 1px `--line`, `8px 8px 0 0`, bottom border matched to body so it merges. Inactive: no fill, `--fg2`.
  - Agent tab icon: `✱` in accent with `text-shadow: 0 0 8px var(--acc)` (glow) · Codex `◎`
  - Shell tab icon: `>_` in Fira Code
  - Close `×` in `--fg3`
- **`+` button** — 28×28, `--bg3`, radius 6. Opens a 250px menu grouped:
  - **AGENTS**: Claude (`✱`, hint "default"), Codex (`◎`), Gemini (`✦`), OpenCode (`▣`)
  - **TERMINALS**: PowerShell 7 (hint `pwsh`), Command Prompt (`cmd`), WSL · Ubuntu (`bash`), Git Bash
  - Group headers 10.5px uppercase `--fg3`; rows hover `--bg3`; hint right-aligned mono `--fg3`
- `⌘K` chip — 5px/10px, `--bg2`, mono
- Window controls `─ ☐ ✕`, 46px each — **custom chrome, not native**

### Status bar (30px, `--bg0`, top border)
- Left: `✱` + 60×4px accent progress bar + `100% left 5h · 97% left 6d · 99% left Fable` + `↻`
- Right: `● On` · `4.00 GB` · `>_ 9` · `⑂ 3` (daemon state, memory, terminal count, worktree count)
- Clicking the usage segment opens the **usage popover**.

### Body grid — `48px | 222px | 1fr`

**Icon rail (48px, `--bg0`)** — 32×32 items, radius 8, active = `--acc14` bg + `--acc` fg:
`▤` Session · `◉` Tasks · `◷` History — then pushed to bottom: `⚙` Settings · `?` Help

## 3. Screen 2a — Session

**Sidebar (222px)**: search "Search projects" (`⌕`, `--bg2`, radius 8) → group header `Dev` with trailing `+` → project rows with 16px hatched-circle avatars (`repeating-linear-gradient(135deg, var(--line) 0 2px, var(--bg3) 2px 4px)`).

The **active project expands** into a nested block with a 2px `--acc` left rail:
- `⑂ master` + `primary` pill (`--line` bg, 10px)
- session rows: 6px accent dot + truncated title + right-aligned age (`21h`)
- shell rows: `>_` + name + age (`3m`)

**Main pane** (padding `0 24px`), top→bottom:
1. **Command block** — `--bg0` card, 1px `--line`, radius 8, `$` prefix in `--fg`, output `--fg2`, `white-space: pre-wrap`, 12px mono
2. **Agent turn** — `✱ claude` header in accent (Space Grotesk 500), then prose at 12.5px/1.65; inline highlights in `--acc`; bold uses `#fff` at weight 500 (not 700)
3. **Footer meta** — `✱ Brewed for 23s · done 12:16 AM` in `--fg3`
4. **Prompt input** — `--bg0`, 1px `--line2`, radius 10, accent focus ring, `❯` prefix in accent, 7×16px solid accent block cursor
5. **Chips row** — pills at 11.5px mono, `--bg2`, key in `--fg3` + value in `--fg`:
   `Model Opus 5` · `Ctx 296.0k` · `⑂ master` · `+0 −0` · `Thinking xhigh` · `In 0.4 t/s` · `Out 155.0 t/s` · `Total 155.4 t/s` · `Mem 12.0G/24.0G` · `Weekly 2.0%` · `Block 3hr` · `Reset 1hr 59m`
   Below, full-width in accent: `▸▸ bypass permissions on (shift+tab to cycle)`
6. **Right-aligned meta**: `296 552 tokens` / `2.1.263 · ✓ up to date` / `new task? /clear to save 296.6k`

**Usage popover** — 360px, anchored bottom-left above the status bar, `--bg2`, radius 12:
- Header: **Usage** · right `all agents` · `↻`
- One row **per provider**: 30×30 icon tile (`--bg0`, accent glyph, 1px `--line`), name + reset (`Resets in 6d`), then inline mini-bars — Claude `5h 100% / wk 97% / Fable 99%`, Codex `5h 64% / wk 81%`. Bars 34×4px, `--line2` track, accent fill. Chevron `›`.
- Footer: `Usage details & history ›` · `Manage accounts… ›`
- **Hover a provider row → detail flyout** (300px): provider header + "Updated just now", then per-window 6px bars with `{pct} left` / `{reset}` in mono, then `CLAUDE ACCOUNT` section → `System default ›`, `Manage accounts… ›`

## 4. Screen 2b — Tasks (= GitHub Issues)

Sidebar swaps to:
- `Search issues`
- **SOURCES**: `◉ GitHub Issues` (count 38, active `--bg3`) · `⇄ Pull requests` (4) · `▦ Projects`
- **REPOSITORIES**: active repo bold with accent dot; others `--fg2`

Main pane:
- **Source chip row**: `◉ GitHub · Local · Shironex/vite-nestjs-template` (mono, `--bg2`) + repo dropdown `Settly ▾` + `↗` open-externally
- **Filter row**: segmented `Open | Assigned to me | Closed` (`--bg0` track, `--bg3` selected) · `≔ Filters` · monospace query field showing `is:issue is:open` with `×` clear · `+` · `↻`
- **Table** — columns `80px | 1fr | 90px | 110px | 120px` = ID · Title/context · Status · Updated · actions. Header 10.5px uppercase `--fg3`. Body in a `--bg0` card, radius 10, 1px `--line`, rows separated by `--line`, hover `--bg2`.
  - ID: `◉ #200` mono `--fg2`
  - Title: 500 weight, ellipsised; sub-line 11.5px `--fg2` = owner · repo · label pills (`--bg3`, 1px `--line`, radius 99)
  - Status: `● Open` pill — 1px `--acc35` border, `--acc` text
  - Updated: relative (`7 days ago`)
  - Actions: **`✱ Start →`** (`--bg3`, 1px `--line2`) + `⋮`

`Start` is the whole point of the screen: it hands the issue to an agent (→ creates worktree + session + tab).

## 5. Screen 2c — Settings › Agents

Settings is a **full-window mode**, not a modal: titlebar shows `← Back to app`. Layout `270px | 1fr`.

**Nav (270px, `--bg0`)** — search `Search settings` with `⌘F` hint, then groups:
- **AI capabilities** — Agents *(active)*, AI provider accounts `optional`, Orchestration, Voice
- **Set up** — Nysia account, General, Appearance, Integrations
- **Workflows** — Automations, Git & source control, Task sources, Terminal, Quick commands
- **Projects** — Settly, shiranami, nightcore, shiroani, deskmate (per-project settings)

**Content** (max-width 980px, padding `22px 40px`):
- Header: `Agents` h1 22px/600 + "Manage AI agents, set a default, and customise commands.", bottom border
- **Card** (`--bg0`, radius 14, 1px `--line`):
  - *Default agent* + note that these are **client preferences** (SSH/remote still validated at run time)
  - Choice chips: `Auto` · `>_ No agent (blank terminal)` · `✱ Claude ✓` *(selected: 1px `--acc`, `--acc14` bg)* · `✱ Claude Agent Teams` · `◎ Codex` · `✕ Grok` · `▣ OpenCode` · `✦ Gemini` · `◇ Cursor`
  - Setting rows (label 14/600 + desc 12.5 `--fg2`, control right-aligned):
    | Row | Control |
    |---|---|
    | Agent status hooks — "Shows working, waiting and done states in Nysia. Turn off to remove managed hooks." | toggle **on** |
    | Auto-generate tab titles — "Derive short stable tab names from the first agent prompt. Manual renames always win." | toggle **on** |
    | Keep computer awake — "Agent mode stays awake only while agents are working." | segmented `On \| Agent \| Off` |
    | Prompt cache timer — "Countdown in the sidebar after a Claude agent becomes idle, so you know when the cache expires." | toggle off |
    | Agent permissions — "Launch agents with fewer permission prompts or with manual checks." | segmented `Yolo \| Manual` |
  - Toggle: 38×22 pill, radius 11, on = `--acc`, knob 16px white
- **Installed** — heading + `4 detected` pill + `↻ Refresh`; rows: icon tile, name, version in mono `--fg3`, `Enabled | Disabled` segmented, `Set default` / `✓ Default` button, `↗`, `⌄`
  - Detected: Claude `2.1.263`, Codex `0.48.0`, Gemini `0.9.2`, OpenCode `1.2.7`

## 6. What the design commits us to (product reads)

1. **A tab is a session, and a session is either an agent or a shell.** One uniform surface — no separate "terminal panel" vs "chat panel". Multi-provider from day one (Claude, Codex, Gemini, OpenCode, Grok, Cursor + plain shells incl. WSL/pwsh/cmd/Git Bash → **Windows is a first-class target**).
2. **Projects are the spine, sessions nest under them**, grouped by branch/worktree. Sidebar shows live status + age per session.
3. **Tasks are GitHub Issues, not a kanban.** No custom task model to maintain — the list is a queryable view over `is:issue is:open`, and `Start →` is the only verb that matters.
4. **Usage is a first-class, always-visible, multi-provider surface** — status bar summary → popover per provider → hover flyout per window. Needs per-provider quota windows (Claude: session/weekly/Fable; Codex: 5h/weekly) and multi-account support ("Manage accounts…").
5. **Settings anticipates: Orchestration, Voice, Automations, Task sources, Quick commands, per-project overrides.** These are nav entries in the design — i.e. planned surface, not shipped surface.
6. **"Agent status hooks" is an explicit, disableable managed integration** — Nysia writes hooks into the agent's config to learn working/waiting/done, and removing the setting removes the hooks. This is the status-detection mechanism.
7. **Custom window chrome** on all platforms.
8. **Theme + accent are live user tweaks**, so everything must be token-driven — no hardcoded colour anywhere.
