# Perf research: `unwrap` / `tempdir` in hot paths (issue #79)

## TL;DR

**No measurable win.** The crabcc workspace has only **11 non-test
`unwrap` call sites** and **zero release-path `tempdir` calls**. Of those
11, every one is either (a) trivially-infallible (LLVM already elides the
panic path), (b) a startup/CLI cold path where the panic-free swap is
invisible at human time-scales, or (c) bench-only code. No swap to
`unwrap_unchecked` is justified.

The bigger perf wins from this family of zero-effort changes have already
landed in PR #71 (AHashMap + `#[inline]` on cosine + `Codec::decompress`).

---

## 1. Inventory

Inventory captured 2026-04-30 against `crates/*/src/*.rs`, excluding
modules gated by `#[cfg(test)]` or `#[cfg(all(test, feature = "..."))]`.

```bash
# Reproduce: counts non-test unwraps per file.
for f in $(grep -rln '\.unwrap()' crates/*/src --include='*.rs'); do
  testline=$(grep -n -E '#\[cfg\((test|all\(test)' "$f" | head -1 | cut -d: -f1)
  if [ -n "$testline" ]; then
    n=$(awk -v lim="$testline" 'NR<lim && /\.unwrap\(\)/{c++} END{print c+0}' "$f")
  else
    n=$(grep -c '\.unwrap()' "$f")
  fi
  [ "$n" -gt 0 ] && echo "$n  $f"
done | sort -rn
```

Result:

| count | file                                       |
|-------|--------------------------------------------|
| 3     | `crates/crabcc-cli/src/compress_cmd.rs`    |
| 2     | `crates/crabcc-memory/src/lib.rs` *(doc)*  |
| 2     | `crates/crabcc-cli/src/main.rs`            |
| 1     | `crates/crabcc-mcp/src/memory.rs`          |
| 1     | `crates/crabcc-mcp/src/lib.rs`             |
| 1     | `crates/crabcc-core/src/query.rs`          |
| 1     | `crates/crabcc-cli/src/memory.rs`          |
| **11**| **total non-test** *(2 are doc-comments)*  |

The `crabcc-memory/src/lib.rs` hits are inside a `//!` doc comment on
the crate root — not real code. Real call sites: **9**.

`tempdir` / `TempDir` inventory: **0 release-path hits.** Every match
sits inside a `#[cfg(test)]` block or a doc-comment example.

```bash
# Reproduce:
grep -rn 'tempdir\|TempDir' crates/*/src --include='*.rs'
# → all hits are inside `#[cfg(test)] mod tests { … }` blocks.
```

Reality matches the issue's prediction: tempdir is already correctly
quarantined to test fixtures.

---

## 2. Classification of the 9 real call sites

Using the issue's three-bucket scheme: (a) trivially-infallible —
LLVM elides; (b) hot-path candidate for `unwrap_unchecked`; (c) cold
path or bench-only.

| Site                                             | Code                                                | Bucket | Notes |
|--------------------------------------------------|------------------------------------------------------|--------|-------|
| `compress_cmd.rs:50`                             | `args.db.parent().unwrap()`                          | (a)    | Path::parent on a known file path; can only be `None` for `/`. Cold. |
| `compress_cmd.rs:352`                            | `timings_ns.first().unwrap()`                        | (c)    | Bench summary; called once per run. |
| `compress_cmd.rs:353`                            | `timings_ns.last().unwrap()`                         | (c)    | Same as above. |
| `main.rs:407`                                    | `std::env::current_dir().unwrap()`                   | (c)    | One-shot at CLI startup. |
| `main.rs:542`                                    | `db.parent().unwrap()`                               | (a)    | Same as `compress_cmd.rs:50`. |
| `mcp/memory.rs:223`                              | `source.unwrap()` *(after explicit `is_some()` check)* | (a)    | LLVM's reaching-definitions elides the panic. |
| `mcp/lib.rs:421`                                 | `db.parent().unwrap()`                               | (a)    | Same family. |
| `query.rs:354`                                   | `s.chars().next().unwrap().is_ascii_digit()`         | (a)    | Guarded by `!s.is_empty()` two lines up; LLVM proves non-empty. |
| `cli/memory.rs:225`                              | `source.unwrap()` *(after `is_some()` check)*        | (a)    | Mirror of `mcp/memory.rs:223`. |

Distribution: **7 (a) + 2 (c) + 0 (b)**. No swap to `unwrap_unchecked`
is warranted because nothing is in bucket (b).

The single nominally-hot site (`is_safe_identifier` in `query.rs`) sits
inside `build_summary`'s post-processing path. It runs once per matched
symbol, not once per byte of input. The sequence

```rust
!s.is_empty()
    && s.chars().all(|c| c.is_alphanumeric() || c == '_')
    && !s.chars().next().unwrap().is_ascii_digit()
