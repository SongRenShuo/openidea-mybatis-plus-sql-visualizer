#!/usr/bin/env bash
# Verify the pinned engine jar before the plugin is allowed to start it.
# Usage: scripts/verify-engine.sh [path-to-jar]
# Exits 0 when the pin is pending (build still allowed) or when the jar matches.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ver_file="$root/engine/ENGINE_VERSION"
sha_file="$root/engine/ENGINE_VERSION.sha256"

if [[ ! -f "$ver_file" ]]; then
  echo "engine: ENGINE_VERSION missing" >&2
  exit 2
fi

version="$(tr -d '[:space:]' < "$ver_file")"

if [[ "$version" == 0.0.0-* ]]; then
  echo "engine: pin is pending ($version) — sidecar stays disabled, text mode only."
  exit 0
fi

jar="${1:-$root/engine/mpsv-engine-$version.jar}"

if [[ ! -f "$jar" ]]; then
  echo "engine: jar not found: $jar" >&2
  exit 3
fi

if [[ ! -f "$sha_file" ]]; then
  echo "engine: ENGINE_VERSION.sha256 missing (pin=$version)" >&2
  exit 4
fi

expected="$(awk '{print $1}' "$sha_file" | tr -d '[:space:]')"
actual="$(sha256sum "$jar" | awk '{print $1}')"

if [[ "$expected" != "$actual" ]]; then
  echo "engine: sha256 mismatch" >&2
  echo "  expected $expected" >&2
  echo "  actual   $actual" >&2
  exit 5
fi

echo "engine: ok ($version, $actual)"
