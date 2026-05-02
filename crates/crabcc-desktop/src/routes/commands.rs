//! Commands launchpad — searchable catalog of the crabcc CLI surface.
//!
//! Greenfield route per the design brief. Renders the CLI command
//! inventory grouped by family with a top-of-route TextInput that
//! filters live as the user types. Sections whose commands all
//! filter out are hidden, so an empty result reads cleanly.
//!
//! No dispatch yet — clicking a row is a follow-up that needs an
//! argument-input sheet for non-trivial commands.
//!
//! The catalog is hand-maintained for now. A follow-up could fetch
//! `--help` output from the server to keep it canonically in sync;
//! today the cost of that machinery outweighs the maintenance cost
//! of the table at the bottom of this file.

use gpui::{
    div, prelude::*, px, Context, Entity, Hsla, IntoElement, Render, SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};

pub struct CommandsRoute {
    /// gpui-component InputState — owns text + focus state.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input's value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing the query for
    /// every match check on every render.
    query_lower: String,
}

impl CommandsRoute {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter commands…"));
        cx.subscribe_in(&query_input, window, |this, state, event, _, cx| {
            if let InputEvent::Change = event {
                this.query_lower = state.read(cx).value().to_string().to_lowercase();
                cx.notify();
            }
        })
        .detach();
        Self {
            query_input,
            query_lower: String::new(),
        }
    }

    fn cmd_matches(&self, cmd: &Command) -> bool {
        if self.query_lower.is_empty() {
            return true;
        }
        cmd.invocation.to_lowercase().contains(&self.query_lower)
            || cmd.summary.to_lowercase().contains(&self.query_lower)
    }
}

impl Render for CommandsRoute {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let card = cx.theme().secondary;
        let primary = cx.theme().primary;

        // Filter the static catalog; track totals so the header line
        // can show "5 of 31" without doing the work twice.
        let total: usize = CATALOG.iter().map(|c| c.commands.len()).sum();
        let visible: Vec<(&Category, Vec<&Command>)> = CATALOG
            .iter()
            .map(|cat| {
                let cmds: Vec<&Command> = cat.commands.iter().filter(|c| self.cmd_matches(c)).collect();
                (cat, cmds)
            })
            .filter(|(_, cmds)| !cmds.is_empty())
            .collect();
        let visible_count: usize = visible.iter().map(|(_, c)| c.len()).sum();

        let header = h_flex()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .child(SharedString::new_static("Commands")),
            )
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(if self.query_lower.is_empty() {
                        format!("{total} commands")
                    } else {
                        format!("{visible_count} of {total} commands match")
                    })),
            );

        let search_field = div()
            .border_1()
            .border_color(border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        let sections = visible
            .into_iter()
            .map(|(cat, cmds)| section(cat, &cmds, muted, border, card, primary))
            .collect::<Vec<_>>();

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_4()
            .child(header)
            .child(search_field)
            .child(v_flex().gap_4().children(sections))
    }
}

struct Category {
    name: &'static str,
    blurb: &'static str,
    commands: &'static [Command],
}

struct Command {
    /// Without the `crabcc` prefix — printed inline.
    invocation: &'static str,
    summary: &'static str,
}

fn section(
    cat: &Category,
    visible_cmds: &[&Command],
    muted: Hsla,
    border: Hsla,
    card: Hsla,
    primary: Hsla,
) -> gpui::Div {
    v_flex()
        .gap_2()
        .pb_3()
        .border_b_1()
        .border_color(border)
        .child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .text_color(primary)
                        .child(SharedString::new_static(cat.name)),
                )
                .child(div().text_color(muted).child(SharedString::new_static(cat.blurb))),
        )
        .child(
            v_flex().gap_1().children(
                visible_cmds
                    .iter()
                    .map(|cmd| command_row(cmd, muted, card))
                    .collect::<Vec<_>>(),
            ),
        )
}

fn command_row(cmd: &Command, muted: Hsla, card: Hsla) -> gpui::Div {
    h_flex()
        .gap_3()
        .py_1()
        .child(
            div()
                .min_w(px(280.0))
                .px_2()
                .py_0p5()
                .bg(card)
                .rounded_md()
                .child(SharedString::from(format!("crabcc {}", cmd.invocation))),
        )
        .child(
            div()
                .text_color(muted)
                .child(SharedString::new_static(cmd.summary)),
        )
}

