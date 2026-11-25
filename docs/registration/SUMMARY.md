# 注册汇总（最终清单）

本文件是 `plugin.toml` / `src/lib.rs` 的**单点事实快照**，由主控在合并全部车道后核对生成；
分车道的申请原文仍在同目录 `L1.md`~`L15.md`。注册是按依赖分三次落地的（基础层、MCP、捕获），
本文件把终态收拢到一处，避免读者在多个提交间拼装。

## 清单计数（`openidea-plugin check .` 的读数）

| 项 | 数量 | 内容 |
| --- | ---: | --- |
| commands | 13 | render-xml / check-sql / perf-advice / generate-crud / rollback-plan / render-log / capture-start / capture-stop / capture-status / capture-poll / capture-open-log / open-logs / open-workbench |
| views | 2 | `sqlv-logs`（bottom，代码渲染）、`sqlv-workbench`（right，包内 web 页） |
| settings | 1 页 | `settings.schema.json`，42 项五组（Dialect / Log / Rendering / Navigation / Engine） |
| documentIndexers | 1 | extensions `xml`、`java` |
| providers | 4 | definition+line-markers（xml/java）、injections（xml）、inlay-hints（xml/java）、diagnostics（xml/java） |
| mcpTools | 11 | `sqlv_render_sql` / `render_logs` / `check_sql` / `perf_advice` / `crud_sql` / `rollback_plan` / `reverse_sql` / `mapper_check` / `history_search` / `schema_context` / `explain` |
| markerIcons | 3 | mapper / statement / resultmap（亮暗各一） |
| fileIcons | 1 | `*Mapper.xml`、`*mapper.xml` |
| menus | 1 | `projectView.context` → `capture-open-log`（选中文件路径即参数） |
| javaAgents | 1 | `agent/mps-sql-agent.jar`，`args=port=17890,stackFrames=24`，`vmOptions=-Dmpsv.capture.transport=ws` |

## 能力

| 能力 | 级别 | 为什么需要 | 拒绝后的降级 |
| --- | --- | --- | --- |
| `project.read` | required / low | 读 mapper、Java 源码、日志 | 无法工作 |
| `events` | required / low | 收 `websocket-ready` 唤醒捕获端 | 只剩日志文件路径 |
| `clipboard` | optional / medium | 面板与工作台复制/导出 | 复制/导出按钮失效 |
| `process:java` | optional / high | 启动钉版本引擎 jar（sha256 校验） | 纯文本模式 |
| `net:127.0.0.1` | optional / medium | 连 agent 的 loopback 捕获 socket | 只剩日志文件路径 |
| `java.agent` | 由 manifest 派生 / high | 注入 Application / Spring Boot / JAR 配置 | 不注入，其余可用 |

## 导出（`register_plugin!`）

`views`、`indexing`、`providers`、`injections`、`inlay_hints`、`diagnostics`、`mcp`。

## 依赖与许可

直接依赖只有 `serde` / `serde_json`（MIT/Apache-2.0）与 `openidea-plugin`（tag `v0.1.51`）。
引擎 jar 与 agent jar 不入库：引擎经 `engine/ENGINE_VERSION` + sha256，agent jar 由主仓
`agentFatJar` 产出后入包；`NOTICE` 记 JSqlParser（Apache-2.0 分支）与 ASM（BSD-3-Clause）。
（L15 已落地：引擎 jar 内含 PostgreSQL JDBC 42.7.1（BSD-2）与 mssql-jdbc 12.4.2（MIT），
NOTICE 第 4 条已记；MySQL 驱动因 GPL 仍不引入。）

## 复读命令

```sh
export PATH="/c/Users/USER/.cargo/bin:$PATH"
openidea-plugin check .        # 应打印上表计数并 ok
cargo test --lib               # 704 passed / 1 ignored
cargo test --test cases_parity # 150 例（144 绿 + 6 白名单）
```