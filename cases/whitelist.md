# 白名单（允许差异）

本文件记录 `cases/cases.jsonl` 中**已确认与当前实现不一致、但差异本身有据可查**的用例。
runner（`tests/cases_parity.rs`）只按这里列出的字段豁免断言；未列出的字段仍然严格比对。

规则：

- 每一行是一条豁免：`| 用例 id | 豁免字段（逗号分隔，或 all） | 理由 | 后续动作 |`。
- 理由必须能指向 MyBatis/参考仓语义或本仓文档（`docs/arc`、`docs/registration/L1.md` 等）。
- **豁免不是洗白**：拿不准的差异写在这里，不许把实现输出抄成 expected。
- 用例一旦真的修好、全部字段都通过，runner 会报「stale whitelist」并变红，要求删除该行。

| id | fields | reason | followup |
| --- | --- | --- | --- |
| xml-parity-foreach-null-001 | confidence | MyBatis 对 null 集合静默跳过（exact）；本仓按 L1 契约把「可能缺段」标为 partial 并给出 `xml.foreach.null_collection`，属信息公开性取舍 | 若确认上游契约改为静默，删除本行并把 xml-foreach-003 一并改掉 |
| xml-parity-include-missing-001 | sql,confidence | MyBatis 构建期对缺失 include 抛 BuilderException（无 SQL）；本仓按 L1 契约降级为 `-- [mpsv] include not found` 占位并标 partial | 面板侧（T2-2）落地后评估是否改成 unresolved |
| ibatis-parity-unknown-001 | sql | iBatis 2 引擎对未知标签丢弃整棵子树；本仓按 `src/core/ibatis/mod.rs` 文档「keeps the text visible」保留标签内容并告警 | 与 L1 确认可见性策略后二选一 |
| params-parity-oracle-bool-001 | sql | 参考仓 `ParameterReplacerTest` 对 Oracle 布尔拒绝展开、保留 `?` 原样；本仓按 L2 契约渲染 1/0 并在 keys 中披露 `params.boolean_literal_unsupported` | 若用户更认可「拒绝展开」，改 bind 并删除本行 |
| log-parity-param-model-001 | params | 参考仓 `LogSqlParam` 的 params 是「原始值 + 类型」；本仓按 L4 契约把 `params` 定义为已渲染的 SQL 字面量 | 面板若需类型列，另开字段，勿改本契约 |
| log-parity-sharding-001 | entry_count | 参考仓有独立 ShardingSphere 解析器（`Logic SQL:`），不在 T1-11 的四族范围内，本仓未实现（L4 已注明） | 若纳入范围，新增 sharding 家族并删除本行 |