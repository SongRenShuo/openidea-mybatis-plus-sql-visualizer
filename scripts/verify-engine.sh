#!/usr/bin/env bash
# Verify the pinned engine jar before the plugin is allowed to start it.
#
# Usage: scripts/verify-engine.sh [--pin-only] [path-to-jar]
#
# --pin-only checks the pin (version + sha256 format) without a jar. CI uses it
# because the jar is never committed: only engine/ENGINE_VERSION and its
# sha256 are in the repository, and the runtime downloads the jar once.
#
# Resolution order (matches src/sidecar::resolve_engine_path):
#   1) the jar passed as $1, else $MPSV_ENGINE_JAR,
#      else engine/mpsv-engine-<version>.jar in this repo (developer / link runs);
#   2) the runtime additionally tries <fs.plugin_dir()>/mpsv-engine-<version>.jar
#      and the configured `engine.downloadUrl` — those are not checked here.
#
# Exit codes (stable — referenced by tests and CI):
#   0  ok, or the pin is still pending (0.0.0-*: sidecar disabled, text mode only)
#   2  engine/ENGINE_VERSION missing or not a plain semver
#   3  jar not found
#   4  engine/ENGINE_VERSION.sha256 missing or malformed (expected 64 hex chars)
#   5  sha256 mismatch (a corrupt or wrong-version jar)
set -euo pipefail

pin_only=0
if [[ "${1:-}" == "--pin-only" ]]; then
  pin_only=1
  shift
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ver_file="$root/engine/ENGINE_VERSION"
sha_file="$root/engine/ENGINE_VERSION.sha256"

if [[ ! -f "$ver_file" ]]; then
  echo "engine: ENGINE_VERSION missing: $ver_file" >&2
  exit 2
fi

version="$(tr -d '[:space:]' < "$ver_file")"

if [[ "$version" == 0.0.0-* ]]; then
  echo "engine: pin is pending ($version) — sidecar stays disabled, text mode only."
  exit 0
fi

# A real pin is a dotted numeric version; anything else means the file drifted.
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "engine: ENGINE_VERSION is not a semver: '$version'" >&2
  exit 2
fi

# Jar selection: explicit argument, then $MPSV_ENGINE_JAR, then the repo-local jar.
jar="${1:-${MPSV_ENGINE_JAR:-$root/engine/mpsv-engine-$version.jar}}"

# Pin-only: validate the sha256 file itself and stop before any jar lookup.
if [[ "$pin_only" == 1 ]]; then
  if [[ ! -f "$sha_file" ]]; then
    echo "engine: ENGINE_VERSION.sha256 missing (pin=$version): $sha_file" >&2
    exit 4
  fi
  expected="$(awk '{print $1}' "$sha_file" | tr -d '[:space:]')"
  if [[ ! "$expected" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "engine: ENGINE_VERSION.sha256 is malformed (want 64 hex chars, got '$expected')" >&2
    exit 4
  fi
  echo "engine: pin ok ($version, ${expected,,}) — jar not checked (--pin-only)"
  exit 0
fi

if [[ ! -f "$jar" ]]; then
  echo "engine: jar not found: $jar" >&2
  echo "  (build it in the IntelliJ repo, or pass a path / set MPSV_ENGINE_JAR)" >&2
  exit 3
fi

if [[ ! -f "$sha_file" ]]; then
  echo "engine: ENGINE_VERSION.sha256 missing (pin=$version): $sha_file" >&2
  exit 4
fi

# First whitespace-delimited token is the hex digest (`<hex>  <file>` format).
expected="$(awk '{print $1}' "$sha_file" | tr -d '[:space:]')"
if [[ ! "$expected" =~ ^[0-9a-fA-F]{64}$ ]]; then
  echo "engine: ENGINE_VERSION.sha256 is malformed (want 64 hex chars, got '$expected')" >&2
  exit 4
fi
expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"

actual="$(sha256sum "$jar" | awk '{print $1}')"

if [[ "$expected" != "$actual" ]]; then
  echo "engine: sha256 mismatch — refusing to start a wrong/corrupt jar" >&2
  echo "  jar      $jar" >&2
  echo "  expected $expected" >&2
  echo "  actual   $actual" >&2
  exit 5
fi

echo "engine: ok ($version, $actual)"