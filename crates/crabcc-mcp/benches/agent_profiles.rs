//! Shared agent-usage workload synthesizer for the crabcc MCP benches.
//!
//! Used by both the in-process criterion bench (`benches/agent_workload.rs`)
//! and the end-to-end replay example (`examples/agent_replay.rs`). Models how
//! different CLI coding agents drive the crabcc MCP server. The profiles
//! differ in *tool mix* and *cadence*; the traffic is **synthesized from real
//! symbol/file names** discovered in an index (no captured traces required):
//!
//!   - `claude_code` — Claude Code gathers context in broad parallel bursts:
//!     locate a symbol, outline its file, pull refs + callers, read a file,
//!     occasionally route through `ctx`. Read-heavy, widest mix.
//!   - `nullclaw`    — github.com/nullclaw/nullclaw runs a single-threaded
//!     agent loop: a lean, strictly sequential lookup cadence (sym → refs →
//!     callers → fuzzy), one call at a time, few reads.
//!   - `zeroclaw`    — automated/headless sibling biased toward dependency
//!     analysis: heavier refs/callers + prefix scans, light on reads.
//!
//! These mixes are deliberately easy to retune in one place if real captured
//! MCP traces become available later.

// Shared across two targets (the criterion bench and the e2e example); each
// consumer uses a different subset of the surface, so per-target dead_code
// warnings are expected and intentionally silenced here.
#![allow(dead_code)]

use serde_json::{json, Value};

/// A CLI-agent usage profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    ClaudeCode,
    Nullclaw,
    Zeroclaw,
}

/// One tool call in a profile's repeating pattern.
#[derive(Clone, Copy)]
enum Step {
    Sym,
    Refs,
    Callers,
    Outline,
    Fuzzy,
    Prefix,
    Read,
    Ctx,
}

impl Profile {
    /// All profiles, for iterating in the bench.
    pub const ALL: [Profile; 3] = [Profile::ClaudeCode, Profile::Nullclaw, Profile::Zeroclaw];

    /// Stable identifier used in bench ids, CLI flags, and report rows.
    pub fn name(self) -> &'static str {
        match self {
            Profile::ClaudeCode => "claude_code",
            Profile::Nullclaw => "nullclaw",
            Profile::Zeroclaw => "zeroclaw",
        }
    }

    /// Parse a profile name (accepts a few spellings).
    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "claude_code" | "claude-code" | "claude" => Some(Profile::ClaudeCode),
            "nullclaw" | "null" => Some(Profile::Nullclaw),
            "zeroclaw" | "zero" => Some(Profile::Zeroclaw),
            _ => None,
        }
    }

    /// The tool pattern this agent cycles through. The synthesizer repeats
    /// the slice until it reaches the requested call count, drawing concrete
    /// symbol/file names round-robin.
    fn pattern(self) -> &'static [Step] {
        use Step::*;
        match self {
            Profile::ClaudeCode => &[
                Sym, Outline, Refs, Sym, Ctx, Callers, Read, Sym, Fuzzy, Outline,
            ],
            Profile::Nullclaw => &[Sym, Refs, Callers, Sym, Fuzzy],
            Profile::Zeroclaw => &[Refs, Callers, Prefix, Refs, Callers, Sym],
        }
    }
}

/// One JSON-RPC `tools/call` request value.
fn call(id: i64, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    })
}

/// Mangle a symbol into a near-miss so `fuzzy` does real Levenshtein work
/// (swap the final character). Short names pass through unchanged.
fn typo(s: &str) -> String {
    let mut v: Vec<char> = s.chars().collect();
    if v.len() < 3 {
        return s.to_string();
    }
    let last = v.len() - 1;
    v[last] = if v[last] == 'x' { 'y' } else { 'x' };
    v.into_iter().collect()
}

/// Synthesize a profile's NDJSON workload of `calls` `tools/call` requests
/// from real `syms` and repo-relative `files`. Names cycle round-robin so the
/// queries hit real index entries; empty inputs fall back to safe defaults.
pub fn synthesize(profile: Profile, syms: &[String], files: &[String], calls: usize) -> Vec<u8> {
    let pattern = profile.pattern();
    let fallback_sym = String::from("main");
    let fallback_file = String::from("lib.rs");
    let pick = |slice: &[String], i: usize, fb: &String| -> String {
        slice
            .get(i % slice.len().max(1))
            .cloned()
            .unwrap_or_else(|| fb.clone())
    };

    let mut buf = Vec::with_capacity(calls * 96);
    for i in 0..calls {
        let step = pattern[i % pattern.len()];
        let sym = pick(syms, i, &fallback_sym);
        let file = pick(files, i, &fallback_file);
        let id = (i + 1) as i64;
        let req = match step {
            Step::Sym => call(id, "sym", json!({ "name": sym })),
            Step::Refs => call(id, "refs", json!({ "name": sym, "limit": 50 })),
            Step::Callers => call(id, "callers", json!({ "name": sym })),
            Step::Outline => call(id, "outline", json!({ "file": file })),
            Step::Fuzzy => call(id, "fuzzy", json!({ "name": typo(&sym) })),
            Step::Prefix => {
                let p: String = sym.chars().take(3).collect();
                call(id, "prefix", json!({ "name": p }))
            }
            Step::Read => call(
                id,
                "read",
                json!({ "path": file, "session_id": format!("bench-{}", profile.name()) }),
            ),
            Step::Ctx => call(id, "ctx", json!({ "tool": "sym", "args": { "name": sym } })),
        };
        buf.extend_from_slice(req.to_string().as_bytes());
        buf.push(b'\n');
    }
    buf
}

/// Scan `root` for source files and a deduped set of symbol-ish names, used to
/// ground synthesized queries in entries the indexer actually recorded. This
/// is a loose multi-language heuristic (not a parser): it collects file paths
/// by extension and identifiers following common definition keywords. Good
/// enough to make sym/refs/callers/outline hit real targets.
pub fn discover(root: &std::path::Path) -> (Vec<String>, Vec<String>) {
    const SOURCE_EXT: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "rb", "java", "c", "cc", "cpp", "h", "hpp",
    ];
    const DEF_KW: &[&str] = &[
        "fn ",
        "func ",
        "def ",
        "class ",
        "struct ",
        "trait ",
        "interface ",
        "function ",
        "type ",
        "enum ",
    ];
    const STRIP: &[&str] = &["pub ", "export ", "async ", "default ", "static ", "final "];

    let mut files = Vec::new();
    let mut syms = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                // Skip artifact / vcs dirs that hold no source.
                if !matches!(
                    name.as_ref(),
                    ".crabcc" | ".git" | "target" | "node_modules"
                ) {
                    stack.push(path);
                }
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !SOURCE_EXT.contains(&ext) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(root) {
                files.push(rel.to_string_lossy().into_owned());
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                for line in text.lines() {
                    let mut t = line.trim_start();
                    for s in STRIP {
                        t = t.strip_prefix(s).unwrap_or(t);
                    }
                    for kw in DEF_KW {
                        if let Some(rest) = t.strip_prefix(kw) {
                            let ident: String = rest
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect();
                            if ident.len() >= 3 {
                                syms.push(ident);
                            }
                        }
                    }
                }
            }
        }
    }
    syms.sort();
    syms.dedup();
    (files, syms)
}
