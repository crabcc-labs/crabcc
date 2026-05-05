# `crabcc-desktop` — Stitch generation prompts

Prompts for the Stitch MCP (`mcp__stitch__generate_screen_from_text`)
to produce ref-design mockups of each greenfield surface in
[`./DESIGN-BRIEF.md`](./DESIGN-BRIEF.md). Initial generation runs on
2026-05-03 timed out at the MCP layer with no screens persisted.
Re-run when the API stabilises.

- Project: `projects/3825002640704158815` (`crabcc-desktop greenfield`)
- Design system: `assets/9253917256800618914` (`crabcc-desktop dark`)
- Device type: `DESKTOP`

For each generated screen, paste its URL back into Appendix B of
`DESIGN-BRIEF.md`.

---

## 1. Tool-call timeline / inspector

> Screen: "Tool-call timeline / inspector" — a new route in the
> crabcc-desktop GPU-rendered Rust app. 1440x900 desktop window.
> Two-column layout. LEFT 70%: vertical timeline of tool calls
> grouped by agent. Each agent is a collapsible group header
> (profile name, mono id, runtime, soft pulsing green dot for
> running). Each row has a 16px tool-family icon (sym=#7C5CFF,
> refs=#3FB6FF, callers=#F5A524, fuzzy/prefix/random-query=#22C55E,
> memory.ingest=#7C5CFF), mono timestamp `12:04:31.218`, bold tool
> name, args inline truncated, right-aligned result count or red
> error pill. Pinned calls render in a "Pinned" sub-section above.
> Newest row gets 200ms slide-in + 600ms alpha highlight; older
> rows alpha-fade between 4s and 60s. RIGHT 30%: inspector pane
> showing arguments JSON pretty-printed, result table or code,
> "Copy" + "Pin/Unpin" + "Open in Logs" + "Re-run" actions.
> Empty inspector state: muted "Select a call to inspect." Header
> above timeline: filter input ("filter by tool, agent, query…"),
> tool-family pills, counter "138 calls · 9 pinned". Density 28px
> rows, 8/12/16 spacing, 4px radius, 1px hairlines #2A2A33,
> bg #0E0E12, cards #1E1E24, text #E6E6EB / muted #8A8A95.
> Headline Geist, body Inter, mono JetBrains Mono.

## 2. Agent-spawn sheet (3 states)

> Screen: "Agent-spawn sheet" sliding over the existing Home route.
> 1440x900 desktop. Show 3 horizontally-stacked variants of the SAME
> sheet (460px wide, 24px gutters, captions: Idle / Launching /
> Streaming). Home route faintly visible behind at 40% opacity (KPI
> strip silhouette). Sheet card: bg #1E1E24, 1px #2A2A33 border, 4px
> radius, 16px padding.
>
> IDLE — title "Launch agent" + close ×. Two columns. LEFT 40%
> "Profile" list of agent profiles with 32px round avatar (initial
> in chart_1..5 colour), mono id, one-line muted description,
> "tools: 6 · model: opus-4.6" footer. Selected row: 2px #7C5CFF
> left accent + lifted bg. RIGHT 60% "Prompt" 8-row textarea, mono
> "model: claude-opus-4-7 · profile: rust-logging-audit" caption,
> bottom-right "Cancel" / primary "Launch".
>
> LAUNCHING — body greyed slightly, "Launch" replaced with 24px
> spinner + "Launching…", thin pulsing #7C5CFF progress bar under
> the prompt, profile column locked.
>
> STREAMING — sheet morphed taller. Title "rust-logging-audit ·
> agt_8f3a912 · +4s · ●" with soft-pulse green dot. Profile column
> gone; textarea collapsed to read-only block + "show full" link.
> New "Output" section: terminal-styled, JetBrains Mono 12px, bg
> #0E0E12, streaming colored lines (muted timestamps, purple tool
> calls), tail cursor blinking, auto-scroll. Bottom action bar:
> "Open in Agents route" link, "Kill" danger, "Detach (keep
> running)" primary.
>
> Header: small label "agent-spawn sheet · greenfield (#213 A.9)".

## 3. Commands launchpad — click-to-run + inline result

> Screen: "Commands launchpad" — the existing /Commands route in
> crabcc-desktop, evolved. 1440x900 desktop. Top nav with "Commands"
> highlighted. Sticky header: full-width search input with magnifier
> ("search commands… (try: index, refs, memory, fetch)"), keyboard
> hint right "↑↓ navigate · ⏎ run · ⌘K focus", counter line "32
> commands · 6 categories · 2 running". Body: vertical list of
> category sections with muted small-caps headers ("INDEX & QUERY",
> "MEMORY", "AGENTS", "FETCH", "GRAPH", "MCP / SERVER"). Rows are
> 28px tall: 16px family-color icon, mono command name (bold),
> one-line muted description, kbd hint `⏎` on focused row.
>
> Demonstrate 4 row STATES inline:
> 1. Idle (most rows).
> 2. Running (`crabcc.fuzzy`): row bg #1E1E24, green pulsing dot +
>    "running…" right side, "Esc to cancel" kbd hint, 4px-tall
>    indeterminate progress bar under the row.
> 3. Done with result (`crabcc.refs`): expanded inline result block
>    indented 32px, header "+1.2s · 23 results" green + "Copy" /
>    "Pin" / "Collapse" links, body 5-row mini-table [file, line,
>    snippet] in JetBrains Mono 12px, footer "show all 23 →".
> 4. Error (`crabcc.fetch`): red "ERR · request failed" pill, body
>    red-bordered code block with stderr + "Retry" button.
>
> Density 8/12/16 spacing, 4px radius, hairlines, Activity-Monitor
> feel.

