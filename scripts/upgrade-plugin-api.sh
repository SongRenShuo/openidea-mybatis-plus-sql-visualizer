#!/usr/bin/env bash
# Move the plugin-API pin to a new API / SDK / CLI triple, in one command.
#
# Usage:
#   scripts/upgrade-plugin-api.sh --api 1.18 --sdk-tag v0.1.54 --cli-tag v0.1.54
#   scripts/upgrade-plugin-api.sh --api 1.18 --sdk-tag v0.1.54 --cli-tag v0.1.54 --dry-run
#   scripts/upgrade-plugin-api.sh ... --update-lock        # also cargo update
#
# It changes every pin that names one of the three versions, and nothing else:
#
#   plugin.toml                        engines.openidea = "^1.15"        -> "^1.18"
#   Cargo.toml                         openidea-plugin = { git = ..., tag = "v0.1.51" }
#   .github/workflows/build.yml        CLI_TAG: v0.1.51
#   .github/workflows/build.yml        grep -F "0.1.51" /tmp/cli-version.txt
#   .github/workflows/release.yml      CLI_TAG: v0.1.51
#   .github/workflows/release.yml      grep -F "0.1.51" /tmp/cli-version.txt
#
# The last two matter as much as the CLI_TAG lines: they assert the version of
# the CLI binary that was just downloaded. Updating the tag but not the
# assertion leaves a workflow that installs one version and demands another,
# which fails on `set -e`. Both are gates, not prose.
#
# Rules:
#   * every pin must be found by a line-anchored pattern, and no file may match
#     one pattern twice: zero matches for a file that should carry the pin, or
#     two candidates in one file, aborts the run naming the lines that did match.
#     A pin is never skipped silently;
#   * backups are taken into the script's temp dir, not next to the sources, so
#     a crash cannot leave *.bak files for `git status` to pick up. Any failure
#     (write or read-back) restores every file;
#   * running it twice with the same arguments is a no-op that exits 0;
#   * after writing, the workflows are re-scanned: a non-comment line still
#     naming an old CLI version raises a WARNING naming file and line (this is
#     how a gate whose shape changed gets caught instead of being skipped);
#   * --dry-run prints the diff and writes nothing.
#
# It never runs the IDE or CI. --update-lock optionally refreshes Cargo.lock
# with `cargo update -p openidea-plugin` (network, tens of seconds).
#
# Environment:
#   MPSV_UPGRADE_ROOT   operate on another checkout instead of the one this
#                       script lives in. Used by the self-test; not needed in
#                       normal use.
set -euo pipefail

die() { printf 'upgrade-plugin-api: %s\n' "$*" >&2; exit 1; }
say() { printf 'upgrade-plugin-api: %s\n' "$*"; }

usage() {
  cat <<'EOF'
Usage: scripts/upgrade-plugin-api.sh --api <x.y> --sdk-tag <vX.Y.Z> --cli-tag <vX.Y.Z>
                                     [--dry-run] [--update-lock]

  --api          plugin API the new SDK targets, written as "^<x.y>" (e.g. 1.18)
  --sdk-tag      git tag of the openidea SDK in Cargo.toml (e.g. v0.1.54)
  --cli-tag      openidea-plugin CLI release (e.g. v0.1.54). Rewrites every
                 CLI_TAG and every `grep -F "<version>"` assertion in
                 .github/workflows/
  --dry-run      print the diffs, write nothing
  --update-lock  after writing, run `cargo update -p openidea-plugin`
                 (network; skipped under --dry-run)
  -h, --help     this text
EOF
}

api=""
sdk_tag=""
cli_tag=""
dry_run=0
update_lock=0

while (($# > 0)); do
  case "$1" in
    --api) api="${2:-}"; shift 2 ;;
    --sdk-tag) sdk_tag="${2:-}"; shift 2 ;;
    --cli-tag) cli_tag="${2:-}"; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    --update-lock) update_lock=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument '$1' (try --help)" ;;
  esac
done

[[ -n "$api" ]] || die "--api is required"
[[ -n "$sdk_tag" ]] || die "--sdk-tag is required"
[[ -n "$cli_tag" ]] || die "--cli-tag is required"

