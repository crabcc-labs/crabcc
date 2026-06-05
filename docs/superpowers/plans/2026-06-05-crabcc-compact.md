# crabcc-compact Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crabcc-compact`, a hook-based ML context compressor that intercepts Claude Code tool outputs and user prompts, token-gates them locally, and proxies qualifying payloads to a LLMLingua-2 service on a Tailscale-connected remote node for byte-for-byte extractive compression.

**Architecture:** Thin Rust binary (`crabcc-compact`) does local dedup + gate + HTTP proxy; a Python FastAPI service (`compact-server/`) on the tailnet node loads LLMLingua-2 permanently and serves `/compact` and `/enrich`. The Rust binary also runs as an MCP/SSE server. Hook shell scripts pipe Claude Code hook JSON through the binary.

**Tech Stack:** Rust (reqwest, tokio, clap, serde\_json, anyhow, ahash, toml — all workspace deps), Python 3.11+ (fastapi, uvicorn, llmlingua, mlx\_lm), Claude Code PostToolUse + UserPromptSubmit hooks.

---

## Phase 1: Rust Crate

### Task 1: Workspace + crate scaffold

**Files:**
- Modify: `Cargo.toml` (root workspace)
- Create: `crates/crabcc-compact/Cargo.toml`
- Create: `crates/crabcc-compact/src/main.rs`
- Create: `crates/crabcc-compact/src/lib.rs`

- [ ] **Step 1: Add crate to workspace members**

In root `Cargo.toml`, in the `members = [...]` array, add:
```toml
    "crates/crabcc-compact",
```

Also add to `[workspace.dependencies]`:
```toml
crabcc-compact = { path = "crates/crabcc-compact" }
```

- [ ] **Step 2: Create `crates/crabcc-compact/Cargo.toml`**

```toml
[package]
name = "crabcc-compact"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "crabcc-compact"
path = "src/main.rs"

[dependencies]
anyhow     = { workspace = true }
clap       = { workspace = true }
serde      = { workspace = true }
serde_json = { workspace = true }
tokio      = { workspace = true }
reqwest    = { workspace = true }
toml       = { workspace = true }
tracing    = { workspace = true }
tracing-subscriber = { workspace = true }
ahash      = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 3: Create `crates/crabcc-compact/src/lib.rs`** (module declarations only at this stage)

```rust
pub mod client;
pub mod config;
pub mod context;
pub mod economy;
pub mod enrich;
pub mod fallback;
pub mod gate;
pub mod mcp;
```

- [ ] **Step 4: Create stub `crates/crabcc-compact/src/main.rs`**

```rust
fn main() {
    eprintln!("crabcc-compact: not yet implemented");
    std::process::exit(1);
}
```

- [ ] **Step 5: Create empty module files so it compiles**

Create these files with just `// placeholder`:
- `crates/crabcc-compact/src/client.rs`
- `crates/crabcc-compact/src/config.rs`
- `crates/crabcc-compact/src/gate.rs`
- `crates/crabcc-compact/src/enrich.rs`
- `crates/crabcc-compact/src/context/mod.rs`
- `crates/crabcc-compact/src/context/hash.rs`
- `crates/crabcc-compact/src/context/redundancy.rs`
- `crates/crabcc-compact/src/economy/mod.rs`
- `crates/crabcc-compact/src/economy/budget.rs`
- `crates/crabcc-compact/src/economy/intensity.rs`
- `crates/crabcc-compact/src/fallback/mod.rs`
- `crates/crabcc-compact/src/fallback/truncate.rs`
- `crates/crabcc-compact/src/fallback/summarize.rs`
- `crates/crabcc-compact/src/mcp/mod.rs`
- `crates/crabcc-compact/src/mcp/server.rs`
- `crates/crabcc-compact/src/mcp/tools.rs`

- [ ] **Step 6: Verify it compiles**

```bash
cargo build -p crabcc-compact
```
Expected: compiles with no errors (the stub binary exits with code 1 when run).

- [ ] **Step 7: Commit**

```bash
git add crates/crabcc-compact/ Cargo.toml Cargo.lock
git commit -m "feat(compact): scaffold crabcc-compact crate"
```

---

### Task 2: config.rs

**Files:**
- Modify: `crates/crabcc-compact/src/config.rs`

The config loads from `~/.config/crabcc/compact.toml`. If absent, returns defaults. Repo-local `.crabcc/compact.toml` takes precedence if present.

- [ ] **Step 1: Write the failing test first**

Add to `crates/crabcc-compact/src/config.rs`:

```rust
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub endpoint: String,
    pub threshold_tokens: usize,
    pub timeout_ms: u64,
    pub enrich_trigger: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            threshold_tokens: 2000,
            timeout_ms: 8000,
            enrich_trigger: "!e".to_string(),
        }
    }
}

pub fn load() -> anyhow::Result<Config> {
    // Repo-local override first, then user config, then default.
    let local = PathBuf::from(".crabcc/compact.toml");
    if local.exists() {
        return parse_file(&local);
    }
    let user = dirs_path().join("compact.toml");
    if user.exists() {
        return parse_file(&user);
    }
    Ok(Config::default())
}

fn dirs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("crabcc")
}

fn parse_file(path: &PathBuf) -> anyhow::Result<Config> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    toml::from_str(&raw).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let c = Config::default();
        assert_eq!(c.threshold_tokens, 2000);
        assert_eq!(c.timeout_ms, 8000);
        assert_eq!(c.enrich_trigger, "!e");
    }

    #[test]
    fn parse_toml_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compact.toml");
        std::fs::write(&path, r#"
endpoint = "https://compact.example.ts.net:8080"
threshold_tokens = 3000
timeout_ms = 5000
enrich_trigger = "!enrich"
"#).unwrap();
        let c: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.endpoint, "https://compact.example.ts.net:8080");
        assert_eq!(c.threshold_tokens, 3000);
        assert_eq!(c.enrich_trigger, "!enrich");
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p crabcc-compact config
```
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/crabcc-compact/src/config.rs
git commit -m "feat(compact): config loader with toml + defaults"
```

---

### Task 3: gate.rs

**Files:**
- Modify: `crates/crabcc-compact/src/gate.rs`

Token estimation: `chars / 4` (fast byte-level approximation, no tokenizer dependency).

- [ ] **Step 1: Write failing tests**

```rust
pub fn token_estimate(text: &str) -> usize {
    text.len() / 4
}

