#!/usr/bin/env bash
# 发版前本地自验（release smoke）：把每一道本地门禁跑完。**不产出发布包。**
#
# 用法（仓库根）：
#   bash scripts/release-smoke.sh            # 全量：fmt / 门禁脚本 / 全量 test / build / check
#   bash scripts/release-smoke.sh --fast     # 跳过全量 cargo test
#
# 为什么不再在这里打签名包
# ------------------------
# 2026-11-29 之前，这个脚本用本机密钥打一个签名包，CI 的 release.yml 又在
# Ubuntu 上重新 build + pack 了**另一个**包——同一份源码在 Windows 与 Linux
# 上编出的 wasm 并不逐字节相同（实测差 128 字节，包 sha256 分别
# 681a61ac… 与 241b0a2b…）。登记表填的是本地那个哈希，于是与 GitHub 资产
# 对不上，`Both mirrors must match the built sha256` 那一步就失去了意义。
#
# 现在的分工：
#   - 本地：只验证源码是好的（fmt / 测试 / build / check）。跑完给你的是读数，
#     不是可发布的包。
#   - CI：release.yml 是**唯一**的构建者。它 build + 签名 + pack + 校验哈希 +
#     双端上传 + 回读。你要装的包，从那条 workflow 的 Release 附件下载。
#
# 想在本机试装，用 `dist/plugin.wasm` 走 link 模式，或直接下 CI 的附件。
set -euo pipefail

SKIP_TESTS=0
[ "${1:-}" = "--fast" ] && SKIP_TESTS=1

say() { printf '\n=== %s\n' "$*"; }

say "1/5 代码格式"
cargo fmt --all --check

say "2/5 版本号与清单门禁"
bash scripts/check-version-format.sh
bash scripts/check-manifest-api.sh

if [ "$SKIP_TESTS" -eq 0 ]; then
    say "3/5 全量 cargo test（与 CI 同一命令）"
    cargo test
else
    say "3/5 cargo test 跳过（--fast）"
fi

say "4/5 构建组件（只验能编过，不打包）"
openidea-plugin build

say "5/5 按 IDE 的安装语义读一遍清单"
openidea-plugin check .

cat <<'CHECKLIST'

=== 发版走查（人工） ===
本地这一轮只证明源码是好的。发布包由 CI 产出，流程是：

1. 改 plugin.toml 与 Cargo.toml 的 version（YYYY.M.D，两处必须一致）
2. CHANGELOG.md 的段名改成同一天，例如 `## [2026.11.29]`
3. 提交并推 master
4. git tag <版本号>（**不带 v 前缀**）→ git push github <版本号>
5. release.yml 自动 build + 签名 + 双端上传；等它绿
6. 从 GitHub Release 或 Gitee 发行版下载附件安装走查：
   - 五个页签各开一遍
   - 静态渲染：mapper XML / 注解 SQL / Wrapper 链 / MPJ 联表各一条
   - 增强捕获：授权 java.agent + net 后启动应用；拒绝档确认报「增强捕获不可用」
   - 粘贴兜底：控制台日志粘进「日志转 SQL」还原成功
   - 设置页各子页打开、改值 Apply、Global/This project 切换
   - MCP：sqlv_render_sql 等只读工具调一次
7. 把 CI 那轮打印的 sha256 回填进 docs/registry/ 的 [[versions]]
   —— 它必须与 Release 附件、Gitee 附件的哈希相同，三方一致才算发布完成
CHECKLIST