# Values are pasted into sed replacements, so keep them boring on purpose.
[[ "$api" =~ ^[0-9]+\.[0-9]+$ ]] ||
  die "--api must look like 1.18 (got '$api')"
[[ "$sdk_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9.-]+)?$ ]] ||
  die "--sdk-tag must look like v0.1.54 (got '$sdk_tag')"
[[ "$cli_tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9.-]+)?$ ]] ||
  die "--cli-tag must look like v0.1.54 (got '$cli_tag')"

# The gates assert the version without the leading "v".
cli_ver="${cli_tag#v}"

root="${MPSV_UPGRADE_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
plugin="$root/plugin.toml"
cargo_toml="$root/Cargo.toml"
workflows_dir="$root/.github/workflows"
[[ -d "$workflows_dir" ]] ||
  die "not found: $workflows_dir (is MPSV_UPGRADE_ROOT a checkout of this repo?)"

workflow_files=()
while IFS= read -r f; do workflow_files+=("$f"); done < <(
  find "$workflows_dir" -maxdepth 1 \( -name '*.yml' -o -name '*.yaml' \) -print | sort
)
((${#workflow_files[@]} > 0)) || die "no workflow files under $workflows_dir"

for f in "$plugin" "$cargo_toml" "${workflow_files[@]}"; do
  [[ -f "$f" ]] || die "not found: $f"
done

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

# --- staging ----------------------------------------------------------------
# One entry per file, even when a file takes several edits: the diff is shown
# (and the backup taken) per file, not per pin.
plan_file=()
plan_out=()
plan_labels=()   # newline-separated "what changed" lines, one per edit
skip_label=()

rel() { printf '%s' "${1#"$root"/}"; }

# one_line <file> <pattern> <what>
# Echo the single line number matching <pattern>, or die naming every line that
# did match. Zero matches means the file drifted; two or more means the pattern
# is no longer specific enough to edit blind. Both abort before any write.
one_line() {
  local file="$1" pat="$2" what="$3" hits lines
  hits="$(grep -cE "$pat" "$file" || true)"
  if [[ "$hits" != "1" ]]; then
    lines="$(grep -nE "$pat" "$file" | cut -d: -f1 | paste -sd, - || true)"
    die "$(rel "$file"): expected exactly one $what line, found $hits (matching line(s): ${lines:-none})"
  fi
  grep -nE "$pat" "$file" | cut -d: -f1
}

stage_index() {
  local f="$1" i
  for i in "${!plan_file[@]}"; do
    [[ "${plan_file[$i]}" == "$f" ]] && { STAGE_INDEX="$i"; return 0; }
  done
  return 1
}

# stage <file> — sets STAGE_INDEX.
# A global rather than stdout on purpose: the caller runs in this shell, so the
# arrays keep growing. `i=$(stage ...)` would stage inside a subshell and lose
# every append.
stage() {
  local f="$1" i
  if stage_index "$f"; then
    return 0
  fi
  i=${#plan_file[@]}
  plan_file+=("$f")
  plan_out+=("$tmpdir/stage.$i")
  plan_labels+=("")
  cp -f "$f" "${plan_out[$i]}"
  STAGE_INDEX="$i"
}

# edit <file> <lineno> <sed-script-tail> <label>
# Applies `${lineno}<tail>` to the file's staging copy and records the label.
edit() {
  local f="$1" ln="$2" expr="$3" label="$4" i next
  stage "$f"
  i="$STAGE_INDEX"
  next="$tmpdir/stage.$i.next"
  sed -E "${ln}${expr}" "${plan_out[$i]}" > "$next"
  mv -f "$next" "${plan_out[$i]}"
  plan_labels[$i]+="${plan_labels[$i]:+$'\n'}$label"
}

# --- 1. plugin.toml: engines.openidea ---------------------------------------
pat_engine='^[[:space:]]*engines\.openidea[[:space:]]*='
lineno="$(one_line "$plugin" "$pat_engine" "'engines.openidea ='")"
old_engine="$(sed -nE "s/$pat_engine[[:space:]]*\"([^\"]*)\".*/\1/p" "$plugin")"
[[ -n "$old_engine" ]] || die "plugin.toml: 'engines.openidea' line has no quoted value"
new_engine="^${api}"
if [[ "$old_engine" == "$new_engine" ]]; then
  skip_label+=("$(rel "$plugin")  engines.openidea  already \"$new_engine\"")
else
  edit "$plugin" "$lineno" "s@(${pat_engine}[[:space:]]*)\"[^\"]*\"@\\1\"${new_engine}\"@" \
    "engines.openidea  \"$old_engine\" -> \"$new_engine\""
fi

# --- 2. Cargo.toml: the openidea-plugin dependency's tag ---------------------
pat_dep='^[[:space:]]*openidea-plugin[[:space:]]*=[[:space:]]*\{'
lineno="$(one_line "$cargo_toml" "$pat_dep" "'openidea-plugin = {'")"
dep_line="$(sed -n "${lineno}p" "$cargo_toml")"
[[ "$dep_line" =~ tag[[:space:]]*=[[:space:]]*\"([^\"]*)\" ]] ||
  die "Cargo.toml:${lineno}: the openidea-plugin dependency has no tag = \"...\" (a branch/main pin is not this script's job)"
old_sdk="${BASH_REMATCH[1]}"
if [[ "$old_sdk" == "$sdk_tag" ]]; then
  skip_label+=("$(rel "$cargo_toml")  openidea-plugin tag  already \"$sdk_tag\"")
else
  edit "$cargo_toml" "$lineno" "s@(tag[[:space:]]*=[[:space:]]*)\"[^\"]*\"@\\1\"${sdk_tag}\"@" \
    "openidea-plugin tag  \"$old_sdk\" -> \"$sdk_tag\""
fi

# --- 3/4. every workflow: CLI_TAG env line, and the CLI version assertion ----
# The assertion is anchored by its shape (`grep -F "<version>"`), not by the old
# number, so it still matches after a partial run or when the numbers drifted.
pat_cli_tag='^[[:space:]]*CLI_TAG[[:space:]]*:'
pat_cli_gate='^[[:space:]]*grep[[:space:]]+-F[[:space:]]*"?[0-9]+\.[0-9]+\.[0-9]+'
old_cli_values=()   # CLI_TAG values we replaced, for the leftover scan
tag_sites=0
gate_sites=0

for wf in "${workflow_files[@]}"; do
  # CLI_TAG
  tag_hits="$(grep -cE "$pat_cli_tag" "$wf" || true)"
  if ((tag_hits > 1)); then
    lines="$(grep -nE "$pat_cli_tag" "$wf" | cut -d: -f1 | paste -sd, -)"
    die "$(rel "$wf"): expected at most one 'CLI_TAG:' line, found $tag_hits (line(s): $lines)"
  fi
  if ((tag_hits == 1)); then
    tag_sites=$((tag_sites + 1))
    lineno="$(one_line "$wf" "$pat_cli_tag" "'CLI_TAG:'")"
    old_cli="$(sed -nE "s/$pat_cli_tag[[:space:]]*([^[:space:]]+).*/\1/p" "$wf")"
    [[ -n "$old_cli" ]] || die "$(rel "$wf"):${lineno}: 'CLI_TAG:' line has no value"
    if [[ "$old_cli" == "$cli_tag" ]]; then
      skip_label+=("$(rel "$wf")  CLI_TAG  already $cli_tag")
    else
      old_cli_values+=("${old_cli#v}")
      edit "$wf" "$lineno" "s@(${pat_cli_tag}[[:space:]]*)[^[:space:]]+@\\1${cli_tag}@" \
        "CLI_TAG  $old_cli -> $cli_tag"
    fi
  fi

  # grep -F "<version>" — the assertion that the downloaded binary is that CLI
  gate_hits="$(grep -cE "$pat_cli_gate" "$wf" || true)"
  if ((gate_hits > 1)); then
    lines="$(grep -nE "$pat_cli_gate" "$wf" | cut -d: -f1 | paste -sd, -)"
    die "$(rel "$wf"): expected at most one CLI version assertion ('grep -F \"<version>\"'), found $gate_hits (line(s): $lines)"
  fi
  if ((gate_hits == 1)); then
    gate_sites=$((gate_sites + 1))
    lineno="$(one_line "$wf" "$pat_cli_gate" "CLI version assertion ('grep -F')")"
    old_gate="$(sed -nE "s/^[[:space:]]*grep[[:space:]]+-F[[:space:]]*\"?([0-9]+\.[0-9]+\.[0-9]+).*/\1/p" "$wf")"
    [[ -n "$old_gate" ]] || die "$(rel "$wf"):${lineno}: could not read the version from the grep -F assertion"
    if [[ "$old_gate" == "$cli_ver" ]]; then
      skip_label+=("$(rel "$wf")  CLI version assertion  already \"$cli_ver\"")
    else
      old_cli_values+=("$old_gate")
      edit "$wf" "$lineno" "s@(grep[[:space:]]+-F[[:space:]]*\"?)[0-9]+\\.[0-9]+\\.[0-9]+@\\1${cli_ver}@" \
        "CLI version assertion  \"$old_gate\" -> \"$cli_ver\""
    fi
  fi
done

if ((tag_sites == 0)); then
  die "no 'CLI_TAG:' line in any of ${#workflow_files[@]} workflow file(s) under $(rel "$workflows_dir") — refusing to leave the CLI pin behind"
fi
if ((gate_sites == 0)); then
  say "note: no CLI version assertion ('grep -F \"<version>\"') in any workflow; only CLI_TAG will move"
fi

# --- nothing to do ----------------------------------------------------------
if ((${#plan_file[@]} == 0)); then
  for line in "${skip_label[@]}"; do say "$line"; done
  say "nothing to do: all pins already hold the requested values (API $new_engine, SDK $sdk_tag, CLI $cli_tag)"
  exit 0
fi

for line in "${skip_label[@]}"; do say "$line"; done
for i in "${!plan_file[@]}"; do
  printf '\n== %s ==\n' "$(rel "${plan_file[$i]}")"
  while IFS= read -r l; do [[ -n "$l" ]] && printf '   %s\n' "$l"; done <<<"${plan_labels[$i]}"
  diff -u --label "a/$(rel "${plan_file[$i]}")" --label "b/$(rel "${plan_file[$i]}")" \
    "${plan_file[$i]}" "${plan_out[$i]}" || true
done

if ((dry_run)); then
  printf '\nupgrade-plugin-api: dry run, %d file(s) would change, nothing written\n' "${#plan_file[@]}"
  exit 0
fi

# --- write, back up, read back ----------------------------------------------
# Backups live in $tmpdir: a crash mid-run must not leave *.bak next to the
# sources. A failure anywhere restores all of them, so a half-applied upgrade
# cannot be left behind.
written=()
backup_from=()
backup_copy=()

restore_backups() {
  local i
  for i in "${!backup_from[@]}"; do
    cp -f "${backup_copy[$i]}" "${backup_from[$i]}" ||
      printf 'upgrade-plugin-api: could not restore %s\n' "${backup_from[$i]}" >&2
  done
}

ok=1
for i in "${!plan_file[@]}"; do
  f="${plan_file[$i]}"
  snap="$tmpdir/backup.$i"
  if ! cp -f "$f" "$snap"; then ok=0; break; fi
  backup_from+=("$f"); backup_copy+=("$snap")
  if ! cp -f "${plan_out[$i]}" "$f"; then ok=0; break; fi
  written+=("$f")
done

if ((ok == 0)); then
  restore_backups
  die "write failed; original files restored"
fi

# Read the pins back from disk: the sed must have landed where intended.
read_back_engine="$(sed -nE "s/$pat_engine[[:space:]]*\"([^\"]*)\".*/\1/p" "$plugin")"
read_back_sdk="$(sed -nE "s/$pat_dep.*(tag[[:space:]]*=[[:space:]]*\"[^\"]*\").*/\1/p" "$cargo_toml")"
[[ "$read_back_engine" == "$new_engine" ]] || {
  restore_backups; die "read-back mismatch: plugin.toml engines.openidea = '$read_back_engine', wanted '$new_engine'; originals restored"; }
[[ "$read_back_sdk" == "tag = \"$sdk_tag\"" ]] || {
  restore_backups; die "read-back mismatch: Cargo.toml = '$read_back_sdk', wanted 'tag = \"$sdk_tag\"'; originals restored"; }

for wf in "${workflow_files[@]}"; do
  if (( $(grep -cE "$pat_cli_tag" "$wf" || true) == 1 )); then
    v="$(sed -nE "s/$pat_cli_tag[[:space:]]*([^[:space:]]+).*/\1/p" "$wf")"
    [[ "$v" == "$cli_tag" ]] || {
      restore_backups; die "read-back mismatch: $(rel "$wf") CLI_TAG = '$v', wanted '$cli_tag'; originals restored"; }
  fi
  if (( $(grep -cE "$pat_cli_gate" "$wf" || true) == 1 )); then
    v="$(sed -nE "s/^[[:space:]]*grep[[:space:]]+-F[[:space:]]*\"?([0-9]+\.[0-9]+\.[0-9]+).*/\1/p" "$wf")"
    [[ "$v" == "$cli_ver" ]] || {
      restore_backups; die "read-back mismatch: $(rel "$wf") version assertion = '$v', wanted '$cli_ver'; originals restored"; }
  fi
done

printf '\n'
say "updated ${#written[@]} file(s):"
for i in "${!plan_file[@]}"; do
  while IFS= read -r l; do [[ -n "$l" ]] && printf '  %s  %s\n' "$(rel "${plan_file[$i]}")" "$l"; done <<<"${plan_labels[$i]}"
done

# --- 5. what the upgrade cannot fix, said out loud ---------------------------
# A workflow line that still names an old CLI version but is not a comment is
# an assertion this script failed to recognise (its shape changed). That is the
# failure mode worth shouting about.
old_versions=()
for v in "${old_cli_values[@]}"; do
  dup=0
  for seen in ${old_versions[@]+"${old_versions[@]}"}; do [[ "$seen" == "$v" ]] && dup=1; done
  ((dup == 0)) && old_versions+=("$v")
done
uncovered=""
for wf in "${workflow_files[@]}"; do
  for v in ${old_versions[@]+"${old_versions[@]}"}; do
    esc="$(printf '%s' "$v" | sed 's/\./\\./g')"
    hits="$(grep -nE -- "$esc" "$wf" |
      sed -E 's/^([0-9]+):(.*)$/\1\t\2/' |
      awk -F'\t' '$2 !~ /^[[:space:]]*#/ { print $1": "$2 }' || true)"
    if [[ -n "$hits" ]]; then
      uncovered+="$(rel "$wf")"$'\n'
      uncovered+="$(printf '%s\n' "$hits" | sed 's/^/  /')"$'\n'
    fi
  done
done
if [[ -n "$uncovered" ]]; then
  printf '\n'
  say "WARNING: these workflow lines still name an old CLI version and are not comments."
  say "         They are assertions the script did not recognise — fix them by hand:"
  printf '%s' "$uncovered" | sed "s@^$root/@  @"
fi

# (b) Comments and prose that still carry the old numbers. Informational: the
#     script does not edit prose, and a stale comment is not a failing gate.
prose="$(grep -nE -- "0\.1\.51|\\^?1\.15" \
  "$cargo_toml" "$root/scripts/README.md" "$workflow_files" 2>/dev/null |
  grep -vE 'upgrade-plugin-api|Cargo.lock' | head -10 || true)"
if [[ -n "$prose" ]]; then
  printf '\n'
  say "note: prose still mentions an old pin (comments/docs, not changed here):"
  printf '%s\n' "$prose" | sed "s@^$root/@  @" | sed 's@^@@'
fi

# --- 6. optional Cargo.lock refresh -----------------------------------------
if ((update_lock)); then
  printf '\n'
  say "running cargo update -p openidea-plugin (network; can take tens of seconds)"
  ( cd "$root" && cargo update -p openidea-plugin ) ||
    die "cargo update failed; plugin.toml/Cargo.toml/workflows are already updated, run it again by hand"
  say "Cargo.lock refreshed"
fi

printf '\n'
say "next: cargo fmt --all --check && cargo test"
say "      openidea-plugin build . && openidea-plugin check ."
if ((!update_lock)); then
  say "      (or rerun with --update-lock to refresh Cargo.lock here)"
fi