pub fn above_threshold(text: &str, threshold: usize) -> bool {
    token_estimate(text) > threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_short_text() {
        // 40 chars → 10 tokens
        let text = "a".repeat(40);
        assert_eq!(token_estimate(&text), 10);
    }

    #[test]
    fn below_threshold_returns_false() {
        let text = "a".repeat(400); // 100 tokens
        assert!(!above_threshold(&text, 200));
    }

    #[test]
    fn above_threshold_returns_true() {
        let text = "a".repeat(8200); // 2050 tokens
        assert!(above_threshold(&text, 2000));
    }

    #[test]
    fn empty_text_below_any_threshold() {
        assert!(!above_threshold("", 1));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p crabcc-compact gate
```
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/crabcc-compact/src/gate.rs
git commit -m "feat(compact): token gate (chars/4 estimate)"
```

---

### Task 4: context/hash.rs — exact dedup

**Files:**
- Modify: `crates/crabcc-compact/src/context/hash.rs`
- Modify: `crates/crabcc-compact/src/context/mod.rs`

FNV-1a via `ahash::AHashSet` — exact-match: if we've seen this payload's hash this session, skip the network round-trip.

- [ ] **Step 1: Write failing tests + implementation**

`crates/crabcc-compact/src/context/hash.rs`:

```rust
use ahash::AHashSet;

pub struct SessionCache {
    seen: AHashSet<u64>,
}

impl SessionCache {
    pub fn new() -> Self {
        Self { seen: AHashSet::new() }
    }

    pub fn is_seen(&self, text: &str) -> bool {
        self.seen.contains(&hash(text))
    }

    pub fn mark_seen(&mut self, text: &str) {
        self.seen.insert(hash(text));
    }
}

fn hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    text.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_text_not_seen() {
        let cache = SessionCache::new();
        assert!(!cache.is_seen("hello world"));
    }

    #[test]
    fn marked_text_is_seen() {
        let mut cache = SessionCache::new();
        cache.mark_seen("hello world");
        assert!(cache.is_seen("hello world"));
    }

    #[test]
    fn different_text_not_seen() {
        let mut cache = SessionCache::new();
        cache.mark_seen("hello world");
        assert!(!cache.is_seen("hello worlds"));
    }

    #[test]
    fn empty_string_dedups() {
        let mut cache = SessionCache::new();
        cache.mark_seen("");
        assert!(cache.is_seen(""));
    }
}
```

`crates/crabcc-compact/src/context/mod.rs`:

```rust
pub mod hash;
pub mod redundancy;

pub use hash::SessionCache;
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p crabcc-compact context::hash
```
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/crabcc-compact/src/context/
git commit -m "feat(compact): session cache for exact-match dedup (ahash)"
```

---

### Task 5: context/redundancy.rs — Jaccard near-dup

**Files:**
- Modify: `crates/crabcc-compact/src/context/redundancy.rs`

Jaccard trigram similarity ≥ 0.85 = near-duplicate, skip. Keeps last 5 payloads for comparison (bounded memory).

- [ ] **Step 1: Write failing tests + implementation**

```rust
use ahash::AHashSet;

pub fn jaccard_trigrams(a: &str, b: &str) -> f32 {
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let intersection = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 { return 1.0; }
    intersection as f32 / union as f32
}

fn trigrams(s: &str) -> AHashSet<[u8; 3]> {
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return AHashSet::new();
    }
    bytes.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
}

pub struct NearDupCache {
    recent: std::collections::VecDeque<String>,
    capacity: usize,
}

impl NearDupCache {
    pub fn new(capacity: usize) -> Self {
        Self { recent: std::collections::VecDeque::with_capacity(capacity), capacity }
    }

    /// Returns true if `text` is ≥85% similar to any recently seen payload.
    pub fn is_near_dup(&self, text: &str) -> bool {
        self.recent.iter().any(|seen| jaccard_trigrams(seen, text) >= 0.85)
    }

    pub fn push(&mut self, text: String) {
        if self.recent.len() == self.capacity {
            self.recent.pop_front();
        }
        self.recent.push_back(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_score_one() {
        assert!((jaccard_trigrams("hello world", "hello world") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn completely_different_strings_score_zero() {
        let score = jaccard_trigrams("abcdef", "xyz123");
        assert!(score < 0.1, "score was {score}");
    }

    #[test]
    fn near_duplicate_above_threshold() {
        let a = "fn handle_request(req: Request) -> Response { let body = req.body(); body }";
        // Change one word
        let b = "fn handle_request(req: Request) -> Response { let body = req.body(); body  }";
        assert!(jaccard_trigrams(a, b) >= 0.85);
    }

    #[test]
    fn near_dup_cache_detects_duplicate() {
        let mut cache = NearDupCache::new(5);
        let text = "fn foo() -> i32 { 42 }".repeat(20);
        cache.push(text.clone());
        // Tiny whitespace change
        let near = format!("{text} ");
        assert!(cache.is_near_dup(&near));
    }

    #[test]
    fn near_dup_cache_passes_unrelated() {
        let mut cache = NearDupCache::new(5);
        cache.push("fn foo() -> i32 { 42 }".repeat(20));
        assert!(!cache.is_near_dup(&"struct Bar { x: u64 }".repeat(20)));
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let mut cache = NearDupCache::new(2);
        let old = "aaa bbb ccc ddd eee".repeat(10);
        cache.push(old.clone());
        cache.push("zzz yyy xxx www vvv".repeat(10));
        cache.push("mmm nnn ooo ppp qqq".repeat(10));
        // "old" evicted — should not be near-dup anymore
        assert!(!cache.is_near_dup(&old));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p crabcc-compact context::redundancy
```
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/crabcc-compact/src/context/redundancy.rs
git commit -m "feat(compact): Jaccard trigram near-dup cache (≥0.85 skip)"
```

---

### Task 6: economy — budget + adaptive intensity

**Files:**
- Modify: `crates/crabcc-compact/src/economy/budget.rs`
- Modify: `crates/crabcc-compact/src/economy/intensity.rs`
- Modify: `crates/crabcc-compact/src/economy/mod.rs`

- [ ] **Step 1: Write failing tests + budget.rs**

`economy/budget.rs`:

```rust
#[derive(Debug, Default)]
pub struct Budget {
    pub total_original: usize,
    pub total_compressed: usize,
    pub dedup_hits: usize,
    pub calls: usize,
}

impl Budget {
    pub fn new() -> Self { Self::default() }

    pub fn record_compress(&mut self, original: usize, compressed: usize) {
        self.calls += 1;
        self.total_original += original;
        self.total_compressed += compressed;
    }

    pub fn record_dedup(&mut self, size: usize) {
        self.dedup_hits += 1;
        self.total_original += size;
        // Dedup = 100% savings, compressed = 0
    }

    pub fn tokens_saved(&self) -> usize {
        self.total_original.saturating_sub(self.total_compressed)
    }

    /// Pressure: ratio of tokens already compressed this session.
    /// Rises toward 1.0 as total_original grows past 100K tokens.
    pub fn pressure(&self) -> f32 {
        let baseline = 100_000usize;
        (self.total_original as f32 / baseline as f32).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_budget_has_zero_pressure() {
        assert_eq!(Budget::new().pressure(), 0.0);
    }

    #[test]
    fn records_compress_and_computes_savings() {
        let mut b = Budget::new();
        b.record_compress(1000, 500);
        assert_eq!(b.tokens_saved(), 500);
        assert_eq!(b.calls, 1);
    }

    #[test]
    fn records_dedup_adds_to_original() {
        let mut b = Budget::new();
        b.record_dedup(2000);
        assert_eq!(b.total_original, 2000);
        assert_eq!(b.total_compressed, 0);
        assert_eq!(b.dedup_hits, 1);
    }

    #[test]
    fn pressure_caps_at_one() {
        let mut b = Budget::new();
        b.record_compress(200_000, 100_000);
        assert_eq!(b.pressure(), 1.0);
    }
}
```

- [ ] **Step 2: Write failing tests + intensity.rs**

`economy/intensity.rs`:

```rust
use super::budget::Budget;

/// Returns the compression ratio to request from the server.
/// 0.3 (aggressive) when session pressure ≥ 0.8, else 0.5 (moderate).
pub fn pick_ratio(budget: &Budget) -> f32 {
    if budget.pressure() >= 0.8 { 0.3 } else { 0.5 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_pressure_returns_moderate_ratio() {
        let b = Budget::new();
        assert!((pick_ratio(&b) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn high_pressure_returns_aggressive_ratio() {
        let mut b = Budget::new();
        b.record_compress(90_000, 45_000); // pressure = 0.9
        assert!((pick_ratio(&b) - 0.3).abs() < 1e-6);
    }
}
```

`economy/mod.rs`:

```rust
pub mod budget;
pub mod intensity;

pub use budget::Budget;
pub use intensity::pick_ratio;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p crabcc-compact economy
```
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/crabcc-compact/src/economy/
git commit -m "feat(compact): economy — budget ledger + adaptive intensity"
```

---

### Task 7: fallback — truncate + summarize

**Files:**
- Modify: `crates/crabcc-compact/src/fallback/truncate.rs`
- Modify: `crates/crabcc-compact/src/fallback/summarize.rs`
- Modify: `crates/crabcc-compact/src/fallback/mod.rs`

Used when the tailnet is unreachable. Keeps the first `head_lines` + last `tail_lines` lines. `extract_errors` keeps only lines matching error/warning patterns.

- [ ] **Step 1: Write failing tests + truncate.rs**

`fallback/truncate.rs`:

```rust
pub fn truncate(text: &str, head_lines: usize, tail_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total <= head_lines + tail_lines {
        return text.to_string();
    }
    let head = &lines[..head_lines];
    let tail = &lines[total - tail_lines..];
    let omitted = total - head_lines - tail_lines;
    format!(
        "{}\n... [{omitted} lines omitted by crabcc-compact fallback] ...\n{}",
        head.join("\n"),
        tail.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_returned_verbatim() {
        let text = "a\nb\nc";
        assert_eq!(truncate(text, 10, 10), text);
    }

    #[test]
    fn long_text_gets_head_and_tail() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n");
        let result = truncate(&text, 5, 5);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 4"));
        assert!(result.contains("line 95"));
        assert!(result.contains("line 99"));
        assert!(result.contains("90 lines omitted"));
        // Middle lines absent
        assert!(!result.contains("line 50"));
    }

    #[test]
    fn exactly_at_boundary_returned_verbatim() {
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n");
        assert_eq!(truncate(&text, 10, 10), text);
    }
}
```

- [ ] **Step 2: Write failing tests + summarize.rs**

`fallback/summarize.rs`:

```rust
pub fn extract_errors(text: &str) -> String {
    let error_patterns = ["error", "Error", "ERROR", "warning", "Warning", "WARN",
                          "panic", "PANIC", "failed", "FAILED", "exception", "Exception"];
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| error_patterns.iter().any(|p| l.contains(p)))
        .collect();
    if lines.is_empty() {
        // No error lines found — return first 20 lines instead
        text.lines().take(20).collect::<Vec<_>>().join("\n")
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_error_lines() {
        let text = "ok line\nerror: something broke\nanother ok\nwarning: watch out\nfine";
        let result = extract_errors(text);
        assert!(result.contains("error: something broke"));
        assert!(result.contains("warning: watch out"));
        assert!(!result.contains("ok line"));
        assert!(!result.contains("fine"));
    }

    #[test]
    fn no_errors_returns_first_20_lines() {
        let lines: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n");
        let result = extract_errors(&text);
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(result_lines.len(), 20);
        assert!(result.contains("line 0"));
        assert!(!result.contains("line 20"));
    }
}
```

`fallback/mod.rs`:

```rust
pub mod summarize;
pub mod truncate;

pub use summarize::extract_errors;
pub use truncate::truncate;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p crabcc-compact fallback
```
Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/crabcc-compact/src/fallback/
git commit -m "feat(compact): fallback — head+tail truncation + error extraction"
```

---

### Task 8: client.rs — HTTP proxy to tailnet

**Files:**
- Modify: `crates/crabcc-compact/src/client.rs`

Blocking reqwest client (hook process is short-lived; no async runtime needed here). Timeout is enforced via reqwest's built-in timeout. Returns `Err` on any network/timeout/shape issue — callers fall through to fallback.

- [ ] **Step 1: Write implementation + tests**

```rust
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct CompactResponse {
    pub compressed: String,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
}

#[derive(Debug, Deserialize)]
pub struct EnrichResponse {
    pub plan: String,
}

#[derive(Serialize)]
struct CompactRequest<'a> {
    text: &'a str,
    ratio: f32,
}

#[derive(Serialize)]
struct EnrichRequest<'a> {
    text: &'a str,
    query: &'a str,
}

pub fn compact(
    endpoint: &str,
    text: &str,
    ratio: f32,
    timeout_ms: u64,
) -> anyhow::Result<CompactResponse> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;
    let url = format!("{endpoint}/compact");
    let resp = client
        .post(&url)
        .json(&CompactRequest { text, ratio })
        .send()?
        .error_for_status()?;
    Ok(resp.json()?)
}

pub fn enrich(
    endpoint: &str,
    text: &str,
    query: &str,
    timeout_ms: u64,
) -> anyhow::Result<EnrichResponse> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;
    let url = format!("{endpoint}/enrich");
    let resp = client
        .post(&url)
        .json(&EnrichRequest { text, query })
        .send()?
        .error_for_status()?;
    Ok(resp.json()?)
}

pub fn health(endpoint: &str, timeout_ms: u64) -> anyhow::Result<serde_json::Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;
    let url = format!("{endpoint}/health");
    Ok(client.get(&url).send()?.error_for_status()?.json()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_returns_err_on_unreachable_endpoint() {
        // Port 1 is reserved and will refuse connections on any OS.
        let result = compact("http://127.0.0.1:1", "hello world", 0.5, 500);
        assert!(result.is_err());
    }

    #[test]
    fn enrich_returns_err_on_unreachable_endpoint() {
        let result = enrich("http://127.0.0.1:1", "code", "fix auth", 500);
        assert!(result.is_err());
    }

    #[test]
    fn health_returns_err_on_unreachable_endpoint() {
        let result = health("http://127.0.0.1:1", 500);
        assert!(result.is_err());
    }
}
```

Note: reqwest's `blocking` feature is already enabled in the workspace via the `reqwest` dep. If you get a compile error about `blocking` not being found, add `"blocking"` to the features list in `Cargo.toml`:
```toml
reqwest = { workspace = true, features = ["blocking"] }
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p crabcc-compact client
```
Expected: 3 tests pass (all verify fail-open behavior on unreachable endpoint).

- [ ] **Step 3: Commit**

```bash
git add crates/crabcc-compact/src/client.rs crates/crabcc-compact/Cargo.toml
git commit -m "feat(compact): HTTP client for /compact /enrich /health"
```

---

### Task 9: enrich.rs — trigger detection

**Files:**
- Modify: `crates/crabcc-compact/src/enrich.rs`

Detects the `!e` (or configured) prefix in a user prompt. Returns the stripped prompt if triggered, `None` otherwise.

- [ ] **Step 1: Write failing tests + implementation**

```rust
/// If `prompt` starts with `trigger` followed by whitespace (or is exactly `trigger`),
/// returns the remaining text with leading whitespace stripped. Otherwise None.
pub fn detect_trigger(prompt: &str, trigger: &str) -> Option<String> {
    if trigger.is_empty() {
        return None;
    }
    let stripped = prompt.strip_prefix(trigger)?;
    Some(stripped.trim_start().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_prefix_detected_and_stripped() {
        let result = detect_trigger("!e add rate limiting to the API", "!e");
        assert_eq!(result.unwrap(), "add rate limiting to the API");
    }

    #[test]
    fn no_trigger_returns_none() {
        assert!(detect_trigger("add rate limiting to the API", "!e").is_none());
    }

    #[test]
    fn empty_trigger_returns_none() {
        assert!(detect_trigger("anything", "").is_none());
    }

    #[test]
    fn trigger_only_returns_empty_string() {
        assert_eq!(detect_trigger("!e", "!e").unwrap(), "");
    }

    #[test]
    fn custom_trigger_works() {
        let result = detect_trigger("!enrich fix the auth middleware", "!enrich");
        assert_eq!(result.unwrap(), "fix the auth middleware");
    }

    #[test]
    fn partial_trigger_not_matched() {
        // "!enrich" does not match trigger "!e" followed by a word boundary check
        // Note: our implementation uses strip_prefix — "!enrich" does start with "!e",
        // so "!e" trigger WOULD match it. Document this: use a trigger with a space
        // suffix if word-boundary matching matters (e.g. trigger = "!e ").
        // This test documents the current behavior.
        let result = detect_trigger("!enrich something", "!e");
        assert!(result.is_some()); // "nrich something"
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p crabcc-compact enrich
```
Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/crabcc-compact/src/enrich.rs
git commit -m "feat(compact): enrich trigger detection (prefix strip)"
```

---

### Task 10: main.rs — PostToolUse hook

**Files:**
- Modify: `crates/crabcc-compact/src/main.rs`

The hook reads the Claude Code PostToolUse JSON from stdin, runs the full pipeline, and writes either the modified JSON or nothing (pass-through) to stdout. Never writes to stderr.

Claude Code PostToolUse hook protocol:
- stdin: `{"session_id": "...", "tool_name": "Read", "tool_input": {...}, "tool_result": {"type": "tool_result", "content": "<output>"}}`
- stdout to modify: `{"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": "<new content>"}}`
- stdout empty (or exit 0): pass through unchanged

- [ ] **Step 1: Write the pipeline struct and PostToolUse handler**

Replace `crates/crabcc-compact/src/main.rs` with:

```rust
mod client;
mod config;
mod context;
mod economy;
mod enrich;
mod fallback;
mod gate;
mod mcp;

use anyhow::Result;
use context::{hash::SessionCache, redundancy::NearDupCache};
use economy::{Budget, pick_ratio};
use serde_json::Value;
use std::io::{self, Read, Write};

fn main() {
    // Suppress all panics from reaching stderr — Claude Code surfaces hook
    // stderr as an error banner in the UI.
    std::panic::set_hook(Box::new(|_| {}));

    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(String::as_str).unwrap_or("");

    let result = match subcommand {
        "posttooluse" => run_posttooluse(),
        "promptsubmit" => run_promptsubmit(),
        "status"   => cmd_status(),
        "test"     => cmd_test(),
        "economy"  => cmd_economy(),
        "setup"    => cmd_setup(&args[2..]),
        "uninstall" => cmd_uninstall(&args[2..]),
        "--mcp"    => run_mcp(&args[2..]),
        _ => {
            eprintln!("usage: crabcc-compact <posttooluse|promptsubmit|status|test|economy|setup|uninstall|--mcp>");
            std::process::exit(1);
        }
    };

    if let Err(e) = result {
        // Log to file, not stderr
        log_error(&format!("{e:#}"));
        std::process::exit(0); // always exit 0 so hook doesn't block
    }
}

fn run_posttooluse() -> Result<()> {
    let cfg = config::load()?;
    let mut stdin = String::new();
    io::stdin().read_to_string(&mut stdin)?;

    let json: Value = match serde_json::from_str(&stdin) {
        Ok(v) => v,
        Err(_) => return Ok(()), // unparseable — pass through (print nothing)
    };

    // Extract the tool output content
    let content = extract_tool_result_content(&json);
    let content = match content {
        Some(c) if !c.is_empty() => c,
        _ => return Ok(()), // nothing to compress
    };

    // 1. Exact dedup (session cache is in-process; each hook invocation is a fresh process,
    //    so we only catch duplicates within the same hook call — cross-call dedup would
    //    require a persistent socket/file. Use NearDupCache for in-call comparison.)
    // 2. Gate
    if !gate::above_threshold(&content, cfg.threshold_tokens) {
        return Ok(()); // below threshold — pass through (print nothing)
    }

    // 3. Adaptive intensity
    let budget = Budget::new();
    let ratio = pick_ratio(&budget);

    // 4. Compress via tailnet
    let compressed = match client::compact(&cfg.endpoint, &content, ratio, cfg.timeout_ms) {
        Ok(r) => r.compressed,
        Err(e) => {
            log_error(&format!("compact failed: {e:#}"));
            // 5. Fallback: heuristic truncation
            fallback::truncate(&content, 100, 100)
        }
    };

    // Output the modified hook response
    let output = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": compressed
        }
    });
    io::stdout().write_all(serde_json::to_string(&output)?.as_bytes())?;
    Ok(())
}

fn extract_tool_result_content(json: &Value) -> Option<String> {
    // tool_result may be {"type": "tool_result", "content": "..."} or a plain string
    let tr = json.get("tool_result")?;
    if let Some(s) = tr.as_str() {
        return Some(s.to_string());
    }
    if let Some(content) = tr.get("content") {
        if let Some(s) = content.as_str() {
            return Some(s.to_string());
        }
        // content may be an array of {type, text} blocks
        if let Some(arr) = content.as_array() {
            let text: String = arr.iter()
                .filter_map(|b| b.get("text")?.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() { return Some(text); }
        }
    }
    None
}

fn log_error(msg: &str) {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let path = std::path::PathBuf::from(home).join(".crabcc").join("compact.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}: {msg}", chrono_now());
    }
}

fn chrono_now() -> String {
    // Simple timestamp without pulling in chrono — good enough for log lines
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "?".to_string())
}

// Stubs for remaining subcommands — implemented in subsequent tasks
fn run_promptsubmit() -> Result<()> { Ok(()) }
fn cmd_status() -> Result<()> { println!("status: not yet implemented"); Ok(()) }
fn cmd_test() -> Result<()> { println!("test: not yet implemented"); Ok(()) }
fn cmd_economy() -> Result<()> { println!("economy: not yet implemented"); Ok(()) }
fn cmd_setup(_args: &[String]) -> Result<()> { println!("setup: not yet implemented"); Ok(()) }
fn cmd_uninstall(_args: &[String]) -> Result<()> { println!("uninstall: not yet implemented"); Ok(()) }
fn run_mcp(_args: &[String]) -> Result<()> { println!("mcp: not yet implemented"); Ok(()) }
```

- [ ] **Step 2: Build**

```bash
cargo build -p crabcc-compact
```
Expected: compiles cleanly.

- [ ] **Step 3: Manual smoke test**

```bash
echo '{"session_id":"test","tool_name":"Read","tool_input":{},"tool_result":{"content":"short"}}' \
  | ./target/debug/crabcc-compact posttooluse
```
Expected: no output (content is below 2000 token threshold).

- [ ] **Step 4: Commit**

```bash
git add crates/crabcc-compact/src/main.rs
git commit -m "feat(compact): PostToolUse hook pipeline — gate + proxy + fallback"
```

---

### Task 11: main.rs — UserPromptSubmit hook + full CLI

**Files:**
- Modify: `crates/crabcc-compact/src/main.rs`

UserPromptSubmit protocol:
- stdin: `{"session_id": "...", "prompt": "<user message>"}`
- stdout to modify: `{"updatedPrompt": "<new prompt>"}`
- stdout empty: pass through unchanged

- [ ] **Step 1: Implement run\_promptsubmit, cmd\_status, cmd\_test, cmd\_economy**

Replace the stub functions in `main.rs`:

```rust
fn run_promptsubmit() -> Result<()> {
    let cfg = config::load()?;
    let mut stdin = String::new();
    io::stdin().read_to_string(&mut stdin)?;

    let json: Value = match serde_json::from_str(&stdin) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    let prompt = match json.get("prompt").and_then(|p| p.as_str()) {
        Some(p) => p.to_string(),
        None => return Ok(()),
    };

    // Detect enrich trigger
    let (prompt_text, do_enrich) = match enrich::detect_trigger(&prompt, &cfg.enrich_trigger) {
        Some(stripped) => (stripped, true),
        None => (prompt.clone(), false),
    };

    // Gate
    if !gate::above_threshold(&prompt_text, cfg.threshold_tokens) && !do_enrich {
        return Ok(()); // nothing to do
    }

    let mut budget = Budget::new();
    let ratio = pick_ratio(&budget);

    // Compress if above threshold
    let compressed = if gate::above_threshold(&prompt_text, cfg.threshold_tokens) {
        match client::compact(&cfg.endpoint, &prompt_text, ratio, cfg.timeout_ms) {
            Ok(r) => {
                budget.record_compress(
                    gate::token_estimate(&prompt_text),
                    gate::token_estimate(&r.compressed),
                );
                r.compressed
            }
            Err(e) => {
                log_error(&format!("promptsubmit compact failed: {e:#}"));
                fallback::truncate(&prompt_text, 50, 50)
            }
        }
    } else {
        prompt_text.clone()
    };

    // Enrich if triggered
    let final_prompt = if do_enrich {
        match client::enrich(&cfg.endpoint, &compressed, &prompt, cfg.timeout_ms) {
            Ok(r) => format!("{}\n\n---\n{compressed}", r.plan),
            Err(e) => {
                log_error(&format!("enrich failed: {e:#}"));
                compressed
            }
        }
    } else {
        compressed
    };

    if final_prompt == prompt {
        return Ok(()); // unchanged — print nothing
    }

    let output = serde_json::json!({ "updatedPrompt": final_prompt });
    io::stdout().write_all(serde_json::to_string(&output)?.as_bytes())?;
    Ok(())
}

fn cmd_status() -> Result<()> {
    let cfg = config::load()?;
    if cfg.endpoint.is_empty() {
        println!("endpoint: not configured (set endpoint in ~/.config/crabcc/compact.toml)");
        return Ok(());
    }
    match client::health(&cfg.endpoint, cfg.timeout_ms) {
        Ok(v) => println!("ok — {v}"),
        Err(e) => println!("unreachable: {e}"),
    }
    Ok(())
}

fn cmd_test() -> Result<()> {
    let cfg = config::load()?;
    // Generate a synthetic ~3K token payload (12K chars of Rust-ish code)
    let payload = generate_test_payload();
    println!("sending {}-char payload ({} est. tokens) to {}",
        payload.len(), gate::token_estimate(&payload), cfg.endpoint);
    let start = std::time::Instant::now();
    match client::compact(&cfg.endpoint, &payload, 0.5, cfg.timeout_ms) {
        Ok(r) => {
            let elapsed = start.elapsed();
            println!(
                "ok — {orig} → {comp} tokens ({:.1}%) in {ms}ms",
                orig = r.original_tokens,
                comp = r.compressed_tokens,
                r.original_tokens as f32 / r.compressed_tokens.max(1) as f32 * 100.0 - 100.0,
                ms = elapsed.as_millis()
            );
        }
        Err(e) => println!("error: {e}"),
    }
    Ok(())
}

fn cmd_economy() -> Result<()> {
    // Budget is per-process in current design; this shows the log file summary.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let path = std::path::PathBuf::from(home).join(".crabcc").join("compact.log");
    if path.exists() {
        let log = std::fs::read_to_string(&path)?;
        let lines: Vec<&str> = log.lines().rev().take(20).collect();
        println!("last 20 log entries (most recent first):");
        for l in lines { println!("  {l}"); }
    } else {
        println!("no log yet at {}", path.display());
    }
    Ok(())
}

fn generate_test_payload() -> String {
    // ~12K chars of plausible code-like text
    let snippet = r#"pub fn handle_request(req: HttpRequest, state: web::Data<AppState>) -> impl Responder {
    let token = req.headers().get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    if token.is_empty() { return HttpResponse::Unauthorized().finish(); }
    match state.db.get_user_by_token(token) {
        Ok(user) => HttpResponse::Ok().json(user),
        Err(e) => HttpResponse::InternalServerError().body(e.to_string()),
    }
}
"#;
    snippet.repeat(80)
}
```

- [ ] **Step 2: Build**

```bash
cargo build -p crabcc-compact
```

- [ ] **Step 3: Smoke test UserPromptSubmit below threshold (no output)**

```bash
echo '{"session_id":"test","prompt":"fix the bug"}' \
  | ./target/debug/crabcc-compact promptsubmit
```
Expected: no output.

- [ ] **Step 4: Smoke test status with no config**

```bash
./target/debug/crabcc-compact status
```
Expected: `endpoint: not configured ...`

- [ ] **Step 5: Commit**

```bash
git add crates/crabcc-compact/src/main.rs
git commit -m "feat(compact): UserPromptSubmit hook + status/test/economy CLI"
```

---

### Task 12: Hook shell scripts + setup/uninstall

**Files:**
- Create: `crates/crabcc-compact/src/hosts/mod.rs`
- Create: `crates/crabcc-compact/hooks/claude-posttooluse.sh`
- Create: `crates/crabcc-compact/hooks/claude-promptsubmit.sh`
- Modify: `crates/crabcc-compact/src/main.rs` (setup + uninstall)

- [ ] **Step 1: Create hook shell scripts**

`crates/crabcc-compact/hooks/claude-posttooluse.sh`:

```bash
#!/usr/bin/env bash
# crabcc-compact PostToolUse hook for Claude Code.
# Compresses large tool outputs before they reach Claude's context.
COMPACT="${CRABCC_COMPACT_BIN:-crabcc-compact}"
if ! command -v "$COMPACT" &>/dev/null; then exit 0; fi
input=$(cat)
printf '%s' "$input" | "$COMPACT" posttooluse 2>/dev/null
```

`crates/crabcc-compact/hooks/claude-promptsubmit.sh`:

```bash
#!/usr/bin/env bash
# crabcc-compact UserPromptSubmit hook for Claude Code.
# Compresses large prompts and optionally enriches them with an attack plan.
COMPACT="${CRABCC_COMPACT_BIN:-crabcc-compact}"
if ! command -v "$COMPACT" &>/dev/null; then exit 0; fi
input=$(cat)
printf '%s' "$input" | "$COMPACT" promptsubmit 2>/dev/null
```

Make them executable:
```bash
chmod +x crates/crabcc-compact/hooks/claude-posttooluse.sh
chmod +x crates/crabcc-compact/hooks/claude-promptsubmit.sh
```

- [ ] **Step 2: Implement setup and uninstall in main.rs**

Replace `cmd_setup` and `cmd_uninstall` stubs:

```rust
fn cmd_setup(args: &[String]) -> Result<()> {
    let host = args.iter()
        .find_map(|a| a.strip_prefix("--host="))
        .unwrap_or("claude-code");

    match host {
        "claude-code" => setup_claude_code()?,
        _ => anyhow::bail!("unknown host: {host}. Supported: claude-code"),
    }
    println!("hooks registered for {host}. Restart the CLI to pick them up.");
    Ok(())
}

fn setup_claude_code() -> Result<()> {
    let home = std::env::var("HOME")?;
    let settings_path = std::path::PathBuf::from(&home)
        .join(".claude")
        .join("settings.json");

    let raw = if settings_path.exists() {
        std::fs::read_to_string(&settings_path)?
    } else {
        "{}".to_string()
    };

    let mut settings: serde_json::Map<String, Value> =
        serde_json::from_str(&raw).unwrap_or_default();

    let hooks = settings.entry("hooks").or_insert_with(|| Value::Object(Default::default()));
    let hooks = hooks.as_object_mut().unwrap();

    let hook_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string()))
        .join("hooks");

    let posttooluse_script = hook_dir.join("claude-posttooluse.sh")
        .to_string_lossy().to_string();
    let promptsubmit_script = hook_dir.join("claude-promptsubmit.sh")
        .to_string_lossy().to_string();

    hooks.insert("PostToolUse".to_string(), serde_json::json!([{
        "hooks": [{"type": "command", "command": posttooluse_script}]
    }]));
    hooks.insert("UserPromptSubmit".to_string(), serde_json::json!([{
        "hooks": [{"type": "command", "command": promptsubmit_script}]
    }]));

    std::fs::create_dir_all(settings_path.parent().unwrap())?;
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    println!("wrote hooks to {}", settings_path.display());
    Ok(())
}

