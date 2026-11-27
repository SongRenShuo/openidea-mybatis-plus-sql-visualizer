# Roadmap

> 本文件是新仓的**执行计划**（唯一权威）。证据与调研过程在主仓：
> `docs/调研报告/20261125-1336-OpenIDEA全面支持-调研报告.md` + `docs/调研报告/20261125-openidea-support/`（主仓提交 `816e8c0a0`）。
> 两处冲突时：**能力边界以主仓报告为准，排期与验收以本文件为准。**
> 任务拆分（34 个任务卡、依赖图、建议批次）见 `docs/TASKS.md`。

## 现状

| 阶段 | 状态 | 产物 |
| --- | --- | --- |
| P0 骨架 | 完成 | 本仓首提交 `e2e6e38`：`plugin.toml`（API 1.15）、`render-xml` 命令、`openidea-plugin check` ok、wasm 110.6 KiB |
| P0 引擎导出 | 完成（主仓） | 主仓 `sub-projects/engine-jar`，`b3a8aeb9a`；jar 4.5 MB、零 IntelliJ 引用、15 方言 |
| P1 纯逻辑引擎 | 完成 | Rust 侧 XML/OGNL/include/注解/iBatis、15 方言、参数、格式化、21 条规则、生成/回滚、日志四族；`cases/cases.jsonl` 150 例对拍（144 绿 + 6 条有据白名单） |
| P2 界面面 | 完成 | 日志面板、预览工作台、设置页（46 项、四页）、双语表、标记/文件图标；GUI 走查待 IDE 重载 |
| P3 导航与注解 | 完成（默认关） | `documentIndexers` 图、definition/line-markers、injections、inlay、diagnostics；Ctrl+B 真机未验 |
| P4 捕获链 | 完成（固定端口） | agent ws 旁路（默认 TCP）、`websocket-ready` 消费、事件唤醒、日志文件路径 |
| P5 sidecar 与执行 | 完成 | 引擎钉值 + sha256、stdio 真往返（204 ms）、Wrapper 文本解析、EXPLAIN 包装与结果模型、schema 缓存；**T5-5 已达成**：引擎自带 PostgreSQL（BSD-2）/ mssql-jdbc（MIT），在 PG16、PG17、SQL Server 2022 三库真跑只读 EXPLAIN（JSON/XML 计划），写语句结构化拒绝 |
| P6 MCP | 完成 | 11 个 `sqlv_*` 工具 + 11 份 schema，全部 readOnly，`partial` 不冒充 `exact` |
| P7 发行 | 完成（T7-4 PR 暂缓：用户先自验） | `scripts/release.sh`（真签真 pack 实测）、`release.yml`、Gitee 镜像、自建索引脚本与文档；官方注册表 PR 按用户 2026-11-26 决定**暂缓**，待其自行验收通过后再提交；本地签名包与上游 TOML 已就绪 |
| P8 上游提案 | 完成（待提交上游） | `docs/upstream/T8-1~T8-3` 三份草案 + 索引；是否另有 CLA 待上游确认 |
| P9 等 1.18 的提前开发 | 完成 | 计划：`docs/开发计划/20261127-0010-等1.18期间的提前开发与升级切换-开发计划.md`；L41–L47 与版本无关，已落地；L48（弹窗切换）随上游 0.1.54 的 API 1.18 落地，见下 |
| P10 UI 与设置对齐原版 | 进行中（三车道） | 计划：`docs/开发计划/20261127-2130-UI与设置对齐原版-三车道复刻计划.md`；前置已落：`web/sql-highlight.js`（两页共用词法高亮）、设置树 8 页骨架与中文化基线 |

## 阶段验收（摘要）

| 阶段 | 退出门禁 |
| --- | --- |
| P0 | `openidea-plugin check` 通过；`link` 后命令可见；CI 三件套绿 |
| P1 | 每类能力 ≥20 例与主仓输出逐字对拍（或差异入白名单） |
| P2 | 8 条主流程真机走查无阻塞；双语覆盖 100% 声明文本 |
| P3 | Ctrl+B 双向跳转在单模块/多模块/MPJ 三仓可用；导航默认关 |
| P4 | Spring Boot 示例启动后 3 秒内面板出 SQL；无 agent 时给替代指引 |
| P5 | Wrapper 语料 30 例 `exact` ≥90%，其余标 `partial`；EXPLAIN 在 3 个真实库出结果 |
| P6 | 外部 agent `tools/list` 见 11 个工具，`render_logs`/`check_sql` 端到端通 |
| P7 | 干净机器从索引安装成功；升级走通；三处回读一致 |
| P10 | 预览窗口与日志列表真机截图对照原版演示帧；日志 SQL 8 类着色；设置树 8 页且关键项切换后行为变化；`ci/gate-46` 绿 |

