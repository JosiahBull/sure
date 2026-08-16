#!/usr/bin/env bash
# Assert every 0.x direct dependency — Cargo crate and npm package alike — is ignored for
# minor bumps in .github/dependabot.yml.
#
# Dependabot classifies 0.16 -> 0.17 as a *minor* update, because the major field is still
# 0. Under the semver both Cargo and npm implement that is a breaking change: while the
# major is 0 the *minor* field is the compatibility boundary, which is why `^0.16.0`
# resolves below 0.17.0 and a bare `0.3` below 0.4. Auto-merge takes Dependabot's own
# classification at its word, so without this the repository lands breaking bumps
# unattended and unreviewed.
#
# That is not hypothetical here. sqlx 0.8.6 -> 0.9.0 and tower-http 0.6.11 -> 0.7.0 both
# landed on main unreviewed, stamped `update-type: version-update:semver-minor` in their own
# commit trailers (bbc6ba9, cfb6491) — straight past the `- dependency-name: "*"` ignore that
# withholds majors, because Dependabot did not consider them majors. And openapi-fetch
# 0.16 -> 0.17 wraps the client's `data` in a helper whose array branch collapses tuples,
# breaking `pnpm --filter @sure/web check` (PR #9, closed; the mechanism is written up in
# packages/client/package.json).
#
# Dependabot has no setting for "treat 0.x minors as major", so the rule lives as an
# explicit list of names in the `ignore:` block of each ecosystem — and a list is only as
# good as whatever keeps it current. That is this script: add a 0.x dependency without
# listing it and this fails, naming exactly what to paste.
#
# Part of the `dependabot-config` job in .github/workflows/checks.yml. Pure text parsing —
# no cargo, no pnpm, no network, no lockfile — so it costs milliseconds and needs no
# toolchain.
#
# Usage:
#   ./scripts/check-dependabot.sh

set -euo pipefail
cd "$(dirname "$0")/.."

fail() {
    if [ -n "${GITHUB_ACTIONS:-}" ]; then
        echo "::error::$*" >&2
    else
        echo "error: $*" >&2
    fi
    exit 1
}

case "${1:-}" in
-h | --help)
    # Print this header block, uncommented. Two -e expressions rather than one
    # `s/^#\( \|$\)//`: that alternation is a GNU extension and BSD sed (macOS, where
    # most of this repo's development happens) silently leaves every `#` in place.
    sed -n '2,/^$/p' "$0" | sed -e 's/^# //' -e 's/^#$//'
    exit 0
    ;;
"") ;;
*) fail "unknown argument '$1' (try --help)" ;;
esac

python3 - <<'PY'
import glob
import json
import os
import re
import sys

CARGO_MANIFESTS = ["Cargo.toml"] + sorted(glob.glob("packages/*/Cargo.toml"))
NPM_MANIFESTS = ["package.json"] + sorted(glob.glob("packages/*/package.json"))
CONFIG = ".github/dependabot.yml"

GHA = bool(os.environ.get("GITHUB_ACTIONS"))


def err(msg):
    # Workflow commands are read off stdout, so this is deliberately not stderr.
    print(f"::error::{msg}" if GHA else f"error: {msg}")


def is_zero_x(req):
    """True when the requirement's compatibility boundary is the minor field.

    Strips whatever comparator prefixes the range (`^0.16.0`, `~0.12`, `>=0.4`, `=0.55`,
    plain `0.3`) and asks only whether the major is 0. A range that starts at 1.0 or above
    is left to Dependabot's own classification, which is correct there.
    """
    return re.sub(r"^[\^~=><v\s]+", "", req).startswith("0.")


def cargo_deps():
    """Direct dependencies carrying a version requirement, across the Cargo workspace.

    Every dependency table in every manifest is read, the root's `[workspace.dependencies]`
    included — that table is where most of this tree's requirements actually live, so
    skipping it would leave the guard checking almost nothing. Entries with no version
    requirement fall out for free rather than by special case: a member's
    `{ workspace = true }` defers to the root table, and a `path`/`git` dependency has no
    published version for Dependabot to bump, so neither matches the patterns below.
    """
    deps = {}
    # `foo = "0.3"` or `foo = { version = "0.3", .. }`, including the multi-line inline
    # table form — the `version` key is on the opening line in every one of ours, and
    # `[^}]*?` refuses to cross into a following entry.
    inline = re.compile(
        r'^([A-Za-z0-9_.-]+)\s*=\s*(?:\{[^}]*?\bversion\s*=\s*"([^"]+)"|"([^"]+)")'
    )
    version_key = re.compile(r'^version\s*=\s*"([^"]+)"')
    # A dependency table in any of its spellings: `[dependencies]`, `[dev-dependencies]`,
    # `[build-dependencies]`, `[workspace.dependencies]`,
    # `[target.'cfg(target_os = "linux")'.dependencies]`, and the `[dependencies.foo]`
    # sub-table (group 1), where the name is the table's rather than the key's.
    table = re.compile(r'(?:^|\.)(?:dev-|build-)?dependencies(?:\.([A-Za-z0-9_-]+))?$')
    for path in CARGO_MANIFESTS:
        in_deps, subtable = False, None
        for line in open(path):
            s = line.strip()
            if s.startswith("[") and s.endswith("]"):
                m = table.search(s.strip("[]"))
                in_deps, subtable = m is not None, m.group(1) if m else None
                continue
            if not in_deps or s.startswith("#"):
                continue
            if subtable:
                m = version_key.match(s)
                if m:
                    deps.setdefault(subtable, m.group(1))
                continue
            m = inline.match(s)
            if m:
                deps.setdefault(m.group(1), m.group(2) or m.group(3))
    return deps