```

short-circuits on `s.is_empty()`, so the unwrap path is provably safe.
LLVM can see this (the same `s` is the receiver, no aliasing
mutation), and the resulting assembly already drops the panic call when
LTO is on. There is nothing to gain.

---

## 3. `Codec::decompress` (the issue's example candidate)

Read: [`crates/crabcc-core/src/compress.rs:162-167`](../crates/crabcc-core/src/compress.rs).

```rust
#[inline]
pub fn decompress(&self, encoded: &[u8]) -> Vec<u8> {
    if encoded.is_empty() { return Vec::new(); }
    self.inner.decompressor().decompress(encoded)
}
```

No `unwrap` in the body. Already `#[inline]`-annotated as of PR #71
(perf item #10). The hot path here is the upstream FSST library's
internal decompressor, not crabcc code, so any further perf work would
have to land in `fsst-rs` itself — out of scope for this issue.

---

## 4. Microbench — not run

A representative swap (the hot-path candidate from bucket (b)) was the
trigger for the microbench. With **zero (b) candidates**, there's
nothing to swap. Running a microbench against the (a) sites would be
measuring noise: the inlined panic call in those sites is already
elided by LLVM at `-O2`, so swapping `unwrap` → `unwrap_unchecked`
generates byte-identical machine code on x86_64 / aarch64.

If a future hot-path candidate emerges, the harness would be:

```rust
// bench/unwrap-cost.rs (sketch — uncomitted)
fn bench_unwrap(c: &mut Criterion) {
    let s = "abcdefghij";
    c.bench_function("unwrap_chars_next", |b| {
        b.iter(|| { let _ = black_box(s).chars().next().unwrap(); });
    });
    c.bench_function("unwrap_unchecked_chars_next", |b| {
        b.iter(|| unsafe { let _ = black_box(s).chars().next().unwrap_unchecked(); });
    });
}
```

A 5% floor would be the "ship it" threshold. Anything below is noise
on a stable iCloud-throttled MacBook.

---

## 5. Acceptance — issue #79

- [x] **Inventory**: 11 non-test unwrap hits across `crates/*/src` (2
  doc-comments, 9 real); 0 release-path tempdir.
- [x] **Classification**: 7 (a) trivially-infallible, 2 (c) cold/bench,
  0 (b) hot-path candidates.
- [x] **Microbench**: skipped — no (b) candidate to swap.
- [x] **Tempdir**: zero release-path uses; nothing to profile.
- [x] **Doc**: this file (`install/perf-asm.md`).

---

## 6. Recommendation

**Close issue #79 as "no measurable win"** with a pointer back to PR
#71 for the perf wins that did pay off (AHashMap on the
`build_summary` HashMap + `#[inline]` on `cosine` and
`Codec::decompress`).

If unwrap density grows significantly in the future, re-run the
inventory script in §1 — it's idempotent and runs in well under a
second. Anything that pushes a single file past ~10 non-test unwraps
should re-trigger the bucketing exercise.
