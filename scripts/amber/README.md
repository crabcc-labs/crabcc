# Amber scripting standard

Amber is the project's scripting language. It compiles to bash, is statically typed, and has built-in parallelism via `pid()` / `await()`.

New shell scripts go here as `.ab` files. The compiled `.sh` output is checked in alongside.

## Install

**macOS:**
```bash
brew install amber-lang/amber/amber-lang
```

**NixOS (bench-node):**
```bash
nix-shell -p amber-lang
# or add to environment.systemPackages
```

**Build from source with mold (fastest amber binary):**
```bash
git clone https://github.com/amber-lang/amber && cd amber
RUSTFLAGS="-C linker=mold -C target-cpu=native" cargo build --release
# binary at target/release/amber
```

## Workflow

```bash
# Check types (fast, no compilation):
amber check scripts/amber/foo.ab

# Compile to bash (--minify strips redundant source comments; .ab is canonical):
amber build scripts/amber/foo.ab scripts/amber/foo.sh --minify

# Run directly (compile + exec, no .sh needed):
amber run scripts/amber/foo.ab
```

Always commit the compiled `.sh` alongside the `.ab` so the script is usable on systems without Amber installed.

## Compiler flags

| Flag | Effect | When to use |
|------|--------|-------------|
| `--minify` | Strip embedded source comments from `.sh` output | Always — the `.ab` is the source of truth |
| `--target zsh` | Emit `emulate ksh` preamble for zsh | macOS-only scripts; don't use for checked-in `.sh` (bench-node is bash) |
| `--target bash-3.2` | Maximum portability (old macOS ships bash 3.2) | When targeting macOS system bash |
| `--no-proc bshchk` | Skip bash syntax checker postprocessor | Faster iteration; safe to use since CI runs the compiled `.sh` directly |

## Parallelism pattern

```amber
// Fan out N tasks concurrently, then barrier-wait.
let pids = [] as [Int]
for item in items {
    trust $long_running_cmd "{item}" &$  // & = background
    pids += [pid()]                       // capture job PID
}
trust await(pids)                         // barrier
```

The generated bash uses `cmd &` + `wait "${pids[@]}"` — plain bash, no external deps.

**Benchmark result** (10 × 0.1s tasks):
- Sequential bash: ~1.2s
- Parallel Amber-generated bash: ~0.2s  →  **6x speedup**

## Scripts

| file | description |
|------|-------------|
| `check-tools.ab` | parallel tool checker (replaces part of `check-deps.sh`) |
| `bench-par.ab` | parallel benchmark workload (Amber side of amber-vs-bash comparison) |
| `bench-seq.sh` | sequential baseline (pure bash, no parallelism) |

## Known warnings

`[] as [Int]` / `[] as [Text]` emit "absurd cast" warnings during `amber build`. These are false positives from Amber 0.6.0-alpha's type inference for empty array literals; the `#[allow_absurd_cast]` annotation is only valid on function declarations, not on `main {}`. The generated code is correct.