def npm_deps():
    """Direct dependencies across the pnpm workspace, `devDependencies` included.

    A dev dependency is no safer than a runtime one here: `openapi-typescript` generates
    the client every typecheck consumes, and `@playwright/test` decides whether the
    committed baselines are even comparable.
    """
    deps = {}
    for path in NPM_MANIFESTS:
        with open(path) as f:
            pkg = json.load(f)
        for field in ("dependencies", "devDependencies"):
            for name, req in pkg.get(field, {}).items():
                # `workspace:*` is another member of this pnpm workspace; the rest are
                # protocols with no registry version for Dependabot to compare against.
                if re.match(r"^(workspace|link|file|git|github|npm|catalog|portal):", req):
                    continue
                deps.setdefault(name, req)
    return deps


def ignored_by_ecosystem():
    """Names ignored for `version-update:semver-minor`, keyed by `package-ecosystem`.

    Partitioning by block is the point: an `ignore:` entry applies only to the `updates:`
    block it sits in, so a crate listed under the npm block protects nothing on the cargo
    side. Matching the file as one blob — which is all a single-ecosystem repo needs —
    would report that arrangement as safe.

    Comment lines are dropped before anything is matched, so prose *about* a semver-minor
    ignore can never be mistaken for one.
    """
    lines = [
        line.rstrip("\n")
        for line in open(CONFIG)
        if not line.lstrip().startswith("#")
    ]
    ecosystem_line = re.compile(r'^\s*-\s*package-ecosystem:\s*"?([A-Za-z0-9_-]+)"?\s*$')
    name_line = re.compile(r'^(\s*)-\s*dependency-name:\s*"?([^"\s]+)"?\s*$')

    blocks, current = {}, None
    for line in lines:
        m = ecosystem_line.match(line)
        if m:
            current = m.group(1)
            blocks.setdefault(current, [])
            continue
        if current is not None:
            blocks[current].append(line)

    out = {}
    for ecosystem, body in blocks.items():
        names, i = set(), 0
        while i < len(body):
            m = name_line.match(body[i])
            if not m:
                i += 1
                continue
            indent, name = len(m.group(1)), m.group(2)
            i += 1
            # The rest of this list item is every following line indented past its `- `;
            # the first line at or below that indent starts the next entry or key.
            entry = []
            while i < len(body):
                line = body[i]
                if line.strip() and len(line) - len(line.lstrip()) <= indent:
                    break
                entry.append(line)
                i += 1
            if "version-update:semver-minor" in "\n".join(entry):
                names.add(name)
        out[ecosystem] = names
    return out


ignored = ignored_by_ecosystem()
failed = False

for ecosystem, semver_of, deps in (
    ("cargo", "Cargo", cargo_deps()),
    ("npm", "npm", npm_deps()),
):
    # A parser that silently stops matching would pass this check by finding nothing to
    # complain about, which is the one failure mode a guard must not have.
    if not deps:
        err(f"{ecosystem}: parsed no dependencies at all — the manifest parser is broken")
        failed = True
        continue

    zero_x = {name: req for name, req in deps.items() if is_zero_x(req)}
    listed = ignored.get(ecosystem, set())
    missing = sorted(set(zero_x) - listed)

    if missing:
        failed = True
        err(
            f"{ecosystem}: {len(missing)} 0.x direct "
            f"{'dependency is' if len(missing) == 1 else 'dependencies are'} not ignored "
            f"for minor bumps in {CONFIG}:"
        )
        for name in missing:
            print(f'  {name} = "{zero_x[name]}"')
        print()
        print(f"A 0.x minor bump is a breaking change under {semver_of}'s semver, but Dependabot")
        print("calls it minor and would merge it unattended. Add each one to the `ignore:` block")
        print(f'of the `package-ecosystem: {ecosystem}` entry in {CONFIG}:')
        print()
        for name in missing:
            print(f'      - dependency-name: "{name}"')
            print('        update-types: ["version-update:semver-minor"]')
        print()
    else:
        print(
            f"OK {ecosystem}: all {len(zero_x)} 0.x direct dependencies "
            f"(of {len(deps)}) are ignored for minor bumps"
        )

    # The reverse direction is a note, not a failure: a dependency that reached 1.0, or one
    # that was removed, leaves an entry that costs nothing but noise.
    stale = sorted(listed - set(zero_x))
    if stale:
        print(f"note: {ecosystem}: listed but no longer a 0.x direct dependency: {', '.join(stale)}")

sys.exit(1 if failed else 0)
PY
