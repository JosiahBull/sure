#!/usr/bin/env node
//
// Verify or regenerate the committed `*-linux.png` Playwright baselines on this machine, inside
// the exact container CI compares them against.
//
//   node scripts/linux-snapshots.mjs verify    # pnpm snapshots:verify — do they still match?
//   node scripts/linux-snapshots.mjs update    # pnpm snapshots:update — rewrite them
//   node scripts/linux-snapshots.mjs verify tests/app.spec.ts   # trailing args go to playwright
//
// ## Why this is local tooling and not a workflow
//
// It replaces `.github/workflows/snapshots.yml`, which did the same job on a runner and pushed
// the result back with the default `GITHUB_TOKEN`. By GitHub's design a push made with that
// token triggers no further workflow runs, so every use of it left a commit no CI run had ever
// checked, and someone had to close/reopen the PR to get one. It also split a UI change across
// two commits — the change, and a bot commit carrying the pixels it caused — so a reviewer could
// not see the rendering the diff was claiming. Generating them here puts the baselines in the
// same commit as the change, and lets CI verify that commit like any other.
//
// ## Why a container can stand in for the runner
//
// Because it is the same image, pinned by the same tag, and the rendering comes out
// byte-identical: run this way under Rosetta on an arm64 Mac, against the baselines CI itself had
// committed, the suite passed all ten tests including all four `toHaveScreenshot` ones (measured
// 2026-08-16). That is the property `verify` exists to keep true — run it before an `update`.
//
// The trap, and the reason `--platform linux/amd64` is not optional: the image is multi-arch, so
// an arm64 host without that flag pulls arm64 Chromium and renders text with a different
// rasteriser. The suite still passes locally and the baselines it mints still *look*
// authoritative — they simply disagree with CI. That is worse than having no tooling at all,
// which is why the preflight below proves the daemon really can run amd64 rather than assuming.
//
// ## Three details that took a while to find
//
//   * The cargo target directory must literally be `<workdir>/target`. `global-setup.ts` spawns
//     `<repoRoot>/target/debug/sure-api` by path, so a redirected `CARGO_TARGET_DIR` builds
//     perfectly and then the suite dies on `spawn ENOENT`. The cache volume is therefore mounted
//     at /work/target rather than pointed at.
//   * The tree is copied out of a read-only mount rather than built in place. The host's
//     `node_modules` and `target` are darwin/arm64 and would poison the container's build;
//     mounted read-write, the container's would poison the host's.
//   * Named volumes for CARGO_HOME, that target directory and the pnpm store are what make a
//     repeat run minutes instead of the first run's emulated cold Rust build.
//
// ## If you have no Docker
//
// A failing CI visual run already uploads the rendered PNG — see docs/TESTING.md.

import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const repoRoot = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
}).trim();

// Named, not derived from the worktree path: the caches are platform state, not per-checkout
// state, and two worktrees sharing them is the point. Two *concurrent* runs would contend on the
// cargo target lock (they block each other, they do not corrupt anything) — see docs/TESTING.md.
const VOLUMES = {
  cargoHome: "sure-linux-cargo-home",
  cargoTarget: "sure-linux-cargo-target",
  pnpmStore: "sure-linux-pnpm-store",
};

// Under target/ so `cargo clean` and scripts/clean.sh already cover it, and so it can never be
// confused with anything tracked. Wiped per run: a stale *-actual.png from a previous failure is
// exactly the file someone would copy over a baseline by mistake.
const outDir = path.join(repoRoot, "target", "linux-snapshots");

const USAGE = `Verify or regenerate the committed *-linux.png Playwright baselines, inside the
pinned container CI compares them against.

  node scripts/linux-snapshots.mjs verify [playwright args...]   (pnpm snapshots:verify)
      Run the web visual suite against the committed baselines. This is the mode that
      proves the container still agrees with CI; run it before trusting an update.

  node scripts/linux-snapshots.mjs update [playwright args...]   (pnpm snapshots:update)
      Re-render every baseline (--update-snapshots=all) and copy the *-linux.png files
      back into the tree. Commit them together with the UI change that moved them.

  node scripts/linux-snapshots.mjs --help

Needs Docker, and a daemon that can run linux/amd64 (Rosetta or QEMU on an arm64 Mac).
The first run builds Rust under emulation and takes a while; later runs reuse three named
Docker volumes: ${Object.values(VOLUMES).join(", ")}.
Artefacts land in ${path.relative(repoRoot, outDir)}/.

The *-darwin.png baselines are a separate set and are not touched by either mode: run
\`pnpm test:web\` on the host for those.`;

function fail(message) {
  console.error(`✗ ${message}`);
  process.exit(1);
}

/**
 * Resolve the mode. No default that guesses one: a mode nobody recognises is a typo, and the
 * expensive way to handle a typo is to run the wrong twenty-minute job with it.
 */
