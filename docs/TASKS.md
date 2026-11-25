# Tasks（实施拆分）

> 由 `docs/ROADMAP.md` 的阶段展开。每个任务可独立提交、独立验收。
> 规模估算按「一人、已熟悉本仓」计；实际并行时按依赖图并车道。

## 依赖图（先做左边）

```
T1 引擎(Rust 纯逻辑) ──┬─► T2 界面 ──┐
                       └─► T6 MCP ───┼─► T7 发行
T3 导航 ───────────────┘             │
T4 捕获 ─────────────────────────────┤
T5 sidecar（依赖 T1 的接口与主仓 engine-jar 扩展）
```

## P1 纯逻辑引擎（Rust，无平台依赖）

| ID | 任务 | 交付 | 验收 | 估 |
| --- | --- | --- | --- | --- |
| T1-1 | XML 动态标签渲染（if/foreach/choose/where/set/trim/bind）+ `${ew.customSqlSegment}` | `src/core/xml/*` | 与主仓对拍 ≥20 例 | 4d |
| T1-2 | OGNL 受限子集（test / bind / `${}`） | `src/core/xml/ognl.rs` | 真值表用例 + 非法表达式 fail-closed | 3d |
| T1-3 | `<include>` 跨命名空间 + 缺失占位注释 | `src/core/xml/include.rs` | 跨文件用例 + 环检测 | 2d |
| T1-4 | 注解 SQL（@Select 等 + `<script>`） | `src/core/annotation.rs` | 与主仓对拍 ≥15 例 | 3d |
| T1-5 | iBatis 2.x（dynamic/isNotNull/iterate/selectKey/parameterMap/`#name#`） | `src/core/ibatis/*` | 与主仓对拍 ≥20 例 | 4d |
| T1-6 | 15 方言 + 能力矩阵（分页/引号/保留字/upsert/identity） | `src/core/dialect/*` | 方言表逐项断言 + 与主仓 jar 输出一致 | 4d |
| T1-7 | 参数还原（`?`/`:name`、类型引号、LIKE、日期 ANSI、预引号不二次转义） | `src/core/params.rs` | 与主仓对拍 ≥25 例 | 3d |
| T1-8 | SQL 格式化（按方言缩进/换行/大小写） | `src/core/format.rs` | 幂等（格式化两次结果相同） | 2d |
| T1-9 | 性能规则 21 条 + 复杂度评分 + 索引建议 | `src/core/perf/*` | 每条规则 ≥2 例；返回 messageKey | 3d |
| T1-10 | 生成：CRUD / SQL→MyBatis-Plus Java / 回滚计划 | `src/core/gen/*` | 与主仓对拍；无主键时拒绝生成回滚 | 3d |
| T1-11 | 日志解析四族 + 去重/N+1/慢查询/错误行 | `src/core/log/*` | 与主仓对拍 ≥30 例 | 3d |
| T1-12 | `cases.jsonl` 对拍框架 + 首批 100 例（从主仓测试派生） | `tests/`、`cases/` | CI 跑对拍，diff 入白名单需写明理由 | 2d |

## P2 界面

| ID | 任务 | 交付 | 验收 | 估 |
| --- | --- | --- | --- | --- |
| T2-1 | SQL 日志面板（list/table、筛选、去重折叠、N+1 徽章） | `src/views/log_panel.rs` | 1000 条吞吐不卡；刷新不闪 | 4d |
| T2-2 | 预览工作台（web 视图）：SQL/参数/复制/导出/方言切换 | `web/`、`src/views/workbench.rs` | 选区带入、复制可执行、CSP 内无外链 | 5d |
| T2-3 | 设置页 schema（首版约 40 字段，分 5 组） | `src/settings.rs` + schema JSON | 改动即生效（除注明需重开）；无「条件字段」依赖 | 3d |
| T2-4 | 双语：locales + `messageKey` 映射（含引擎返回的键） | `locales/`、`src/i18n.rs` | 无裸键泄漏（测试断言） | 2d |
| T2-5 | 图标：markerIcons / fileIcons（亮暗） | `icons/`、manifest | 16px 可辨；不回落 gutter-navigate | 1d |

## P3 导航与注解

| ID | 任务 | 交付 | 验收 | 估 |
| --- | --- | --- | --- | --- |
| T3-1 | `documentIndexers`：Mapper↔XML 图（namespace/statement/resultMap/include/alias 继承） | `src/indexer/*` | 单模块/多模块/MPJ 三仓双向跳转可用 | 5d |
| T3-2 | line-markers + definition（名字级；默认关） | `src/providers/nav.rs` | 设置项默认关；开启后图标与跳转对 | 3d |
| T3-3 | `injections`：mapper XML 内 SQL 区间 | `src/providers/inject.rs` | 与宿主 PR #3641 共存不重复高亮 | 2d |
| T3-4 | inlay：INSERT 目标列提示 | `src/providers/inlay.rs` | 只读视图生效；不干扰宿主 inlay | 2d |
| T3-5 | diagnostics：未知列、方言引号不匹配 | `src/providers/diag.rs` | 无 schema 时不误报；有 schema 时报准 | 3d |

