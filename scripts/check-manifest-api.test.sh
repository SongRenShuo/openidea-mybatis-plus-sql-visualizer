#!/usr/bin/env bash
# Self-test for scripts/check-manifest-api.sh.
#
# Usage: bash scripts/check-manifest-api.test.sh
#
# Builds fixture manifests in a temp directory (nothing in the repo is touched)
# and asserts the gate's exit code and message. Case c copies the repository's
# real plugin.toml, so it fails the day someone adds an API 1.16+/1.18 key to
# the real manifest without raising engines.openidea — which is the point.
#
# Exit: 0 when every assertion passes, 1 when any fails.
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
gate="$root/scripts/check-manifest-api.sh"

if [[ ! -f "$gate" ]]; then
  echo "fail: gate script missing: $gate" >&2
  exit 1
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/mpsv-manifest-api.XXXXXX")"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0

# check <name> <expected-exit> <manifest> [expected substring of stderr+stdout]
check() {
  local name="$1" want="$2" file="$3" needle="${4:-}" out rc
  out="$(bash "$gate" "$file" 2>&1)"
  rc=$?
  if [[ "$rc" -ne "$want" ]]; then
    echo "fail: $name — exit $rc, want $want"
    sed 's/^/      /' <<< "$out"
    fail=$((fail + 1))
    return
  fi
  if [[ -n "$needle" ]] && ! grep -qF -- "$needle" <<< "$out"; then
    echo "fail: $name — output does not contain: $needle"
    sed 's/^/      /' <<< "$out"
    fail=$((fail + 1))
    return
  fi
  echo "pass: $name (exit $rc)"
  pass=$((pass + 1))
}

# a) a 1.18 key below the floor must be red
cat > "$work/a.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = "^1.15"

[[contributes.commands]]
id = "x"

[[contributes.dialogs]]
id = "d"
title = "D"
web = "web/d.html"
width = 900
height = 600
TOML
check "a: ^1.15 + dialogs is red" 1 "$work/a.toml" \
  'error: contributes.dialogs needs plugin API 1.18, but engines.openidea is "^1.15"'

# b) the same key at the matching floor is green
cat > "$work/b.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = "^1.18"

[[contributes.dialogs]]
id = "d"
title = "D"
web = "web/d.html"
width = 900
height = 600
TOML
check "b: ^1.18 + dialogs is green" 0 "$work/b.toml" "ok (engines.openidea \"^1.18\""

# c) the repository's own manifest, twice: green at the floor it declares, and
#    red when that floor is forced down to ^1.15 — the keys it uses need more.
#    This replaces the earlier "the real manifest has no 1.16+ key" tripwire,
#    which fired for the right reason once the repo moved to 1.18: the case has
#    to describe the manifest as it is now, not as it was when written.
if [[ -f "$root/plugin.toml" ]]; then
  check "c: the real manifest passes at its own floor" 0 "$root/plugin.toml" \
    "ok (engines.openidea"
  sed 's/^[[:space:]]*engines\.openidea[[:space:]]*=.*/engines.openidea = "^1.15"/' \
    "$root/plugin.toml" > "$work/c15.toml"
  check "c: the real manifest at ^1.15 is red" 1 "$work/c15.toml" \
    'error: contributes.dialogs needs plugin API 1.18'
else
  echo "fail: c: repository plugin.toml not found"
  fail=$((fail + 1))
fi

# d) a 1.16 key at a 1.16 floor is green
cat > "$work/d.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = "^1.16"

[[contributes.frames]]
id = "f"
url = "https://example.invalid/"
TOML
check "d: ^1.16 + frames is green" 0 "$work/d.toml" 'floor 1.16'

# e) 1.16 keys under a 1.15 floor are red (the three spellings of that surface)
cat > "$work/e.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = "^1.15"

[[contributes.markdownHover]]
provider = "hover"

[[contributes.customEditors]]
id = "bin"

[[contributes.customEditors.files]]
extensions = ["bin"]

[contributes.customEditors.binary]
magic = "00"

[[contributes.frames]]
id = "f"

[[contributes.overlays]]
id = "o"
TOML
check "e: ^1.15 + frames/markdownHover/customEditors.binary/overlays is red" 1 "$work/e.toml" \
  'error: contributes.customEditors.binary needs plugin API 1.16'
out_e="$(bash "$gate" "$work/e.toml" 2>&1)"
for key in contributes.frames contributes.markdownHover contributes.overlays; do
  if grep -qF -- "$key needs plugin API" <<< "$out_e"; then
    echo "pass: e: $key reported (exit 1)"
    pass=$((pass + 1))
  else
    echo "fail: e: $key was not reported"
    fail=$((fail + 1))
  fi
done

# f) an unrecognised engines form warns but does not fail
cat > "$work/f.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = "*"

[[contributes.dialogs]]
id = "d"
TOML
check "f: unknown engines form warns, exit 0" 0 "$work/f.toml" "warning: cannot determine the lower bound"

# g) a manifest without engines.openidea is an error, not a pass
cat > "$work/g.toml" <<'TOML'
id = "t"
version = "0.0.1"
TOML
check "g: missing engines.openidea is exit 2" 2 "$work/g.toml" "error: plugin.toml has no engines.openidea"

# h) `>=` spelling yields a floor too
cat > "$work/h.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = ">=1.16"

[[contributes.frames]]
id = "f"
TOML
check "h: >=1.16 + frames is green" 0 "$work/h.toml" 'floor 1.16'

# i) the reverse check, kept as a regression: a manifest that declares a dialog
#    entry at ^1.15 must be red, and the same file with a raised floor green.
#    Without this, case c) alone would pass even if the key detector went blind
#    (nothing to find, so nothing to report). Synthetic on purpose: deriving the
#    fixture from the real manifest ties this test to whatever the repo happens
#    to ship today and breaks the moment the repo moves past it.
cat > "$work/i.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = "^1.15"

[[contributes.dialogs]]
id = "sqlv-preview"
title = "SQL Preview"
web = "web/preview.html"
width = 900
height = 600
TOML
check "i: dialogs at ^1.15 is red" 1 "$work/i.toml" \
  'error: contributes.dialogs needs plugin API 1.18'
sed 's/^[[:space:]]*engines\.openidea[[:space:]]*=.*/engines.openidea = "^1.18"/' \
  "$work/i.toml" > "$work/j.toml"
check "j: dialogs at ^1.18 is green" 0 "$work/j.toml" \
  'ok (engines.openidea "^1.18"'

# k) a commented-out higher-version key is not a declaration: this is the
#    natural "park the dialog block until 1.18" move and must stay green.
cat > "$work/k.toml" <<'TOML'
id = "t"
version = "0.0.1"
engines.openidea = "^1.15"   # raised to ^1.18 with the dialog below

# [[contributes.dialogs]]
# id = "sqlv-preview"
# web = "web/preview.html"

[[contributes.commands]]
id = "x"
TOML
check "k: commented-out dialogs stays green" 0 "$work/k.toml" 'floor 1.15, 0 key(s) checked'

echo
echo "pass=$pass fail=$fail"
[[ "$fail" -eq 0 ]] || exit 1
