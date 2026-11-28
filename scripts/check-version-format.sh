#!/usr/bin/env bash
# 版本号门禁：plugin.toml 与 Cargo.toml 的 version 必须逐字一致，且落在本仓
# 的 YYYY.M.D 日期制上（规则见 docs/规范/版本号规范-说明文档.md）。
#
# 用法：scripts/check-version-format.sh [仓库根]
#       缺省为脚本所在仓库。
#
# 为什么需要这道门禁：两处版本号一致此前全靠人肉同步。漏改 Cargo.toml 的后果
# 不是构建失败，而是 wasm 里的 CARGO_PKG_VERSION（About 行的版本段、build-info
# 的输出）悄悄停在旧值，只在用户报障时才发现。宿主只校验 plugin.toml，所以
# `openidea-plugin check` 绿着也说明不了 Cargo.toml 对没对上。
#
# 判定：
#   1. 两个 version 都存在且逐字相同
#   2. 匹配 ^[0-9]{4}\.[0-9]{1,2}\.[0-9]{1,2}$，或该形态后跟 -beta.<数字>
#      日期制不接受前导零（2026.08.13）、四段（2026.11.28.1）与字母段——
#      这两点宿主与 Cargo 的 semver 解析都拒绝，本脚本提前在源码侧拦下。
#
# 退出码（稳定——自测脚本断言这三个值）：
#   0  一致且格式合法
#   1  不一致或格式非法
#   2  文件缺失或读不出 version
set -uo pipefail

root="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
manifest="$root/plugin.toml"
cargo_toml="$root/Cargo.toml"

die() {
  printf 'version: %s\n' "$*" >&2
  exit "$1"
}

# 按段读 version：plugin.toml 的键在文件顶部（第一个表头之前），Cargo.toml 的
# 在 [package] 段里。不按段读会读错文件：
#   - plugin.toml 里 `[[contributes.commands]]` 等块自带自己的键；
#   - Cargo.toml 的第一行就是 [package]，遇到表头即退出的写法会一个键都读不到。
# 所以 `section` 为空表示「第一个表头之前」，否则表示只在 `[section]` 内匹配。
read_version() {
  awk -v section="$2" '
    # 空 section 表示「第一个表头之前」：那些键从文件第一行就生效。
    BEGIN { inside = (section == "") }
    /^[[:space:]]*\[/ {
      if (section == "") exit
      line = $0
      gsub(/[[:space:]]/, "", line)
      inside = (line == "[" section "]")
      next
    }
    inside && $0 ~ "^[[:space:]]*version[[:space:]]*=" {
      line = $0
      sub(/^[^=]*=[[:space:]]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$1"
}

[[ -f "$manifest" ]] || die 2 "plugin.toml not found: $manifest"
[[ -f "$cargo_toml" ]] || die 2 "Cargo.toml not found: $cargo_toml"

plugin_version="$(read_version "$manifest" "")"
cargo_version="$(read_version "$cargo_toml" "package")"

[[ -n "$plugin_version" ]] || die 2 "plugin.toml has no top-level version"
[[ -n "$cargo_version" ]] || die 2 "Cargo.toml has no top-level version"

if [[ "$plugin_version" != "$cargo_version" ]]; then
  die 1 "version mismatch: plugin.toml has $plugin_version, Cargo.toml has $cargo_version
  Both name the same build; edit them together."
fi

# YYYY.M.D，月与日不补前导零。beta 后缀规则保留未启用，但仍接受合法形态，
# 免得将来启用时这道门禁先红。
if [[ ! "$plugin_version" =~ ^[0-9]{4}\.([0-9]|[1-9][0-9])\.([0-9]|[1-9][0-9])(-beta\.[0-9]+)?$ ]]; then
  die 1 "version is not a YYYY.M.D date version: $plugin_version
  Expected e.g. 2026.11.28 (no leading zeros, three numeric parts, optional -beta.N).
  See docs/规范/版本号规范-说明文档.md."
fi

prefix="${plugin_version%%-beta.*}"
year="${prefix%%.*}"
rest="${prefix#*.}"
month="${rest%%.*}"
day="${rest#*.}"

(( 10#$month >= 1 && 10#$month <= 12 )) ||
  die 1 "version month is out of range: $plugin_version"
(( 10#$day >= 1 && 10#$day <= 31 )) ||
  die 1 "version day is out of range: $plugin_version"
(( 10#$year >= 2020 )) ||
  die 1 "version year looks wrong: $plugin_version"

printf 'version: %s (plugin.toml = Cargo.toml)\n' "$plugin_version"
exit 0
