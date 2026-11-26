# 本平台不支持的功能

下面 19 项来自 IntelliJ 版，在 OpenIDEA 的插件模型上做不到——不是没做，
是平台根本没有对应扩展点。每条都写了替代做法，能用的功能都还在，只是入口换了地方。
编号沿用调研清单（F 编号），便于和两个仓的发行说明对照。

**源码结构类**

1. **F13 CriteriaQuery 链**：JPA Criteria 是拼在 lambda 里的类型化查询，平台没有类型推断，读不出最终 SQL；请改用 `@Query`/JPQL，或在 Wrapper 面板查看等价条件。
2. **F21 MBG Example 调用点还原**：要把「哪个 `example.createCriteria()` 拼到哪条 SQL」反推回去，需要全项目引用检索加方法体内变量追踪，平台给不了；请在 XML 语句或注解处直接预览。
3. **F59 MBG Example 条件还原**：根因同 F21，条件分散在调用点；请改用 Wrapper 中间模型（P5 起支持）。
4. **F64 CriteriaQuery / CriteriaBuilder**：需要变量类型与变量追踪；替代同 F13。
5. **F65 Spring Data Specification（lambda）**：需要分析 lambda 体，平台无 AST；替代同 F13。

**IDE 集成类**

6. **F26 数据库表节点右键**：Database 面板不向插件暴露表节点，右键菜单挂不上去；请在实体类上执行「生成 CRUD」。
7. **F95 平台方言注册（DM 等 5 个方言）**：平台没有方言注册扩展点，新增方言无法让 IDE 自带的 SQL 功能认识；这些方言仅在插件自己的校验里生效（插件自带解析器）。
8. **F98 方言映射到 IDE SQL 语言**：没有写入文件语言属性的 API，无法把 `*.mapper.xml` 里的方言段落声明成某种 SQL；预览结果以纯文本呈现，不着色。
9. **F131 源码拦截器模板注入**：需要 PSI 写与模板解析；改为把生成的模板输出到 Output 面板或剪贴板，由你粘贴进源码。
10. **F142 JOIN 虚拟外键注入原生关系图**：没有 `modelRelationProvider` 的等价物，关系图塞不进 IDE 原生 Diagrams；JOIN 关系图用插件自绘的 HTML 视图呈现。
11. **F145 数据源下拉接管**：平台没有替换内置动作的机制，抢不了 Database 的数据源下拉；保留原下拉，插件提供独立入口。
12. **F147 执行文档写入纪律**：IntelliJ 版靠「先存盘再执行」保证执行的是磁盘上的 SQL，平台用版本化编辑直接接管，这一层纪律不再需要。
13. **F178 设置项进 IDE 全局搜索**：没有设置项贡献到搜索的接口；插件设置页自带搜索框。
14. **F182 自建更新三通道**：平台没有自建更新通道，更新统一走插件市场或自建索引（自建索引可用，闭源发行也走这条路）。
15. **F187 workspace.xml 自愈**：平台没有 VFS/PropertiesComponent 那套配置落盘，配置只存在插件的 storage 里，不存在损坏修复问题。
16. **F196 iBatis DTD 离线映射**：不能向平台注册离线 DTD 资源；包内自带 DTD 文本，由插件自己做 iBatis 语法校验。
17. **F200 缩进/空白偏好对齐**：读不到编辑器的缩进与空白设置；生成的 SQL 使用固定两空格缩进，可在插件设置里改。
18. **F201 竞品共存让位**：没有「已安装插件列表」API，检测不到同类插件；导航/注入/inlay 默认开启，诊断默认关闭（需要列清单才给结果），都可在设置里改，改后需重开文件或重启。

**AI 协作类**

19. **F175 ACP 会话 MCP 传输修复**：那是 AI Assistant 专有面，新平台无对应实现；MCP 能力走平台标准的工具声明（P6 阶段交付）。
20. **方法/请求来源（PSI 级）**：平台没有 PSI/类型解析；想借 IDE 的 jdtls 代劳也不行——插件能发的 LSP 只允许**服务器自有命名空间**，
    `textDocument/*`、`workspace/*`、`callHierarchy/*` 等 14 个标准命名空间在宿主侧被直接拒绝
    （`crates/ide-lsp/src/plugin_requests.rs:11-38` 的 `plugin_method_allowed`），所以 `documentSymbol` / `definition` 发不出去。
    替代做法：日志行自带的 logger 名就是 `类.方法`（如 `c.e.m.mapper.UserMapper.selectById`），列表头用它显示 `[类#方法]`；
    文件:行与「跳转 Java」用插件自建的 Java 方法索引（`src/indexer/`）反查；请求维度只在日志里出现 `traceId`/`requestId` 时显示，没有就不显示。
21. **MCP 工具入参上限 1 MiB**：宿主在调用插件工具前就拒绝更大的参数（实测 1.3 MB 日志返回
    `MCP arguments exceed 1 MiB`）。大日志请走面板（`capture-open-log` 读文件）或 `render-log` 命令；MCP 的
    `sqlv_render_logs` 适合中小片段（实测 7200 行 / 935 KB 可用，median 231 ms）。

