# MyBatis SQL Visualizer（OpenIDEA 版）

[![gate](https://img.shields.io/github/actions/workflow/status/SongRenShuo/openidea-mybatis-plus-sql-visualizer/build.yml?branch=master&label=gate)](https://github.com/SongRenShuo/openidea-mybatis-plus-sql-visualizer/actions/workflows/build.yml)

> 服务未启，SQL 先知。

MyBatis SQL Visualizer 是 OpenIDEA 的 SQL 调试插件。写代码时，不启动应用就能把 mapper XML、注解 SQL、MyBatis-Plus / MPJ Wrapper 链和 iBatis 2.x `<sqlMap>` 还原成数据库真正会收到的那条 SQL，参数自动回填；运行应用后，Java agent 增强捕获与控制台日志把真实参数、耗时放在一起看。每条结果都先过检查：方言写法、不安全的写操作、21 条性能规则——判不出来的直接标 `unresolved` 并给原因，不猜。

[官网与在线文档](https://sql-visualizer.jvtools.cn) · [插件市场](https://plugins.jetbrains.com/plugin/33633-mybatis-sql-visualizer) · [快速开始](https://sql-visualizer.jvtools.cn/guide/getting-started) · [原版更新记录](https://gitee.com/songrenshuo/mybatis-plus-sql-visualizer/blob/master/CHANGELOG.md) · [问题反馈](https://sql-visualizer.jvtools.cn/about/feedback)

## 这是哪一版

本仓是 IntelliJ 插件 **MyBatis SQL Visualizer** 的 OpenIDEA 端口。两个版本同名、同图标，区别只在宿主平台。功能更全、日常在用的那一版在这里：

- Gitee：https://gitee.com/songrenshuo/mybatis-plus-sql-visualizer
- GitHub：https://github.com/SongRenShuo/mybatis-plus-sql-visualizer

用着顺手的话，留一条真实评价比什么都管用：[JetBrains Marketplace 评分](https://plugins.jetbrains.com/plugin/33633-mybatis-sql-visualizer/reviews)。

两个版本的渲染引擎是同一份：引擎不复制进本仓，只钉版本与 sha256（`engine/ENGINE_VERSION`），由 `process:java` 启动，两边对「一条语句是什么意思」给出同样的结论。引擎起不来或版本对不上时自动降级为纯文本模式并在界面明示。本仓只做 OpenIDEA 平台侧的事——界面对接、索引、捕获通道、MCP。

## 能解决什么问题

| 场景 | 用法 |
|---|---|
| Wrapper 或动态条件太复杂，不确定最终 SQL | 在 Wrapper 链根或注解行点预览图标（或 `Alt+Shift+Q`），勾选条件、填写参数，直接查看 SQL |
| 老项目还在用 iBatis，`<sqlMap>` 没有预览与跳转 | mapper XML、注解、iBatis 语句统一走预览命令，`#name#` 参数与动态条件可交互 |
| 日志全是占位符，难以复现问题 | Java agent 增强捕获真实执行；或把 MyBatis / iBatis / Hibernate / JdbcTemplate 四族控制台日志粘进面板还原 |
| 找到了慢 SQL，却不知道是谁调用的 | 日志行自带 logger 名（`类.方法`），列表头显示 `[类#方法]`，文件:行经插件自建的 Java 方法索引反查跳转 |
| 写操作有风险，想先看后果 | 无界 UPDATE / DELETE 风险清单、回滚预案（快照 + 逆向语句），缺主键或缺修改前数据时拒绝生成 |
| SQL 已调好，还要重新写 Wrapper | 「SQL 转 Java」页签与 `sqlv_reverse_sql` 把单表 SELECT 反向生成 Wrapper 链 |
| 让 AI 帮忙排查 SQL，但不能只靠猜 | 12 个只读 MCP 工具提供渲染、核查、性能建议、回滚预案与结构上下文，`unresolved` 不出 SQL |

## 功能总览

### 编码期：静态预览

- **MyBatis-Plus / MPJ**：Lambda / Query / Update / Delete Wrapper 链与服务链；MPJ 联表（`leftJoin` / `innerJoin` / `rightJoin` / `fullJoin` 的别名、lambda ON、`ext` 追加、raw join 片段）、联表更新与删除（`JoinWrappers.update` / `delete`）、`in` / `exists` 子查询，以及 `selectFunc` / `selectSub` / `selectCollection` / `from` / `around` 等投影形态。ON 与条件列按列自己的实体类定别名（与原版同语义）；副表实体不在工程索引里时该链标 `unresolved`，不猜表名。
- **MyBatis XML / 注解**：`if` / `choose` / `where` / `set` / `foreach` / `bind` 动态标签、OGNL 条件、跨命名空间 `<include>`；`@Select` / `@Insert` / `@Update` / `@Delete` 与 Provider 注解。
- **iBatis 2.x**：`<sqlMap>` 语句预览，`#name#` 占位与动态条件标签可交互。
- **参数回填**：`?` 按类型回填标量、集合与日期；未填写的参数保留 `?`，填完即得可执行 SQL。

静态预览不等于运行结果：运行时分支、数据库内容、无法静态求值的变量需要手动提供参数。JPA / HQL / Criteria 的静态预览本版没有（平台无类型推断，见已知限制），日志侧的 Hibernate 四族还原不受影响。

### 运行期：捕获与日志

- **增强捕获**：Java agent 挂到 Application / Spring Boot / JAR Application 运行配置上，经回环 WebSocket 采集真实执行；拒绝 `process:java` 或 `net:127.0.0.1` 时降级为日志文件路径，功能不消失。
- **四族日志还原**：MyBatis（`Preparing:` / `Parameters:`）、iBatis、Hibernate（`Hibernate:` + binding parameter）、JdbcTemplate（`Executing prepared SQL statement`），按线程分桶还原并发日志。
- **工具窗五页签**：SQL 日志、日志转 SQL、SQL 转 Java、历史（快照存插件自有 storage）、SQL 预览。慢查询阈值、同结构聚类、去重与屏蔽规则可在设置里调。

### 检查、生成与回滚

- **检查**：方言引号不匹配、未知列、无界 UPDATE / DELETE、破坏性 DDL，共 21 条性能规则与 0–100 复杂度评分；诊断默认关闭，每条规则可单独开关，无 schema 时不猜。
- **生成**：实体类生成 CRUD SQL（自增主键默认不选，可选必配全选/全不选）；单表 SELECT 反向生成 `LambdaQueryWrapper` 链；不可映射的结构给降级提示，不冒充完整支持。
- **回滚预案**：UPDATE / DELETE 的快照 + 逆向语句；缺主键直接拒绝，缺修改前数据时不会凭空生成恢复 SQL。
- **EXPLAIN**：引擎侧语句包装与计划结果模型已在 PostgreSQL 16、PostgreSQL 17、SQL Server 2022 三个真实库只读验证；写语句结构化拒绝。

### MCP：给 AI 的确定性工具

12 个 `sqlv_*` 工具全部只读、无破坏性操作，生成 SQL 只以文本返回，`partial` 不冒充 `exact`：

| 用途 | 工具 |
|---|---|
| 从源码与日志还原 SQL | `sqlv_render_sql`、`sqlv_render_logs` |
| 获取表结构与执行计划 | `sqlv_schema_context`、`sqlv_explain` |
| 检查风险、性能与 Mapper 契约 | `sqlv_check_sql`、`sqlv_perf_advice`、`sqlv_mapper_check` |
| 生成 CRUD、Wrapper 代码与回滚预案 | `sqlv_crud_sql`、`sqlv_reverse_sql`、`sqlv_rollback_plan` |
| 检索与诊断 | `sqlv_history_search`、`sqlv_index_stats` |

工具不提供任意写 SQL 执行入口；`sqlv_explain` 只格式化调用方提供的计划，从不执行 SQL。注意宿主对 MCP 入参有 1 MiB 上限，大日志请走面板或 `render-log` 命令。

## 界面与入口

| 入口 | 内容 |
| --- | --- |
| 命令面板（16 个） | 渲染 XML、校验 SQL、性能建议、生成 CRUD、回滚计划、渲染日志、捕获启停与状态、打开 SQL 日志、打开工作台、索引统计、SQL 预览（含 diff 形态） |
| 右侧工具窗 `SQL Visualizer` | SQL 日志、日志转 SQL、SQL 转 Java、历史、SQL 预览五个页签 |
| 编辑器右键 | 预览 SQL / 预览 SQL（diff），当前文件自动作为参数 |
| 项目视图右键 | 选中文件即作为命令参数，日志文件不必手抄路径 |
| 代码左侧图标 | mapper XML 语句、`@Select` 族注解、Wrapper 链根三处预览图标；Mapper 接口 ↔ XML 语句定义跳转 |
| 文件图标与高亮 | `*Mapper.xml` 专属文件图标；mapper XML 内 SQL 注入高亮与 INSERT 列 inlay（默认开启） |
| 设置页 | `MyBatis SQL Visualizer`，8 页（UI and Actions / Shortcuts / SQL Preview / Table Name Mapping / SQL Log / MCP Tools / About / Engine）、142 项，schema 见 `settings/*.json` |
| 快捷键 | `Alt+Shift+Q` 打开 SQL 预览 |

每条渲染结果都带置信度：`exact` / `partial` / `unresolved`。判不出来就写 `unresolved` 并给原因和补充入口，不猜；MCP 工具也不把 `partial` 当 `exact` 返回。

## 安装

```sh
openidea-plugin build          # → dist/plugin.wasm，按安装态校验
openidea-plugin pack           # → songrenshuo.mybatis-plus-sql-visualizer-<version>.oiplugin
```

在 Settings › Plugins 里安装生成的 `.oiplugin`；开发期也可以直接挂目录：

```sh
openidea-plugin link .         # 之后 Settings › Plugins › Reload
```

注意 `pack` 不校验组件新鲜度，发布前先 `build` 再 `pack`。本仓需要插件 API 1.18（`engines.openidea = "^1.18"`，模态预览弹窗与对话框能力依赖该线）。

## 能力申请，以及每一项为什么需要

插件只要最低限度的那几项。能力会在安装页展示给用户，多要一项都是审核拒绝的理由。

| 能力 | 风险 | 为什么需要 |
| --- | --- | --- |
| `project.read` | 低 | 插件的工作就是读项目里的 mapper XML、Java 源码和 SQL 日志再渲染。不给读权限等于没有功能。它不能写、删、改项目里的任何东西。 |
| `events` | 低 | 用 `websocket-ready` 唤醒捕获消费，不轮询。没有它只剩日志文件一条路。 |
| `storage` | 低 | 「SQL 历史」页把日志快照存进插件自己的键值存储（按项目隔离，每项目最多 10 个、单文件 512 KB）。不碰项目文件。 |
| `clipboard`（可选） | 中 | 日志面板和工作台复制/导出渲染出的 SQL。拒绝它只关掉这两个按钮，渲染、导航、校验都照旧。 |
| `clipboard.read`（可选） | 中 | 「日志转 SQL」页签可从剪贴板读取日志文本。拒绝它仍有粘贴输入框。 |
| `process:java`（可选） | 高 | 启动钉住版本且校验 sha256 的引擎 jar（`engine/ENGINE_VERSION`），用于 Wrapper 渲染与 EXPLAIN 语句包装，同时承载 Java agent 挂载。拒绝它降级成纯文本模式，其他功能都在。 |
| `net:127.0.0.1`（可选） | 中 | 以客户端身份连 Java agent 的回环捕获 WebSocket（沙箱没有裸 socket）。拒绝它仍有日志文件路径。 |
| `editor.read`（可选） | 中 | `preview-sql` 不带参数时读当前编辑器的选区或全文。拒绝它仍可用路径 + 语句 id 与右键入口。 |

插件不写项目文件、不连数据库、不自己发起对外网络连接。引擎 jar 只有 sha256 对上才会启动。

## 文档

| 文件 | 内容 |
| --- | --- |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | 阶段 P0–P10、每阶段退出门禁、待证伪前提（本仓排期与验收的唯一权威） |
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

OpenIDEA 的插件模型有几件事根本做不到，也有一部分是本版刻意缩小口径。逐条列在 [`docs/LIMITS.md`](docs/LIMITS.md)（平台做不到的 19 条 + 本版实现边界），常见的几条：

- 平台没有 PSI 与类型解析，且插件能发的 LSP 只允许服务器自有命名空间，`textDocument/*` 一律被宿主拒绝。所以「从调用点反推 MBG/Criteria 查询」「按类型解析分派」这类功能不可行；JPA / HQL / Criteria 与 JdbcTemplate 字符串 SQL 没有预览图标，替代入口是 XML 语句、注解处预览或 Wrapper 面板。
- Database 面板不向插件暴露表节点，右键挂不上去；请在实体类上执行「生成 CRUD」。
- 新增方言只在本插件自己的校验里生效，IDE 自带的 SQL 智能不认识它们；方言预览是纯文本、不着色。
- 导航图一次最多装 20,000 个匹配文件、200,000 个关联，超限整图拒绝更新；标记依赖索引，跨文件的标记得等 Mapper 接口那批索引完才出现（`Index Stats` 命令给读数）。
- MCP 工具入参上限 1 MiB（宿主在调用前就拒绝），大日志请走面板或 `render-log` 命令。
- 设置改完不重开文件不生效（宿主没有 provider 失效接口），设置页已写明。

## 交流群

扫码添加作者微信或加入交流群。两张都是限时码；群码失效时直接添加作者微信，拉你进群。

| 作者微信 | 交流群 |
|---|---|
| ![作者微信](assets/wechat-qr.png) | ![交流群](assets/group-qr.jpg) |

问题与建议请提交到 [官网需求池](https://sql-visualizer.jvtools.cn/about/feedback)。

## 许可

Apache-2.0。第三方声明（含 JSqlParser 的双许可选择，走 `Apache-2.0` 分支、版本 5.3）见 [`NOTICE`](NOTICE)。

---

English: this is the OpenIDEA port of the IntelliJ plugin [MyBatis SQL Visualizer](https://plugins.jetbrains.com/plugin/33633-mybatis-sql-visualizer) — "the SQL arrives before the service starts." It renders MyBatis mapper XML, annotation SQL, iBatis 2.x sqlMap, MyBatis-Plus / MPJ wrapper chains (joins, join updates and deletes, subqueries) and SQL logs into the executable SQL the database will receive, with real parameters filled in, then checks every statement: dialect quoting, unsafe writes, 21 performance rules. Every result carries an exact / partial / unresolved confidence — unresolved never invents SQL — and 12 read-only MCP tools expose the same engine to AI agents. Rendering, checks and generation live in a shared engine jar pinned by `engine/ENGINE_VERSION`; this repository holds only the platform side. Docs and evidence are in Chinese.
