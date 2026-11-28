#!/usr/bin/env bash
# scripts/check-version-format.sh 的自测。
#
# 用法：bash scripts/check-version-format.test.sh
#
# 在临时目录里造夹具仓库（不碰仓库本体），断言门禁的退出码与文案。
# 用例 c 复制仓里真的 plugin.toml 与 Cargo.toml，所以只要有人把两处版本号
# 改岔、或把日期制改回 0.x，这条就红——这正是它的用处。
#
# 退出：全部断言通过为 0，任一失败为 1。
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
gate="$root/scripts/check-version-format.sh"

if [[ ! -f "$gate" ]]; then
  echo "fail: gate script missing: $gate" >&2
  exit 1
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/mpsv-version-format.XXXXXX")"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0

# check <用例名> <期望退出码> <仓库目录> [输出里应出现的子串]
check() {
  local name="$1" want="$2" dir="$3" needle="${4:-}" out rc
  out="$(bash "$gate" "$dir" 2>&1)"
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
  pass=$((pass + 1))
}

# fixture <目录名> <plugin.toml 版本> <Cargo.toml 版本>
# 两个版本都写进最小可读的清单：段结构要与真文件一致，否则测不到真实形状。
fixture() {
  local dir="$work/$1" pv="$2" cv="$3"
  mkdir -p "$dir"
  {
    echo 'id = "songrenshuo.probe"'
    echo 'name = "Probe"'
    printf 'version = "%s"\n' "$pv"
    echo 'engines.openidea = "^1.18"'
    echo
    echo '[[contributes.commands]]'
    echo 'id = "probe"'
    echo 'title = "Probe"'
  } > "$dir/plugin.toml"
  {
    echo '[package]'
    echo 'name = "probe"'
    printf 'version = "%s"\n' "$cv"
    echo 'edition = "2024"'
    echo
    echo '[dependencies]'
    echo 'serde = "1"'
  } > "$dir/Cargo.toml"
}

# --- a: 合法日期版本，两处一致 ------------------------------------------------
fixture a 2026.11.28 2026.11.28
check "a: a date version in both files passes" 0 "$work/a" "2026.11.28"

# 月与日不补零的边界写法也得过：同日只差位数，测的是正则没把个位数拒了。
fixture a2 2026.1.9 2026.1.9
check "a2: single-digit month and day pass" 0 "$work/a2" "2026.1.9"

# --- b: 两处不一致 ------------------------------------------------------------
fixture b 2026.11.28 2026.11.27
check "b: differing versions fail" 1 "$work/b" "version mismatch"

# --- c: 仓里真文件（防回转：有人把日期制改回 0.x 或改岔两处，这条就红） ------
mkdir -p "$work/c"
if [[ -f "$root/plugin.toml" && -f "$root/Cargo.toml" ]]; then
  cp "$root/plugin.toml" "$work/c/plugin.toml"
  cp "$root/Cargo.toml" "$work/c/Cargo.toml"
  check "c: the real manifests pass at their own version" 0 "$work/c"
else
  echo "fail: c: repository manifests not found"
  fail=$((fail + 1))
fi

# --- d: 前导零（宿主与 Cargo 的 semver 都拒） --------------------------------
fixture d 2026.08.13 2026.08.13
check "d: a leading-zero month fails" 1 "$work/d" "not a YYYY.M.D date version"

# --- e: 四段（同日递增后缀，规范明令禁止） -----------------------------------
fixture e 2026.11.28.1 2026.11.28.1
check "e: a four-part version fails" 1 "$work/e" "not a YYYY.M.D date version"

# --- f: 旧的 0.x 形态（切换前的格式，不该再出现） ----------------------------
fixture f 0.1.0 0.1.0
check "f: the pre-switch 0.1.0 form fails" 1 "$work/f" "not a YYYY.M.D date version"

# --- g: 字母段 ----------------------------------------------------------------
fixture g 2026.11.28.M19 2026.11.28.M19
check "g: a lettered version fails" 1 "$work/g" "not a YYYY.M.D date version"

# --- h: beta 后缀（规则保留，合法形态不该被拦） ------------------------------
fixture h 2026.11.28-beta.1 2026.11.28-beta.1
check "h: a -beta.N suffix passes" 0 "$work/h" "2026.11.28-beta.1"

# --- i: 月份越界 --------------------------------------------------------------
fixture i 2026.13.1 2026.13.1
check "i: month 13 fails" 1 "$work/i" "month is out of range"

# --- j: 日号越界 --------------------------------------------------------------
fixture j 2026.11.32 2026.11.32
check "j: day 32 fails" 1 "$work/j" "day is out of range"

# --- k: 缺 version ------------------------------------------------------------
mkdir -p "$work/k"
printf 'id = "p"\nname = "P"\nengines.openidea = "^1.18"\n' > "$work/k/plugin.toml"
printf '[package]\nname = "p"\nedition = "2024"\n' > "$work/k/Cargo.toml"
check "k: a missing version fails" 2 "$work/k" "has no top-level version"

# --- l: 文件缺失 --------------------------------------------------------------
mkdir -p "$work/l"
check "l: a missing manifest fails" 2 "$work/l" "not found"

# --- m: 只读第一个表头之前 / 只读 [package] —— 段的边界 ----------------------
# plugin.toml 里贡献块自带 version 键时，必须仍取顶部的那个；Cargo.toml 的
# [package] 段在前，取到的必须是它的 version 而不是别的段的同名键。
mkdir -p "$work/m"
{
  echo 'id = "p"'
  echo 'name = "P"'
  echo 'version = "2026.11.28"'
  echo 'engines.openidea = "^1.18"'
  echo
  echo '[[contributes.commands]]'
  echo 'id = "c"'
  echo 'version = "9.9.9"'
} > "$work/m/plugin.toml"
{
  echo '[package]'
  echo 'name = "p"'
  echo 'version = "2026.11.28"'
  echo 'edition = "2024"'
  echo
  echo '[dependencies.foo]'
  echo 'version = "9.9.9"'
} > "$work/m/Cargo.toml"
check "m: section boundaries are respected" 0 "$work/m" "2026.11.28"

# --- n: 段边界反向——贡献块的 9.9.9 被误读时这条会红 ------------------------
# 与 m 同形，但顶部版本非法、块内合法：只有正确按段读才会判红。
mkdir -p "$work/n"
{
  echo 'id = "p"'
  echo 'name = "P"'
  echo 'version = "0.1.0"'
  echo 'engines.openidea = "^1.18"'
  echo
  echo '[[contributes.commands]]'
  echo 'id = "c"'
  echo 'version = "2026.11.28"'
} > "$work/n/plugin.toml"
{
  echo '[package]'
  echo 'name = "p"'
  echo 'version = "0.1.0"'
  echo 'edition = "2024"'
} > "$work/n/Cargo.toml"
check "n: a bad top-level version is not masked by a block" 1 "$work/n" "not a YYYY.M.D date version"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
(( fail == 0 ))
