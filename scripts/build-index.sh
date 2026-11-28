#!/usr/bin/env bash
# Build the marketplace index from the .oiplugin packages in one directory.
#
# Usage: scripts/build-index.sh
#
# Environment:
#   MPSV_KEY_FILE        publisher signing key (required); it registers the key
#                        in the index it writes.
#   MPSV_PUBLISHER       publisher id (required) — the prefix of the plugin id.
#   MPSV_DOWNLOAD_BASE   URL prefix the packages are served from (required),
#                        e.g. https://github.com/<owner>/<repo>/releases/download/2026.11.28
#   MPSV_NAME            publisher display name. Default: plugin.toml `name`.
#   MPSV_DIST_DIR        directory holding the .oiplugin files. Default: dist
#   MPSV_INDEX_OUT       where index.json is written. Default: market/index.json
#   MPSV_PREVIOUS_KEY    key file of the key being replaced (rotation only; it
#                        must already be registered in MPSV_INDEX_OUT — the CLI
#                        checks this and refuses a key it has not seen).
#   MPSV_OFFICIAL_INDEX  path or URL of another index to fold in with
#                        `merge --keep <publisher>`; the official marketplace
#                        index gives users everything this repo does not host.
#
# Note: the index is written only at release time, from real signed packages.
# A placeholder index is worse than none — the IDE would offer downloads that
# 404, so this repository does not commit a sample.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/plugin.toml"

die() {
  printf 'index: %s\n' "$*" >&2
  exit 1
}

toml_top_level() {
  awk -v key="$1" '
    /^[[:space:]]*\[/ { exit }
    $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
      line = $0
      sub(/^[^=]*=[[:space:]]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$manifest"
}

command -v openidea-plugin >/dev/null 2>&1 ||
  die "openidea-plugin not found on PATH — install the 0.1.51 CLI (see scripts/README.md)."

[[ -n "${MPSV_KEY_FILE:-}" ]] || die "MPSV_KEY_FILE is not set.
  With a key the index registers the publisher; without one the CLI only lists
  packages and cannot keep them updatable. Generate the key once with:
      openidea-plugin keygen \"\$HOME/mpsv-publisher.key\""
[[ -f "$MPSV_KEY_FILE" && -s "$MPSV_KEY_FILE" ]] ||
  die "MPSV_KEY_FILE is missing or empty: ${MPSV_KEY_FILE:-<unset>}"

[[ -n "${MPSV_PUBLISHER:-}" ]] || die "MPSV_PUBLISHER is not set (e.g. songrenshuo)."
[[ -n "${MPSV_DOWNLOAD_BASE:-}" ]] ||
  die "MPSV_DOWNLOAD_BASE is not set — the index needs an absolute URL prefix
  the packages are served from, e.g.
      export MPSV_DOWNLOAD_BASE=https://github.com/<owner>/<repo>/releases/download/2026.11.28"

publisher="$MPSV_PUBLISHER"
name="${MPSV_NAME:-$(toml_top_level name)}"
[[ -n "$name" ]] || die "MPSV_NAME is not set and plugin.toml has no name"

dist_dir="${MPSV_DIST_DIR:-$root/dist}"
index_out="${MPSV_INDEX_OUT:-$root/market/index.json}"

[[ -d "$dist_dir" ]] || die "package directory not found: $dist_dir (run scripts/release.sh first)"
dist_dir="$(cd "$dist_dir" && pwd)"

shopt -s nullglob
packages=("$dist_dir"/*.oiplugin)
shopt -u nullglob
(( ${#packages[@]} > 0 )) || die "no .oiplugin in $dist_dir (run scripts/release.sh first)"

mkdir -p "$(dirname "$index_out")"

# --previous-key is checked against the keys already in the index at --out, so a
# rotation has to write in place; a plain build goes through a temp file so a
# failed run cannot leave a truncated index behind.
work_out="$index_out"
tmp_dir=""
if [[ -n "${MPSV_PREVIOUS_KEY:-}" ]]; then
  [[ -f "$MPSV_PREVIOUS_KEY" && -s "$MPSV_PREVIOUS_KEY" ]] ||
    die "MPSV_PREVIOUS_KEY is missing or empty: $MPSV_PREVIOUS_KEY"
  [[ -f "$index_out" ]] || die "MPSV_PREVIOUS_KEY needs an existing index at $index_out
  to endorse against; run a plain build first."
else
  # The CLI parses whatever is already at --out, so the temp path must not
  # exist: an empty file is read as a broken index.
  tmp_dir="$(mktemp -d)"
  tmp_out="$tmp_dir/index.json"
  work_out="$tmp_out"
  trap 'rm -rf "$tmp_dir"' EXIT
fi

echo "index: ${#packages[@]} package(s) from $dist_dir"
index_args=(
  index "$dist_dir"
  --publisher "$publisher"
  --name "$name"
  --key "$MPSV_KEY_FILE"
  --download-base "$MPSV_DOWNLOAD_BASE"
  --out "$work_out"
)
if [[ -n "${MPSV_PREVIOUS_KEY:-}" ]]; then
  index_args+=(--previous-key "$MPSV_PREVIOUS_KEY")
fi

openidea-plugin "${index_args[@]}"

if [[ -n "${MPSV_OFFICIAL_INDEX:-}" ]]; then
  echo "index: merging ${MPSV_OFFICIAL_INDEX} (keeping $publisher)"
  openidea-plugin merge "$work_out" "$MPSV_OFFICIAL_INDEX" --keep "$publisher" --out "$work_out"
fi

if [[ -n "$tmp_dir" ]]; then
  mv "$tmp_out" "$index_out"
  rm -rf "$tmp_dir"
  tmp_dir=""
fi

# --- read back --------------------------------------------------------------
validator=""
if command -v python3 >/dev/null 2>&1; then
  validator="python3"
elif command -v python >/dev/null 2>&1; then
  validator="python"
fi

plugins_listed="?"
if [[ -n "$validator" ]]; then
  plugins_listed="$("$validator" -I -c '
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    doc = json.load(fh)
assert doc.get("format") == 1, "unexpected index format"
print(len(doc.get("plugins", [])))
' "$index_out")"
else
  echo "index: warning — no python on PATH, skipped the JSON read-back"
fi

digest="$(sha256sum "$index_out" | awk '{print $1}')"

echo
echo "index: done"
printf '  file     %s\n' "$index_out"
printf '  packages %s in %s\n' "${#packages[@]}" "$dist_dir"
printf '  plugins  %s listed\n' "$plugins_listed"
printf '  base     %s\n' "$MPSV_DOWNLOAD_BASE"
printf '  sha256   %s\n' "$digest"