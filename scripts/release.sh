#!/usr/bin/env bash
# Build, sign and verify one release package.
#
# Usage: scripts/release.sh
#
# Environment:
#   MPSV_KEY_FILE         publisher signing key (required). Generate once with
#                         `openidea-plugin keygen <file>` and keep it out of the repo.
#   MPSV_PUBLISHER        publisher id the key is registered under in the
#                         marketplace index. Defaults to authors[0] in plugin.toml.
#   MPSV_OUT_DIR          where the package and its .sha256 go. Default: dist
#
# Steps, in this order and for a reason:
#   1. openidea-plugin build .   — `pack` copies whatever dist/plugin.wasm is
#      lying around, it does not check whether the component is stale.
#   2. openidea-plugin pack      — signed .oiplugin, never unsigned.
#   3. sha256sum                 — the checksum the Release/Gitee mirror agree on.
#   4. openidea-plugin check     — re-read the package as the IDE would install it.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$root/plugin.toml"

die() {
  printf 'release: %s\n' "$*" >&2
  exit 1
}

# Top-level keys only: stop at the first table header so a `[[contributes.commands]]`
# block's own `id = ...` cannot be mistaken for the plugin id.
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

toml_first_author() {
  awk '
    /^[[:space:]]*\[/ { exit }
    /^[[:space:]]*authors[[:space:]]*=/ {
      line = $0
      sub(/^[^[]*\[[[:space:]]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$manifest"
}

human_size() {
  awk -v bytes="$1" 'BEGIN {
    if (bytes < 1024)            printf "%d B", bytes
    else if (bytes < 1048576)    printf "%.1f KiB", bytes / 1024
    else                         printf "%.1f MiB", bytes / 1048576
  }'
}

command -v openidea-plugin >/dev/null 2>&1 ||
  die "openidea-plugin not found on PATH — install the 0.1.51 CLI (see scripts/README.md)."
command -v sha256sum >/dev/null 2>&1 ||
  die "sha256sum not found on PATH — coreutils (Git Bash, Linux, macOS with coreutils)."

[[ -f "$manifest" ]] || die "plugin.toml not found: $manifest"

plugin_id="$(toml_top_level id)"
plugin_version="$(toml_top_level version)"
[[ -n "$plugin_id" ]] || die "could not read 'id' from $manifest"
[[ -n "$plugin_version" ]] || die "could not read 'version' from $manifest"

# --- signing key -------------------------------------------------------------
if [[ -z "${MPSV_KEY_FILE:-}" ]]; then
  die "MPSV_KEY_FILE is not set — refusing to produce an unsigned package.
  Create a publisher key once and keep it outside the repository:
      openidea-plugin keygen \"\$HOME/mpsv-publisher.key\"
      export MPSV_KEY_FILE=\"\$HOME/mpsv-publisher.key\"
  Then register the public key it prints under your publisher id in the
  marketplace index (see market/README.md). Back the key file up: losing it
  means a new key, a new endorsement and affected users."
fi

[[ -f "$MPSV_KEY_FILE" ]] || die "MPSV_KEY_FILE does not exist: $MPSV_KEY_FILE
  Generate it with: openidea-plugin keygen \"\$MPSV_KEY_FILE\""
[[ -s "$MPSV_KEY_FILE" ]] || die "MPSV_KEY_FILE is empty: $MPSV_KEY_FILE"

publisher="${MPSV_PUBLISHER:-}"
publisher_from="${MPSV_PUBLISHER:+MPSV_PUBLISHER}"
if [[ -z "$publisher" ]]; then
  publisher="$(toml_first_author)"
  publisher_from="plugin.toml authors[0]"
fi
[[ -n "$publisher" ]] || die "MPSV_PUBLISHER is not set and plugin.toml has no authors[0]"

out_dir="${MPSV_OUT_DIR:-$root/dist}"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"

wasm="$root/dist/plugin.wasm"
artifact="$out_dir/${plugin_id}-${plugin_version}.oiplugin"

printf 'release: %s %s\n' "$plugin_id" "$plugin_version"
printf 'release: publisher %s (from %s)\n' "$publisher" "$publisher_from"

# --- 1. build (pack copies the wasm verbatim, stale or not) ------------------
echo "release: building component"
(cd "$root" && openidea-plugin build .)

[[ -s "$wasm" ]] || die "build produced no $wasm"

# --- 2. pack (signed) --------------------------------------------------------
echo "release: packing"
rm -f "$artifact"
(cd "$root" && openidea-plugin pack . \
  --key "$MPSV_KEY_FILE" \
  --publisher "$publisher" \
  --out "$artifact")

[[ -s "$artifact" ]] || die "pack produced no $artifact"

# --- 3. checksum -------------------------------------------------------------
sha_file="$artifact.sha256"
(cd "$out_dir" && sha256sum "$(basename "$artifact")" > "$sha_file")
digest="$(awk '{print $1}' "$sha_file")"

# --- 4. read back the package as the IDE installs it -------------------------
echo "release: checking $artifact"
openidea-plugin check "$artifact"

# --- readings ---------------------------------------------------------------
wasm_bytes="$(wc -c < "$wasm" | tr -d '[:space:]')"
pkg_bytes="$(wc -c < "$artifact" | tr -d '[:space:]')"

echo
echo "release: done"
printf '  wasm     %-52s %s (%s bytes)\n' "dist/plugin.wasm" "$(human_size "$wasm_bytes")" "$wasm_bytes"
printf '  package  %-52s %s (%s bytes)\n' "$(basename "$artifact")" "$(human_size "$pkg_bytes")" "$pkg_bytes"
printf '  sha256   %s\n' "$digest"
printf '  checksum %s\n' "$sha_file"