fn cmd_uninstall(args: &[String]) -> Result<()> {
    let host = args.iter()
        .find_map(|a| a.strip_prefix("--host="))
        .unwrap_or("claude-code");
    match host {
        "claude-code" => {
            let home = std::env::var("HOME")?;
            let settings_path = std::path::PathBuf::from(home)
                .join(".claude").join("settings.json");
            if !settings_path.exists() { return Ok(()); }
            let raw = std::fs::read_to_string(&settings_path)?;
            let mut settings: serde_json::Map<String, Value> =
                serde_json::from_str(&raw).unwrap_or_default();
            if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                hooks.remove("PostToolUse");
                hooks.remove("UserPromptSubmit");
            }
            std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
            println!("removed hooks from {}", settings_path.display());
        }
        _ => anyhow::bail!("unknown host: {host}"),
    }
    Ok(())
}
```

- [ ] **Step 3: Build**

```bash
cargo build -p crabcc-compact
```

- [ ] **Step 4: Commit**

```bash
git add crates/crabcc-compact/hooks/ crates/crabcc-compact/src/main.rs
git commit -m "feat(compact): hook scripts + setup/uninstall for claude-code"
```

---

### Task 13: MCP/SSE server

**Files:**
- Modify: `crates/crabcc-compact/src/mcp/server.rs`
- Modify: `crates/crabcc-compact/src/mcp/tools.rs`
- Modify: `crates/crabcc-compact/src/mcp/mod.rs`
- Modify: `crates/crabcc-compact/src/main.rs` (run\_mcp)

The MCP SSE transport: client connects to `GET /sse` (receives SSE stream), sends requests to `POST /message?sessionId=<id>`. We use tokio + manual TCP for a minimal implementation with no additional web framework dep. Default port 3456.

- [ ] **Step 1: mcp/tools.rs — tool definitions**

```rust
use serde_json::{json, Value};
use crate::{client, config, gate, economy::Budget};

