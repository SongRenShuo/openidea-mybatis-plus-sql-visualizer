# AGENTS.md（OpenIDEA 插件仓）

本仓是 IntelliJ 版 `mybatis-plus-sql-visualizer` 的 OpenIDEA 端口。开始改动前按顺序读：

1. `docs/ROADMAP.md`——阶段、退出门禁、待证伪前提（**排期与验收的唯一权威**）。
2. `docs/ARCHITECTURE.md`——平台硬约束、双路径、引擎契约、置信度模型。
3. `docs/FEATURES.md`——201 条四档范围，19 条明确不做。
4. `docs/LIMITS.md`——对用户明示的不支持项。

能力边界与证据在 **IntelliJ 仓**：`docs/调研报告/20261125-1336-OpenIDEA全面支持-调研报告.md`（提交 `816e8c0a0`）+ `docs/调研报告/20261125-openidea-support/`。两处冲突：**能力以该报告为准，排期以本仓 ROADMAP 为准。**

## 基线与工具链

| 项 | 值 |
| --- | --- |
| 插件 API | `engines.openidea = "^1.15"`；**不得**使用 1.16 特性（`frames`、`markdown_hover`、`customEditors.binary`） |
| SDK | `openidea-plugin = { git = "...", tag = "v0.1.55" }`（插件 API 仍 1.18）；脚手架用 0.1.51 的 CLI 生成 |
| Rust | 1.98.1（`rust-toolchain.toml`），target `wasm32-wasip2` |
| Node | 22.22.3（`.nvmdrc`，NVM Desktop 目录级切换） |
| 版本号 | `YYYY.M.D` 日期制（与原版仓一致），tag 不带 `v`；规则见 `docs/规范/版本号规范-说明文档.md` |

## 门禁（本地定向，全量交 CI）

完整门禁**不在本机跑**：公开仓的标准 runner 免费，而十几条车道各自编译 wasm SDK 会把内存与磁盘打满。分三层：

| 层 | 在哪跑 | 内容 |
| --- | --- | --- |
| 车道内 | 本地 | `cargo fmt --all --check` + 只跑本模块测试（`cargo test <module>`）；不跑 clippy、不跑全量 |
| 合并点 | 本地 | 一次 `cargo test`（增量）+ 一次 `openidea-plugin build && openidea-plugin check .`（约 5 s，缓存后） |
| **改 `web/**` 时** | 本地 | `node scripts/check-web-syntax.mjs`。Rust 侧从不读 `web/`，`cargo` 全绿证明不了面板能跑：一次 JS 解析失败（例如模板字符串里出现反引号）就让整个工具窗没有页签、没有回调、正文空白 |
| 权威全量 | CI（`.github/workflows/build.yml`） | `fmt` + `clippy -D warnings` + `test` + `build` + `check` + `scripts/verify-engine.sh` + `check-manifest-api` + `check-web-syntax` + 上传 wasm |

- 合并候选不想动 master 时：`git push github HEAD:refs/heads/ci/gate-<n>`，CI 的 `ci/**` 触发已接好，`gh run watch` 读结论。
- 子代理与长命令一律**后台**：Agent 默认异步，派完立刻做下一件事；**禁止**阻塞等待（`TaskOutput(block=true)` 长等、`sleep`、空转）。靠 task-notification 继续；要看进度只 `tail` 一次。
- 超过约 20 秒的命令一律后台跑，靠通知继续；**禁止** `sleep` 轮询。
- `openidea-plugin pack` 不校验组件新鲜度：发布前必须「先 build 再 pack」。
- CI 的 CLI 步骤不是 `continue-on-error`：拿不到 0.1.51 就红，不允许「跳过 build/check 假装绿」。

## 引擎（jar）边界

- 引擎**不复制**进本仓：只认 `engine/ENGINE_VERSION`（版本 + sha256），由 `process:java` 启动。
- 改引擎（`EngineApi` 门面、类型投影规则、方言表）去 **IntelliJ 仓** `sub-projects/engine-jar`，再在新仓升 `ENGINE_VERSION`。
- 引擎返回的是 `messageKey`，不是文案；本仓负责 key → 本地化。
- 版本不匹配或 sidecar 起不来时：**降级为纯文本模式**并在 UI 明示，绝不跑错版本。

## 产品纪律：不猜

每条渲染结果带 `confidence`：`exact` / `partial` / `unresolved`。`unresolved` 不出 SQL，只给原因与补充入口；MCP 工具禁止把 `partial` 当 `exact` 返回。

## 依赖与许可

- 新增 crate 必须许可白名单（Apache-2.0 / MIT / BSD 系）；**禁 GPL/AGPL**。
- 不为小功能引入大依赖；能在 wasm 侧手写的不加 crate。
- 第三方许可变化要同步 `NOTICE`。

## 提交与推送

- 提交信息中文、单一主题；用 `git commit -F <文件>`（**禁 `-m` 带反引号**，Git Bash 会吃掉内容）。
- **不擅自 push / 建 tag / 发 Release**：需用户明确授权。
- GitHub 推送走 `gh` 凭据（本机已 `gh auth setup-git`，不弹 GCM）；Gitee 用仓库级令牌，**新仓必须在令牌的「选择仓库」范围内**，否则 403 且提示同「无权限」。
- 禁止裸推到未配置的 remote；`github` remote 固定 https。

## 能力（capabilities）

新增能力必须同时：① 在 `plugin.toml` 声明；② 在 `README.md` 能力表写「为什么需要」；③ 高风险能力（`process`/`java.agent`/`fs.write`）标 optional，缺省时必须有可用降级。

## 已知平台坑（别重踩）

| 坑 | 事实 |
| --- | --- |
| provider 预算 | 软 50 ms / 硬 500 ms；每次传整文档 → 必须增量 + 缓存 |
| 无 PSI | 类型/成员查询官方推到 API 2.0；`textDocument/*` 对插件封死 |
| `database` 不执行 SQL | EXPLAIN/预览执行只能走 sidecar |
| 读不到运行控制台 | 捕获只能 `java.agent` 或「导出日志 + 读文件」 |
| 无 provider 失效接口 | 设置改完不重开文件不生效，设置页要写明 |
| `web/**` 的 JS 没有任何 cargo 门禁 | 面板逻辑在 `web/*.js` 与 `log-panel.html` 的行内 `<script type="module">` 里，Rust 侧从不读它们。一次解析失败（如模板字符串里出现反引号）会让整段脚本不执行 → 页签无文字、按钮无回调、正文空白，看着像宿主断链。改完必跑 `node scripts/check-web-syntax.mjs` |