## 4. Relations graph node-detail drawer

> Screen: "Relations graph node-detail drawer" for the existing
> /Graph route in crabcc-desktop. 1440x900 desktop. Top nav with
> "Graph" highlighted.
>
> LEFT 70%: relations graph canvas, ~96 nodes / ~276 edges,
> force-directed. Nodes 5-7px circles muted purple, edges 1px
> #2A2A33. ONE node selected (slightly larger, 2px #7C5CFF ring),
> 8 immediate neighbours highlighted #3FB6FF (with their connecting
> edges in #3FB6FF at 70% alpha). Non-neighbour nodes/edges dimmed
> alpha 0.35. Header bar above canvas: `nodes 96 · edges 276 ·
> zoom 1.6× · selected: Store::open`, pan/zoom controls (⊕ ⊖ ⌖
> reset), shortcut hint "wheel: zoom · drag: pan · click: select ·
> ⎋: clear".
>
> RIGHT 30%: node-detail drawer with shadow on left edge (slide-in
> implied). Header: `fn` kind badge (purple for fn, blue struct,
> amber enum, green trait, muted const), mono name `Store::open`,
> file:line muted mono `crates/crabcc-core/src/store.rs:128`,
> top-right × close + "open in editor" link icon. Sections:
> "Signature" — code block JetBrains Mono 12px, light syntax
> highlight; "Edges" — two stacked lists "Incoming (5)" + "Outgoing
> (3)", each row: kind badge + mono name + file:line muted +
> hover "→ go"; "Actions" — "Open in editor" (primary) / "Show
> callers" (secondary) / "Copy file:line" (secondary); "Snippet"
> collapsed by default, expands to ±5 lines around line 128 with
> gutter line numbers + highlighted line in chart_1 alpha 15%.
>
> Inset preview at top: drawer-closed state showing only canvas +
> muted hint "click a node to inspect" bottom-left of canvas.

## 5. Knowledge graph canvas

> Screen: "Knowledge graph canvas" — a NEW route distinct from the
> Relations graph. 1440x900 desktop. Top nav with "K-Graph"
> highlighted. Header: title "Knowledge graph", counter "drawers
> 412 · cross-refs 1,108", multi-select wing pills (`agents`
> #7C5CFF · `feedback` #3FB6FF · `project` #22C55E · `reference`
> #F5A524 · `user` #EF4444), pan/zoom buttons + "force layout"
> toggle.
>
> Canvas distinct from relations graph: nodes are 12×6 rounded-rect
> pills (color = wing), labels inside at high zoom; edges are
> dashed thin muted #8A8A95 alpha 0.6; hub drawers bigger with
> always-visible labels; faint colored "wing" cluster blobs around
> same-wing groups; ONE selected node ringed in #E6E6EB
> (foreground, NOT relations purple — to differentiate the two
> graphs).
>
> Right rail 340px "Drawer detail": header (wing chip + room mono
> `feedback / testing` + relative ts "· 4 days ago"), title H1 from
> drawer body ("Mock the database in tests"), 8-line markdown body
> preview JetBrains Mono 12px, "References" list (3 outgoing) +
> "Referenced by" list (5 incoming) with wing colour chips, action
> row "Open full" primary / "Edit" secondary / "Forget" danger.
> Bottom strip: "last refresh 6s ago · refresh every 10s".
>
> Slightly more breathing room than relations graph; Roam-like map
> in Activity Monitor density palette.

## 6. In-window notifications strip

> Screen: "In-window notifications strip" — top-of-window strip
> layered above any active route. Show with Home faintly visible
> behind. Strip lives below native title bar + below in-app top
> nav. Full window width, bg #1E1E24, 1px bottom border #2A2A33.
>
> Stack of 5 toasts (newest top), 56px tall each:
> - left: 18px event icon in level color (success #22C55E, info
>   #3FB6FF, warning #F5A524, danger #EF4444, primary #7C5CFF).
> - center: bold one-line title + smaller muted subline.
> - right: relative ts mono · × dismiss · 1-2 inline action text-
>   buttons.
>
> Five toasts demonstrating states:
> 1. SUCCESS — "Agent rust-logging-audit finished — 23 hits" — green
>    + "Open log" + "Copy ID".
> 2. INFO — "Index refreshed — 224 files / 2,844 symbols" — blue.
> 3. WARNING — "OTLP collector slow (last probe 2.4s)" — amber +
>    "Open System".
> 4. DANGER — "Agent agt_771ad1 failed — model timeout" — red +
>    "Retry" + "Open log".
> 5. PRIMARY — "New memory drawer ingested · feedback / testing" —
>    purple + "Open in Knowledge".
>
> Above stack: meta strip with bell icon + "Notifications · 2
> unread" + right "mute (3 stop) · Settings". Below stack: "Show
> last 50 →" link to full log; auto-collapse items >30s old into
> "+ 7 more" pill.
>
> Inset captions illustrate motion (no actual animation): new toast
> 200ms slide + 600ms alpha highlight, self-dismissing toasts pulse
> a thin underline progress bar (success 5s, info 3s, warning
> persists, danger persists), "↗ system" tag on toasts that are
> mirrored to the OS notification centre (Track C of #213).
