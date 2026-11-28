# 登记仓 PR 草稿（l0ng-ai/openidea-plugins）

首次上架。**提 PR 前需要用户真机测试通过**——本文件是可提交审阅的最终稿，微调直接改本文件。

## 提交内容

两个文件（内容见同目录两个 .toml）：

1. `publishers/songrenshuo.toml` —— 注册发布者与公钥
2. `plugins/songrenshuo.mybatis-plus-sql-visualizer.toml` —— 插件仓库地址与版本表

## PR 标题

```
Add songrenshuo.mybatis-plus-sql-visualizer
```

## PR 正文

```markdown
Registers a new publisher `songrenshuo` and its first plugin,
`songrenshuo.mybatis-plus-sql-visualizer` — an SQL debugger for OpenIDEA.

### What it does

Static restore of the SQL a MyBatis project will really send: mapper XML,
annotated SQL, iBatis 2.x `<sqlMap>`, and MyBatis-Plus / MPJ Wrapper chains
(joins, join UPDATE/DELETE, subqueries), with parameters filled in. At runtime
it captures real executions through a Java agent, or restores pasted console
logs (MyBatis / iBatis / Hibernate / JdbcTemplate). Every result is checked
(dialect, unsafe writes, performance rules, rollback plan) and carry a
confidence: `unresolved` results return a reason instead of a guessed SQL.

### Source repository

https://github.com/SongRenShuo/openidea-mybatis-plus-sql-visualizer

Apache-2.0. The published package is built from this repository by its own CI
(`.github/workflows/release.yml`, tag-driven), which also runs fmt, clippy,
the full test suite and `openidea-plugin check` on every push.

### Why it needs the capabilities it asks for

Required (agreed at install, no way around them):

- `project.read` — render and check statements against the project's mapper
  XML, Java sources and log files. Read-only; the plugin never writes project
  files.
- `events` — wake the capture consumer on `websocket-ready` instead of
  polling; also gutter/navigation markers. Nothing on disk is touched.
- `storage` — the SQL history tab keeps log snapshots in the plugin's own
  key-value store (per project, max 10 files / 512 KB each).

Optional (each has a working fallback, and every feature that needs one says
so in its UI when it is refused):

- `java.agent` (high) — attaches the plugin's capture agent to Application /
  Spring Boot / JAR run configurations so real parameters and timings arrive
  over a loopback socket. Refused: enhanced capture is unavailable and the
  plugin says so, pointing at the paste path below. The agent jar is shipped
  in the package and pinned by hash by the host.
- `net:127.0.0.1` (medium) — the same capture channel, as a WebSocket client
  to `127.0.0.1:17890`. Nothing leaves the machine; the manifest declares the
  loopback host only, no other network capability.
- `process:java` (high) — starts the bundled, version- and sha256-pinned local
  engine jar used for Wrapper rendering and EXPLAIN wrapping. Refused: the
  plugin degrades to plain-text mode and says so in the UI.
- `clipboard` / `clipboard.read` (medium) — copy a rendered SQL out, and read
  logs pasted from the clipboard. Refused: both buttons explain the refusal
  and the panel's own input box still works.
- `editor.read` (low) — `preview-sql` with no argument reads the current
  editor's selection or text. Refused: path + statement id and the context
  menu still work.

Paste restore (MyBatis / iBatis / Hibernate / JdbcTemplate console logs into
the panel) needs **no** capability at all, so the plugin is useful even with
every optional capability refused.

### How it was verified

- `scripts/release-smoke.sh`: fmt check, the full `cargo test` suite (2,273
  tests), `openidea-plugin build`, signed `pack`, `openidea-plugin check` on
  the signed package, SHA-256.
- The published package was downloaded again and verified: SHA-256 matches
  the attached `.sha256`, `check` passes, `SIGNATURE.json` names this
  publisher and key.
- Manual pass in the IDE from the installed package: five panel tabs, static
  rendering (mapper XML / annotated SQL / Wrapper chain / MPJ join), both the
  capture-denied and capture-granted paths, paste restore, settings pages
  (including the Global / This project switch), and an MCP tool call.

### Versioning

`engines.openidea = "^1.18"`. One version, 0.1.0, first release.
```

## 提交步骤（真机测试通过后）

```bash
gh repo fork l0ng-ai/openidea-plugins --clone=false
gh repo clone <your-fork> /tmp/openidea-plugins
cd /tmp/openidea-plugins
git checkout -b add-songrenshuo-mybatis-plus-sql-visualizer
cp <本仓>/docs/registry/publishers.songrenshuo.toml publishers/songrenshuo.toml
cp <本仓>/docs/registry/plugins.songrenshuo.mybatis-plus-sql-visualizer.toml \
   plugins/songrenshuo.mybatis-plus-sql-visualizer.toml
git add publishers/songrenshuo.toml plugins/songrenshuo.mybatis-plus-sql-visualizer.toml
git commit -F <提交信息文件>
git push -u origin HEAD
gh pr create --repo l0ng-ai/openidea-plugins --title "Add songrenshuo.mybatis-plus-sql-visualizer" --body-file <本文件里 PR 正文那一段>
```

## 审核方会查的点（policy.mdx）

- 源码仓公开、包由该源码构建 ✓（CI tag 驱动发版）
- README 说明插件做什么 + 为什么需要每项高风险能力 ✓（本仓 README 能力表）
- 无混淆/压缩代码 ✓（Rust → wasm 由 CI 构建，源码在仓）
- 登记仓 CI 会下载包、校验 SHA-256、跑 `check`、验签名
- 新发布者与新插件由维护者人工读源码
