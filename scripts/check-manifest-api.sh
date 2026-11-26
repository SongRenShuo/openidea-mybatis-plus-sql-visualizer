#!/usr/bin/env bash
# Manifest API gate: refuse a plugin.toml that declares a contributes key newer
# than its own `engines.openidea` floor allows.
#
# Usage: scripts/check-manifest-api.sh [path/to/plugin.toml]
#        (default: <repo>/plugin.toml)
#
# Why this exists when `openidea-plugin check` already reads the manifest: the
# CLI only knows the extension points of *its own* release. An unknown point is
# downgraded to `warning: contributes.dialogs: not supported by this version of
# the IDE; skipped` — harmless-looking. The host, once its release knows the
# point, calls `needs_api("contributes.dialogs", 18)` and a floor below 1.18 is
# a hard error that fails the whole package's activation. So the CLI cannot
# stand in for this check: it is silent exactly in the case we must catch.
#
# Key → required plugin API (maintain here when a new point lands upstream):
#   1.16  contributes.frames
#   1.16  contributes.markdownHover / contributes.markdown_hover
#   1.16  contributes.customEditors *block that contains a `binary` key*
#   1.18  contributes.dialogs
#   1.18  contributes.overlays
# Anything else present today (commands, keymaps, menus, views, settings,
# providers, markerIcons, fileIcons, documentIndexers, mcpTools, javaAgents,
# skills, …) is available at 1.15 and is not judged.
#
# Floor parsing: `^1.15`, `~1.15`, `>=1.16`, `>1.15`, `1.15`, `1.15.0` and `v1.15`
# yield the minor number. Any other form (e.g. `*`) is an unknown floor: the
# script prints a warning and does not fail.
#
# Exit codes (stable — the self-test asserts them):
#   0  ok (possibly with a warning about an unrecognised engines form)
#   1  at least one declared key needs a newer plugin API than the floor
#   2  the manifest is unreadable or has no parseable engines.openidea
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="${1:-$root/plugin.toml}"

if [[ ! -f "$manifest" ]]; then
  echo "error: manifest not found: $manifest" >&2
  exit 2
fi

# --- engines.openidea -------------------------------------------------------
# Accept the dotted key (`engines.openidea = "…"`, what this repo uses) and a
# `[engines]` table with `openidea = "…"` inside it.
engine_value="$(
  awk '
    { line = $0; sub(/#.*/, "", line) }
    line ~ /^[ \t]*$/ { next }
    line ~ /^[ \t]*\[/ {
      sec = line
      gsub(/[][ \t]/, "", sec)
      next
    }
    line ~ /^[ \t]*engines\.openidea[ \t]*=/ {
      sub(/^[^=]*=[ \t]*/, "", line)
      print line
      exit
    }
    sec == "engines" && line ~ /^[ \t]*openidea[ \t]*=/ {
      sub(/^[^=]*=[ \t]*/, "", line)
      print line
      exit
    }
  ' "$manifest"
)"

# Trim whitespace and the surrounding quotes; the message quotes the raw form.
engine_value="$(printf '%s' "$engine_value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e "s/^[\"']//" -e "s/[\"']$//")"

if [[ -z "$engine_value" ]]; then
  echo "error: plugin.toml has no engines.openidea: $manifest" >&2
  exit 2
fi

# Lower bound as a minor number; empty means "unknown".
floor=""
case "$engine_value" in
  '^'1.*|'~'1.*|'='1.*|v1.*|1.*)
    rest="${engine_value#[\^~=]}"
    rest="${rest#v}"
    if [[ "$rest" =~ ^1\.([0-9]+)(\.[0-9]+)?([.+-].*)?$ ]]; then
      floor="${BASH_REMATCH[1]}"
    fi
    ;;
  '>='*|'>'*)
    rest="${engine_value#>}"
    rest="${rest#=}"
    rest="$(printf '%s' "$rest" | sed 's/^[[:space:]]*//')"
    if [[ "$rest" =~ ^1\.([0-9]+)(\.[0-9]+)?([.+-].*)?$ ]]; then
      floor="${BASH_REMATCH[1]}"
    fi
    ;;
esac

# --- declared keys that need a newer API ------------------------------------
# One `version<TAB>key` record per hit; sort -u collapses repeated entries
# (`[[contributes.commands]]` style manifests repeat array tables).
needed="$(
  awk '
    function emit(v, k) { print v "\t" k }
    BEGIN { sec = "" }
    /^[ \t]*#/ { next }
    /^[ \t]*\[/ {
      hdr = $0
      sub(/[ \t]*(#.*)?$/, "", hdr)
      gsub(/^[ \t]*\[\[?/, "", hdr)
      gsub(/\]\]?[ \t]*$/, "", hdr)
      sec = hdr
      if (sec == "contributes.frames")          emit(16, "contributes.frames")
      if (sec == "contributes.markdownHover")   emit(16, "contributes.markdownHover")
      if (sec == "contributes.markdown_hover")  emit(16, "contributes.markdown_hover")
      if (sec == "contributes.dialogs")         emit(18, "contributes.dialogs")
      if (sec == "contributes.overlays")        emit(18, "contributes.overlays")
      # The two ways a customEditor declares a binary payload: a `binary` field
      # in the entry, or a `[contributes.customEditors.binary]` sub-table.
      if (sec == "contributes.customEditors.binary") emit(16, "contributes.customEditors.binary")
      next
    }
    # Field form: `binary = …` while inside a customEditors entry or sub-table.
    sec ~ /^contributes\.customEditors(\.|$)/ && /^[ \t]*binary[ \t]*=/ {
      emit(16, "contributes.customEditors.binary")
    }
    # Dotted form at the top level of the manifest.
    /^[ \t]*contributes\.frames[ \t]*=/               { emit(16, "contributes.frames") }
    /^[ \t]*contributes\.markdownHover[ \t]*=/        { emit(16, "contributes.markdownHover") }
    /^[ \t]*contributes\.markdown_hover[ \t]*=/       { emit(16, "contributes.markdown_hover") }
    /^[ \t]*contributes\.dialogs[ \t]*=/              { emit(18, "contributes.dialogs") }
    /^[ \t]*contributes\.overlays[ \t]*=/             { emit(18, "contributes.overlays") }
    /^[ \t]*contributes\.customEditors\..*\.binary[ \t]*=/ { emit(16, "contributes.customEditors.binary") }
  ' "$manifest" | sort -u
)"

if [[ -z "$floor" ]]; then
  echo "warning: cannot determine the lower bound of engines.openidea \"$engine_value\"; API check skipped" >&2
  echo "ok (engines.openidea \"$engine_value\", floor unknown)"
  exit 0
fi

status=0
checked=0
while IFS=$'\t' read -r need key; do
  [[ -z "${need:-}" ]] && continue
  checked=$((checked + 1))
  if [[ "$floor" -lt "$need" ]]; then
    echo "error: $key needs plugin API 1.$need, but engines.openidea is \"$engine_value\"" >&2
    status=1
  fi
done <<< "$needed"

if [[ "$status" -ne 0 ]]; then
  echo "       set engines.openidea to \"^1.<major>.0\" in the same change that adds the key." >&2
  exit 1
fi

echo "ok (engines.openidea \"$engine_value\", floor 1.$floor, $checked key(s) checked)"
exit 0
