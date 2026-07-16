# CI / CD

GitHub Actions runs the test suite on every push and PR, and publishes a Docker image
whenever a version tag is pushed. Everything lives under [`.github/`](../.github).

## Workflows

| Workflow | Trigger | What it does |
| --- | --- | --- |
| `ci.yml` | push to `main`, any PR | Calls the reusable `checks.yml` suite. |
| `checks.yml` | `workflow_call` | The merge gates (see below). Shared by CI and Release so a release runs the identical checks. |
| `release.yml` | push tag `v*` | Runs `checks.yml`, builds + pushes the multi-arch image to GHCR, then cuts a GitHub Release. |
| `snapshots.yml` | manual (`workflow_dispatch`) | Regenerates + commits the Playwright `-linux.png` baselines in the exact CI environment. |
| `dependabot-auto-merge.yml` | Dependabot PRs | Enables auto-merge so green dependency bumps land on their own. |

### Merge gates (`checks.yml`)

- **Rustfmt** — `cargo fmt --all --check`
- **Clippy** — `cargo clippy --workspace --all-targets --all-features -D warnings`
- **Typecheck** — `svelte-check` (web) + `tsc` (api-tests), after `pnpm gen:client`
- **API e2e tests** — `pnpm test:api` (Playwright driving the real backend, no browser)
- **Web visual tests** — `pnpm test:web` (Playwright screenshot suite), run inside the
  pinned `mcr.microsoft.com/playwright` image so rendering matches the committed baselines
- **Versions** — `scripts/check-versions.sh`: Cargo.toml and package.json must agree, and
  on a tag the tag must match

## Releasing

The image version comes from the git tag, which must match the version declared in
`Cargo.toml` and `package.json`.

```bash
# bump the version in Cargo.toml ([workspace.package]) and package.json to X.Y.Z first
git tag vX.Y.Z
git push origin vX.Y.Z
```

This builds `ghcr.io/<owner>/<repo>` for `linux/amd64` + `linux/arm64`, tags it
`X.Y.Z`, `X.Y`, and `latest`, pushes to GHCR, and publishes a GitHub Release with
auto-generated notes.

> arm64 is built under QEMU emulation on the amd64 runner, so a release build is slow
> (tens of minutes). That is the trade-off for a multi-arch image without dedicated
> arm64 runners.

## Running the image

The image is the "single binary" deployment from the README: one container serves both
the API and the SPA.

```bash
docker run -p 8080:8080 -v sure-data:/data ghcr.io/<owner>/<repo>:latest
```

- `-v sure-data:/data` persists the SQLite database (`DATABASE_URL=sqlite:/data/sure.db`).
- The server binds `0.0.0.0:8080` inside the container (overridable via `BIND_ADDR`).
- Other env vars (`RUST_LOG`, etc.) work exactly as documented in the README.

Build it locally the same way CI does (context is the repo root):

```bash
docker build -f packages/api/Dockerfile -t sure .
```

## One-time repository setup

These need to be done once in the GitHub repo settings after the first push:

1. **Actions permissions** — Settings → Actions → General → Workflow permissions:
   allow GitHub Actions to create/write packages (the release job pushes to GHCR with
   the built-in `GITHUB_TOKEN`) and, for Dependabot auto-merge, "Allow auto-merge" under
   Settings → General.
2. **Branch protection** on `main` — require the CI checks (Rustfmt, Clippy, Typecheck,
   API e2e tests, Web visual tests, Versions) as status checks. Dependabot auto-merge
   relies on these being required.
3. **Bootstrap the Linux screenshot baselines** — the committed baselines are macOS
   (`-darwin.png`); CI runs on Linux and needs `-linux.png`. Run the **Update snapshots**
   workflow once (Actions tab → Update snapshots → Run workflow). It generates the
   baselines in the exact CI environment and commits them, after which the Web visual
   tests gate is green. Re-run it after any intentional UI change.
