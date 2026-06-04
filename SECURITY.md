# Security policy

## Reporting a vulnerability

Please **do not** open a public issue for security reports. Use **[GitHub Security Advisories](https://github.com/peterlodri-sec/crabcc/security/advisories/new)** so the report stays private until a fix is ready.

What to include:

- A description of the issue and the impact (RCE? data exfiltration? denial of service?).
- A minimal reproduction — exact commands, inputs, and observed behavior.
- Affected versions if known (`crabcc --version`).
- Optional: a suggested fix or mitigation.

## What gets reviewed

| Status | Component |
|---|---|
| In scope | the `crabcc` binary and all crates in this workspace, the MCP server (`crabcc --mcp`), the HITL/Telegram agent (`apps/crabcc-hitl-agent`), the viz dashboard (`crates/crabcc-viz/web`). |
| In scope | the `.devcontainer/` and CI workflows in `.github/workflows/`. |
| Out of scope | bugs in dependencies (please report upstream — but flag us so we can pin around them). |
| Out of scope | self-inflicted misuse (running `crabcc` against an untrusted repo with `--effort max` and complaining about RCE in evaluated code). |

## Response timeline

- **Acknowledgement** — within 5 working days.
- **Triage + severity** — within 10 working days.
- **Fix + advisory** — depends on severity. Critical issues get patched on the next minor release; lower-severity issues are bundled into the regular release cadence.

## Known posture

- Network: by default, crabcc binds to `127.0.0.1` only. The relay components (`crabcc-hitl-agent`, the MCP HTTP transport when enabled) require explicit configuration to expose ports.
- Secrets: the bootstrap script reads from `~/.config/crabcc/secrets.env` (chmod 600). No secrets are committed; `.env` files in apps/ are git-ignored.
- Dependencies: weekly Dependabot scans (see [`.github/dependabot.yml`](.github/dependabot.yml)) plus periodic `cargo audit` and `cargo deny` runs in CI.
