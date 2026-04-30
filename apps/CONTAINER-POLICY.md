# Container image policy

> Canonical reference for every container image this repo produces.
> Phase 1 deliverable of [#195](https://github.com/peterlodri-sec/gabcc/issues/195).
> Update this doc *before* changing a Dockerfile or adding a new one.

## Default: distroless wherever possible

| Runtime | Base image | When to use |
|---|---|---|
| Static-linked Rust / Go | `gcr.io/distroless/static-debian12:nonroot` | The default. Use musl + static linking when the dep graph allows. |
| Dynamically-linked Rust (glibc, openssl, sqlite-bundled, etc.) | `gcr.io/distroless/cc-debian12:nonroot` | When static-musl trips on native libs. ~30 MB vs ~12 MB for `static`, but no toolchain pain. |
| Node.js services | `gcr.io/distroless/nodejs20-debian12:nonroot` | Pure-JS workers (BullMQ, etc.). Rules out anything that exec's `bash -c`. |
| Python | `gcr.io/distroless/python3-debian12:nonroot` | None today; future-proofing the policy. |

**Always use the `:nonroot` variant.** Distroless ships uid 65532; we never run as root in production.

## Fallback: docker-slim

Use [docker-slim](https://github.com/slimtoolkit/slim) when distroless doesn't fit:

- The runtime needs a shell, `curl`, `jq`, `git`, or other classic Unix tooling at runtime
- A non-distroless base (e.g. `ubuntu:22.04`) is required for OS-level system libs
- An interpreted runtime not covered by distroless (Ruby, Java, etc.)

Pattern:

```bash
docker build -t crabcc-svc:fat .
docker-slim build --target crabcc-svc:fat --tag crabcc-svc:slim
```

Typically yields 80–95 % size reduction. Today **no service in this repo needs docker-slim** — keep it documented for the day one shows up (likely candidate: Chrome-bridge headless harness from #184).

## Image production

### Multi-stage builds (mandatory)

Every Dockerfile in this repo follows the pattern:

```dockerfile
FROM <fat-builder> AS build
# Install toolchain, COPY source, run cargo build / npm ci, etc.

FROM <distroless-runtime> AS runtime
# COPY --from=build only the binary + its runtime data.
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/<name>"]
```

### Multi-arch (mandatory)

Every image ships `linux/amd64` + `linux/arm64` as a single multi-arch manifest. CI uses `docker buildx build --platform linux/amd64,linux/arm64`. Local `task docker-build-*` targets default to the host arch; pass `PLATFORMS="linux/amd64,linux/arm64"` to opt in.

### `.dockerignore` parity (mandatory)

Each Dockerfile sits next to a `.dockerignore` covering:

- `target/` (Rust build artifacts — huge)
- `node_modules/` (Node)
- `.git/` (build context bloat; not needed at runtime)
- `.env`, `*.local.*`, secret files
- `.crabcc/` (per-repo index databases)
- `~/.cargo/` (host cache; never copy in)
- The macOS app surface (`installer/`, `apps/macos/`)

## Image naming

Pattern: `ghcr.io/peterlodri-sec/<image>:<tag>`

### Tag rules

| Tag | Movable | Use |
|---|---|---|
| `<semver>` (e.g. `0.1.0`) | No | Production deploys MUST pin to a semver tag. |
| `latest` | Yes | Dev convenience. Never in production. |
| `sha-<7-char>` (e.g. `sha-65963b3`) | No | Per-commit, gc-able. CI debugging only. |
| `@sha256:<digest>` | No | Content-addressable. Recommended for the strictest deploys. |

### Image inventory (current state)

| Image | Source | Base | Status |
|---|---|---|---|
| `ghcr.io/peterlodri-sec/crabcc` | `crates/crabcc-cli/Dockerfile` | `gcr.io/distroless/cc-debian12:nonroot` | **Phase 1 (this doc)** |
| `ghcr.io/peterlodri-sec/crabcc-telegram` | `apps/crabcc-telegram/Dockerfile` | `gcr.io/distroless/cc-debian12:nonroot` | Phase 2 (planned) |
| `ghcr.io/peterlodri-sec/crabcc-viz` | `crates/crabcc-viz/Dockerfile` | `gcr.io/distroless/cc-debian12:nonroot` | Phase 2 (planned) |
| `ghcr.io/peterlodri-sec/jobs-worker` | `apps/jobs-worker/Dockerfile` | `gcr.io/distroless/nodejs20-debian12:nonroot` | Phase 3 (depends on #170) |
| `ghcr.io/peterlodri-sec/crabcc-docs-api` | `crates/crabcc-docs/Dockerfile` (TBD) | `gcr.io/distroless/cc-debian12:nonroot` | Phase 5 (depends on #172) |

## Supply-chain integrity

### Signing (mandatory in CI)

Every image published to GHCR is **cosign-signed via Sigstore keyless OIDC** — no long-lived private key in repo secrets. The `.github/workflows/release.yml` flow:

```yaml
- uses: sigstore/cosign-installer@v3
- run: |
    cosign sign --yes ghcr.io/peterlodri-sec/${{ matrix.image }}:${{ github.ref_name }}
    cosign sign --yes ghcr.io/peterlodri-sec/${{ matrix.image }}:latest
```

Deploy-time verification:

```bash
cosign verify \
    --certificate-identity-regexp 'https://github.com/peterlodri-sec/crabcc/.github/workflows/release.yml@.+' \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    ghcr.io/peterlodri-sec/<image>:<tag>
```

### SBOM (mandatory in CI)

Every published image gets an SPDX SBOM attached as a GHCR artifact via `anchore/sbom-action`. Snyk / Trivy / Grype consume these in scheduled scans.

### Reproducibility (best-effort)

- Rust 1.79+ honors `SOURCE_DATE_EPOCH` for `cargo build` mtimes; CI passes `SOURCE_DATE_EPOCH=$(git log -1 --format=%ct HEAD)`.
- Node builds are not byte-reproducible; we accept that and rely on lockfile pinning for content reproducibility.

## Dockerfile conventions (style guide)

1. **No `RUN curl | sh`.** Every dep comes from a package manager with pinned hashes (`apt-get install --no-install-recommends -y X=VERSION`, `cargo install --locked`, `npm ci`, `bun install --frozen-lockfile`).
2. **Cache-friendly layering.** Order steps from most-stable to least-stable: base → toolchain install → manifest files → dep build → source → app build. Never `COPY . .` before deps are cached.
3. **Single ARG block.** Declare versions / arch flags at the top. `ARG TARGETARCH` is automatically populated by `buildx`.
4. **`HEALTHCHECK`** for any service that listens on a port. Skip for one-shot CLIs.
5. **`LABEL org.opencontainers.image.*`** for source / version / license metadata. CI fills these in automatically; Dockerfile just declares them.
6. **No mutable `/tmp` writes** in the runtime stage — distroless makes this hard anyway, but worth stating.

## Taskfile targets

Each container has a paired Taskfile target named `docker-build-<service>` and `docker-push-<service>`:

```yaml
docker-build-crabcc:
    desc: Build ghcr.io/peterlodri-sec/crabcc:<version> locally (#195)
    dir: .
    cmds:
        - docker buildx build -f crates/crabcc-cli/Dockerfile -t ghcr.io/peterlodri-sec/crabcc:{{.CRABCC_VERSION}} --load .

docker-push-crabcc:
    desc: Push the locally-built crabcc image to GHCR (#195)
    deps: [docker-build-crabcc]
    cmds:
        - docker push ghcr.io/peterlodri-sec/crabcc:{{.CRABCC_VERSION}}
```

## How to add a new image

1. Read this doc start to finish.
2. Pick the base from the table above (or document why neither distroless nor docker-slim fits).
3. Write `<service-path>/Dockerfile` following the multi-stage pattern.
4. Write `<service-path>/.dockerignore` following the parity rules.
5. Add `task docker-build-<service>` and `task docker-push-<service>` to `Taskfile.yml`.
6. Update the **Image inventory** table above.
7. Add a CI matrix entry to `.github/workflows/release.yml` (when #195 phase 5 lands).
8. Open a PR linking #195 with the diff.

## See also

- [#195](https://github.com/peterlodri-sec/crabcc/issues/195) — issue this policy implements
- [#185](https://github.com/peterlodri-sec/crabcc/issues/185) — CI refactor (phase 4 hosts the actual `release.yml` push step)
- `.tools` — `slim` (docker-slim CLI) and the broader fast-CLI roster