// ── Catalog ──────────────────────────────────────────────────────────
// Update this table when `crabcc --help` grows new top-level surfaces.
// The structure mirrors the help output's natural grouping; per-command
// subcommands are listed inline (e.g. `memory remember`).

const CATALOG: &[Category] = &[
    Category {
        name: "SYMBOLS",
        blurb: "Symbol-aware code lookup — the hot path.",
        commands: &[
            Command { invocation: "sym <NAME>", summary: "Find the definition of a symbol." },
            Command { invocation: "refs <NAME>", summary: "List references to a name. --files-only for a deduped path list." },
            Command { invocation: "callers <NAME>", summary: "List callers of a function. --count for a single integer." },
            Command { invocation: "fuzzy <QUERY>", summary: "Levenshtein-2 fuzzy match against indexed symbols." },
            Command { invocation: "prefix <QUERY>", summary: "Prefix match — auto-complete-style lookup." },
            Command { invocation: "outline <FILE>", summary: "Show every fn / struct / impl in a file with line ranges." },
            Command { invocation: "files [FILTERS]", summary: "List indexed source files with --under / --ext / --limit." },
        ],
    },
    Category {
        name: "GRAPH",
        blurb: "Call-graph queries — built from the populated edges table.",
        commands: &[
            Command { invocation: "graph build", summary: "One-shot SQL scan to populate the edges table." },
            Command { invocation: "graph walk <NAME>", summary: "BFS callers / callees from a named root." },
            Command { invocation: "graph cycles", summary: "Strongly-connected components of size ≥ 2." },
            Command { invocation: "graph orphans", summary: "Defined fns with no incoming callers." },
        ],
    },
    Category {
        name: "MEMORY",
        blurb: "Per-repo memory drawer — stores notes alongside the index.",
        commands: &[
            Command { invocation: "memory init", summary: "Create .crabcc/memory.db with FTS5 + vec scaffolding." },
            Command { invocation: "memory remember <ID> <BODY>", summary: "Stash a note keyed by id." },
            Command { invocation: "memory search <QUERY>", summary: "Hybrid BM25 ⊕ vector search with RRF fusion." },
            Command { invocation: "memory list", summary: "Recent drawers, newest-first." },
            Command { invocation: "memory get <ID>", summary: "Fetch one drawer by id." },
            Command { invocation: "memory ingest", summary: "Pipe stdin or a file into a new drawer." },
            Command { invocation: "memory mine project", summary: "Walk the repo and ingest every file as a drawer." },
            Command { invocation: "memory mine sessions", summary: "Walk Claude Code JSONL transcripts." },
            Command { invocation: "memory health", summary: "Drawer count, FTS state, embedder dim." },
        ],
    },
    Category {
        name: "AGENTS",
        blurb: "Local agent dispatch — Ollama / Anthropic via a per-profile registry.",
        commands: &[
            Command { invocation: "agents run --profile <ID>", summary: "Launch an agent with a saved profile." },
            Command { invocation: "agents launch --prompt …", summary: "One-off agent without persisting a profile." },
        ],
    },
    Category {
        name: "FETCH",
        blurb: "URL extraction + per-domain content adapters.",
        commands: &[
            Command { invocation: "fetch <URL>", summary: "Download + extract a URL to markdown. --remember to ingest." },
        ],
    },
    Category {
        name: "INDEX",
        blurb: "Symbol-index lifecycle.",
        commands: &[
            Command { invocation: "index", summary: "Walk the repo and (re)build .crabcc/symbols.db." },
            Command { invocation: "smoke", summary: "Smoke-test the index pipeline against a fixture." },
            Command { invocation: "compress", summary: "Train an FSST symbol table and re-encode rows." },
        ],
    },
    Category {
        name: "SERVE",
        blurb: "Live dashboard + MCP transports.",
        commands: &[
            Command { invocation: "serve", summary: "Boot the dashboard at 127.0.0.1:7878 (this app talks to it)." },
            Command { invocation: "--mcp", summary: "Stdio MCP transport — wire into Claude Code / Cursor." },
        ],
    },
    Category {
        name: "META",
        blurb: "Token economy + version + install helpers.",
        commands: &[
            Command { invocation: "track", summary: "Token-savings ledger for the session." },
            Command { invocation: "install-claude", summary: "Symlink the skill + slash command into ~/.claude/." },
            Command { invocation: "--version", summary: "Print version + git rev." },
        ],
    },
];