pub struct ToolResult {
    pub content: Value,
    pub is_error: bool,
}

pub fn list_tools() -> Value {
    json!({
        "tools": [
            {
                "name": "compact.compress",
                "description": "Compress a text payload via LLMLingua-2 on the tailnet node.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "description": "Text to compress"},
                        "ratio": {"type": "number", "description": "Target compression ratio (0.0-1.0, default 0.5)"}
                    },
                    "required": ["text"]
                }
            },
            {
                "name": "compact.enrich",
                "description": "Enrich compressed code context with a structured attack plan.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "query": {"type": "string", "description": "The user's task/question"}
                    },
                    "required": ["text", "query"]
                }
            },
            {
                "name": "compact.status",
                "description": "Check the tailnet compact-server health.",
                "inputSchema": {"type": "object", "properties": {}}
            },
            {
                "name": "compact.economy",
                "description": "Return session token savings stats.",
                "inputSchema": {"type": "object", "properties": {}}
            }
        ]
    })
}

pub fn call_tool(name: &str, params: &Value) -> ToolResult {
    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => return err(format!("config error: {e}")),
    };

    match name {
        "compact.compress" => {
            let text = match params.get("text").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return err("missing 'text' parameter"),
            };
            let ratio = params.get("ratio").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32;
            match client::compact(&cfg.endpoint, text, ratio, cfg.timeout_ms) {
                Ok(r) => ToolResult {
                    content: json!({
                        "compressed": r.compressed,
                        "original_tokens": r.original_tokens,
                        "compressed_tokens": r.compressed_tokens,
                        "ratio": r.compressed_tokens as f32 / r.original_tokens.max(1) as f32
                    }),
                    is_error: false,
                },
                Err(e) => err(format!("compact failed: {e}")),
            }
        }
        "compact.enrich" => {
            let text = match params.get("text").and_then(|v| v.as_str()) {
                Some(t) => t,
                None => return err("missing 'text' parameter"),
            };
            let query = match params.get("query").and_then(|v| v.as_str()) {
                Some(q) => q,
                None => return err("missing 'query' parameter"),
            };
            match client::enrich(&cfg.endpoint, text, query, cfg.timeout_ms) {
                Ok(r) => ToolResult { content: json!({"plan": r.plan}), is_error: false },
                Err(e) => err(format!("enrich failed: {e}")),
            }
        }
        "compact.status" => {
            match client::health(&cfg.endpoint, cfg.timeout_ms) {
                Ok(v) => ToolResult { content: v, is_error: false },
                Err(e) => err(format!("unreachable: {e}")),
            }
        }
        "compact.economy" => {
            let b = Budget::new();
            ToolResult {
                content: json!({
                    "tokens_saved": b.tokens_saved(),
                    "calls": b.calls,
                    "dedup_hits": b.dedup_hits
                }),
                is_error: false,
            }
        }
        _ => err(format!("unknown tool: {name}")),
    }
}

