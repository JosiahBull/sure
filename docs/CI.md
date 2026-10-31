# CI / CD

GitHub Actions runs the test suite on every push and PR, and publishes a Docker image
whenever a version tag is pushed. Everything lives under [`.github/`](../.github).

## Workflows

| Workflow | Trigger | What it does |
| --- | --- | --- |
| `ci.yml` | push to `main`, any PR | Calls the reusable `checks.yml` suite. |
| `checks.yml` | `workflow_call` | The merge gates (see below). Shared by CI and Release so a release runs the identical checks. |
| `release.yml` | push tag `v*` | Runs `checks.yml`, builds the image for each architecture on a native runner, publishes them to GHCR as one multi-arch tag, then cuts a GitHub Release. |
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
- **Workspace dependencies** — `cargo autoinherit` then `git diff --exit-code`: every
  requirement and every feature flag lives in the root `[workspace.dependencies]`, and a member
  manifest says only `foo = { workspace = true }`. The tool is the check and the fix in one, so
  a diff in this job's log is the patch to apply
- **Rustfmt** — `cargo fmt --all --check`
- **Clippy** — `cargo clippy --workspace --all-targets --all-features -D warnings`
- **Cargo tests** — `cargo test --workspace --all-features` (unit, integration and doctests;
  deliberately not `--all-targets`, which for `cargo test` *excludes* doctests)
- **Typecheck** — `pnpm gen:client`, failing if that changed anything in `packages/client`, then
  `svelte-check` (web) + `tsc` (api-tests). The OpenAPI spec and the typed client generated from
  it are committed, and this is the one place they are regenerated: everything else, the release
  image included, uses the committed copies, so this gate is what holds them to the backend
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

### Rust toolchain and caches

Every job that runs cargo installs the version [`rust-toolchain.toml`](../rust-toolchain.toml)
names, which is also what the pre-commit hook, local builds and the release image compile with —
so a release is built by the compiler its checks ran on, and a new stable arrives as a Dependabot
PR that runs every gate rather than silently on the next run. The `rust` and `node` tags in
`packages/api/Dockerfile` are pinned and bumped by Dependabot too.

The `Swatinem/rust-cache` entries are saved by `main` only. A PR run restores main's entry for
its job and saves nothing, so open PRs no longer add a 3–4 GB set each to the repository's 10 GB
cache budget, which they shared with main's set and the release's layer cache. Clippy and sqlx
offline metadata share one entry, because they build the same dependency artifacts. The comments
at the top of `checks.yml` have the detail.

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

Each architecture builds in its own job on a runner of that architecture: `ubuntu-latest` for
amd64, and `ubuntu-24.04-arm`, free for public repositories, for arm64. Each pushes an untagged
image by digest; a final job joins the two digests into one manifest list with
`docker buildx imagetools create` and applies the tags, so no tag exists until both
architectures do. This is Docker's
[distribute build across multiple runners](https://docs.docker.com/build/ci/github-actions/multi-platform/#distribute-build-across-multiple-runners)
recipe written out by hand; that page now points at the `docker/github-builder` reusable
workflow instead, which also signs and attests what it pushes.

Up to v0.7.0, arm64 was built under QEMU on the amd64 runner, and the emulated compile set the
length of a release: `cargo build --release` took 3,144 s for arm64 against 433 s for amd64 in
v0.6.0, and 5,493 s against 643 s in v0.7.0, whose build job ran for 100 minutes. On native
runners, measured cold before this change merged, it took 254 s on `ubuntu-24.04-arm` and 363 s
on `ubuntu-latest`; each whole build job, without the push or the cache export, took 5 and 7
minutes.

Each build job exports a GitHub Actions layer cache (`type=gha,mode=max`) under its own scope.
That export costs minutes per job (456 s and 339 s for the two-architecture builds of v0.6.0 and
v0.7.0), and a run on a tag can read only caches written on that tag or on `main`. Nothing on
`main` writes them, so the cache only helps a re-run of the same tag's workflow.

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
