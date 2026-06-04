# Signing `ucracc-lsp` releases with cosign

This guide wires up [Sigstore **cosign**](https://docs.sigstore.dev/) for the
`ucracc-lsp` release pipeline so consumers (the Zed extension, agent runners,
CI) can cryptographically verify what they download.

**What gets signed**

| Artifact | Workflow | Default method |
|---|---|---|
| Docker Hub image (`…/ucracc-lsp`) | `release-ucracc-lsp-image.yml` | **keyless** (Sigstore OIDC) |
| Release tarballs (`ucracc-lsp-v*-*.tar.gz`) | `release-ucracc-lsp.yml` | **key-based** (opt-in) |

Both workflows are **inert until you add secrets** — they detect the missing
config and skip cleanly, so nothing breaks before you opt in.

There are two signing models. You can use either or both:

- **Keyless (recommended for the image).** No private key to manage. cosign
  gets a short-lived certificate from Sigstore's Fulcio CA, bound to the
  GitHub Actions OIDC identity (the workflow that ran), and logs it in the
  Rekor transparency log. Verification checks "who signed" (the workflow)
  rather than "which key."
- **Key-based.** A long-lived keypair you generate. Required for the tarball
  `sign-blob` step here, and usable for the image too. You distribute the
  public key; consumers verify against it.

---

## 1. Docker Hub (Pro) — push credentials

The image workflow pushes to Docker Hub. With your **Pro** account:

1. Docker Hub → **Account Settings → Personal access tokens → Generate**.
   - Description: `crabcc-ci`; Permissions: **Read & Write**.
   - Copy the token (shown once).
2. Add the secrets to the repo (CLI or GitHub UI → *Settings → Secrets and
   variables → Actions*):

   ```bash
   gh secret set DOCKERHUB_USERNAME --repo crabcc-labs/crabcc --body 'yourname'
   gh secret set DOCKERHUB_TOKEN    --repo crabcc-labs/crabcc --body 'dckr_pat_…'
   # Optional — override the image name (default: <username>/ucracc-lsp):
   gh secret set DOCKERHUB_IMAGE    --repo crabcc-labs/crabcc --body 'crabcc-labs/ucracc-lsp'
   ```

That alone enables the **keyless-signed** image publish on the next
`ucracc-lsp-v*` tag.

---

## 2. Keyless image signing (no key needed)

Nothing more to configure — `release-ucracc-lsp-image.yml` requests an OIDC
token (`permissions: id-token: write`) and runs `cosign sign --yes <ref>`.

**Verify a pulled image:**

```bash
cosign verify \
  --certificate-identity-regexp 'https://github.com/crabcc-labs/crabcc/\.github/workflows/release-ucracc-lsp-image\.yml@.*' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  crabcc-labs/ucracc-lsp:0.4.0
```

A successful verify proves the image was signed by *that workflow in this
repo* and is recorded in the public Rekor log.

---

## 3. Key-based signing (tarballs, and optionally the image)

### Generate a keypair

```bash
# Pick a strong password when prompted; you'll store it as a secret too.
cosign generate-key-pair
# → cosign.key  (PRIVATE — never commit)
# → cosign.pub  (public — safe to publish/commit)
```

> Tip: `cosign generate-key-pair github://crabcc-labs/crabcc` can write the
> private key + password straight into the repo's Actions secrets for you
> (needs `gh auth` with admin). Otherwise add them manually below.

### Add the secrets

```bash
gh secret set COSIGN_PRIVATE_KEY --repo crabcc-labs/crabcc < cosign.key
gh secret set COSIGN_PASSWORD    --repo crabcc-labs/crabcc --body 'the-password'
```

With these set:
- `release-ucracc-lsp.yml` runs `cosign sign-blob --bundle` on every
  `*.tar.gz` and attaches a `.cosign.bundle` per tarball to the GitHub
  release. (Current cosign emits verification material via `--bundle`; the
  older `--output-signature`/`--output-certificate` flags were removed.)
- `release-ucracc-lsp-image.yml` switches from keyless to key-based image
  signing automatically (it prefers `COSIGN_PRIVATE_KEY` when present).

Commit `cosign.pub` somewhere discoverable (e.g. this repo) so consumers can
verify. **Do not commit `cosign.key`.**

### Verify a tarball

```bash
cosign verify-blob \
  --key cosign.pub \
  --bundle ucracc-lsp-v0.4.0-aarch64-apple-darwin.tar.gz.cosign.bundle \
  ucracc-lsp-v0.4.0-aarch64-apple-darwin.tar.gz
```

### Verify a key-signed image

```bash
cosign verify --key cosign.pub crabcc-labs/ucracc-lsp:0.4.0
```

---

## 4. Cut a signed release

```bash
# version already bumped in crates/ucracc-lsp/Cargo.toml (e.g. 0.4.0)
git tag ucracc-lsp-v0.4.0
git push origin ucracc-lsp-v0.4.0
```

This fires both workflows:
- `release-ucracc-lsp.yml` → builds 3 targets, tarballs + SHA-256 (+ cosign
  `.sig`/`.pem` if keys are set), GitHub release.
- `release-ucracc-lsp-image.yml` → multi-arch Docker image, pushed + cosign
  signed (keyless or key-based).

---

## 5. Operational notes

- **SHA-256 vs cosign.** The `.sha256` files prove *integrity* (the bytes
  weren't corrupted). cosign proves *authenticity* (who produced them) and,
  keyless, adds a public transparency-log record. Keep both.
- **Key hygiene (key-based).** `cosign.key` lives only in repo secrets and
  your offline backup. Rotate by regenerating the pair, updating the
  secrets, and republishing `cosign.pub`.
- **Prefer keyless** where the runner has OIDC (the image job, on
  GitHub-hosted `ubuntu-latest`). The tarball job runs on the self-hosted
  Hetzner runner; key-based avoids depending on its OIDC setup.
- **Nothing leaks if unset.** Both workflows guard on secret presence, so a
  fork or an un-provisioned repo just skips signing/publishing.
