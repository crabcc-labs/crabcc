# Workflows

## Linux runner pool

All **Linux** jobs use the Hetzner self-hosted fleet:

```yaml
runs-on: [self-hosted, linux, hetzner]
```

See `install/github-runner/README.md` for registration. If jobs sit in
**Queued** forever, the runner is offline or labels do not match.

**macOS** jobs (`macos-latest`) remain GitHub-hosted until a Mac runner exists.

## Billing note

`ubuntu-latest` jobs bill against GitHub Actions minutes. After moving Linux
workloads to Hetzner, only macOS matrix legs consume hosted minutes.