function parseArgs(argv) {
  const [mode, ...rest] = argv;
  if (mode === "--help" || mode === "-h" || mode === "help") {
    console.log(USAGE);
    process.exit(0);
  }
  // Everything after the mode is forwarded to playwright verbatim, so `pnpm snapshots:verify
  // tests/app.spec.ts` narrows the run. A leading `--` is dropped rather than forwarded, since
  // that separator is muscle memory from npm and playwright would take it as a filter.
  const forwarded = rest[0] === "--" ? rest.slice(1) : rest;
  switch (mode) {
    case "verify":
      return { update: false, forwarded };
    case "update":
      return { update: true, forwarded };
    default:
      console.error(mode ? `✗ unknown mode: ${mode}\n` : "✗ a mode is required\n");
      console.error(USAGE);
      return process.exit(2);
  }
}

/**
 * The image tag, read out of the workflow that actually gates a PR rather than restated here.
 *
 * The baselines are only reproducible against one Chromium build and font stack, so the property
 * that makes this script trustworthy is "same image as the `web-tests` job" — and the only way to
 * keep that true without a human remembering is to read it from that job. A copy here would drift
 * silently on the next Playwright bump, and the failure would be a baseline that looks fine and
 * fails in CI.
 */
function pinnedImage() {
  const workflow = path.join(repoRoot, ".github", "workflows", "checks.yml");
  const text = fs.readFileSync(workflow, "utf8");
  const found = [...text.matchAll(/^\s*image:\s*(mcr\.microsoft\.com\/playwright:\S+)\s*$/gm)].map(
    (m) => m[1],
  );
  const unique = [...new Set(found)];
  if (unique.length !== 1) {
    fail(
      `expected exactly one mcr.microsoft.com/playwright image in ${path.relative(repoRoot, workflow)}, found ${unique.length}` +
        (unique.length ? `: ${unique.join(", ")}` : "") +
        "\n  This script pins itself to whatever the web-tests job uses; teach it the new shape if that job changed.",
    );
  }
  const image = unique[0];

  // The other half of the lockstep the comment in packages/web/package.json asks for. A newer
  // @playwright/test than the image's browsers is not a clean failure — it is a different
  // rendering, i.e. baselines that disagree with the ones CI compares against.
  const webPkg = JSON.parse(
    fs.readFileSync(path.join(repoRoot, "packages", "web", "package.json"), "utf8"),
  );
  const declared = webPkg.devDependencies["@playwright/test"];
  const declaredVersion = declared.replace(/^\D*/, "");
  const imageVersion = image.slice(image.indexOf(":") + 1).replace(/^v/, "").replace(/-.*$/, "");
  if (declaredVersion !== imageVersion) {
    fail(
      `@playwright/test is ${declared} but ${path.relative(repoRoot, workflow)} pins ${image}.\n` +
        "  Those must be the same version — the committed baselines are only reproducible against\n" +
        "  the Chromium the image ships. Bump both, then run `pnpm snapshots:update`.",
    );
  }
  return image;
}

function docker(args, { stdio } = {}) {
  return spawnSync("docker", args, { encoding: "utf8", stdio });
}

/**
 * Fail with something actionable *before* the twenty-minute path, for the two ways this cannot
 * work: no Docker at all, and a daemon that cannot execute amd64.
 *
 * The amd64 check is a real container rather than an inspection of `docker info`, because what
 * matters is not whether emulation is configured somewhere but whether *this image* comes up
 * x86_64 here. It doubles as the pull, which has to happen anyway.
 */
