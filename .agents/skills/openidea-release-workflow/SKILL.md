---
name: openidea-release-workflow
description: OpenIDEA 插件（plugin.toml + Rust/wasm）双通道发行作业法——本地自验不打包，tag 驱动 CI 成为唯一构建者，CI 出包后双端上传并做三方 sha256 回读，最后回填登记条目。当用户说「发版」「发行正式版」「打个版本」「更新发行说明」「回填登记 sha256」「Gitee 镜像失败」时使用。覆盖版本号日期制、CHANGELOG 段名、tag 形态、单一构建者约束、Gitee token 权限、三方校验与回填的完整 SOP。
---

# OpenIDEA 插件双通道发行

改编自原版 IntelliJ 仓的 `release-workflow` skill，按本仓事实重写：本仓没有 Gradle、没有 Marketplace、没有自更新通道，
发版链路是 **tag → GitHub Actions（`release.yml`）→ GitHub Release + Gitee 镜像**。

规则正本：`docs/规范/版本号规范-说明文档.md`。冲突时以它为准，本文是执行版。

## 0 一条铁律：只有一个构建者

**发布包由 CI 构建，本地绝不打包。**

2026-11-29 实测教训：本地 `release-smoke.sh` 在 Windows 上 build+pack 一个包，CI 的 `release.yml` 又在 Ubuntu 上
build+pack 另一个。同一份源码在两个平台上编出的 wasm **不逐字节相同**——同源下包体 1433699 vs 1433571 字节，
sha256 分别 `681a61ac…`（本地）与 `241b0a2b…`（CI）。登记表填本地哈希，与 GitHub 资产对不上；
`release.yml` 里那条 `Both mirrors must match the built sha256` 本来就是防这个的，却因为排在 Gitee 上传之后被连带跳过。

所以：

- `scripts/release-smoke.sh` 只跑 `fmt` / 版本号门禁 / 清单门禁 / `cargo test` / `build` / `check`，**不产出包**。
- 装机的包从 GitHub Release 或 Gitee 发行版的附件下，那就是登记表里的同一份字节。
- 想在本机试装，用 `dist/plugin.wasm` 走 link 模式，别自己打包。

**推论**：本地包哈希与 CI 包哈希必然不同，这不是 bug。任何「本地包和线上包哈希应该一致」的期望都是错的。

## 1 版本号与 tag 形态

| 项 | 规则 |
|---|---|
| 格式 | `YYYY.M.D`，月日**不补前导零**（`2026.11.29`，不是 `2026.11.09`） |
| 位置 | `plugin.toml` 的 `version`；`Cargo.toml` 的 `package.version` 必须**逐字一致** |
| tag | 即版本号原文，**不带 `v` 前缀** |
| 频率 | 一日一版；同日热修并入次日，禁四段（`2026.11.29.1`） |
| `-beta.N` | 规则保留**未启用**——本仓无自更新通道，后缀没有承载物 |

版本号还出现在两处，必须同步改：`settings/about.json` 与 `src/settings.rs` 的 `about.buildNumber` 前缀
（`cargo test settings::` 有守卫，漏改会红）。

门禁：`bash scripts/check-version-format.sh`（CI 的 static 作业也跑）。它校验两处逐字一致 + 日期制形态 + 月日范围，
15 条自测在 `scripts/check-version-format.test.sh`。

**为什么需要这道门禁**：宿主只读 `plugin.toml`，漏改 `Cargo.toml` 时 `openidea-plugin check` 仍绿，
而 wasm 里的 `CARGO_PKG_VERSION`（About 行、`build-info` 输出）会悄悄停在旧值，只在用户报障时才发现。

## 2 发版前置检查

