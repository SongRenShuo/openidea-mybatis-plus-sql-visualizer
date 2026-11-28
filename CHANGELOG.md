# 更新记录

发行说明的唯一来源，`.oiplugin` 的 change notes 由它生成。
每条一行、一版不超过八条：用户在升级弹窗里能一眼看完。

## [2026.11.29]

首个正式版。`0.1.0` 是切换前的试验版，已撤下，其功能并入本版。

### 新增

- 16 个命令：渲染 mapper XML、校验 SQL、性能建议、生成 CRUD、回滚计划、渲染 SQL 日志、捕获启停与状态、打开日志、索引统计，以及预览 SQL（mapper 语句 / 路径 / 行内 SQL / 当前编辑器 / 注解 SQL / iBatis 2 / MyBatis-Plus Wrapper 链）与差异预览（原文↔SQL）；mapper 语句行的 gutter 按钮、`Alt+Shift+Q`、右键三处入口。
- `SQL Visualizer` 工具窗五个页签（SQL 日志 / 日志转 SQL / SQL 转 Java / 历史 / SQL 预览），46 项设置分页，界面中英双语；导航、SQL 注入与 inlay 默认开启，诊断默认关闭。
- 日志列表按原版形态（`#序号 [时间] 耗时 [库] [类#方法]`、折叠 `… (+N)`、类型色条与徽章、按方法/请求/模板聚焦），SQL 历史两级浏览，快照按项目持久化。
- 编辑期反馈：语句与 ResultMap 标记、`*Mapper.xml` 文件图标、SQL 高亮、Mapper↔XML 双向跳转、语句悬停出 SQL、INSERT 列 inlay、列名与方言引号诊断；gutter 标记由宿主图绘制，大工程不再触顶导航关联上限。
- 纯逻辑引擎：XML 动态标签、受限 OGNL、include 环检测、注解 SQL、iBatis 2.x、15 方言、参数绑定、21 条性能规则、CRUD 与 Java 生成、四族日志解析；中文注释与中文标识符不再导致索引崩溃。
- Java agent 经回环 WebSocket 实时捕获，无 agent 时读导出日志；引擎 sidecar 按版本与 sha256 钉住，不匹配降级纯文本。
- 12 个只读 MCP 工具复用同一引擎，结果带 `exact` / `partial` / `unresolved`；生成 SQL 判不出来时标 unresolved 并给原因，绝不猜。

### 变更

- 版本号改成日期制 `YYYY.M.D`（与原版 IntelliJ 版一致），发行标签同步改成不带 `v` 前缀的版本号原文。
