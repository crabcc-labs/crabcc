//! Multi-line startup banner + ANSI helper.
//!
//! `print_banner` writes the version / bound URL / repo root / index
//! presence summary to stderr at server start. Goes to stderr so a
//! piping invocation like `crabcc serve --no-open 2>/dev/null` stays
//! silent. Colors honor `NO_COLOR` and `CRABCC_NO_COLOR`.

use crate::{runtime, Config};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

/// Multi-line startup banner showing version, bound URL, repo root, index
/// presence, and a few quick links. Goes to stderr so a piping invocation
/// like `crabcc serve --no-open 2>/dev/null` is silent. ANSI colors honor
/// `NO_COLOR` (https://no-color.org) and are stripped if stderr isn't a tty.
pub(crate) fn print_banner(cfg: &Config, addr: SocketAddr, init: Option<&runtime::InitOutcome>) {
    let c = Style::for_stderr();
    let url = format!("http://{}:{}", addr.ip(), addr.port());
    let index_db = cfg.root.join(".crabcc").join("index.db");
    let graph_json = cfg.root.join(".crabcc").join("graph.json");

    let index_state = describe_path(&index_db);
    let graph_state = describe_path(&graph_json);

    let mut routes = String::new();
    routes.push_str(&format!(
        "  {} {}/                         (interactive call-graph viewer)\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/live                     (live monitoring dashboard)\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/graph?root=&dir=&depth=\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/activity?since=TS&limit=N\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/memory/recent?since=TS&limit=N\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/memory/graph?limit=N\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!(
        "  {} {}/api/memory/get?id=ID\n",
        c.dim("GET"),
        url
    ));
    routes.push_str(&format!("  {} {}/api/memory/ingest\n", c.dim("POST"), url));
    routes.push_str(&format!("  {} {}/api/bootstrap\n", c.dim("GET"), url));
    routes.push_str(&format!("  {} {}/api/health\n", c.dim("GET"), url));

    eprintln!();
    eprintln!(
        "{}  {}",
        c.brand("crabcc viz"),
        c.dim(&format!("v{}", env!("CARGO_PKG_VERSION")))
    );
    eprintln!("{}", c.dim("─".repeat(54).as_str()));
    eprintln!("  {}    {}", c.label("listen"), c.bold(&url));
    eprintln!("  {}      {}", c.label("root"), cfg.root.display());
    eprintln!("  {}     {}", c.label("index"), index_state);
    eprintln!("  {}     {}", c.label("graph"), graph_state);
    eprintln!("  {}      {}", c.label("bind"), describe_bind(cfg.bind, &c));
    eprintln!(
        "  {}   {}",
        c.label("threads"),
        c.dim("tiny_http default pool")
    );
    if let Some(o) = init {
        let bits = format!(
            "{} files, {} symbols, {} graph edges, {} drawers",
            o.files, o.symbols, o.graph_edges, o.drawers
        );
        let action = if o.created_index {
            "indexed"
        } else {
            "refreshed"
        };
        eprintln!("  {}      {} ({bits})", c.label("init"), action);
    } else if !cfg.init {
        eprintln!(
            "  {}      {}",
            c.label("init"),
            c.dim("skipped (--no-init)")
        );
    }
    eprintln!();
    eprintln!("{}", c.dim("routes"));
    eprint!("{routes}");
    eprintln!();
    eprintln!("  {} {}", c.dim("→"), c.dim("Ctrl-C to stop"));
    eprintln!();
}

fn describe_path(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(meta) => {
            let size = meta.len();
            let kb = size as f64 / 1024.0;
            let suffix = if kb >= 1024.0 {
                format!("{:.1} MB", kb / 1024.0)
            } else if kb >= 1.0 {
                format!("{kb:.1} KB")
            } else {
                format!("{size} B")
            };
            format!("{} ({})", p.display(), suffix)
        }
        Err(_) => format!(
            "{} (missing — run `crabcc index` and `crabcc graph build`)",
            p.display()
        ),
    }
}

fn describe_bind(ip: IpAddr, c: &Style) -> String {
    if ip.is_loopback() {
        format!("{} {}", ip, c.dim("(loopback only)"))
    } else {
        format!(
            "{} {}",
            ip,
            c.warn("(non-loopback — viewer is unauthenticated)")
        )
    }
}

/// Tiny ANSI helper that disables colors when `NO_COLOR` is set, when
/// `CRABCC_NO_COLOR` is set (project-specific override), or when stderr
/// is not a tty (e.g. redirected to a logfile). We don't pull in `nu-ansi`
/// or `colored` for this — half a dozen escape codes don't justify a dep.
struct Style {
    on: bool,
}

impl Style {
    fn for_stderr() -> Self {
        let no_color =
            std::env::var_os("NO_COLOR").is_some() || std::env::var_os("CRABCC_NO_COLOR").is_some();
        #[cfg(unix)]
        let is_tty = libc_isatty(2);
        #[cfg(not(unix))]
        let is_tty = true;
        Self {
            on: !no_color && is_tty,
        }
    }
    fn brand(&self, s: &str) -> String {
        self.wrap(s, "\x1b[1;38;5;208m")
    }
    fn label(&self, s: &str) -> String {
        self.wrap(s, "\x1b[38;5;244m")
    }
    fn dim(&self, s: &str) -> String {
        self.wrap(s, "\x1b[2m")
    }
    fn bold(&self, s: &str) -> String {
        self.wrap(s, "\x1b[1m")
    }
    fn warn(&self, s: &str) -> String {
        self.wrap(s, "\x1b[1;33m")
    }
    fn wrap(&self, s: &str, prefix: &str) -> String {
        if self.on {
            format!("{prefix}{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

#[cfg(unix)]
fn libc_isatty(fd: i32) -> bool {
    // SAFETY: `isatty` only inspects a file-descriptor table entry; no
    // pointer dereference, no aliasing concerns.
    unsafe extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(fd) == 1 }
}