1. `git status --porcelain` 干净、在 master。
2. 两处版本号一致：`bash scripts/check-version-format.sh` 退出 0。
3. 版本号 = 今天（规范要求「版本号即发布当天日期」）。跨午夜时问用户要哪个日期，别自己猜。
4. `git tag -l <版本>` 为空（已存在会被 `release.yml` 拒绝或需要强推）。
5. 本地自验全绿：`bash scripts/release-smoke.sh`（加 `--fast` 跳过全量测试）。
6. `openidea-plugin check .` 回显的版本号正确。

## 3 CHANGELOG 与 change-notes

`release.yml` 按 `awk` 从 `CHANGELOG.md` 提取 `## [<版本>]` 段作为 Release 正文。**段名必须等于版本号原文。**

两条用户口径：

- 一条一行、一版 **≤8 条**（升级弹窗一眼看完）。
- `plugin.toml` 的 `change-notes` 是**详情页「新功能」**的内容，与 CHANGELOG 各自维护；改版时两处都要看。

**拆段的坑**（2026-11-29 踩到）：首次发版时 `CHANGELOG.md` 只有一个 `[Unreleased]` 段，里面堆着已发布版本的功能清单。
若整段改名成新版本号，发行说明会把**旧功能说成新增**。正确做法是把历史清单归到它真正属于的那个版本段，新段只写本次真实变更。

**验证提取结果**（改完必跑，别只看文件）：
```sh
TAG=2026.11.29; awk -v ver="$TAG" '
  BEGIN { want = "## [" ver "]"; p = 0 }
  { if (p && substr($0, 1, 4) == "## [") exit
    if (index($0, want) == 1) { p = 1; next }
    if (p) print }' CHANGELOG.md
```

## 4 发行提交与 tag

```sh
git add <改动>
git commit -F .local/msg-<主题>.txt     # 禁 -m 带反引号，Git Bash 会吃掉
git push github master
git push origin master
git tag <版本>                          # 不带 v
git push github <版本>                  # 这一步触发 release.yml
```

**tag 必须指向包含全部改动的那个提交。** 如果打了 tag 之后又改了要发的东西，`git tag -f <版本> <新提交>` + `git push -f github <版本>`。

推送凭据：GitHub 走 `gh`（本机已 `gh auth setup-git`），Gitee 走仓库级令牌。
推 Gitee 报 TLS 握手失败时用 `git -c http.proxy= -c https.proxy= push origin master` 直连。

## 5 CI 发行轮（唯一构建者）

tag 推送后 `release.yml` 自动跑，三个作业：

| 作业 | 做什么 |
|---|---|
| `build + sign + verify` | `cargo` 构建 wasm → `scripts/release.sh`（build + 签名 pack + sha256 + check）→ 上传 artifact |
| `GitHub Release` | 建 Release、挂包与 `.sha256`、**回读资产校 sha256** |
| `Gitee mirror` | 调 Gitee API 建发行版、传同样两个附件、**`Both mirrors must match the built sha256`** |

读数（结论只认 `conclusion`，不认 `gh run watch` 的退出码）：
```sh
gh run list --workflow=release.yml --limit 3
gh run view <run_id> --json conclusion,status,jobs
gh run view <run_id> --log-failed          # 红了先看这个
```

CI 一轮约 3~5 分钟，用后台任务跑 `gh run watch <run_id> --exit-status`，别前台干等。

## 6 三方 sha256 校验（不执行不算发布完成）

```sh
# CI 自己报的
gh run view <run_id> --log | grep -E "sha256 " | tail -1

# GitHub 资产
gh release view <版本> --json assets    # digest 字段即 sha256

# 两端下载物各算一遍（别复用本地文件）
TOKEN=$(python -c "import json;print(json.load(open('.local/gitee-token.json'))['gitee_personal_access_token'])")
curl -fsSL -o /tmp/gh.oiplugin   "https://github.com/<owner>/<repo>/releases/download/<版本>/<id>-<版本>.oiplugin"
curl -fsSL -o /tmp/ge.oiplugin   "https://gitee.com/<owner>/<repo>/releases/download/<版本>/<id>-<版本>.oiplugin"
sha256sum /tmp/gh.oiplugin /tmp/ge.oiplugin
```

