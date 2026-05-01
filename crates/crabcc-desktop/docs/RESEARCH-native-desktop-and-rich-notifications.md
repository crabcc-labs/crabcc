# Native Desktop UI + Rich macOS Notifications — Research & Plan

> Status: **kickoff (phase 0)** — this document scopes the multi-track
> initiative. No production code lands in the kickoff PR; only the
> placeholder crate scaffolding + this research doc + a stub for the
> Apple Notification Content Extension.
>
> Tracking issue: <to-be-filed>
> Related PR: <this PR>

## Vision

Today crabcc ships a Rust workspace + a **web** dashboard rendered by a
React SPA bundled via esbuild and served from `crabcc serve` on
`127.0.0.1:7878`. The dashboard is dense and useful but constrained by
the browser sandbox: no GPU-accelerated charts, no native menu bar
integration, no rich macOS notifications, no native window chrome.

The Longbridge desktop trading app
([screenshot reference][lb-screenshot]) demonstrates what a Rust-native,
GPU-rendered crabcc surface could look like — multi-pane layouts,
sub-second render budgets, native scrolling, system menus, dock badges.
We have all the pieces in-tree (telemetry, agents, memory, graph) to
hydrate that surface; we are bottlenecked only on the rendering layer
and on the macOS notification depth.

This initiative has four tracks:

1. **Track A — `crabcc-desktop`**: a new crate that uses
   [longbridge/gpui-component][gpui-component] to host the dashboard
   natively. Reuses the existing HTTP API / SSE channel.
2. **Track B — shadcn-flavoured web refresh**: keep the React dashboard
   alive (the desktop app and the web dashboard are first-class peers),
   but pivot the design system toward [shadcn/ui][shadcn] primitives so
   the visual language is consistent across surfaces.
3. **Track C — Native macOS rich notifications** via [`objc2`][objc2]
   bindings + a Swift `UNNotificationContentExtension`. Replaces the
   current `task notify` shell-out with a real `UserNotifications`
   pipeline that supports actions, inline reply, attachments, and
   custom-styled banners.
4. **Track D — shadcn MCP**: install
   `npx shadcn@latest mcp init --client claude` so future agents can
   pull shadcn components on demand. **Done in this PR** — see
   `.mcp.json`.

## Inputs

