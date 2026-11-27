# MyBatis SQL Visualizer（OpenIDEA 版）

[![gate](https://github.com/SongRenShuo/openidea-mybatis-plus-sql-visualizer/actions/workflows/build.yml/badge.svg)](https://github.com/SongRenShuo/openidea-mybatis-plus-sql-visualizer/actions/workflows/build.yml)

> 服务未启，SQL 先知。

把 MyBatis mapper XML、注解 SQL、MyBatis-Plus Wrapper 链和 MyBatis / Hibernate 日志还原成数据库真正会收到的那条 SQL，再做检查：方言写法、不安全的写操作、21 条性能规则。

## 先看 IntelliJ 版

本仓是 IntelliJ 插件 **MyBatis SQL Visualizer** 的 OpenIDEA 端口。两个版本同名、同图标，区别只在宿主平台。功能更全、日常在用的那一版在这里：

[官网与在线文档](https://sql-visualizer.jvtools.cn) · [插件市场](https://plugins.jetbrains.com/plugin/33633-mybatis-sql-visualizer) · [快速开始](https://sql-visualizer.jvtools.cn/guide/getting-started) · [原版更新记录](https://gitee.com/songrenshuo/mybatis-plus-sql-visualizer/blob/master/CHANGELOG.md) · [问题反馈](https://sql-visualizer.jvtools.cn/about/feedback)

- Gitee：https://gitee.com/songrenshuo/mybatis-plus-sql-visualizer
- GitHub：https://github.com/SongRenShuo/mybatis-plus-sql-visualizer

用着顺手的话，留一条真实评价比什么都管用：[JetBrains Marketplace 评分](https://plugins.jetbrains.com/plugin/33633-mybatis-sql-visualizer/reviews)。

两个版本的渲染引擎是同一份：引擎不复制进本仓，只钉版本与 sha256（`engine/ENGINE_VERSION`），由 `process:java` 启动，两边对「一条语句是什么意思」给出同样的结论。所以本仓只做 OpenIDEA 平台侧的事——界面对接、索引、捕获通道、MCP。

## 安装

```sh
openidea-plugin build          # → dist/plugin.wasm，按安装态校验
openidea-plugin pack           # → songrenshuo.mybatis-plus-sql-visualizer-<version>.oiplugin
```

在 Settings › Plugins 里安装生成的 `.oiplugin`；开发期也可以直接挂目录：

```sh
openidea-plugin link .         # 之后 Settings › Plugins › Reload
```

注意 `pack` 不校验组件新鲜度，发布前先 `build` 再 `pack`。

## 做了什么

| 入口 | 内容 |
| --- | --- |
| 命令面板（14 个） | 渲染 XML、校验 SQL、性能建议、生成 CRUD、回滚计划、渲染日志、捕获启停与状态、打开 SQL 日志、打开工作台、索引统计 |
| 右侧工具窗 `SQL Logs` | 与原版同名工具窗：SQL 日志、日志转 SQL、SQL 转 Java、历史、SQL 预览五个页签 |
| 项目视图右键 | 选中文件即作为命令参数，日志文件不必手抄路径 |
| MCP 工具（12 个 `sqlv_*`） | `render_sql`、`render_logs`、`check_sql`、`perf_advice`、`crud_sql`、`rollback_plan`、`reverse_sql`、`mapper_check`、`history_search`、`schema_context`、`explain`、`index_stats`。全部只读、不带破坏性操作，SQL 只以文本返回 |
| 编辑期反馈 | mapper XML 上的语句/ResultMap 标记、`*Mapper.xml` 文件图标、SQL 高亮、定义跳转、INSERT 列 inlay、列名与方言引号诊断。导航、XML 内 SQL 注入与 inlay 默认开启（`nav.enabled` / `injections.enabled` / `inlay.enabled`），诊断默认关闭 |
| 设置页 | 入口名 `MyBatis SQL Visualizer`，四页（UI and Actions / SQL Preview / SQL Log / Engine）、56 项，schema 见 `settings/*.json` |

每条渲染结果都带置信度：`exact` / `partial` / `unresolved`。判不出来就写 `unresolved` 并给原因和补充入口，不猜；MCP 工具也不把 `partial` 当 `exact` 返回。

## 能力申请，以及每一项为什么需要

插件只要最低限度的那几项。能力会在安装页展示给用户，多要一项都是审核拒绝的理由。

| 能力 | 风险 | 为什么需要 |
| --- | --- | --- |
| `project.read` | 低 | 插件的工作就是读项目里的 mapper XML、Java 源码和 SQL 日志再渲染。不给读权限等于没有功能。它不能写、删、改项目里的任何东西。 |
| `events` | 低 | 用 `websocket-ready` 唤醒捕获消费，不轮询。没有它只剩日志文件一条路。 |
| `storage` | 低 | 「SQL 历史」页把日志快照存进插件自己的键值存储（按项目隔离，每项目最多 10 个、单文件 512 KB）。不碰项目文件。 |
| `clipboard`（可选） | 中 | 日志面板和工作台复制/导出渲染出的 SQL。拒绝它只关掉这两个按钮，渲染、导航、校验都照旧。 |
| `process:java`（可选） | 高 | 启动钉住版本且校验 sha256 的引擎 jar（`engine/ENGINE_VERSION`），用于 Wrapper 渲染与 EXPLAIN 语句包装，同时承载 Java agent 挂载。拒绝它降级成纯文本模式，其他功能都在。 |
| `net:127.0.0.1`（可选） | 中 | 以客户端身份连 Java agent 的回环捕获 WebSocket（沙箱没有裸 socket）。拒绝它仍有日志文件路径。 |

插件不写项目文件、不连数据库、不自己发起网络连接。引擎 jar 只有 sha256 对上才会启动。

## 文档

| 文件 | 内容 |
| --- | --- |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | 阶段 P0–P8、每阶段退出门禁、7 条待证伪前提（本仓排期与验收的唯一权威） |
| [`docs/TASKS.md`](docs/TASKS.md) | 34 个可独立提交的任务卡（交付/验收/估时）与建议批次 |
| [`docs/ORCHESTRATION.md`](docs/ORCHESTRATION.md) | 多子代理并行规则：车道表、目录所有权、注册申请制、合并与清理 SOP |
| [`docs/FEATURES.md`](docs/FEATURES.md) | 201 条功能的四档范围（98 现成 / 83 变通 / 1 上游 / 19 不可行）与优先顺序 |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | 平台硬约束、wasm 即时路径 + jar sidecar 双路径、引擎 JSON 契约、置信度模型 |
| [`docs/LIMITS.md`](docs/LIMITS.md) | 用户可读的「本平台不支持」清单，每条都给了替代做法 |
| [`CHANGELOG.md`](CHANGELOG.md) | 本仓发行说明的唯一来源，`.oiplugin` 的 change notes 由它生成 |

调研证据（12 份分册、逐条编号与行号）在原版仓：
`docs/调研报告/20261125-1336-OpenIDEA全面支持-调研报告.md`（提交 `816e8c0a0`）。
能力边界以那份报告为准，排期与验收以 `docs/ROADMAP.md` 为准。

## 已知限制

OpenIDEA 的插件模型有几件事根本做不到，也有一部分只能近似。19 条逐条列在 [`docs/LIMITS.md`](docs/LIMITS.md)，常见的几条：

- 平台没有 PSI 与类型解析，且插件能发的 LSP 只允许服务器自有命名空间，`textDocument/*` 一律被宿主拒绝。所以「从调用点反推 MBG/Criteria 查询」「按类型解析分派」这类功能不可行，替代入口是 XML 语句、注解处预览或 Wrapper 面板。
- Database 面板不向插件暴露表节点，右键挂不上去；请在实体类上执行「生成 CRUD」。
- 新增方言只在本插件自己的校验里生效，IDE 自带的 SQL 智能不认识它们。
- 不能把 `*.mapper.xml` 里的方言段落声明成 SQL，预览结果是纯文本、不着色。
- MCP 工具入参上限 1 MiB（宿主在调用前就拒绝），大日志请走面板或 `render-log` 命令。

## 许可

Apache-2.0。第三方声明（含 JSqlParser 的双许可选择，走 `Apache-2.0` 分支、版本 5.3）见 [`NOTICE`](NOTICE)。

---

English: this is the OpenIDEA port of the IntelliJ plugin [MyBatis SQL Visualizer](https://plugins.jetbrains.com/plugin/33633-mybatis-sql-visualizer). Rendering, checks and generation live in a shared engine jar pinned by `engine/ENGINE_VERSION`; this repository holds only the platform side. Docs and evidence are in Chinese.