fn err(msg: impl Into<String>) -> ToolResult {
    ToolResult { content: json!({"error": msg.into()}), is_error: true }
}
```

- [ ] **Step 2: mcp/server.rs — minimal SSE MCP server**

```rust
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use super::tools;

pub fn run(port: u16) -> anyhow::Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)?;
    println!("crabcc-compact MCP server listening on http://{addr}");
    println!("SSE endpoint: http://{addr}/sse");
    println!("Wire with: claude mcp add --transport sse compact http://{addr}/sse");

    // Channel map: session_id -> Sender for routing POST /message responses to SSE stream
    let sessions: Arc<Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<String>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    for stream in listener.incoming() {
        let stream = match stream { Ok(s) => s, Err(_) => continue };
        let sessions = Arc::clone(&sessions);
        thread::spawn(move || {
            if let Err(e) = handle_connection(stream, sessions) {
                let _ = e; // ignore per-connection errors
            }
        });
    }
    Ok(())
}

fn handle_connection(
    mut stream: std::net::TcpStream,
    sessions: Arc<Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<String>>>>,
) -> anyhow::Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    let mut lines = reader.lines();
    lines.next().map(|l| l.map(|s| request_line = s));

    // Consume headers
    let mut content_length = 0usize;
    let mut raw_headers = vec![];
    for line in lines.by_ref() {
        let line = line?;
        if line.is_empty() { break; }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length: ") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        raw_headers.push(line);
    }

    if request_line.starts_with("GET /sse") {
        handle_sse(stream, sessions)?;
    } else if request_line.starts_with("POST /message") {
        let session_id = extract_session_id(&request_line);
        // Read body
        let mut body = vec![0u8; content_length];
        use std::io::Read;
        stream.read_exact(&mut body)?;
        let body = String::from_utf8_lossy(&body).to_string();
        handle_message(&mut stream, &session_id, &body, sessions)?;
    } else {
        stream.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n")?;
    }
    Ok(())
}

