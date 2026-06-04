# sshq

A token-thrifty `ssh` wrapper for AI coding agents. One binary that bakes
in five SSH output/latency optimizations so neither an agent nor a human
has to remember the flag soup. It shells out to the system `ssh` — it is
**not** an SSH implementation.

```
sshq [OPTIONS] <HOST> [COMMAND]...
```

## What it does (and why)

| # | Optimization | How `sshq` applies it |
|---|---|---|
| 1 | **Clean, unstyled output** | `-T` (no PTY) + `-q` so no ANSI repaint sequences or banner reach the agent. |
| 2 | **Remote-side filtering** | `--tail N` / `--grep PAT` / `--count` build a `… 2>&1 \| grep \| tail \| wc -l` pipeline *on the remote*, so only the signal crosses the wire. |
| 3 | **Terse tool output** | Injects `export NO_COLOR=1 TERM=dumb CI=1;` ahead of the command. |
| 4 | **Connection multiplexing** | `ControlMaster=auto` + a short `%C` socket path + `ControlPersist=10m`, wired in by default — no `~/.ssh/config` edits. |
| 5 | **Escape-free scripts** | `--script FILE` (or `-` for stdin) streams a multi-line script to a remote `bash -s`, sidestepping local-shell quoting entirely. |

## Examples

```bash
# Run a build, keep only the last 20 lines, count ERROR lines:
sshq --grep ERROR --tail 20 --count deploy@web1 cargo test

# Pipe a local script to the remote with zero quote-escaping:
sshq --script deploy.sh ci@build7
cat deploy.sh | sshq --script - ci@build7

# See the exact ssh invocation without running it:
sshq --dry-run --tail 5 web1 systemctl status nginx

# Interactive shell (auto: forces -t, drops -q/BatchMode):
sshq db1.internal
```

> **Flags go before the command.** Everything after `<HOST>` is captured
> verbatim as the remote command (same grammar as `ssh HOST cmd …`), so
> `--dry-run` etc. must precede it.

## Escape hatches

- `--tty` — allocate a PTY and allow interactive auth (remote TUIs, passphrase prompts).
- `--color` — skip the `NO_COLOR`/`TERM=dumb`/`CI` injection.
- `--no-mux` — disable multiplexing.
- `--no-merge` — keep stderr separate from stdout before filtering.
- `-o KEY=VALUE` — pass any extra option straight to `ssh` (repeatable).
- `-p PORT`, `--persist DURATION`.

## Caveats

- Targets POSIX remotes (the env-prefix assumes an `sh`-like remote
  shell and `grep`/`tail`/`wc`).
- When a filter (`--tail`/`--grep`/`--count`) is active, the remote
  command runs under `bash -c` and re-exits with `${PIPESTATUS[0]}` so
  the **user command's** exit status propagates — not the trailing
  filter's. (Otherwise `--tail` would mask a failed command and `--grep`
  would report failure on no match.) Requires `bash` on the remote.
- `--script` runs the script under `bash` on the remote.
- This optimizes a **self-hosted local → remote** workflow; it has no
  bearing on hosted environments that don't reach your code over SSH.
