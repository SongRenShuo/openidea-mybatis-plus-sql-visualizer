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
18. **F201 竞品共存让位**：没有「已安装插件列表」API，检测不到同类插件；导航类功能默认关闭，需要你在设置里手动打开。

**AI 协作类**

19. **F175 ACP 会话 MCP 传输修复**：那是 AI Assistant 专有面，新平台无对应实现；MCP 能力走平台标准的工具声明（P6 阶段交付）。

## 还有一类：本版实现边界

下面这些不是平台做不到，而是本版刻意缩小了口径；每条都写清替代做法，避免把「能做」误读成「全做」。
1. **渲染不读选区**：`render-xml` 的输入是工程内 mapper 文件路径（可带 `#namespace.id`），
   不申请 `editor.read`，所以不读光标位置或选区；需要预览某条语句时用路径 + 语句 id。
2. **Wrapper 渲染在 wasm 侧**：引擎 jar 的 `render` 只覆盖日志/检查/性能三类；
   `LambdaQueryWrapper` 文本→链由 `src/analyze` 解析（无 PSI 的类型推断），
   `unresolved` 时只给原因，不出 Java 代码。
3. **EXPLAIN 不连库**：引擎只做 `EXPLAIN <语句>` 包装与格式分类，不执行、不自省表结构；
   `sqlv_explain` 只格式化调用方已经拿到的 EXPLAIN 结果，缺结果返回 `unresolved`。
4. **schema 由调用方提供**：`sqlv_schema_context` 格式化入参里的表/列/索引，不做数据库自省；
   诊断与 inlay 在没有列清单时一律不出结果（宁可不报，也不误报）。
5. **MCP `sqlv_history_search` 只搜入参文本**：平台没有 IDE 历史记录 API，所以它不检索历史面板，
   只对本次调用传入的日志文本做字面量搜索。
6. **捕获只覆盖三类运行配置**：`[[contributes.javaAgents]]` 只注入 Application / Spring Boot /
   JAR Application（Run 与 Debug）；JUnit、Maven/Gradle goal、应用服务器、远程目标不注入。
   没有 agent 时用 `capture-open-log` 读工程内导出的日志文件；项目外路径需要额外文件授权。
7. **捕获端口是固定的**：manifest 的 agent `args` 是静态串，装不下运行期协商的临时端口；
   本版用 `port=<固定端口>`，`capture-start` 显式传端口。端口冲突时换端口并重启目标应用。
8. **引擎 jar 不入库**：仓库只钉 `engine/ENGINE_VERSION` 与 sha256；安装态首次使用需要
   `engine.downloadUrl`（默认空）。留空即不联网，直接用本地/仓库 jar，缺失时降级为纯文本模式。