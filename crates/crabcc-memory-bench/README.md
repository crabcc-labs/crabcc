# `crabcc-memory-bench` — LongMemEval R@k harness

Closes the bench gate for [issue #2](../../../../issues/2). Computes
recall-at-K against a LongMemEval-shaped JSON dataset; exits non-zero
when R@5 falls below the configured threshold (default `0.966`).

## Quick run (synthetic, in-tree)

```bash
cargo run --release -p crabcc-memory-bench
```

Bundled 12-question fixture. Output:

```
n=12, mode=Lexical, granularity=session, R@1=1.000, R@5=1.000, R@10=1.000
PASS: R@5=1.000 ≥ 0.966
```

The fixture is engineered with strong keyword signal so BM25-only
hybrid clears the gate; its job is to gate **regressions** in the
ranking stack, not to validate the headline number on production data.

## Real LongMemEval (450q held-out set)

The harness does not ship the dataset. To pull it:

```bash
mkdir -p crates/crabcc-memory-bench/data
curl -L -o crates/crabcc-memory-bench/data/longmemeval_oracle.json \
  https://huggingface.co/datasets/xiaowu0162/LongMemEval/resolve/main/longmemeval_oracle.json

cargo run --release -p crabcc-memory-bench -- \
  --dataset crates/crabcc-memory-bench/data/longmemeval_oracle.json \
  --output  target/bench/memory-real.ndjson
```

For semantic ranking against the real set, build with the
`memory-embed` feature (downloads ~25 MB MiniLM-L6-v2 ONNX on first
use, cached under `~/.cache/crabcc-memory/`):

```bash
cargo run --release -p crabcc-memory-bench \
  --features crabcc-memory/memory-embed \
  -- --dataset crates/crabcc-memory-bench/data/longmemeval_oracle.json --mode hybrid
```

## Flags

| Flag | Default | Notes |
|---|---|---|
| `--dataset` | (synthetic) | Path to a LongMemEval JSON file. |
| `--output` | `target/bench/memory.ndjson` | NDJSON: per-question rows + a final `{"summary":...}` line. |
| `--mode` | compile-time default | `hybrid` / `lexical` / `vector`. |
| `--k` | `5` | Top-K cutoff used by the gate. |
| `--threshold` | `0.966` | Non-zero exit if `R@k < threshold`. |
| `--granularity` | `session` | `session` (one drawer per session) or `turn` (one drawer per `(user, assistant)` pair). |

## Dataset schema

```jsonc
[
  {
    "question_id": "q1",
    "question": "What did I tell you about ...?",
    "answer": "...",
    "haystack": [
      { "session_id": "s1", "turns": [
        {"role": "user", "content": "..."},
        {"role": "assistant", "content": "..."}
      ]}
    ],
    "answer_session_ids": ["s1"]
  }
]
```

## Output format

NDJSON. Per-question rows like:

```jsonc
{"question_id":"q1","gold":["g1"],"retrieved":["g1","d1a","d1b"],"hit_at":{"1":true,"5":true,"10":true}}
```

…followed by one summary line:

```jsonc
{"summary":{"mode":"Lexical","n":12,"recall_at":{"1":1.0,"5":1.0,"10":1.0}}}
```