fn handle_sse(
    mut stream: std::net::TcpStream,
    sessions: Arc<Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<String>>>>,
) -> anyhow::Result<()> {
    let session_id = format!("s{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    sessions.lock().unwrap().insert(session_id.clone(), tx);

    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
    )?;

    // Send the endpoint event so the MCP client knows where to POST
    let endpoint_event = format!("event: endpoint\ndata: /message?sessionId={session_id}\n\n");
    stream.write_all(endpoint_event.as_bytes())?;

    // Stream JSON-RPC responses back to the client
    for msg in rx {
        let event = format!("event: message\ndata: {msg}\n\n");
        if stream.write_all(event.as_bytes()).is_err() { break; }
    }
    sessions.lock().unwrap().remove(&session_id);
    Ok(())
}

fn handle_message(
    stream: &mut std::net::TcpStream,
    session_id: &str,
    body: &str,
    sessions: Arc<Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<String>>>>,
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_str(body).unwrap_or(json!(null));
    let id = req.get("id").cloned().unwrap_or(json!(null));
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    let response = match method {
        "initialize" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "crabcc-compact", "version": env!("CARGO_PKG_VERSION")}
            }
        }),
        "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": tools::list_tools()}),
        "tools/call" => {
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let tool_params = params.get("arguments").cloned().unwrap_or(json!({}));
            let result = tools::call_tool(tool_name, &tool_params);
            json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "content": [{"type": "text", "text": serde_json::to_string(&result.content)?}],
                    "isError": result.is_error
                }
            })
        }
        _ => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}),
    };

    let resp_str = serde_json::to_string(&response)?;

    // Route via SSE channel if session exists, else return in HTTP response body
    let sent_via_sse = {
        let guard = sessions.lock().unwrap();
        if let Some(tx) = guard.get(session_id) {
            tx.send(resp_str.clone()).is_ok()
        } else {
            false
        }
    };

    if !sent_via_sse {
        stream.write_all(
            format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{resp_str}",
                resp_str.len()).as_bytes()
        )?;
    } else {
        stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")?;
    }
    Ok(())
}

fn extract_session_id(request_line: &str) -> String {
    request_line
        .split_whitespace().nth(1).unwrap_or("")
        .split('?').nth(1).unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("sessionId=").map(|s| s.to_string()))
        .unwrap_or_default()
}
```

`mcp/mod.rs`:

```rust
pub mod server;
pub mod tools;
```

- [ ] **Step 3: Wire run\_mcp in main.rs**

Replace the `run_mcp` stub:

```rust
fn run_mcp(args: &[String]) -> Result<()> {
    let port: u16 = args.iter()
        .find_map(|a| a.strip_prefix("--port=").and_then(|p| p.parse().ok()))
        .unwrap_or(3456);
    mcp::server::run(port)?;
    Ok(())
}
```

- [ ] **Step 4: Build**

```bash
cargo build -p crabcc-compact
```

- [ ] **Step 5: Smoke test MCP server starts**

```bash
./target/debug/crabcc-compact --mcp --port=3456 &
sleep 1
curl -s http://127.0.0.1:3456/sse &
sleep 0.5
curl -s -X POST http://127.0.0.1:3456/message \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
kill %1 %2 2>/dev/null || true
```
Expected: JSON response listing the 4 compact.* tools.

- [ ] **Step 6: Commit**

```bash
git add crates/crabcc-compact/src/mcp/
git commit -m "feat(compact): MCP/SSE server — 4 tools (compress/enrich/status/economy)"
```

---

## Phase 2: Python Server (compact-server)

### Task 14: compact-server scaffold

**Files:**
- Create: `compact-server/pyproject.toml`
- Create: `compact-server/server.py` (skeleton)
- Create: `compact-server/compress.py` (skeleton)
- Create: `compact-server/enrich.py` (skeleton)
- Create: `compact-server/deploy/install.sh`

- [ ] **Step 1: Create pyproject.toml**

`compact-server/pyproject.toml`:

```toml
[project]
name = "compact-server"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = [
    "fastapi>=0.111",
    "uvicorn[standard]>=0.29",
    "llmlingua>=0.2.2",
    "mlx-lm>=0.14",
]

