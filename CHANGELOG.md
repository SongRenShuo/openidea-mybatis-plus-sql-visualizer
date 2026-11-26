# 更新记录

发行说明的唯一来源，`.oiplugin` 的 change notes 由它生成。
每条一行、一版不超过八条：用户在升级弹窗里能一眼看完。

## [Unreleased]

### 新增

- 16 个命令：渲染 mapper XML、校验 SQL、性能建议、生成 CRUD、回滚计划、渲染 SQL 日志、捕获启停与状态、打开日志、索引统计，以及**预览 SQL**（`<mapper.xml>#<语句>` / 路径 / 行内 SQL / 当前编辑器）与**差异预览**（原文↔SQL，可在窗口中打开）。
- 预览是主入口：mapper 语句行的 gutter 预览按钮、`Alt+Shift+Q`、右键都能打开它；预览页签按原版排布（方言与建议行、复制/格式化/显示模式/刷新、参数左、SQL·原文·列信息右）。
- `SQL Visualizer` 工具窗与五个页签（SQL 日志 / 日志转 SQL / SQL 转 Java / 历史 / SQL 预览），45 项设置分四页（UI and Actions / SQL Preview / SQL Log / Engine），界面中英双语；项目视图与编辑器右键都收进 `SQL Visualizer` 一级菜单，导航、SQL 注入与 inlay 默认开启，诊断默认关闭。
- 日志列表按原版形态：`#序号 [时间] 耗时 [库] [类#方法]`、SQL 折叠 `… (+N)`、类型色条与徽章、按方法/请求/模板聚焦、跳转源码与 Mapper XML、发送到 SQL 转 Java / 日志转 SQL；SQL 历史按原版两级（日志文件列表 → 语句列表，返回/刷新/滚动/快速预览/复制/跳转），快照按项目持久化。
- 编辑期反馈：语句与 ResultMap 标记、`*Mapper.xml` 文件图标、SQL 高亮、Mapper↔XML 双向定义跳转（含跨文件）、语句悬停出 SQL、INSERT 列 inlay、列名与方言引号诊断。
- 纯逻辑引擎：XML 动态标签、受限 OGNL、include 环检测、注解 SQL、iBatis 2.x、15 方言、参数绑定、幂等格式化、21 条性能规则、CRUD 与 Java 生成、四族日志解析（含 logback 前缀的 Hibernate 行）；中文注释与中文标识符不再导致索引崩溃。
- Java agent 经回环 WebSocket 实时捕获；无 agent 时读导出的日志文件。引擎 sidecar 按版本与 sha256 钉住，钉值缺失或不匹配降级纯文本。
- 12 个 MCP 工具复用同一引擎，结果带 `exact` / `partial` / `unresolved`；发行链（签名包、Release、Gitee 镜像与自建索引）不变。
