//! Commands launchpad — searchable catalog of the crabcc CLI surface.
//!
//! Two flavours of row in the same list:
//!
//!   * **Runnable** rows (the new `API (runnable)` category at the
//!     top) map 1:1 to a no-arg HTTP method on [`crate::api::Client`]
//!     — clicking dispatches via [`AppState::submit_command_run`] and
//!     the response (Debug-formatted) renders inline below the row.
//!   * **CLI-shape** rows (everything else) stay display-only because
//!     they need user input the desktop client doesn't have an input
//!     surface for yet (e.g. `crabcc sym <NAME>`). They're flagged
//!     with a muted "(needs input · CLI only)" tail; promoting them
//!     to runnable is a follow-up that needs an argument sheet.
//!
//! Per the design brief (#293/#295): single-slot run state — at most
//! one row pulses "running…" at a time. A new click cancels (visually)
//! the prior run and replaces both `running_command` + `last_command_run`.

use gpui::{
    div, prelude::*, px, ClipboardItem, Context, Entity, Focusable, Hsla, IntoElement, MouseButton,
    Render, SharedString, Window,
};
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex, ActiveTheme,
};

use crate::routes::empty::empty_state;
use crate::state::AppState;
use gpui_component::tooltip::Tooltip;

/// Identifier for a runnable Commands row. One variant per no-arg
/// HTTP method on [`crate::api::Client`]; the dispatch table lives
/// in `state::run_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnableCommand {
    Health,
    Bootstrap,
    Services,
    Agents,
    AgentProfiles,
    AgentKills,
    AgentModels,
    OllamaKey,
    OtlpHealth,
    Reindex,
    RandomQuery,
    SeedGraph,
    MemoryRecent,
}

impl RunnableCommand {
    /// Stable string key — used for gpui ElementIds + matching the
    /// CATALOG row to its runnable variant.
    pub fn key(&self) -> &'static str {
        match self {
            RunnableCommand::Health => "health",
            RunnableCommand::Bootstrap => "bootstrap",
            RunnableCommand::Services => "services",
            RunnableCommand::Agents => "agents",
            RunnableCommand::AgentProfiles => "agent_profiles",
            RunnableCommand::AgentKills => "agent_kills",
            RunnableCommand::AgentModels => "agent_models",
            RunnableCommand::OllamaKey => "ollama_key",
            RunnableCommand::OtlpHealth => "otlp_health",
            RunnableCommand::Reindex => "reindex",
            RunnableCommand::RandomQuery => "random_query",
            RunnableCommand::SeedGraph => "seed_graph",
            RunnableCommand::MemoryRecent => "memory_recent",
        }
    }
}

pub struct CommandsRoute {
    state: Entity<AppState>,
    /// gpui-component InputState — owns text + focus state.
    query_input: Entity<InputState>,
    /// Lower-cased mirror of the input's value, kept in sync via
    /// `InputEvent::Change`. Avoids re-lowercasing the query for
    /// every match check on every render.
    query_lower: String,
}

