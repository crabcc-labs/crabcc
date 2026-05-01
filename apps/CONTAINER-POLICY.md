# Container image policy

> Canonical reference for every container image this repo produces.
> Phase 1 deliverable of [#195](https://github.com/peterlodri-sec/gabcc/issues/195).
> Update this doc *before* changing a Dockerfile or adding a new one.

## Local Docker daemon: OrbStack

On macOS, **OrbStack** is the recommended Docker daemon — not Docker
Desktop. Reasons:

- arm64-native (matches our [linux/arm64-only](#platform-linuxarm64-only) build target)
- Materially faster cold-start + image build vs Docker Desktop
- Lighter on RAM / battery (no fixed-size linux VM)
- Free for personal use; commercial license available

One-shot install + link:

```bash
task setup-orbstack         # or: bash scripts/setup-orbstack.sh
```

The script is idempotent — installs OrbStack via `brew install --cask
orbstack` if missing, launches it, waits for the docker daemon,
switches `docker context` to `orbstack`, ensures a `buildx` builder
exists, and runs `docker run hello-world` as a smoke check.

Verify the link:

```bash
docker context show         # → orbstack
docker buildx ls            # default builder present
```

If a contributor on Linux runs the same Taskfile targets, they get the
system Docker daemon — no setup script needed.

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

### Platform: linux/arm64 only

The deploy target is **Apple Silicon Mac running Docker Desktop** —
single platform, single arch. We deliberately drop multi-arch manifests
in this revision because:

- Every consumer today (developer Macs, the menubar `Crabcc.app` host)
  is `arm64`.
- Multi-arch builds via QEMU emulation roughly double CI time and
  triple build-stage memory. The cost isn't justified until a
  non-arm64 consumer shows up.
- Apple-Silicon-only is consistent with our build-target choice for
  the Swift app side (`apps/macos/Project.swift` targets arm64-apple-
  macos13.0).

If a `linux/amd64` consumer (e.g. a Linux build runner, an EC2 worker)
ever lands, re-add `--platform linux/amd64,linux/arm64` in the
Taskfile build target and convert this section to "multi-arch
mandatory".

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
| `<semver>` (e.g. `0.1.0`) | No | **The only tag we publish.** Read from `Cargo.toml` workspace version via `scripts/version.sh`. |
| `sha-<7-char>` (e.g. `sha-65963b3`) | No | Per-commit, gc-able. CI debugging only — never in production. |
| `@sha256:<digest>` | No | Content-addressable. Recommended for the strictest deploys (full digest pin). |

> **No `:latest`.** Per follow-up review of #195, we deliberately do not
> publish a movable `:latest` tag. Movable tags invite the failure mode
> where a developer pulls `:latest` and gets a build older than the latest
> semver tag (because someone tagged a release but never re-pushed
> `:latest`). Production deploys MUST pin to a `<semver>` or
> `@sha256:<digest>` — both are immutable and the registry record is
> the single source of truth. This is consistent with the `latest`-is-not-
> "newest" warning in [Kalafatis, "Docker Image Naming and Tagging"
> (dev.to, 2024)](https://dev.to/kalkwst/docker-image-naming-and-tagging-1pg9).

### Image inventory (current state)

| Image | Source | Base | Status |
|---|---|---|---|
| `ghcr.io/peterlodri-sec/crabcc` | `crates/crabcc-cli/Dockerfile` | `gcr.io/distroless/cc-debian12:nonroot` | **Phase 1 (this doc)** |
| `ghcr.io/peterlodri-sec/crabcc-telegram` | `apps/crabcc-telegram/Dockerfile` | `gcr.io/distroless/cc-debian12:nonroot` | **Phase 2 (this PR)** |
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
5. **Full OCI labels** (mandatory). Every image declares the canonical
   `org.opencontainers.image.*` annotation set so registries, signers,
   and SBOM tools have consistent metadata to consume. Build-time
   `ARG`s for `VERSION` / `REVISION` / `CREATED` are populated by the
   `task docker-build-*` target — Dockerfile defaults keep `docker
   build` outside Taskfile self-contained.

   Required label set:

   | Label | Source | Purpose |
   |---|---|---|
   | `org.opencontainers.image.title` | static (image name) | Short human-readable name |
   | `org.opencontainers.image.description` | static | One-line description |
   | `org.opencontainers.image.source` | static (repo URL) | Source-of-truth git URL |
   | `org.opencontainers.image.url` | static | Project landing page |
   | `org.opencontainers.image.documentation` | static | README / docs link |
   | `org.opencontainers.image.licenses` | static (SPDX id) | e.g. `MIT` |
   | `org.opencontainers.image.vendor` | static | Org name (`peterlodri-sec`) |
   | `org.opencontainers.image.authors` | static | Maintainer email |
   | `org.opencontainers.image.version` | `ARG VERSION` (semver from Cargo.toml) | Image semver |
   | `org.opencontainers.image.revision` | `ARG REVISION` (`git rev-parse HEAD`) | Source-of-truth commit SHA |
   | `org.opencontainers.image.created` | `ARG CREATED` (RFC 3339 build time) | Build timestamp (UTC) |
   | `org.opencontainers.image.base.name` | static | Base-image reference |
6. **No mutable `/tmp` writes** in the runtime stage — distroless makes this hard anyway, but worth stating.

## Taskfile targets

Each container has three paired Taskfile targets:

| Target | Purpose |
|---|---|
| `task docker-build-<service>` | Single-arch (linux/arm64) build with `--load` + full OCI label injection |
| `task docker-push-<service>` | Push the semver tag to GHCR (no `:latest`) |
| `task docker-sbom-<service>` | Generate SPDX JSON SBOM via Syft, write to `dist/sbom/<service>-<version>.spdx.json` |

The `crabcc-cli` triple is the canonical reference (see `Taskfile.yml`)
— copy that block when adding a new image.

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

## Further reading

- [Kalafatis, "Docker Image Naming and Tagging"](https://dev.to/kalkwst/docker-image-naming-and-tagging-1pg9)
  — primer on the tagging fundamentals (semver vs git-sha vs `latest`,
  why `latest` is a footgun in team environments). Our policy goes
  further on supply-chain (cosign, SBOM, multi-arch, distroless) but
  the tagging-policy section above is direct lineage.
- [Google distroless image catalog](https://github.com/GoogleContainerTools/distroless#what-is-distroless) — base-image options + when to pick which.
- [Sigstore cosign reference](https://docs.sigstore.dev/cosign/overview) — keyless signing flow used in CI.
- [Anchore SBOM action](https://github.com/anchore/sbom-action) — the SPDX SBOM generator we attach to every published image.