[project.scripts]
compact-server = "server:main"
```

- [ ] **Step 2: Create server.py skeleton**

`compact-server/server.py`:

```python
import time
import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

import compress
import enrich as enrich_mod

app = FastAPI(title="compact-server")
_start = time.time()

class CompactRequest(BaseModel):
    text: str
    ratio: float = 0.5

class CompactResponse(BaseModel):
    compressed: str
    original_tokens: int
    compressed_tokens: int

class EnrichRequest(BaseModel):
    text: str
    query: str

class EnrichResponse(BaseModel):
    plan: str

@app.post("/compact", response_model=CompactResponse)
def route_compact(req: CompactRequest) -> CompactResponse:
    result = compress.compact(req.text, req.ratio)
    return CompactResponse(**result)

@app.post("/enrich", response_model=EnrichResponse)
def route_enrich(req: EnrichRequest) -> EnrichResponse:
    plan = enrich_mod.enrich(req.text, req.query)
    return EnrichResponse(plan=plan)

@app.get("/health")
def route_health():
    return {
        "status": "ok",
        "models": compress.loaded_models() + enrich_mod.loaded_models(),
        "uptime_s": int(time.time() - _start),
    }

def main():
    uvicorn.run("server:app", host="0.0.0.0", port=8080, reload=False)

if __name__ == "__main__":
    main()
```

- [ ] **Step 3: Build venv and verify imports**

```bash
cd compact-server
python3 -m venv .venv
source .venv/bin/activate
pip install -e .
python3 -c "import fastapi, uvicorn; print('ok')"
deactivate
cd ..
```
Expected: `ok`

- [ ] **Step 4: Commit scaffold**

```bash
git add compact-server/pyproject.toml compact-server/server.py
git commit -m "feat(compact-server): FastAPI server scaffold"
```

---

### Task 15: compress.py — LLMLingua-2 wrapper

**Files:**
- Modify: `compact-server/compress.py`

LLMLingua-2 is a token-classification model (XLM-RoBERTa) from Microsoft. It runs a single forward pass — no autoregressive generation. The `llmlingua` package provides `PromptCompressor`.

- [ ] **Step 1: Write failing pytest + implementation**

`compact-server/compress.py`:

```python
from __future__ import annotations
import functools

_MODEL_NAME = "microsoft/llmlingua-2-xlm-roberta-large-for-general-compression"

@functools.lru_cache(maxsize=1)
def _compressor():
    from llmlingua import PromptCompressor
    return PromptCompressor(
        model_name=_MODEL_NAME,
        use_llmlingua2=True,
        device_map="mps",  # Apple Silicon; change to "cuda" or "cpu" as needed
    )

def compact(text: str, ratio: float = 0.5) -> dict:
    """Compress text. Returns dict with compressed, original_tokens, compressed_tokens."""
    compressor = _compressor()
    result = compressor.compress_prompt(
        text,
        rate=ratio,
        force_tokens=["\n"],       # preserve newlines for code structure
        chunk_end_tokens=[".", "!", "?", "\n"],
    )
    original_tokens = _estimate_tokens(text)
    compressed_tokens = _estimate_tokens(result["compressed_prompt"])
    return {
        "compressed": result["compressed_prompt"],
        "original_tokens": original_tokens,
        "compressed_tokens": compressed_tokens,
    }

def loaded_models() -> list[str]:
    if _compressor.cache_info().currsize > 0:
        return [_MODEL_NAME]
    return []