impl CommandsRoute {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        let query_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter commands…"));
        cx.subscribe_in(&query_input, window, |this, state, event, _, cx| {
            if let InputEvent::Change = event {
                this.query_lower = state.read(cx).value().to_string().to_lowercase();
                cx.notify();
            }
        })
        .detach();
        Self {
            state,
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

    fn run(&self, cmd: RunnableCommand, cx: &mut Context<Self>) {
        self.state.update(cx, |s, _cx| s.submit_command_run(cmd));
    }

    fn copy_last_result(&self, cx: &mut Context<Self>) {
        let payload =
            self.state
                .read(cx)
                .last_command_run
                .as_ref()
                .map(|(_cmd, body)| match body {
                    Ok(s) => s.clone(),
                    Err(s) => s.clone(),
                });
        if let Some(text) = payload {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
}

impl Render for CommandsRoute {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let card = cx.theme().secondary;
        let primary = cx.theme().primary;
        let success = cx.theme().success;
        let danger = cx.theme().danger;
        let foreground = cx.theme().foreground;

        // Snapshot run state so the per-row closures don't need to
        // re-borrow.
        let running_command = self.state.read(cx).running_command;
        let last_command_run = self.state.read(cx).last_command_run.clone();

        // Filter the static catalog; track totals so the header line
        // can show "5 of 31" without doing the work twice.
        let total: usize = CATALOG.iter().map(|c| c.commands.len()).sum();
        let runnable_total: usize = CATALOG
            .iter()
            .flat_map(|c| c.commands.iter())
            .filter(|c| c.runnable.is_some())
            .count();
        let visible: Vec<(&Category, Vec<&Command>)> = CATALOG
            .iter()
            .map(|cat| {
                let cmds: Vec<&Command> = cat
                    .commands
                    .iter()
                    .filter(|c| self.cmd_matches(c))
                    .collect();
                (cat, cmds)
            })
            .filter(|(_, cmds)| !cmds.is_empty())
            .collect();
        let visible_count: usize = visible.iter().map(|(_, c)| c.len()).sum();

        let count_label = if self.query_lower.is_empty() {
            format!("{total} commands · {runnable_total} runnable")
        } else {
            format!("{visible_count} of {total} commands match")
        };
        let running_label: gpui::AnyElement = match running_command {
            Some(_) => div()
                .text_color(success)
                .child(SharedString::new_static("· running"))
                .into_any_element(),
            None => div().into_any_element(),
        };
        let header = h_flex()
            .gap_3()
            .child(div().text_lg().child(SharedString::new_static("Commands")))
            .child(
                div()
                    .text_color(muted)
                    .child(SharedString::from(count_label)),
            )
            .child(running_label);

        // Wrapper border brightens to `primary` while the input is
        // focused — same focus-indicator pattern as the other route
        // filters.
        let filter_focused = self
            .query_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        let filter_border = if filter_focused { primary } else { border };
        let search_field = div()
            .border_1()
            .border_color(filter_border)
            .rounded_md()
            .px_2()
            .py_1()
            .child(Input::new(&self.query_input).appearance(false));

        let sections = visible
            .into_iter()
            .map(|(cat, cmds)| {
                section(
                    cat,
                    &cmds,
                    muted,
                    border,
                    card,
                    primary,
                    foreground,
                    success,
                    danger,
                    running_command,
                    last_command_run.as_ref(),
                    cx.entity(),
                )
            })
            .collect::<Vec<_>>();

        // Filter narrowed everything out — surface a centered hint
        // so the body doesn't read as a layout glitch where only the
        // search field stayed.
        let body: gpui::AnyElement = if sections.is_empty() && !self.query_lower.is_empty() {
            empty_state(
                "\u{1F50D}",
                "No commands match the filter",
                &format!(
                    "Nothing matches \u{201C}{}\u{201D} — try a shorter query.",
                    self.query_lower
                ),
                muted,
                foreground,
            )
            .into_any_element()
        } else {
            v_flex().gap_4().children(sections).into_any_element()
        };

        v_flex()
            .size_full()
            .px_5()
            .py_4()
            .gap_4()
            .child(header)
            .child(search_field)
            .child(body)
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
    /// `Some(_)` for rows that map to a no-arg HTTP method — clicking
    /// dispatches via `AppState::submit_command_run`. `None` for
    /// CLI-shape rows that need user input the desktop launchpad
    /// doesn't have a sheet for yet.
    runnable: Option<RunnableCommand>,
}

#[allow(clippy::too_many_arguments)]
fn section(
    cat: &Category,
    visible_cmds: &[&Command],
    muted: Hsla,
    border: Hsla,
    card: Hsla,
    primary: Hsla,
    foreground: Hsla,
    success: Hsla,
    danger: Hsla,
    running_command: Option<RunnableCommand>,
    last_command_run: Option<&(RunnableCommand, Result<String, String>)>,
    view: Entity<CommandsRoute>,
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
                .child(
                    div()
                        .text_color(muted)
                        .child(SharedString::new_static(cat.blurb)),
                ),
        )
        .child(
            v_flex().gap_1().children(
                visible_cmds
                    .iter()
                    .map(|cmd| {
                        command_row(
                            cmd,
                            muted,
                            card,
                            border,
                            foreground,
                            primary,
                            success,
                            danger,
                            running_command,
                            last_command_run,
                            view.clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        )
}

#[allow(clippy::too_many_arguments)]
fn command_row(
    cmd: &Command,
    muted: Hsla,
    card: Hsla,
    border: Hsla,
    foreground: Hsla,
    primary: Hsla,
    success: Hsla,
    danger: Hsla,
    running_command: Option<RunnableCommand>,
    last_command_run: Option<&(RunnableCommand, Result<String, String>)>,
    view: Entity<CommandsRoute>,
) -> gpui::AnyElement {
    let runnable = cmd.runnable;
    let is_running = matches!((runnable, running_command), (Some(r), Some(rc)) if r == rc);
    let last_for_this = last_command_run
        .filter(|(c, _)| Some(*c) == runnable)
        .map(|(_, body)| body.clone());

    // Invocation chip — slightly elevated. For runnable rows, give it
    // a subtle border in the primary colour so the click affordance
    // reads.
    let chip_border = if runnable.is_some() { primary } else { border };
    let chip_id_str = match runnable {
        Some(r) => format!("commands-row-{}", r.key()),
        None => format!("commands-row-static-{}", cmd.invocation),
    };
    let chip_view = view.clone();
    let chip_runnable = runnable;
    // Only runnable rows get cursor + hover. CLI-shape rows are
    // display-only — pretending they're clickable would set up a
    // disappointing click. The hover for runnable rows borrows
    // `border` (the dim row-border on non-runnable rows) so a
    // hovered runnable chip "pre-glows" with its non-runnable
    // sibling's border, then settles back when the mouse leaves.
    // Inverse direction would be more idiomatic but `primary` is
    // the stronger colour and we're already using it for the
    // active border — overlapping reads as "more active".
    let mut chip = div()
        .id(SharedString::from(chip_id_str))
        .min_w(px(280.0))
        .px_2()
        .py_0p5()
        .bg(card)
        .border_1()
        .border_color(chip_border)
        .rounded_md()
        .text_color(foreground);
    if runnable.is_some() {
        let summary: SharedString = SharedString::new_static(cmd.summary);
        chip = chip
            .cursor_pointer()
            .hover(move |s| s.bg(card).border_color(primary).text_color(primary))
            .tooltip(move |window, cx| Tooltip::new(summary.clone()).build(window, cx));
    }
    let chip = chip
        .child(SharedString::from(format!("crabcc {}", cmd.invocation)))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            if let Some(r) = chip_runnable {
                cx.stop_propagation();
                chip_view.update(cx, |this, cx| {
                    this.run(r, cx);
                    cx.notify();
                });
            }
        });

    // Status tail — runnable rows get a "click to run" hint until they
    // run; CLI-shape rows get the disabled hint.
    let tail: gpui::AnyElement = if is_running {
        h_flex()
            .gap_2()
            .child(
                div()
                    .text_color(success)
                    .child(SharedString::new_static("● running…")),
            )
            .into_any_element()
    } else if runnable.is_some() {
        div()
            .text_color(muted)
            .child(SharedString::new_static("(click to run)"))
            .into_any_element()
    } else {
        div()
            .text_color(muted)
            .child(SharedString::new_static("(needs input · CLI only)"))
            .into_any_element()
    };

    // Inline result block, when the most-recent run targeted this row.
    let result_block: gpui::AnyElement = match last_for_this {
        None => div().into_any_element(),
        Some(Ok(body)) => {
            let copy_view = view.clone();
            v_flex()
                .gap_1()
                .pl_4()
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_color(success)
                                .text_xs()
                                .child(SharedString::new_static("✓ done")),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!(
                                    "commands-row-copy-{}",
                                    runnable.map(|r| r.key()).unwrap_or("?")
                                )))
                                .px_2()
                                .py_0p5()
                                .border_1()
                                .border_color(border)
                                .rounded_md()
                                .text_color(muted)
                                .text_xs()
                                .cursor_pointer()
                                .hover(move |s| s.border_color(primary).text_color(primary))
                                .tooltip(|window, cx| {
                                    Tooltip::new("Copy result to clipboard").build(window, cx)
                                })
                                .child(SharedString::new_static("Copy"))
                                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                    cx.stop_propagation();
                                    copy_view.update(cx, |this, cx| {
                                        this.copy_last_result(cx);
                                        cx.notify();
                                    });
                                }),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "commands-row-result-{}",
                            runnable.map(|r| r.key()).unwrap_or("?")
                        )))
                        .max_h(px(240.0))
                        .px_2()
                        .py_1()
                        .border_1()
                        .border_color(border)
                        .rounded_md()
                        .bg(gpui::black().opacity(0.35))
                        .text_color(foreground)
                        .text_xs()
                        .overflow_y_scroll()
                        .child(SharedString::from(body)),
                )
                .into_any_element()
        }
        Some(Err(err)) => v_flex()
            .gap_1()
            .pl_4()
            .child(
                div()
                    .text_color(danger)
                    .text_xs()
                    .child(SharedString::from(format!("✗ {err}"))),
            )
            .into_any_element(),
    };

    v_flex()
        .gap_1()
        .py_1()
        .child(
            h_flex()
                .gap_3()
                .child(chip)
                .child(
                    div()
                        .flex_1()
                        .text_color(muted)
                        .child(SharedString::new_static(cmd.summary)),
                )
                .child(tail),
        )
        .child(result_block)
        .into_any_element()
}