22. **本版还没有插件模态对话框**：原版「SQL 预览」从 gutter 图标/`Alt+Shift+Q` 弹出 `SqlPreviewDialog`（模态窗）；
    本平台只能打开工具窗/页签，所以预览统一落在右侧 `SQL 日志` 工具窗的第 5 页签（`SQL 预览`），
    gutter 图标与快捷键都打开同一个页签（状态行给出说明）。渲染链与原版一致，差别只在外壳。
    上游已在插件 API 1.18 提供 `[[contributes.dialogs]]`（`views.reveal(id)` 打开、`closed` 事件、页面 `openidea.dialog.close()`），
    但截至 OpenIDEA 0.1.53 的已发布版本最高只到 1.17，本版因此仍用页签；升级 SDK 到 1.18 后改为弹窗。
    证据与升级影响见 `docs/调研报告/20261126-2330-OpenIDEA插件弹窗能力到货验证-调研报告.md`。

23. **导航图上限 20,000 个文件、200,000 个关联，且标记有索引延迟**：宿主的插件导航图一次最多装
    2 万个匹配文件、整图 20 万个关联（定义/引用/标记/别名），任一超限即整图拒绝更新，
    Ctrl+B 与 gutter 标记都不出（提示 `navigation association limit reached`）；
    渲染、预览、面板不受影响。本版只为 Mapper 接口发布方法级关联（普通 Java 文件只发布类型定义），
    但 Mapper 特别多、方法特别密的工程仍可能触顶。标记还依赖索引：项目打开后宿主按 16 个文件一批索引，
    跨文件的标记得等接口那一批索引完才出现；`Index Stats` 命令给出 java/xml 索引的文件数与被跳过的文件数。
    本插件对 `*Mapper.java` / `*Dao.java` 豁免索引总预算，生成代码再多也不会把跳转目标挤掉。

## 还有一类：本版实现边界

下面这些不是平台做不到，而是本版刻意缩小了口径；每条都写清替代做法，避免把「能做」误读成「全做」。
1. **渲染不读选区**：`render-xml` 的输入是工程内 mapper 文件路径（可带 `#namespace.id`），
   不申请 `editor.read`，所以不读光标位置或选区；需要预览某条语句时用路径 + 语句 id。
2. **Wrapper 渲染在 wasm 侧**：引擎 jar 的 `render` 只覆盖日志/检查/性能三类；
   `LambdaQueryWrapper` 文本→链由 `src/analyze` 解析（无 PSI 的类型推断），
   `unresolved` 时只给原因，不出 Java 代码。
3. **EXPLAIN 只在引擎侧真执行**：平台 `database` 不执行 SQL，所以执行面只有 sidecar 的 JVM。
   引擎 jar 现在自带 **PostgreSQL JDBC 42.7.1（BSD-2）** 与 **mssql-jdbc 12.4.2（MIT）**，
   支持对 PostgreSQL / SQL Server 跑**只读** `EXPLAIN`（PG 返回 JSON、SQL Server 返回 ShowPlan XML）；
   写语句、DDL、多语句一律结构化拒绝，连接只读 + 查询超时。MySQL/MariaDB 仍不执行：
   Connector/J 是 GPLv2+FOSS、H2 是 MPL/EPL，均不在许可白名单内。schema 自省仍不做，
   `sqlv_schema_context` 只格式化调用方给的结构；`sqlv_explain` 缺结果时返回 `unresolved`。
4. **schema 由调用方提供**：`sqlv_schema_context` 格式化入参里的表/列/索引，不做数据库自省；
   诊断与 inlay 在没有列清单时一律不出结果（宁可不报，也不误报）。
5. **MCP `sqlv_history_search` 只搜入参文本**：UI 的「SQL 历史」页会把快照按项目存进插件 storage
   （每个项目最多 10 个、单文件 512 KB，打开/捕获日志时自动保存），但 MCP 工具不读这份存储，
   只对本次调用传入的日志文本做字面量搜索；平台也没有供插件跨工具读取历史记录的统一 API。
6. **捕获只覆盖三类运行配置**：`[[contributes.javaAgents]]` 只注入 Application / Spring Boot /
   JAR Application（Run 与 Debug）；JUnit、Maven/Gradle goal、应用服务器、远程目标不注入。
   没有 agent 时用 `capture-open-log` 读工程内导出的日志文件；项目外路径需要额外文件授权。
7. **捕获端口是固定的**：manifest 的 agent `args` 是静态串，装不下运行期协商的临时端口；
   本版用 `port=<固定端口>`，`capture-start` 显式传端口。端口冲突时换端口并重启目标应用。
8. **引擎 jar 不入库**：仓库只钉 `engine/ENGINE_VERSION` 与 sha256；安装态首次使用需要
   `engine.downloadUrl`（默认空）。留空即不联网，直接用本地/仓库 jar，缺失时降级为纯文本模式。
9. **SQL 历史的存储位置不同**：原版把日志自动保存成 `.idea` 下的日志文件，历史页读那个目录；
   本版把快照按项目写进插件 `storage`（每项目最多 10 个、单文件 512 KB），因此历史文件不会出现在
   项目树里、不能被外部编辑器打开，也不跨机器同步。交互（文件列表 / 语句列表 / 返回 / 刷新 /
   滚动到底 / 快速预览 / 复制 / 跳转）按原版对齐。