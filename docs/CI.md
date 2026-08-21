# CI / CD

GitHub Actions runs the test suite on every push and PR, and publishes a Docker image
whenever a version tag is pushed. Everything lives under [`.github/`](../.github).

## Workflows

| Workflow | Trigger | What it does |
| --- | --- | --- |
| `ci.yml` | push to `main`, any PR | Calls the reusable `checks.yml` suite. |
| `checks.yml` | `workflow_call` | The merge gates (see below). Shared by CI and Release so a release runs the identical checks. |
| `release.yml` | push tag `v*` | Runs `checks.yml`, builds + pushes the multi-arch image to GHCR, then cuts a GitHub Release. |
| `dependabot-auto-merge.yml` | Dependabot PRs | Enables auto-merge so green dependency bumps land on their own. |

### Merge gates (`checks.yml`)

- **Personal-data scan** — `node scripts/pii-scan.mjs --all` (CLAUDE.md rule 3). First, mirroring
  the pre-commit hook, because it is the cheapest gate and the only one guarding something a
  later gate cannot undo. It is *also* in CI and not only in the hook: a hook is bypassable with
  `--no-verify` and is installed per clone by the `prepare` script, so the hook is the
  convenience and this is the gate.
- **sqlx offline metadata** — `pnpm sqlx:check`. Every query in `sure-dal` is compile-time
  checked against the committed `.sqlx/` directory, and `.cargo/config.toml` pins
  `SQLX_OFFLINE=true` so no build anywhere opens a database to do it. A *missing* entry already
  fails Clippy and Cargo tests (the macro cannot resolve at all); what only this gate catches is
  metadata that no longer agrees with `packages/dal/migrations` — a query edited without
  re-running `pnpm sqlx:prepare`, or a new migration that changes a column a cached query still
  describes the old way. The script applies the migrations to a throwaway database under
  `target/`, never `data/sure.db`.
- **Dependabot config** — `scripts/check-dependabot.sh`: every 0.x direct dependency, crate and
  npm package alike, must be withheld from *minor* bumps in `.github/dependabot.yml`. While a
  major is 0 the minor field is the compatibility boundary, but Dependabot calls such a bump
  minor and auto-merge takes it at its word — so a name missing from that list is a breaking
  change that can land unattended
- **Workspace dependencies** — `node scripts/check-workspace-deps.mjs`: every requirement and
  every feature flag lives in the root `[workspace.dependencies]`, and a member manifest says
  only `foo = { workspace = true }`. Cargo unifies features across a build, so a `features = [..]`
  written in one member is a workspace-wide choice made where nobody looks, and a version written
  in one member can resolve to a second copy of the crate whose types are not the first's. The
  fixer the failure message points at is `cargo autoinherit`; the feature half is moved by hand
- **Rustfmt** — `cargo fmt --all --check`
- **Clippy** — `cargo clippy --workspace --all-targets --all-features -D warnings`
- **Cargo tests** — `cargo test --workspace --all-features` (unit, integration and doctests;
  deliberately not `--all-targets`, which for `cargo test` *excludes* doctests)
- **Typecheck** — `svelte-check` (web) + `tsc` (api-tests), after `pnpm gen:client`
- **API e2e tests** — `pnpm test:api` (Playwright driving the real backend, no browser)
- **Web visual tests** — `pnpm test:web` (Playwright screenshot suite), run inside the
  pinned `mcr.microsoft.com/playwright` image so rendering matches the committed baselines.
  That image tag is also what `pnpm snapshots:update` reads out of `checks.yml` to regenerate
  the `-linux.png` baselines locally — see
  [Regenerating the Linux baselines](TESTING.md#regenerating-the-linux-baselines). On failure the
  job uploads `web-playwright-report`, whose `test-results/**/<name>-actual.png` *is* the new
  baseline if the change was intended
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

These need doing once after the first push — the first two in the GitHub repo settings, the
third from a clone:

1. **Actions permissions** — Settings → Actions → General → Workflow permissions:
   allow GitHub Actions to create/write packages (the release job pushes to GHCR with
   the built-in `GITHUB_TOKEN`) and, for Dependabot auto-merge, "Allow auto-merge" under
   Settings → General.
2. **Branch protection** on `main` — require the CI checks (Personal-data scan, Dependabot
   config, Workspace dependencies, sqlx offline metadata, Rustfmt, Clippy, Cargo tests,
   Typecheck, API e2e tests, Web visual tests, Versions) as status
   checks. Dependabot auto-merge relies on these being required, so a job missing from this
   list is a job a green-looking dependency bump can merge past.
3. **Bootstrap the Linux screenshot baselines** — a baseline is per-platform, so a Mac's
   `pnpm test:web` only produces `-darwin.png` while CI compares `-linux.png`. Run
   `pnpm snapshots:update` once and commit what it writes; the Web visual tests gate is green
   from there, and the same command regenerates them after any intentional UI change. It runs
   the suite in the image `checks.yml` pins, so the output matches what this job compares
   against — the reasoning, the `--platform` trap and the no-Docker fallback are in
   [Regenerating the Linux baselines](TESTING.md#regenerating-the-linux-baselines).

   There used to be an **Update snapshots** workflow doing this on a runner. It pushed with the
   default `GITHUB_TOKEN`, which by design triggers no workflow run, so it always left a commit
   no CI run had checked — plus a bot commit carrying pixels separated from the change that
   caused them.