// ── Catalog ──────────────────────────────────────────────────────────
// Update this table when `crabcc --help` grows new top-level surfaces.
// The structure mirrors the help output's natural grouping; per-command
// subcommands are listed inline (e.g. `memory remember`).

const CATALOG: &[Category] = &[
    Category {
        name: "API (runnable)",
        blurb: "No-arg HTTP probes the desktop client can dispatch directly. Click a row to run.",
        commands: &[
            Command {
                invocation: "health",
                summary: "GET /api/health — quick readiness probe.",
                runnable: Some(RunnableCommand::Health),
            },
            Command {
                invocation: "bootstrap",
                summary: "GET /api/bootstrap — index counts + version + repo.",
                runnable: Some(RunnableCommand::Bootstrap),
            },
            Command {
                invocation: "services",
                summary: "GET /api/services — local service-discovery report.",
                runnable: Some(RunnableCommand::Services),
            },
            Command {
                invocation: "agents.list",
                summary: "GET /api/agents — running + recent agents.",
                runnable: Some(RunnableCommand::Agents),
            },
            Command {
                invocation: "agents.profiles",
                summary: "GET /api/agents/profiles — declared profiles.",
                runnable: Some(RunnableCommand::AgentProfiles),
            },
            Command {
                invocation: "agents.kills",
                summary: "GET /api/agents/kills — recent SIGKILL log.",
                runnable: Some(RunnableCommand::AgentKills),
            },
            Command {
                invocation: "agents.models",
                summary: "GET /api/agents/models — provider × model registry.",
                runnable: Some(RunnableCommand::AgentModels),
            },
            Command {
                invocation: "ollama.key",
                summary: "GET /api/ollama-key — local Ollama API-key state.",
                runnable: Some(RunnableCommand::OllamaKey),
            },
            Command {
                invocation: "otlp.health",
                summary: "GET /api/otlp-health — collector readiness pill.",
                runnable: Some(RunnableCommand::OtlpHealth),
            },
            Command {
                invocation: "reindex",
                summary: "POST /api/reindex — rebuild the symbol index.",
                runnable: Some(RunnableCommand::Reindex),
            },
            Command {
                invocation: "random_query",
                summary: "POST /api/random-query — fire one synthetic activity event for demo.",
                runnable: Some(RunnableCommand::RandomQuery),
            },
            Command {
                invocation: "seed_graph",
                summary: "GET /api/seed-graph — relations graph snapshot (nodes + edges).",
                runnable: Some(RunnableCommand::SeedGraph),
            },
            Command {
                invocation: "memory.recent",
                summary: "GET /api/memory/recent — newest memory drawers.",
                runnable: Some(RunnableCommand::MemoryRecent),
            },
        ],
    },
    Category {
        name: "SYMBOLS",
        blurb: "Symbol-aware code lookup — the hot path.",
        commands: &[
            Command {
                invocation: "sym <NAME>",
                summary: "Find the definition of a symbol.",
                runnable: None,
            },
            Command {
                invocation: "refs <NAME>",
                summary: "List references to a name. --files-only for a deduped path list.",
                runnable: None,
            },
            Command {
                invocation: "callers <NAME>",
                summary: "List callers of a function. --count for a single integer.",
                runnable: None,
            },
            Command {
                invocation: "fuzzy <QUERY>",
                summary: "Levenshtein-2 fuzzy match against indexed symbols.",
                runnable: None,
            },
            Command {
                invocation: "prefix <QUERY>",
                summary: "Prefix match — auto-complete-style lookup.",
                runnable: None,
            },
            Command {
                invocation: "outline <FILE>",
                summary: "Show every fn / struct / impl in a file with line ranges.",
                runnable: None,
            },
            Command {
                invocation: "files [FILTERS]",
                summary: "List indexed source files with --under / --ext / --limit.",
                runnable: None,
            },
        ],
    },
    Category {
        name: "GRAPH",
        blurb: "Call-graph queries — built from the populated edges table.",
        commands: &[
            Command {
                invocation: "graph build",
                summary: "One-shot SQL scan to populate the edges table.",
                runnable: None,
            },
            Command {
                invocation: "graph walk <NAME>",
                summary: "BFS callers / callees from a named root.",
                runnable: None,
            },
            Command {
                invocation: "graph cycles",
                summary: "Strongly-connected components of size ≥ 2.",
                runnable: None,
            },
            Command {
                invocation: "graph orphans",
                summary: "Defined fns with no incoming callers.",
                runnable: None,
            },
        ],
    },
    Category {
        name: "MEMORY",
        blurb: "Per-repo memory drawer — stores notes alongside the index.",
        commands: &[
            Command {
                invocation: "memory init",
                summary: "Create .crabcc/memory.db with FTS5 + vec scaffolding.",
                runnable: None,
            },
            Command {
                invocation: "memory remember <ID> <BODY>",
                summary: "Stash a note keyed by id.",
                runnable: None,
            },
            Command {
                invocation: "memory search <QUERY>",
                summary: "Hybrid BM25 ⊕ vector search with RRF fusion.",
                runnable: None,
            },
            Command {
                invocation: "memory list",
                summary: "Recent drawers, newest-first.",
                runnable: None,
            },
            Command {
                invocation: "memory get <ID>",
                summary: "Fetch one drawer by id.",
                runnable: None,
            },
            Command {
                invocation: "memory ingest",
                summary: "Pipe stdin or a file into a new drawer.",
                runnable: None,
            },
            Command {
                invocation: "memory mine project",
                summary: "Walk the repo and ingest every file as a drawer.",
                runnable: None,
            },
            Command {
                invocation: "memory mine sessions",
                summary: "Walk Claude Code JSONL transcripts.",
                runnable: None,
            },
            Command {
                invocation: "memory health",
                summary: "Drawer count, FTS state, embedder dim.",
                runnable: None,
            },
        ],
    },
    Category {
        name: "AGENTS",
        blurb: "Local agent dispatch — Ollama / Anthropic via a per-profile registry.",
        commands: &[
            Command {
                invocation: "agents run --profile <ID>",
                summary: "Launch an agent with a saved profile.",
                runnable: None,
            },
            Command {
                invocation: "agents launch --prompt …",
                summary: "One-off agent without persisting a profile.",
                runnable: None,
            },
        ],
    },
    Category {
        name: "FETCH",
        blurb: "URL extraction + per-domain content adapters.",
        commands: &[Command {
            invocation: "fetch <URL>",
            summary: "Download + extract a URL to markdown. --remember to ingest.",
            runnable: None,
        }],
    },
    Category {
        name: "INDEX",
        blurb: "Symbol-index lifecycle.",
        commands: &[
            Command {
                invocation: "index",
                summary: "Walk the repo and (re)build .crabcc/symbols.db.",
                runnable: Some(RunnableCommand::Reindex),
            },
            Command {
                invocation: "smoke",
                summary: "Smoke-test the index pipeline against a fixture.",
                runnable: None,
            },
            Command {
                invocation: "compress",
                summary: "Train an FSST symbol table and re-encode rows.",
                runnable: None,
            },
        ],
    },
    Category {
        name: "SERVE",
        blurb: "Live dashboard + MCP transports.",
        commands: &[
            Command {
                invocation: "serve",
                summary: "Boot the dashboard at 127.0.0.1:7878 (this app talks to it).",
                runnable: None,
            },
            Command {
                invocation: "--mcp",
                summary: "Stdio MCP transport — wire into Claude Code / Cursor.",
                runnable: None,
            },
        ],
    },
    Category {
        name: "META",
        blurb: "Token economy + version + install helpers.",
        commands: &[
            Command {
                invocation: "track",
                summary: "Token-savings ledger for the session.",
                runnable: None,
            },
            Command {
                invocation: "install-claude",
                summary: "Symlink the skill + slash command into ~/.claude/.",
                runnable: None,
            },
            Command {
                invocation: "--version",
                summary: "Print version + git rev.",
                runnable: None,
            },
        ],
    },
];
