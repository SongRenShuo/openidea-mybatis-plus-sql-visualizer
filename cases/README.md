# 对拍语料（T1-12）

本仓把 SQL 纯逻辑从 IntelliJ 版搬进 `src/core/`，由 12 条车道并行填充。语料的作用只有一句话：
**用与实现无关的 JSONL 契约钉住行为，证明搬过来的不是「差不多」。**

跑法：

```text
cargo test --test cases_parity                 # 汇总 + 逐例断言
cargo test --test cases_parity -- --nocapture  # 打印 cases/passed/whitelisted 与 diff
```

汇总行形如 `cases: N, passed: N, whitelisted: M, failed: F | kinds: xml=… params=…`。

## 一行一例

```json
{"id":"xml-if-001","kind":"xml","input":{"xml":"…","statement":"s1","params":{"status":"A"}},"expect":{"sql":"…","confidence":"exact"}}
```

- `id` 全局唯一，命名 `kind-主题-序号`；`kind` 决定分派。
- `expect` **只写要断言的字段**：写 `sql` 就只比 SQL，写 `params` 才比参数。
- 期望值从 MyBatis 语义或参考仓测试推导后手写。**禁止**「先跑当前实现、再把输出抄进 expect」——
  那样语料就退化成实现的镜子，永远绿，也永远发现不了问题。

## kind 契约

| kind | input | expect 可用字段 |
| --- | --- | --- |
| `xml` | `xml`（或 `docs` 多文档）、`statement`、`params` | `sql`、`confidence`、`params`、`keys_contains`、`keys_empty` |
| `annotation` | `java`、`method` 或 `index`、`params` | `count`、`kind`、`is_script`、`sql`、`confidence`、`params`、`keys_contains` |
| `ibatis` | `xml`、`statement`、`params` | `sql`、`confidence`、`params`、`keys_contains` |
| `params` | `sql`、`params`（数组）、`dialect` | `sql`、`confidence`、`params`（`SqlLiteral` 序列化形态）、`keys_contains` |
| `dialect` | `dialect`、`op`（`limit` / `quote_identifier` / `quote_if_keyword` / `quote_if_needed` / `upsert` / `upsert_statement` / `identity_wrap` / `exists`）及 op 参数 | `result`（字符串或 null） |
| `perf` | `sql`、`dialect` | `rule_ids`（精确串）、`rule_ids_contains`、`rule_ids_absent`、`severities` |
| `gen` | `op`（`crud` / `crud_batch` / `java` / `rollback`）、`table`、`dialect`、`sql`、`entity` | `statements`（整表或按 `kind` 匹配）、`statement_count`、`code`、`params`、`confidence`、`unresolved_key`、`error` |
| `log` | `text`、可选 `options`（`ParseOptions` JSON） | `entry_count`、`entries[]`（`index` + 字段）、`duplicate_count`、`n_plus_one_count`、`slow_count`、`errors_count`、`unparsed_count`、`duplicates`、`n_plus_one`、`unparsed` |

`confidence` 取值 `exact` / `partial` / `unresolved`，与 `docs/ARCHITECTURE.md` §4 一致：
`unresolved` 恒为空 SQL。

## 空白口径

SQL 比较用 `compact()`：`split_whitespace` 后以单空格连接（参考仓各测试的同一个 helper）。
其余字段逐字比较。这样既不被 XML 缩进干扰，又能抓住 `ANDORG_NAME` 这类真实的粘连。

## 新增一例

1. 先写「按 MyBatis/参考仓语义应当是」的整条 SQL 与 confidence，不参考本仓输出。
2. 在 `cases.jsonl` 末尾追加一行（UTF-8，一行一个对象，无尾逗号）。
3. `cargo test --test cases_parity -- --nocapture`。
4. 红了就分流：
   - 明确是实现的 bug → 在 `src/core/**` 做最小修复 + 单测，语料不动；
   - 是**有据可查的取舍差异** → 写进 `cases/whitelist.md`，语料仍保留参考行为。

## 白名单规则

`cases/whitelist.md` 每行一条：

```text
| 用例 id | 豁免字段（逗号分隔，或 all） | 理由 | 后续动作 |
```

- runner 只豁免列出的字段；同一条用例的其他字段仍严格比对。
- 理由必须指向语义或本仓文档，不能写「先这样」。
- **stale 检测**：某条用例修好后所有字段都过，runner 会报 `whitelist entry is stale` 并变红，
  要求删除该行——豁免不会悄悄堆成技术债。
- `whitelist_is_exercised_and_documented` 断言白名单 ≥5 条且每条有理由；
  `case_inventory_is_sane` 断言 id 唯一、8 个 kind 都有覆盖。

## 与参考仓测试的对应

参考仓只读，取行为不取代码（不复制 Kotlin 源码）：

| 语料 | 参考仓测试（`src/test/kotlin/cn/song/mybatisplus/visualizer/`） |
| --- | --- |
| `xml` | `xml/XmlSqlRendererTest.kt`、`xml/AnnotationSqlParserTest.kt`、`xml/ibatis/SqlMapSqlRendererTest.kt` |
| `params` | `sql/ParameterReplacerTest.kt`、`sql/SqlDateLiteralTest.kt` |
| `dialect` | `sql/DialectCapabilitiesTest.kt`、`sql/DialectTest.kt` |
| `perf` | `sql/SqlPerfAnalyzerTest.kt` + `PerfRulesBatch1..4` 的 ruleId |
| `gen` | `generator/CrudSqlGeneratorTest.kt`、`sql/rollback/*` |
| `log` | `log/MyBatisLogParserTest.kt`、`log/HibernateLogParserTest.kt`、`log/JdbcTemplateLogParserTest.kt`、`log/LogDurationParserTest.kt` |

覆盖薄的地方（后续补）：`xml` 的 `<trim>` 组合形态、`perf` 的规则交叉、
`log` 的多行 SQL 与时间戳族。