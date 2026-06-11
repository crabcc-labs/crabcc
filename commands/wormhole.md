---
description: Bootstrap, pair, and operate the wormhole operator control channel.
---

Wormhole is the E2EE operator control channel for crabcc. See `docs/WORMHOLE.md`
for the full spec.

## Subcommands

### pair (node side)
Run on the remote node to generate a one-time pairing code:
```bash
crabcc wormhole pair
```
Prints `<nameplate>-<word>-<word>-<word>` (3-word PGP wordlist, ~34 bits entropy).
Code expires in 120s and is single-use.

### pair (operator side)
Enter the code from the node:
```bash
crabcc wormhole pair --code <nameplate>-<word>-<word>-<word>
```

### attach
Attach to a running session on a paired node:
```bash
crabcc wormhole attach <node_id_hex>
crabcc wormhole attach <node_id_hex> --session <session_id_hex>
```

### list
Show paired nodes and their current presence status:
```bash
crabcc wormhole list
```

### sessions
Show active/recent sessions on a node:
```bash
crabcc wormhole sessions <node_id_hex>
```

### redshift
Re-mint the biscuit for a paired node without re-pairing. TTL resets to +1h.
Fired automatically every 50 min by the operator refresh loop; call manually
when you want to revoke-and-reissue immediately:

```bash
crabcc wormhole redshift <node_id_hex>
crabcc wormhole redshift --all          # refresh every paired node
```

Output:
```
dev-cx53   token refreshed   expires in 60m   seq 1024
```

### lensing
Connection diagnostics. Sends path probes at 5 payload sizes and prints RTT,
jitter, and channel state — a traceroute for the wormhole session:

```bash
crabcc wormhole lensing <node_id_hex>
crabcc wormhole lensing <node_id_hex> --rounds 10   # default 5
```

Example output:
```
node       dev-cx53  (a1b2c3d4)
session    cafebabe  (active 4h 23m)
route      relay  ->  relay.crabcc.app:443
token      valid, expires in 47m  (redshift in 7m)
seq        sent=1024  recv=987  gap=none

path probes (5 rounds):
  payload    p50     p95    jitter
    64 B    12ms    14ms    0.8ms
   256 B    13ms    15ms    0.9ms
     1 KB   13ms    16ms    1.1ms
     4 KB   15ms    18ms    1.4ms
    16 KB   21ms    26ms    2.3ms

relay       0 missed pongs  (last 5m)
ntfy last   connected 4h ago
```

If the relay is the bottleneck (16 KB RTT >> 64 B RTT), the delta is the
relay's buffering overhead. On a Tailscale direct path all sizes should be
within 1–2ms of each other.

### status
Check local wormhole daemon health:
```bash
crabcc wormhole status
```

## Supported targets (wormhole-node runs on all of these)

| Host | Arch | Init | Notes |
|------|------|------|-------|
| hetzner / VMs | x86_64 | systemd | NixOS module `modules/wormhole-node.nix` |
| dev-cx53 | x86_64 | systemd | same NixOS module |
| mbp / Mac | aarch64 | launchd | `install/integrations/os/launchd-wormhole-node.plist` |
| pi | aarch64 | systemd | cross-compiled via nix; `pi.fragment.json` includes wormhole-node |
| omp nodes | aarch64/x86_64 | systemd/launchd | `omp.fragment.json` includes wormhole-node |
| nullclaw | x86_64 | systemd | `nullclaw.fragment.json` includes wormhole-node |

OpenCode is retired (v4.5); not a target.

## Cleanup on remote hosts

The node daemon writes session records to `/tmp/wormhole-<hex8>.session` and
the fallback `~/.crabcc/wormhole-sessions/`. On graceful shutdown (SIGTERM):
- Session records in `/tmp` are removed.
- Records in `~/.crabcc/wormhole-sessions/` are kept for 72h for post-mortem
  analysis, then pruned by the next daemon start.
- The `ReplayLog` in-memory buffer is flushed to disk if the log store is
  file-backed; in-memory-only log is discarded on exit.

Run explicitly:
```bash
crabcc wormhole cleanup          # prune old session records on this host
crabcc wormhole cleanup --all    # also clear relay-side log for this node_id
```

## Security notes

- The `Authorization: Bearer` token for ntfy (§13) is NOT the node identity key.
  Keep them in separate sops entries.
- Never share pairing codes over channels that cross the relay (e.g. do not paste
  the code into a wormhole session itself — use Signal, voice, or physical proximity).
- If the relay is suspected compromised: rotate by re-pairing all nodes. The relay
  holds only encrypted ciphertext and BLAKE3-hashed identities; no plaintext keys.

## Wave status (see docs/WORMHOLE.md §11)

- [x] Wave 1 Task 1: wormhole-proto (types, seq, replay, session record)
- [ ] Wave 1 Task 2: wormhole-relay (axum + blob log)
- [ ] Wave 1 Task 3: NixOS relay module
- [ ] Wave 2–4: node daemon, pairing, operator CLI, auth
