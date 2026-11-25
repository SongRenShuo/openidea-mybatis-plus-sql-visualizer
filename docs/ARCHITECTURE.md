# Architecture

> 平台事实与证据在主仓报告（提交 `816e8c0a0`，分册 01/03/04/06）。本文件只写**本仓的实现约定**。
> 分层、目录职责、核心流程与开发测试指南见 `docs/架构设计/`（01 总览 / 02 核心流程 / 03 开发与测试）。

## 1 平台硬约束（决定架构，不可绕）

| 约束 | 事实 | 影响 |
| --- | --- | --- |
| 没有 PSI / 类型解析 | 官方 README：类型解析推迟到 API 2.0；`textDocument/*` 对插件封死 | Wrapper 判型、实体字段类型、JPA 继承都得自建或外挂 |
| 编辑路径预算 | provider 软 50 ms / 硬 500 ms；命令/事件 5 s；`tasks` 后 60 s；内存 256 MiB | 解析必须增量 + 缓存；全项目扫描只能进 task |
| `database` 不执行 SQL | 只有建源/凭据/connect | EXPLAIN、预览执行走 sidecar |
| 读不到运行控制台 | PR #5361 仍 open | 捕获只剩 `java.agent` 或「导出日志文件 + 读文件」 |
| 沙箱 | 无裸 socket、无环境变量、无直连网络 | agent 通信走 loopback WebSocket（`net.websocket-connect`，API 1.6+） |

## 2 双路径

```
编辑器事件 ──► crate/ (wasm)
                 ├─ 即时路径：tree-sitter 解析 + Project::symbols + read-file + 版本缓存
                 │    输出 gutter / inlay / diagnostics / definition
                 └─ 重路径：sidecar 请求（JSON）
                        └─ process:java  ──► mpsv-engine-<ver>.jar
                                             渲染 / 检查 / 性能规则 / EXPLAIN / schema
```

- **即时路径**永不阻塞：首次返回空，后台解析后按 `path+version` 缓存切片。
- **重路径**只在命令、视图动作、MCP 调用里发起；要连库的必须 `tasks.begin` 并周期 `report` 续期。

## 3 引擎契约（jar 侧）

`engine/ENGINE_VERSION` 钉版本 + sha256；调用前校验，不匹配拒绝启动（不降级跑错版本）。

```
请求  { kind: "wrapper"|"xml"|"annotation"|"jpa"|"jdbcTemplate"|"logs"|"check"|"perf"|"explain",
        source: { path, text, range? }, dialect?, params?, entityTables? }
响应  { ok, sql?, params?, confidence: "exact"|"partial"|"unresolved",
        notes[], findings[], warnings[], messageKeys[] }
```

- jar 不带 `messages/*.properties`，`perf` 等返回的是 **messageKey**（如 `perf.leading_wildcard`）；本仓负责 key → 本地化文案。
- 引擎 `EngineApi` 现有三个门面：`renderLogs` / `checkSql` / `perfAdvice`；后续按 P5 扩 `render`/`explain`/`schema`。
- 版本不匹配或 sidecar 启动失败时：**降级为纯文本模式**并在 UI 明示，不产出可能错的 SQL。
- **分发（2026-11-25 探针实测修正）**：安装态**读不到包内 jar**——安装只复制 manifest 引用的文件 + wasm，且没有安装目录路径，只有 `fs.plugin_dir()`。因此 jar 由插件**首次使用时通过 `net:` 下载到 `fs.plugin_dir()`**，用 `ENGINE_VERSION.sha256` 校验通过后才允许启动；`link` 开发态可直接用仓库根相对路径。下载地址与 sha256 由 P5 产出并写进 `engine/`。

## 4 置信度模型（核心产品纪律）

| 级别 | 条件 | 呈现 |
| --- | --- | --- |
| `exact` | 字面量/注解/XML 直读，无推断 | 正常展示，可复制可执行 |
| `partial` | 部分条件未解析（已标占位） | 显著标注 + 「哪些没解析」 |
| `unresolved` | 关键信息缺失 | **不给 SQL**，给原因与补充入口 |

MCP 工具同样遵守：禁止把 `partial` 当 `exact` 返回给 agent。

## 5 能力与权限

- 当前仅 `project.read`（Low）；`P4` 加 `process:java`（High，optional），`P5` 视需要加 `editor.read`/`editor.write`。
- 每个新增能力必须在 `README.md` 的能力表里写**为什么需要**（市场审核要求）。
- `process:java` 作为 optional：用户拒绝时插件仍可用，只是失去渲染/EXPLAIN。

## 6 目录职责

| 目录 | 职责 | 不允许 |
| --- | --- | --- |
| `src/commands/` | 命令实现，薄壳，只做参数与编排 | 放解析逻辑 |
| `src/core/` | 纯逻辑（XML/OGNL/方言/规则/日志） | 引用 `host::` |
| `src/providers/` | definition / line-markers / completion / inlay / diagnostics / injections | 长耗时同步调用 |
| `src/indexer/` | documentIndexer 图（Mapper↔XML） | 持久化到 storage 当索引 |
| `src/views/` | 面板数据装配 | 在视图里做解析 |
| `src/sidecar/` | jar 生命周期、JSON 协议、降级判定 | 业务规则 |
| `web/` | 预览工作台 / EXPLAIN 页面 | 网络请求、本地存储 |
| `engine/` | `ENGINE_VERSION` 与校验和 | 放 jar 本体 |