function preflight(image) {
  const version = docker(["version", "--format", "{{.Server.Version}}"]);
  if (version.error?.code === "ENOENT") {
    fail(
      "docker is not on PATH.\n" +
        "  Install Docker Desktop (or colima/OrbStack) and start it, or use the no-Docker route\n" +
        "  in docs/TESTING.md: download the *-actual.png from a failing CI run's\n" +
        "  web-playwright-report artifact.",
    );
  }
  if (version.status !== 0) {
    fail(
      "docker is installed but the daemon is not answering.\n" +
        `  ${(version.stderr || "").trim().split("\n")[0]}\n` +
        "  Start Docker Desktop (or `colima start`) and try again.",
    );
  }

  console.log(`▶ checking the daemon can run linux/amd64 (pulling ${image} if this is a cold cache)`);
  // stderr inherited so the pull's progress is visible — the first one is gigabytes and a silent
  // five minutes reads as a hang.
  const probe = docker(["run", "--rm", "--platform", "linux/amd64", image, "uname", "-m"], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  const arch = (probe.stdout || "").trim();
  if (probe.status !== 0 || arch !== "x86_64") {
    fail(
      `the daemon could not run ${image} as linux/amd64 (uname -m said ${arch || "nothing"}).\n` +
        "  On an arm64 Mac this needs Rosetta (Docker Desktop → Settings → General →\n" +
        "  'Use Rosetta for x86_64/amd64 emulation') or QEMU/binfmt.\n" +
        "  Do not work around it by dropping --platform: an arm64 Chromium renders differently,\n" +
        "  so the baselines it produces disagree with CI while looking authoritative.",
    );
  }
  console.log(`✓ ${image} runs as ${arch}`);
}

/** Copy the exported baselines over the ones in the tree, reporting only what actually moved. */
function copyBaselinesBack() {
  const exported = path.join(outDir, "snapshots");
  if (!fs.existsSync(exported)) {
    fail(`the container exported no baselines to ${path.relative(repoRoot, exported)}`);
  }
  const webDir = path.join(repoRoot, "packages", "web");
  const changed = [];
  let unchanged = 0;
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const from = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(from);
        continue;
      }
      // Only the Linux half. The *-darwin.png baselines in the same directory belong to the
      // host's own `pnpm test:web` run and must survive a container update untouched.
      if (!entry.name.endsWith("-linux.png")) continue;
      const to = path.join(webDir, path.relative(exported, from));
      const next = fs.readFileSync(from);
      if (fs.existsSync(to) && fs.readFileSync(to).equals(next)) {
        unchanged++;
        continue;
      }
      fs.mkdirSync(path.dirname(to), { recursive: true });
      fs.writeFileSync(to, next);
      changed.push(path.relative(repoRoot, to));
    }
  };
  walk(exported);

  if (changed.length === 0) {
    console.log(`✓ ${unchanged} baseline(s) already current — nothing to commit`);
    return;
  }
  console.log(`✓ updated ${changed.length} baseline(s) (${unchanged} unchanged):`);
  for (const file of changed) console.log(`    ${file}`);
  console.log(
    "\n  Look at each one before committing, and commit them with the UI change that moved\n" +
      "  them — a baseline landing on its own is a pixel diff nobody can review.",
  );
}

const { update, forwarded } = parseArgs(process.argv.slice(2));
const image = pinnedImage();
preflight(image);

fs.rmSync(outDir, { recursive: true, force: true });
fs.mkdirSync(outDir, { recursive: true });

const args = [
  "run",
  "--rm",
  // Not optional, on any host. See the header.
  "--platform",
  "linux/amd64",
  "-v",
  `${repoRoot}:/src:ro`,
  "-v",
  `${outDir}:/out`,
  "-v",
  `${VOLUMES.cargoHome}:/cargo-home`,
  // Mounted *at* the path the build must use, rather than exported as CARGO_TARGET_DIR — see the
  // header: global-setup.ts spawns target/debug/sure-api by path.
  "-v",
  `${VOLUMES.cargoTarget}:/work/target`,
  "-v",
  `${VOLUMES.pnpmStore}:/pnpm-store`,
  "-e",
  "CARGO_HOME=/cargo-home",
  // rustup's own state defaults to $HOME/.rustup, which is container filesystem and therefore
  // discarded — parking it in the cargo volume is what stops every run re-downloading a toolchain.
  "-e",
  "RUSTUP_HOME=/cargo-home/rustup",
  "-e",
  "COREPACK_HOME=/pnpm-store/.corepack",
  // The baselines were generated by a CI run, so generate them the same way: CI is observable to
  // the app and to Playwright's own defaults.
  "-e",
  "CI=1",
  "-e",
  `HOST_UID=${process.getuid?.() ?? 0}`,
  "-e",
  `HOST_GID=${process.getgid?.() ?? 0}`,
  image,
  "bash",
  // Read straight off the read-only mount. Nothing has to be copied in, and the script that runs
  // is unambiguously the one in this worktree.
  "/src/scripts/linux-snapshots.container.sh",
  update ? "update" : "verify",
  ...forwarded,
];

console.log(`▶ docker ${args.join(" ")}`);
const run = spawnSync("docker", args, { stdio: "inherit" });
const status = run.status ?? 1;

const results = path.join(outDir, "test-results");
if (fs.existsSync(results)) {
  console.log(
    `\n  Playwright artefacts (including any <name>-actual.png): ${path.relative(repoRoot, results)}`,
  );
}

if (status !== 0) {
  if (update) {
    // Deliberately not copied back. A failed update run may have re-rendered some baselines and
    // not others, and half a set silently overwriting the committed one is the worst outcome
    // available here — worse than doing nothing, because it looks like a completed job.
    console.error(
      `\n✗ the suite failed, so nothing was copied into the tree.\n` +
        `  Any baselines it did render are under ${path.relative(repoRoot, path.join(outDir, "snapshots"))}.`,
    );
  } else {
    console.error(
      "\n✗ the visual suite failed against the committed baselines.\n" +
        "  If the UI change is intended, `pnpm snapshots:update`. If it is not, this is the\n" +
        "  regression CI would have caught.",
    );
  }
  process.exit(status);
}

if (update) copyBaselinesBack();
else console.log("\n✓ the committed *-linux.png baselines still match this container's rendering");
