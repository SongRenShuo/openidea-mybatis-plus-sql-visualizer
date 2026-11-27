#!/usr/bin/env bash
# check-web-syntax.mjs 的自测。
#
# 为什么必须有：如果这个脚本的正则匹配不到东西（比如 web/ 目录路径写错、
# 内联脚本正则失配），它对真实的语法错误就是瞎的，却仍然 `ok: 0 web script(s)
# parse` 退 0——这就是典型的空绿。下面三档夹具各钉住一条：干净必须绿、
# 模板字符串里出现反引号必须红、HTML 行内脚本语法错必须红。
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
check="$here/check-web-syntax.mjs"
pass=0
fail=0

report() { # report <name> <expected-exit> <actual-exit>
  if [ "$2" = "$3" ]; then
    pass=$((pass + 1))
    printf 'ok   %s\n' "$1"
  else
    fail=$((fail + 1))
    printf 'FAIL %s (expected exit %s, got %s)\n' "$1" "$2" "$3"
  fi
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- 1. 干净夹具：一个正常模块 + 一个正常行内脚本 → 绿 ---------------------
mkdir -p "$work/clean/web"
cat >"$work/clean/web/app.js" <<'JS'
const TEMPLATE = `<div class="x"><!-- no backticks here --></div>`;
export function mount(root) { root.innerHTML = TEMPLATE; }
JS
cat >"$work/clean/web/page.html" <<'HTML'
<!doctype html>
<html><body>
<script type="module">
  import { mount } from './app.js';
  mount(document.body);
</script>
</body></html>
HTML
node "$check" --quiet --root "$work/clean"
report "clean fixture is green" 0 $?

# --- 2. 模板字符串里出现反引号 → 必须红 -----------------------------------
# 这就是 2026-11-27 面板整体失效的形态：HTML 注释写在模板字符串内部，
# 注释里的反引号把字面量提前闭合，后面整段变成非法 JS。
mkdir -p "$work/backtick/web"
cat >"$work/backtick/web/preview.js" <<'JS'
const TEMPLATE = `<div>
  <!-- rendering `table.column` needs the table -->
  <span></span>
</div>`;
export default TEMPLATE;
JS
node "$check" --quiet --root "$work/backtick" 2>/dev/null
report "backtick inside template literal is red" 1 $?

# --- 3. HTML 行内 module 脚本语法错 → 必须红 ------------------------------
# 面板的主要逻辑就在 log-panel.html 的 <script type="module"> 里，
# 这道闸若不覆盖它，"文件级干净" 依然是假绿。
mkdir -p "$work/inline/web"
cat >"$work/inline/web/app.js" <<'JS'
export const ok = 1;
JS
cat >"$work/inline/web/page.html" <<'HTML'
<!doctype html>
<html><body>
<script type="module">
  import { ok } from './app.js';
  if (ok === ) { console.log('broken'); }
</script>
</body></html>
HTML
node "$check" --quiet --root "$work/inline" 2>/dev/null
report "broken inline module script is red" 1 $?

# --- 4. 非 JS 的 <script> 不参与（避免 1.15 不认的 shape 误伤） -------------
mkdir -p "$work/othertype/web"
cat >"$work/othertype/web/page.html" <<'HTML'
<!doctype html>
<html><body>
<script type="application/json">{"a": 1,}</script>
<script type="module">const x = 1;</script>
</body></html>
HTML
node "$check" --quiet --root "$work/othertype"
report "non-JS script type is skipped" 0 $?

# --- 5. 缺 web/ 目录要报错，不能静默通过 ---------------------------------
mkdir -p "$work/empty"
node "$check" --quiet --root "$work/empty" 2>/dev/null
report "missing web/ is an error" 1 $?

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
