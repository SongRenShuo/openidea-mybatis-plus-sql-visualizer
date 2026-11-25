# 更新记录

发行说明的唯一来源，`.oiplugin` 的 change notes 由它生成。
每条一行、一版不超过八条：用户在升级弹窗里能一眼看完。

## [Unreleased]

### 新增

- 14 个命令：渲染 mapper XML、校验 SQL、性能建议、生成 CRUD、回滚计划、渲染 SQL 日志、捕获启停与状态、打开日志与工作台、索引统计。
- `SQL Logs` 工具窗与五个页签（SQL 日志 / 日志转 SQL / SQL 转 Java / 历史 / SQL 预览），42 项设置分五组，界面中英双语。
- 编辑期反馈：语句与 ResultMap 标记、`*Mapper.xml` 文件图标、SQL 高亮、定义跳转、INSERT 列 inlay、列名与方言引号诊断；导航与注入默认关闭。
- 纯逻辑引擎：XML 动态标签、受限 OGNL、include 环检测、注解 SQL、iBatis 2.x、15 方言、参数绑定、幂等格式化、21 条性能规则、CRUD 与 Java 生成、四族日志解析。
- Java agent 经回环 WebSocket 实时捕获 SQL，未挂 agent 时走日志文件路径。
- 引擎 sidecar 走行式 JSON 协议，按版本与 sha256 钉住；钉值缺失或不匹配降级纯文本，不跑错版本。
- 12 个 MCP 工具复用同一引擎，每条结果带 `exact` / `partial` / `unresolved` 置信度。
- 发行链：签名包、GitHub Release、Gitee 镜像与自建插件索引。