#!/usr/bin/env bash
# Build the release container image — the single binary that serves both the JSON API and
# the built SPA (see packages/api/Dockerfile). Run via `pnpm docker:build`.
#
# This is the same image the Release workflow publishes on a version tag; the point of
# having it here is that you can find out whether it builds *without* pushing a tag and
# waiting for CI. The Dockerfile copies each workspace member by name, so adding a crate
# and forgetting to add it there breaks only this build — nothing `pnpm test` runs would
# notice.
#
#   ./scripts/build.sh                      # host arch, loaded into your local docker
#   ./scripts/build.sh --tag sure:wip       # a name of your choosing (repeatable)
#   ./scripts/build.sh --platform linux/amd64,linux/arm64 --push
#   ./scripts/build.sh --no-cache
set -euo pipefail
cd "$(dirname "$0")/.."

readonly DOCKERFILE="packages/api/Dockerfile"
# The build context is the repo root, not the Dockerfile's directory: every stage copies
# workspace manifests, `.cargo` and `.sqlx` from the top.
readonly CONTEXT="."
readonly DEFAULT_IMAGE="sure"

fail() { echo "error: $*" >&2; exit 1; }

tags=()
platform=""
push=false
extra=()

while [ $# -gt 0 ]; do
  case "$1" in
    -t|--tag)
      [ $# -ge 2 ] || fail "$1 needs a value"
      tags+=("$2"); shift 2 ;;
    --platform)
      [ $# -ge 2 ] || fail "$1 needs a value"
      platform="$2"; shift 2 ;;
    --push)   push=true; shift ;;
    --no-cache) extra+=(--no-cache); shift ;;
    -h|--help)
      sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) fail "unknown argument '$1' (try --help)" ;;
  esac
done

command -v docker >/dev/null 2>&1 || fail "docker is not installed"
# `docker version` talks to the daemon; `--version` does not, and would pass with Docker
# Desktop closed — which is the actual failure people hit.
docker version >/dev/null 2>&1 || fail "the docker daemon is not responding (is Docker running?)"

# The version the image is tagged with. Asserted equal across the Rust and Node manifests
# first, because otherwise "the 0.1.0 image" is ambiguous about which 0.1.0 it means — and
# this is the same gate the Release workflow runs before it publishes anything.
./scripts/check-versions.sh >/dev/null \
  || fail "the version gate did not pass — run ./scripts/check-versions.sh to see why"
version=$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p;}' Cargo.toml)
[ -n "$version" ] || fail "could not parse the version from Cargo.toml"

# Default tags mirror what the Release workflow publishes, minus the registry: a plain
# local build should produce the thing you would have got from `docker pull`.
if [ ${#tags[@]} -eq 0 ]; then
  tags=("${DEFAULT_IMAGE}:${version}" "${DEFAULT_IMAGE}:latest")
fi

args=(buildx build --file "$DOCKERFILE" --pull)
for t in "${tags[@]}"; do args+=(--tag "$t"); done
if [ -n "$platform" ]; then args+=(--platform "$platform"); fi

if $push; then
  args+=(--push)
elif [[ "$platform" == *,* ]]; then
  # A multi-platform result is a manifest list, and the local image store cannot hold one:
  # `--load` fails outright. Building without either flag still type-checks the Dockerfile
  # across both arches, which is the useful half, so do that and say what was skipped.
  echo "note: multi-platform build without --push; the result stays in the build cache"
  echo "      rather than being loaded into your local docker images."
else
  # Single platform: put it in the local image store so `docker run` can find it.
  args+=(--load)
fi

# Guarded rather than expanded directly: macOS still ships bash 3.2, where `"${arr[@]}"`
# on an *empty* array trips `set -u` and aborts. That is the no-flags path, i.e. the one
# almost every run takes.
if [ ${#extra[@]} -gt 0 ]; then args+=("${extra[@]}"); fi
args+=("$CONTEXT")

echo "Building ${tags[0]}${platform:+ for $platform}…"
docker "${args[@]}"

if $push || [[ "$platform" == *,* ]]; then
  echo "Done."
else
  echo
  echo "Built:"
  for t in "${tags[@]}"; do echo "  $t"; done
  # Mirrors the Dockerfile's own runtime config: /data is the volume the SQLite database
  # lives on, and 8080 is what BIND_ADDR listens on inside the container.
  echo
  echo "Run it with:"
  echo "  docker run --rm -p 8080:8080 -v sure-data:/data ${tags[0]}"
fi
