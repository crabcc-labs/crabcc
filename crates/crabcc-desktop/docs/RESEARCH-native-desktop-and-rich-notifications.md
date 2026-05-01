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

### Phasing

- **B.0 (this PR)**: install the shadcn MCP via
  `npx shadcn@latest mcp init --client claude` so future agents have
  access to it. **Done.**
- **B.1**: tailwind-cssvars adoption — keep the existing CSS-variable
  theme but rename to shadcn's conventions (`--background`,
  `--foreground`, `--primary`, `--muted`, `--accent`, `--destructive`,
  `--ring`). Trivial sed; preserves visual continuity.
- **B.2**: introduce shadcn's `Button` / `Input` / `Dialog` /
  `DropdownMenu` / `Select` primitives in **new** components only;
  don't refactor existing ones until they need work.
- **B.3**: motion — adopt `framer-motion` micro-animations on tile
  hover, route transitions, ingest-result reveal. Bundle delta budget:
  ≤ 25 KB.
- **B.4**: full migration of `<DashTile />` and `<NodeInfo />` to
  shadcn `Card` once B.1 lands.

### Open questions for Track B

1. We're not running Tailwind today (esbuild + plain CSS). shadcn
   officially expects Tailwind; the [shadcn/ui CSS-in-JS path] is
   experimental. **Decision needed**: ship Tailwind into the bundle
   (~4 KB compressed when purged) or adapt shadcn's CSS variables
   manually.
2. Dark theme parity — shadcn's default palette is built around
   `slate`; our `--bg #161618` is closer to neutral. Probably fine
   to override the palette, but document it.

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
2. Register `crabcc-desktop` in `[workspace.members]` once it builds
   on macOS without warnings.
3. Decide Tailwind-yes-or-no for Track B (B.1 is blocked on this).
4. Confirm Apple Developer Team ID + App Groups provisioning for
   Track C.

[gpui-component]: https://github.com/longbridge/gpui-component
[gpui-gallery]: https://longbridge.github.io/gpui-component/gallery/
[lb-desktop]: https://longbridge.com/desktop/
[lb-screenshot]: https://longbridge.com/desktop/
[shadcn]: https://ui.shadcn.com/docs
[objc2]: https://github.com/madsmtm/objc2
[zed-gpui]: https://github.com/zed-industries/zed/tree/main/crates/gpui