def _estimate_tokens(text: str) -> int:
    return max(1, len(text) // 4)
```

`compact-server/tests/test_compress.py`:

```python
import pytest

def test_compact_reduces_token_count():
    from compress import compact
    # Long repetitive text that LLMLingua-2 can compress well
    text = "def handle_request(req):\n    return req.body()\n" * 40
    result = compact(text, ratio=0.5)
    assert "compressed" in result
    assert result["compressed_tokens"] < result["original_tokens"]
    assert len(result["compressed"]) > 0

def test_compact_returns_required_keys():
    from compress import compact
    result = compact("hello world " * 600, ratio=0.5)
    assert set(result.keys()) == {"compressed", "original_tokens", "compressed_tokens"}

def test_compact_preserves_some_content():
    from compress import compact
    text = "fn authenticate_user(token: &str) -> bool {\n    validate_jwt(token)\n}\n" * 40
    result = compact(text, ratio=0.5)
    # Extractive — must contain substrings from original
    assert any(word in result["compressed"] for word in ["authenticate", "token", "fn"])
```

- [ ] **Step 2: Run tests** (requires venv with llmlingua installed and model downloaded ~500MB on first run)

```bash
cd compact-server
source .venv/bin/activate
pytest tests/test_compress.py -v
deactivate
cd ..
```
Expected: 3 tests pass. First run downloads the model — allow ~2 min.

- [ ] **Step 3: Commit**

```bash
git add compact-server/compress.py compact-server/tests/
git commit -m "feat(compact-server): LLMLingua-2 compress wrapper"
```

---

### Task 16: enrich.py — lazy-loaded enricher model

**Files:**
- Modify: `compact-server/enrich.py`

The enricher model (Qwen2.5-14B-Instruct or DeepSeek-R1-Distill-Qwen-14B via MLX) generates a structured Markdown attack plan from compressed code + user query. Lazy-loaded on first call.

- [ ] **Step 1: Write implementation**

`compact-server/enrich.py`:

```python
from __future__ import annotations
import functools

_MODEL_ID = "Qwen/Qwen2.5-14B-Instruct-4bit"
_MAX_TOKENS = 512

SYSTEM_PROMPT = """You are a senior software engineer acting as a Planner.
You receive:
1. COMPRESSED CONTEXT — relevant code, compressed for brevity.
2. TASK — the engineer's question or objective.

Output a tight Markdown checklist (max 8 items) mapping the task to specific
locations in the context. Be concrete: name files, functions, line patterns.
No prose. Just the checklist."""

@functools.lru_cache(maxsize=1)
def _model_and_tokenizer():
    from mlx_lm import load
    return load(_MODEL_ID)

def enrich(text: str, query: str) -> str:
    """Generate a structured attack plan for the given compressed context + query."""
    model, tokenizer = _model_and_tokenizer()
    from mlx_lm import generate

    prompt = (
        f"COMPRESSED CONTEXT:\n```\n{text[:8000]}\n```\n\n"
        f"TASK: {query}\n\n"
        "Write the attack plan checklist:"
    )
    messages = [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": prompt},
    ]
    formatted = tokenizer.apply_chat_template(
        messages, tokenize=False, add_generation_prompt=True
    )
    response = generate(model, tokenizer, prompt=formatted, max_tokens=_MAX_TOKENS)
    return response.strip()

def loaded_models() -> list[str]:
    if _model_and_tokenizer.cache_info().currsize > 0:
        return [_MODEL_ID]
    return []
```

`compact-server/tests/test_enrich.py`:

```python
import pytest

def test_enrich_returns_nonempty_string():
    from enrich import enrich
    code = "fn handle_auth(token: &str) -> bool { validate_jwt(token) }\n" * 10
    plan = enrich(code, "add rate limiting")
    assert isinstance(plan, str)
    assert len(plan) > 10

def test_enrich_returns_markdown_checklist():
    from enrich import enrich
    code = "def authenticate(token):\n    return verify(token)\n" * 10
    plan = enrich(code, "add token expiry check")
    # Attack plan should contain at least one checklist marker
    assert any(marker in plan for marker in ["- [ ]", "- [x]", "1.", "* "])
```

Note: these tests download the 14B model (~8GB) on first run. Skip in CI with `pytest -k "not test_enrich"` unless the model cache is populated.

- [ ] **Step 2: Run tests (model download required)**

```bash
cd compact-server
source .venv/bin/activate
pytest tests/test_enrich.py -v -s
deactivate
cd ..
```

- [ ] **Step 3: Commit**

```bash
git add compact-server/enrich.py compact-server/tests/test_enrich.py
git commit -m "feat(compact-server): enricher model wrapper (Qwen2.5-14B, lazy-load)"
```

---

### Task 17: Full server integration + deploy script

**Files:**
- Verify: `compact-server/server.py` (already complete from Task 14)
- Create: `compact-server/deploy/install.sh`
- Create: `compact-server/tests/test_server.py`

- [ ] **Step 1: Write integration tests for the FastAPI routes**

`compact-server/tests/test_server.py`:

```python
import pytest
from fastapi.testclient import TestClient
from server import app

client = TestClient(app)

def test_health_returns_ok():
    resp = client.get("/health")
    assert resp.status_code == 200
    data = resp.json()
    assert data["status"] == "ok"
    assert "uptime_s" in data
    assert isinstance(data["models"], list)

def test_compact_route_returns_valid_shape():
    text = "fn foo() -> i32 { 42 }\n" * 600  # ~2400 tokens
    resp = client.post("/compact", json={"text": text, "ratio": 0.5})
    assert resp.status_code == 200
    data = resp.json()
    assert "compressed" in data
    assert data["compressed_tokens"] < data["original_tokens"]

def test_compact_route_rejects_missing_text():
    resp = client.post("/compact", json={"ratio": 0.5})
    assert resp.status_code == 422

def test_enrich_route_returns_plan():
    code = "def auth(token):\n    return verify(token)\n" * 10
    resp = client.post("/enrich", json={"text": code, "query": "add expiry"})
    assert resp.status_code == 200
    assert "plan" in resp.json()
    assert len(resp.json()["plan"]) > 0
```

- [ ] **Step 2: Run server tests**

```bash
cd compact-server
source .venv/bin/activate
pytest tests/test_server.py -v
deactivate
cd ..
```
Expected: 4 tests pass.

- [ ] **Step 3: Create deploy/install.sh**

`compact-server/deploy/install.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

# Deploy compact-server to the current machine.
# Run on the tailnet node where models will live.
INSTALL_DIR="${COMPACT_INSTALL_DIR:-$HOME/.local/compact-server}"
SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${COMPACT_PORT:-8080}"

echo "Installing compact-server to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
cp -r "$SCRIPT_DIR"/{server.py,compress.py,enrich.py,pyproject.toml} "$INSTALL_DIR/"

# Create venv
python3 -m venv "$INSTALL_DIR/.venv"
"$INSTALL_DIR/.venv/bin/pip" install -e "$INSTALL_DIR" --quiet

# Pre-download LLMLingua-2 model (required; ~500MB)
echo "Downloading LLMLingua-2 model (first run only)..."
"$INSTALL_DIR/.venv/bin/python3" -c "
from llmlingua import PromptCompressor
PromptCompressor('microsoft/llmlingua-2-xlm-roberta-large-for-general-compression', use_llmlingua2=True, device_map='cpu')
print('LLMLingua-2 ready')
"

# Install launchd plist (macOS) or systemd unit (Linux)
if [[ "$(uname)" == "Darwin" ]]; then
    PLIST="$HOME/Library/LaunchAgents/cc.crabcc.compact-server.plist"
    cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>       <string>cc.crabcc.compact-server</string>
    <key>ProgramArguments</key>
    <array>
        <string>$INSTALL_DIR/.venv/bin/python3</string>
        <string>$INSTALL_DIR/server.py</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>COMPACT_PORT</key><string>$PORT</string>
    </dict>
    <key>RunAtLoad</key>   <true/>
    <key>KeepAlive</key>   <true/>
    <key>StandardOutPath</key> <string>$HOME/.crabcc/compact-server.log</string>
    <key>StandardErrorPath</key> <string>$HOME/.crabcc/compact-server.err</string>
</dict>
</plist>
PLIST
    launchctl load "$PLIST"
    echo "Loaded launchd service. compact-server running on port $PORT"
else
    UNIT="$HOME/.config/systemd/user/compact-server.service"
    mkdir -p "$(dirname "$UNIT")"
    cat > "$UNIT" <<UNIT
[Unit]
Description=crabcc compact-server
After=network.target

[Service]
ExecStart=$INSTALL_DIR/.venv/bin/python3 $INSTALL_DIR/server.py
Environment=COMPACT_PORT=$PORT
Restart=always
StandardOutput=append:$HOME/.crabcc/compact-server.log
StandardError=append:$HOME/.crabcc/compact-server.err

[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload
    systemctl --user enable --now compact-server
    echo "Enabled systemd user service. compact-server running on port $PORT"
fi
```

```bash
chmod +x compact-server/deploy/install.sh
```

- [ ] **Step 4: Commit**

```bash
git add compact-server/tests/test_server.py compact-server/deploy/
git commit -m "feat(compact-server): server integration tests + deploy script"
```

---

## Phase 3: Integration

### Task 18: End-to-end integration test

**Files:**
- No new files — uses `crabcc compact test` command implemented in Task 11.

This task verifies the full stack: Rust binary → tailnet → Python server → LLMLingua-2.

- [ ] **Step 1: Deploy compact-server to your tailnet node**

On the remote node:
```bash
# Copy compact-server/ to the remote node
scp -r compact-server/ user@tailnet-node:~/compact-server/
ssh user@tailnet-node "bash ~/compact-server/deploy/install.sh"
```

- [ ] **Step 2: Configure the local client**

```bash
mkdir -p ~/.config/crabcc
cat > ~/.config/crabcc/compact.toml <<'EOF'
endpoint = "http://tailnet-node:8080"  # replace with your tailnet hostname
threshold_tokens = 2000
timeout_ms = 8000
enrich_trigger = "!e"
EOF
```

- [ ] **Step 3: Verify connectivity**

```bash
./target/release/crabcc-compact status
```
Expected: `ok — {"status":"ok","models":["microsoft/llmlingua-2-..."],"uptime_s":N}`

- [ ] **Step 4: Run the synthetic payload test**

```bash
cargo build -p crabcc-compact --release
./target/release/crabcc-compact test
```
Expected: output like `ok — 3040 → 1520 tokens (50.0%) in 1842ms`

- [ ] **Step 5: Register hooks for Claude Code**

```bash
./target/release/crabcc-compact setup --host=claude-code
```
Expected: `hooks registered for claude-code. Restart the CLI to pick them up.`

Restart Claude Code. Open a large file (>8K chars) with the Read tool and verify the tool output in the conversation is compressed.

- [ ] **Step 6: Test MCP wiring**

```bash
./target/release/crabcc-compact --mcp --port=3456 &
claude mcp add --transport sse compact http://localhost:3456/sse
# In Claude Code: use the compact.compress tool with a text argument
kill %1
```

- [ ] **Step 7: Final build + lint**

```bash
cargo build -p crabcc-compact --release
cargo test -p crabcc-compact
cargo clippy -p crabcc-compact -- -D warnings
```
Expected: all tests pass, no clippy warnings.

- [ ] **Step 8: Commit**

```bash
git add .
git commit -m "feat(compact): full integration verified — hook + MCP wiring"
```

---

## Quick Reference

| Command | What |
|---|---|
| `crabcc-compact posttooluse` | PostToolUse hook (stdin → stdout) |
| `crabcc-compact promptsubmit` | UserPromptSubmit hook (stdin → stdout) |
| `crabcc-compact status` | Health check against configured endpoint |
| `crabcc-compact test` | Synthetic 3K-token payload test |
| `crabcc-compact economy` | Show log tail |
| `crabcc-compact setup --host=claude-code` | Register hooks in ~/.claude/settings.json |
| `crabcc-compact uninstall --host=claude-code` | Remove hooks |
| `crabcc-compact --mcp --port=3456` | MCP/SSE server |
| `claude mcp add --transport sse compact http://localhost:3456/sse` | Wire MCP |