- [longbridge/gpui-component][gpui-component] — Rust GPUI component
  library powering Longbridge desktop. ~150 components (Button, Input,
  Table, Chart, Modal, Notification, ContextMenu, Drawer, Tabs, …),
  GPU-accelerated, uses [Zed's `gpui`][zed-gpui] under the hood.
- [longbridge/gpui-component gallery][gpui-gallery] — live demos of
  every component. Use this as the reference for what to compose.
- [Longbridge desktop][lb-desktop] — the production app built on
  `gpui-component`. Visual benchmark.
- [shadcn/ui][shadcn] — Radix + Tailwind primitives we'll borrow for
  the web dashboard's design refresh.
- [`madsmtm/objc2`][objc2] — Rust bindings to the Objective-C runtime
  and Apple frameworks. The `objc2-user-notifications`,
  `objc2-user-notifications-ui`, and `objc2-foundation` crates carry
  the `UserNotifications` surface we need.
- The Apple-notification research dossier (verbatim below) for the
  rich-notification track.

## Why now

- The web dashboard has hit its frame budget on the relations graph at
  ≥ 2k nodes (canvas + d3-force + React reconciliation). GPUI moves the
  draw call to native + GPU.
- Telegram bot covers `/dashboard` notifications well, but **macOS
  desktop notifications** are stuck at title + body. Agents that fail
  silently don't surface; long-running indexes don't ring; recovered
  PRs don't post.
- The shadcn/Radix design system has matured into the de-facto React
  primitive layer. Lifting from it accelerates every panel rewrite.
- The shadcn MCP unlocks "agent-native" component installs — when we
  spin up future frontend agents, they can query shadcn components
  directly instead of remembering APIs.

## Track A — `crabcc-desktop` (gpui-component)

### Crate layout

```
crates/crabcc-desktop/
  Cargo.toml          # gpui, gpui-component, crabcc-core, reqwest, tokio
  src/
    main.rs           # entrypoint: open the main window
    lib.rs            # re-exports + AppState
    app.rs            # GPUI App definition
    routes/           # one module per dashboard route
      home.rs
      logs.rs
      system.rs
      knowledge.rs
    api.rs            # typed wire client over /api/* (mirrors web)
    sse.rs            # SSE consumer (reqwest-eventsource or hand-rolled)
    theme.rs          # dark/light tokens matching the web --bg/--fg
```

### Phasing

- **A.0 (kickoff PR)**: scaffold crate, depend on `gpui` + `gpui-component`,
  open a 1280×800 window with the brand "crabcc · live" and a static
  panel showing `repo / version`. **Just proves the toolchain.**
- **A.1**: API client port — generate Rust types from `openapi.yaml`
  via `oapi-gen` or hand-write to mirror `web/src/api.ts`.
- **A.2**: SSE consumer + AppState reactive layer — gpui's
  `Entity` + `cx.notify()` model lets us push activity / agents
  updates the same way React's SSE handler does today.
- **A.3**: Port the **DashboardHome** route (KPI strip + tiles) using
  `gpui-component`'s `Card`, `Stack`, `Spark`, `Pill` — visual parity
  with `#/` on the web. **Milestone 1: dashboard demo.**
- **A.4**: Port relations graph using `gpui-component`'s `Graph`/`Chart`
  + d3-force-rs (or a port of the existing canvas hit-testing).
  **Milestone 2: graph perf benchmark vs. web.**
- **A.5**: Logs / system / knowledge routes.
- **A.6**: Native menu bar (`gpui-component::Menu`), dock badge,
  system tray.

### Resolved decisions for Track A

**A.1 GPUI rev pinning — RESOLVED (corrected after build).** First
attempt: pin `gpui` + `gpui_platform` to the rev `gpui-component`'s
gallery binary builds against (resolved from upstream's `Cargo.lock`).
This **does not work** as a top-level pin — `gpui-component` itself
declares `gpui = { git = "...zed" }` with no rev, so cargo treats our
`{ git, rev = "<x>" }` and gpui-component's `{ git }` as separate
sources. Two zed checkouts get pulled, two compiled copies of the
`Render` / `Styled` traits exist, and the desktop crate fails to
compile with `expected vs. found trait` errors pointing at the same
type from different revs.

Working approach: **don't pin a rev at the top level. Mirror
gpui-component's revless source URL** so cargo unifies to a single
zed checkout, and rely on the **binary crate's `Cargo.lock`** for
reproducibility. The pin lives in the lockfile, which is checked in.

```toml
# crates/crabcc-desktop/Cargo.toml
gpui           = { git = "https://github.com/zed-industries/zed" }
gpui_platform  = { git = "https://github.com/zed-industries/zed", features = ["font-kit", "x11", "wayland", "runtime_shaders"] }
gpui-component = { git = "https://github.com/longbridge/gpui-component", rev = "41f51428c563597af4f04fb476141d3311a1c0c4" }
```

Bump procedure: bump the `gpui-component` rev → `cargo update -p
gpui-component` → review the new `Cargo.lock` zed entries → commit
both manifest and lockfile together. Never `cargo update -p gpui`
without bumping `gpui-component` first; the two move together.

**A.1 workspace integration — RESOLVED: standalone crate, not a
workspace member.** Two reasons surface immediately:

1. `gpui-component` pulls a non-optional `tree-sitter = "0.25"` with
   `links = "tree-sitter"` — cargo enforces a single version per such
   native-linked crate across the resolution. Crabcc's existing
   tree-sitter is 0.22 (with the grammar fleet at 0.21), so joining
   the workspace would force a coordinated tree-sitter-22→25 bump
   across six grammar crates.
2. The desktop crate's plan is to talk to the rest of crabcc over
   HTTP/SSE on `127.0.0.1:7878` (loopback only), so no path dep on
   `crabcc-core` is required.

The crate keeps its own `[workspace]` table (empty) so cargo treats
it as its own root, gets its own `Cargo.lock` (checked in for
reproducibility), and builds with `cd crates/crabcc-desktop && cargo
run`. Re-evaluate joining the parent workspace if/when crabcc-core
moves to tree-sitter 0.25.

### Open questions for Track A

1. **Bundling** — `cargo bundle` vs. Xcode + `cargo-xcode`. Apple
   Silicon code-signing is mandatory; this overlaps with Track C.
2. **Hot-reload** — gpui has a `gpui-hot-reload` story; verify it
   plays nicely with our build. If not, dev loop is `cargo run`
   like Iced/Slint.
3. **Bridge to existing `crabcc serve`** — desktop client points at
   `http://127.0.0.1:7878` for everything (loopback only). Same SSE
   stream, same APIs. No backend changes required.
4. **Cross-platform** — gpui supports macOS + Linux + Windows, but
   our Apple-specific work (Track C) won't compile elsewhere. Gate
   behind `#[cfg(target_os = "macos")]`.

## Track B — shadcn-flavoured web refresh

### Goal

Don't rip out the Lucide / CSS-modules / esbuild stack we just shipped
in PR #212. Instead, **adopt shadcn's design tokens, motion primitives,
and component shapes** and migrate panels opportunistically when they
need work anyway.

### Resolved decisions for Track B

**Tailwind — RESOLVED: adopt it.** Stripping Tailwind out and mapping
shadcn's utility classes back to plain CSS variables is feasible but
introduces a permanent translation tax on every shadcn component we
copy in. Adopting Tailwind keeps us at parity with upstream, lets us
paste components directly from `npx shadcn@latest add …`, and is the
de-facto path the project documents. Tradeoff accepted: PostCSS +
Tailwind CLI in the esbuild pipeline. Bundle cost is bounded by
JIT-purging (only utilities the dashboard actually uses ship).

Phasing reflects this:

### Phasing

- **B.0 (this PR)**: install the shadcn MCP via
  `npx shadcn@latest mcp init --client claude` so future agents have
  access to it. **Done.**
- **B.1**: wire Tailwind into the esbuild pipeline (`bun add -d
  tailwindcss postcss autoprefixer`, `tailwind.config.ts`,
  `postcss.config.js`, `@tailwind base; @tailwind components;
  @tailwind utilities;` at the top of `styles.css`). Configure the
  `content:` glob to scan `src/**/*.{ts,tsx}` so JIT purges to a tight
  utility set.
- **B.2**: rename CSS variables to shadcn conventions (`--background`,
  `--foreground`, `--primary`, `--muted`, `--accent`, `--destructive`,
  `--ring`). Trivial sed; preserves visual continuity.
- **B.3**: introduce shadcn's `Button` / `Input` / `Dialog` /
  `DropdownMenu` / `Select` primitives in **new** components only;
  don't refactor existing ones until they need work.
- **B.4**: motion — adopt `framer-motion` micro-animations on tile
  hover, route transitions, ingest-result reveal. Bundle delta budget:
  ≤ 25 KB.
- **B.5**: full migration of `<DashTile />` and `<NodeInfo />` to
  shadcn `Card` once B.2 lands.

### Open questions for Track B

1. **Dark theme parity** — shadcn's default palette is built around
   `slate`; our `--bg #161618` is closer to neutral. Probably fine
   to override the palette, but document it.
2. **PostCSS + Tailwind CLI in the esbuild pipeline** — verify the
   build script's added latency stays under 300 ms incremental
   (otherwise reach for `tailwindcss-oxide` once stable).

## Track C — Native macOS rich notifications

This is the deepest track. The full research dossier (verbatim from
the kickoff message) lives in
[`./RESEARCH-apple-rich-notifications-dossier.md`](./RESEARCH-apple-rich-notifications-dossier.md).
Highlights below.

### Why we can't just use `notify-rust`

> Generic abstractions fundamentally lack robust support for capturing
> notification interactions, such as button clicks or inline text
> responses, rendering actionable notifications effectively impossible
> through standard cross-platform crates. (— dossier)

`notify-rust` is fine for "fire-and-forget toast"; it cannot:

- Render attachments (images, audio, video).
- Surface action buttons or inline-reply text input.
- Display custom-styled banners (only `UNNotificationContentExtension`
  can do that).
- Receive `didReceiveNotificationResponse:` callbacks reliably.
- Use the new `UNUserNotifications` framework on Apple Silicon
  without `panic_unwind`-vs-`panic_abort` gymnastics on releases.

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  crabcc host process (Rust)                                  │
│  ┌────────────────────────┐   ┌─────────────────────────┐    │
│  │ crabcc-notify (lib)    │──▶│ objc2-user-notifications│    │
│  │  - register categories │   │ FFI to UserNotifications│    │
│  │  - submit requests     │   └─────────────────────────┘    │
│  │  - delegate impl via   │   ┌─────────────────────────┐    │
│  │    declare_class!      │──▶│ App Group shared state  │    │
│  └────────────────────────┘   │  group.dev.crabcc       │    │
│             │                 └─────────────────────────┘    │
│             ▼                            │                   │
│  ┌────────────────────────┐              │                   │
│  │ UNNotificationCenter   │◀─────────────┘                   │
│  │ (system process)       │                                  │
│  └────────────┬───────────┘                                  │
│               │                                               │
└───────────────┼───────────────────────────────────────────────┘
                ▼
   ┌──────────────────────────────────┐
   │ apps/crabcc-notify-ext/          │  ← Swift / Xcode-built .appex
   │  - Notification Content Extension │
   │  - reads App Group → renders UI   │
   │  - performNotificationDefaultAction│
   │    on tap → host process resumes  │
   └──────────────────────────────────┘
```

### Phasing

- **C.0 (this PR)**: stub the `crabcc-notify` crate with the bridge
  module + a placeholder Swift extension folder
  (`apps/crabcc-notify-ext-poc/`). Documents the architecture; **no
  runtime code**.
- **C.1**: minimal `objc2`-based "fire a banner" path —
  `UNMutableNotificationContent` + `addNotificationRequest:`. Replaces
  the `task notify` shell-outs.
- **C.2**: register `UNNotificationCategory` + actions ("Open log",
  "Dismiss", "Reply"). Wire the `UNUserNotificationCenterDelegate`
  via `declare_class!`.
- **C.3**: Apple-developer-account-gated path — entitlements + App
  Group + first `.appex`. Requires a Developer ID Application
  certificate. Documented but not built locally without one.
- **C.4**: `UNNotificationContentExtension` — Swift target,
  Storyboard / SwiftUI view, App Group reads, `performNotificationDefaultAction`
  handoff.
- **C.5**: APNs path — server-side Node or Rust HTTP/2 client (the
  dossier covers the payload shape) for **remote** rich pushes.
  Out-of-scope until C.0–C.4 land.

### Open questions for Track C

1. **Code signing** — is the user's Apple Developer Team ID
   provisioned for App Groups? Without it, C.3+ stalls.
2. **Distribution surface** — do notifications come from the host
   `crabcc serve` process, the desktop app (Track A), or the
   `crabcc-telegram` bot? Probably **the desktop app**: it has the
   long-lived process and the user's attention; the bot relays a
   summary out-of-band.
3. **Build pipeline** — `cargo bundle` doesn't co-bundle `.appex`
   targets. Need a custom `setup-apple-extension.sh` per the dossier,
   or a switch to `xcodegen` + `cargo-xcode`.

## Track D — shadcn MCP — DONE in this PR

`.mcp.json` now includes:

```json
{
  "mcpServers": {
    "shadcn": { "command": "npx", "args": ["shadcn@latest", "mcp"] }
  }
}
```

Future frontend agents can call shadcn-MCP-provided tools (`registry`,
`add`, `init`, etc.) directly. Confirm it loads on next Claude Code
restart.

## Cross-track dependencies

```
Track D (shadcn MCP) ─────────────┐
                                  ▼
Track B (web refresh) ◀──────── shadcn registry calls
        │
        ▼
   shared design tokens
        ▲
        │
Track A (gpui-desktop) ─── reuses tokens (theme.rs)
        │
        ▼
   shared HTTP/SSE API client (no backend changes)
        ▲
        │
Track C (rich notifications) ── lives in the desktop process,
                                fires via crabcc-notify
```

A and B can be pursued in parallel. C depends on A (the desktop
process is the natural notification host). D unblocks B.

## Phase-0 deliverables (this PR)

- [x] `.mcp.json` with shadcn MCP server
- [x] `crates/crabcc-desktop/docs/RESEARCH-native-desktop-and-rich-notifications.md` (this
      document)
- [x] `crates/crabcc-desktop/docs/RESEARCH-apple-rich-notifications-dossier.md` (verbatim
      research from the kickoff message)
- [x] `crates/crabcc-desktop/` scaffolding — Cargo.toml + minimal
      `main.rs` that prints the workspace banner and exits cleanly.
      Not registered in `[workspace.members]` until A.1 to keep
      `cargo build --release` warning-free.
- [x] `apps/crabcc-notify-ext-poc/` placeholder dir + README
      describing the future Swift extension target.
- [x] PR + issue describing all four tracks for community review.

Out of scope:
- Actual gpui rendering (A.0 minimal binary only).
- Any rich-notification runtime code (C.0 docs only).
- Tailwind installation or shadcn component imports (B.0 MCP only).

## Next steps after merge

1. File issues for A.1 / B.1 / C.1 — split the kickoff into
   trackable units.
2. **A.1** — resolve the upstream `gpui` rev from
   `longbridge/gpui-component`'s `crates/story/Cargo.toml`, pin
   identically in `crates/crabcc-desktop/Cargo.toml`, register the
   crate in `[workspace.members]`.
3. **B.1** — wire Tailwind + PostCSS into the esbuild pipeline.
4. **C.0** — confirm Apple Developer Team ID + App Groups
   provisioning before C.3.

[gpui-component]: https://github.com/longbridge/gpui-component
[gpui-gallery]: https://longbridge.github.io/gpui-component/gallery/
[lb-desktop]: https://longbridge.com/desktop/
[lb-screenshot]: https://longbridge.com/desktop/
[shadcn]: https://ui.shadcn.com/docs
[objc2]: https://github.com/madsmtm/objc2
[zed-gpui]: https://github.com/zed-industries/zed/tree/main/crates/gpui