## P4 捕获链

| ID | 任务 | 交付 | 验收 | 估 |
| --- | --- | --- | --- | --- |
| T4-1 | agent 传输：裸 TCP → loopback WebSocket（事件语义不变） | 主仓 agent 侧 + 新仓 `src/capture/*` | 存量 301 例语义等价；断线自动重连 | 4d |
| T4-2 | `contributes.javaAgents` + 授权/降级 | `plugin.toml`、`src/capture/agent.rs` | 拒绝授权时插件仍可用 | 2d |
| T4-3 | `websocket-connect` 消费 + 背压/缓冲上限 | `src/capture/ws.rs` | 8 连接上限；1 MiB 分片不丢 | 3d |
| T4-4 | 面板实时刷新（节流、暂停锚定） | `src/views/log_panel.rs` | 高频写入下 UI 不抢焦点 | 2d |
| T4-5 | 无 agent 替代：运行配置导出日志 + `read-file` 解析 | `src/capture/file_tail.rs` | 手动路径可用并给指引 | 2d |

## P5 sidecar 与执行

| ID | 任务 | 交付 | 验收 | 估 |
| --- | --- | --- | --- | --- |
| T5-1 | jar 分发 + `ENGINE_VERSION`/sha256 校验 | `engine/`、`scripts/verify-engine.sh` | 版本错/校验错拒绝启动并明示 | 2d |
| T5-2 | `process:java` 生命周期（启动/健康/超时/降级） | `src/sidecar/*` | 冷启 <2 s；崩溃重启 ≤3 次后降级 | 3d |
| T5-3 | JSON 协议 + 引擎门面扩 `render`/`explain`/`schema` | `src/sidecar/proto.rs` + 主仓 EngineApi | 双向契约测试；超时返回既有错误码 | 4d |
| T5-4 | 文本 → WrapperChain（首版：LambdaQuery 链 + 实体注解 + 表列映射） | `src/analyze/wrapper.rs` | 语料 30 例 `exact` ≥90% | 6d |
| T5-5 | EXPLAIN 执行 + 结果渲染（树/表） | `src/execute/explain.rs`、`web/explain` | 3 个真实库出结果；超时/权限错误分类 | 3d |
| T5-6 | schema 元数据缓存 + 诊断联动 | `src/execute/schema.rs` | DDL 后失效；离线不误报 | 2d |
| T5-7 | 投影行为对拍（jar vs 主仓测试期望） | `tests/engine_parity.rs` | 关键 5 类各 ≥10 例 | 2d |

## P6 MCP

| ID | 任务 | 交付 | 验收 | 估 |
| --- | --- | --- | --- | --- |
| T6-1 | 11 个工具 schema 声明 | `plugin.toml`（mcpTools） | `openidea-plugin check` 通过 | 1d |
| T6-2 | `mcp-provider.call` 实现 + task 续期 | `src/mcp/*` | 连库类工具 60 s 内完成或明确超时 | 3d |
| T6-3 | 只读/破坏性标注 + 错误契约 | 同上 | 写类工具标 destructive；错误可读 | 1d |

## P7 发行

| ID | 任务 | 交付 | 验收 | 估 |
| --- | --- | --- | --- | --- |
| T7-1 | keygen / pack / sha256 / 回读 | `scripts/release.sh` | 干净机器安装成功 | 2d |
| T7-2 | GitHub Release + Gitee 镜像 | CI | 两处资产一致（sha256） | 1d |
| T7-3 | 自建索引（`index` + `merge --keep`） | `market/index.json` | IDE 填 URL 可见并可更新 | 2d |
| T7-4 | 官方注册表 PR（开源前提下） | PR | 审核通过并上架 | 外部依赖 |

## P8 上游提案（并行，不阻塞主线）

| ID | 提案 | 交付 |
| --- | --- | --- |
| T8-1 | `database.execute` / `explain`（只读 + 显式确认） | WIT 变更 + host 落点 + 测试设计稿 |
| T8-2 | 暴露符号 `detail`（方法形参/字段类型） | `workspace.wit` 变更稿 |
| T8-3 | 语言服务器只读方法白名单 | 安全与预算分析稿 |

## 建议批次

| 批次 | 内容 | 出口 |
| --- | --- | --- |
| 第 1 批 | T1-1~T1-6 + T1-12 框架 | 「XML/注解/iBatis 能渲染」可演示 |
| 第 2 批 | T1-7~T1-11 + T2-1/T2-2 | 「日志还原 + 校验 + 工作台」可演示 |
| 第 3 批 | T3-1~T3-5 + T2-3~T2-5 | 导航/设置/双语齐 |
| 第 4 批 | T4-1~T4-5 | 真机捕获可用 |
| 第 5 批 | T5-1~T5-7 | Wrapper 渲染 + EXPLAIN |
| 第 6 批 | T6-1~T6-3 + T7 | MCP + 可安装 |