## 待证伪前提（动手前先验）

| # | 前提 | 怎么验 | 不成立的后果 |
| --- | --- | --- | --- |
| P-1 | `symbols("User.")` 能列实体字段 | **成立（2026-11-25 用户真机确认）**：容器限定查询返回字段名 | 实体→列映射退化为文件名启发 |
| P-2 | 50 ms 预算内能增量解析单文档 | 2,000 行 XML + 800 行 Java 实测 p95 | gutter 触发面缩到仅 XML 或改命令式 |
| P-3 | `process:java` 能跑引擎 jar | **部分成立（2026-11-25 探针实测）**：`process:java` 可跑；但安装态**读不到包内 jar**（安装只复制 manifest 引用文件 + wasm，无安装目录路径），只有 `fs.plugin_dir()` | sidecar 改为「首次 `net:` 下载 jar 到 `plugin_dir` + sha256 校验」，或要求用户指定外部 jar |
| P-4 | `websocket-ready` 在 1.15 真机可用 | **成立（2026-11-25 用户真机确认）**：连本地 ws 收到帧 | 捕获链退化 HTTP 轮询 |
| P-5 | `injections` 与宿主 PR #3641 不打架 | mapper XML 上同时开两者 | 放弃 injections |
| P-6 | 自建索引在中国网络可用 | Gitee Pages 托管 index.json 实测 | 只走官方注册表（要求开源） |
| P-7 | `documentIndexer` 图能驱动 Ctrl+B | 最小插件建假 Mapper 图 | 导航退回 provider 现算 |
| P-8 | 插件模态弹窗可用（升 SDK 到 API 1.18 后） | **已落地（2026-11-27，SDK/CLI v0.1.54，插件 API 1.18）**：`[[contributes.dialogs]] id = "sqlv-preview"` 已进 `plugin.toml`，`views::reveal` 在命令/事件上下文调用，`closed` 事件清瞬态不清模型，页面按 `openidea.dialog` 是否存在决定是否接 Esc 与关闭按钮；契约、清单闸、单测齐。**真机弹出／Esc／尺寸记忆仍待在 IDE 里逐项走查**（本车道未跑 runIde）。证据见 `docs/调研报告/20261126-2330-OpenIDEA插件弹窗能力到货验证-调研报告.md` | 预览默认回页签（`preview.presentation = tab`），不改外壳——该设置已作为用户可选项保留 |

本版落地后的前提状态（2026-11-25 收口）：

- **P-2 未实测**：没有做 2,000 行 XML / 800 行 Java 的 p95 读数；为了不赌预算，导航与注入默认关闭
  （`nav.enabled=false`、`injections.enabled=false`），provider 只读当前文档文本、单遍、无 IO。
- **P-3 已落地为降级路径**：引擎 jar 不入库，只钉 `engine/ENGINE_VERSION` + sha256；安装态需要
  `engine.downloadUrl`（默认空）才下载，留空即降级纯文本。补了 `--stdio` 入口并实测真进程往返
  （冷启 204 ms），但「安装态从网络下载」这一步没有真机验证。
- **P-5 未实测**：injections 默认关，避免与宿主 PR #3641 重复高亮；打开后是否冲突仍需真机确认。
- **P-6 未实测**：`scripts/build-index.sh` 与 `market/README.md` 已备，Gitee Pages 托管未跑。
- **P-7 部分验证**：L6 在临时目录用真实 manifest + 导出跑过 `build/check`，键图有单测；
  Ctrl+B 跨文件跳转未在真机验收。
- **P-8 已落地，真机待验收（2026-11-27）**：清单声明、`views::reveal` 调用点、`closed` 路由、
  页面 `openidea.dialog` 门控都在代码与单测里钉住；「弹窗真的弹出、Esc 真的关闭、尺寸真的被记住」
  只有真机看得到，本车道未跑 runIde，交给 UI 验收。

## 三条工程纪律

1. **不猜**：每条结果带 `exact/partial/unresolved`；`unresolved` 不出 SQL，只给「为什么 + 去哪补」。
2. **不复制主仓源码**：引擎只认 `engine/ENGINE_VERSION` + sha256；语料走 `cases.jsonl` 对拍。
3. **基线 1.18**：`engines.openidea = "^1.18"`（L48 随上游 0.1.54 升上来）；1.16 的 `frames` / `markdown_hover` / `customEditors.binary` 仍不进代码，1.18 只取 `dialogs`。
