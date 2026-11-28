#!/usr/bin/env bash
# 发版冒烟（release smoke）：打一个可上架的签名包并把每一道本地门禁跑完。
#
# 用法（仓库根）：
#   bash scripts/release-smoke.sh              # 全量：fmt/test/build/签名 pack/包校验
#   bash scripts/release-smoke.sh --fast       # 跳过全量 cargo test（只 fmt/build/pack）
#
# 密钥：.local/songrenshuo.key（见 .local/README.md）。缺失时第 5 步失败——
# 签名是登记仓上架的前置，无签名包不发版。
# 产出：<id>-<version>.oiplugin + SHA-256；脚本末尾打印安装态真机走查清单。
set -euo pipefail

KEY=".local/songrenshuo.key"
PUBLISHER="songrenshuo"
SKIP_TESTS=0
[ "${1:-}" = "--fast" ] && SKIP_TESTS=1

say() { printf '\n=== %s\n' "$*"; }

version_of() {
    sed -n 's/^version *= *"\(.*\)".*/\1/p' plugin.toml | head -1
}

id_of() {
    sed -n 's/^id *= *"\(.*\)".*/\1/p' plugin.toml | head -1
}

say "1/6 fmt"
cargo fmt --all --check

if [ "$SKIP_TESTS" -eq 0 ]; then
    say "2/6 全量 cargo test（与 CI 同一命令）"
    cargo test
else
    say "2/6 cargo test 跳过（--fast）"
fi

say "3/6 openidea-plugin build（wasm + 清单校验）"
openidea-plugin build

say "4/6 签名 pack"
[ -f "$KEY" ] || { echo "缺发布者密钥 $KEY（openidea-plugin keygen 生成，见 .local/README.md）"; exit 1; }
ID="$(id_of)"
VERSION="$(version_of)"
OUT="${ID}-${VERSION}.oiplugin"
openidea-plugin pack . --key "$KEY" --publisher "$PUBLISHER" --out "$OUT"

say "5/6 校验签名包（按 IDE 安装语义解包校验）"
openidea-plugin check "$OUT"

say "6/6 SHA-256（登记 PR 与 Release 说明都要记录）"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$OUT"
else
    shasum -a 256 "$OUT"
fi

cat <<CHECKLIST

=== 安装态真机走查（人工，发版前必过） ===
1. openidea-plugin unlink（若处于链接态）→ 重启 IDE
2. 插件页齿轮 Install 安装本包 → 权限逐项确认（拒绝/授权两档都走过再发）
3. 重启 IDE：五页签（SQL 日志/日志转 SQL/SQL 转 Java/历史/SQL 预览）各开一遍
4. 静态渲染：mapper XML / 注解 SQL / Wrapper 链 / MPJ 联表各验一条
5. 增强：授权 java.agent + net 后点捕捉启动应用；拒绝时确认报「增强捕获不可用」
6. 粘贴兜底：控制台日志粘进「日志转 SQL」还原成功
7. 设置页：各子页打开、改值 Apply、Global/This project 切换生效
8. MCP：sqlv_render_sql 等只读工具在 MCP 客户端里调用一次
9. 走查通过 → tag + GitHub Release（附件=$OUT + 上面的 SHA-256）→ 登记 PR
CHECKLIST