**三处一致才算发布完成**：CI 自报 = GitHub 资产 = Gitee 资产。任一不等就是 blocker，先查是不是又出现了第二个构建者。

## 7 登记条目回填

`docs/registry/plugins.songrenshuo.mybatis-plus-sql-visualizer.toml` 加一个 `[[versions]]`：

```toml
[[versions]]
version = "2026.11.29"
package = "https://github.com/<owner>/<repo>/releases/download/2026.11.29/<id>-2026.11.29.oiplugin"
sha256 = "<CI 那轮的实际值>"
```

三处要同步（哈希对不上注册仓 CI 直接拒收）：登记表、`market/official-pr/registry-entry.json`、
`market/official-pr/upstream/plugins/<id>.toml`。

**索引允许多版本混列**（`crates/ide-extension/src/package.rs` 的 `offered()` 按 id 分组用 semver 选最新），
所以旧条目不必删；但**删了 tag 就必须删条目**——指向已删包的条目会让下载 404。

## 8 已踩过的坑

| 坑 | 事实 |
|---|---|
| **两个构建者** | 见 §0。本地包与 CI 包哈希必然不同 |
| **Gitee 400** | CI 的 `GITEE_TOKEN` secret 必须能写**本仓**。原版仓的 token 若「选择仓库」范围不含本仓，建 release 报 `curl: (22) ... 400`；手动用有效 token 复现能成功即证明是 token 问题。换法：`echo "$TOKEN" \| gh secret set GITEE_TOKEN` |
| **删 tag 连带删发行版** | Gitee 的发行版挂在 tag 上，`git push origin :refs/tags/<tag>` 会一并清掉发行版，无需单独调 releases 接口（实测 404 确认已不存在） |
| **`Both mirrors` 被跳过** | 它排在 Gitee 上传之后，Gitee 一红这步就 skipped——三方校验形同虚设。所以 Gitee token 必须一次修对 |
| **tag glob** | `release.yml` 用 `tags: ['[0-9]*']` 匹配数字开头的版本 tag。字符类在此处可用（`v[0-9]*` 在生产验证过） |
| **`gh release create` 接受无 v tag** | 实测通过（2026-11-29 首轮） |
| **跨午夜** | 实施跨过午夜时版本号该填哪天，是用户决定，别自己猜 |

## 9 Gitee API 速查

```sh
TOKEN=$(python -c "import json;print(json.load(open('.local/gitee-token.json'))['gitee_personal_access_token'])")
API="https://gitee.com/api/v5/repos/songrenshuo/openidea-mybatis-plus-sql-visualizer"

# 列发行版
curl -s "$API/releases?access_token=${TOKEN}&per_page=100"
# 建发行版（target_commitish 必填，GitHub 不要）
curl -s -X POST "$API/releases" --data-urlencode "access_token=${TOKEN}" \
  --data-urlencode "tag_name=<版本>" --data-urlencode "target_commitish=master" \
  --data-urlencode "name=<版本>" --data-urlencode "body=<正文>"
# 传附件
curl -s -X POST "$API/releases/<id>/attach_files" -F "access_token=${TOKEN}" -F "file=@<包>"
# 删发行版
curl -s -X DELETE "$API/releases/<id>?access_token=${TOKEN}"
```

令牌存 `.local/gitee-token.json`（`.local/*` 已被 gitignore，唯一放行 `README.md`）。**绝不入库、不写进日志。**

## 10 不做

- 不擅自 push tag / 发 Release / 提登记 PR——需用户明确授权。
- 不改已发布版本的 tag（下载地址依赖它）；只有**未发布**的试验版可撤。
- 不在本地打包（§0）。
- 不把 `engine/ENGINE_VERSION` 绑到本仓版本号——那是引擎 jar 自己的版本，归 IntelliJ 仓管。
