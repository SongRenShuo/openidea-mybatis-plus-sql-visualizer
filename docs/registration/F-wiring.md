# F 注册申请：把三个「实现有、入口无」接上

本车道（`feat/wire-sources`，工作树 `.worktrees/lane-f`）把三块**写完了、有测试、但没有任何入口**的渲染实现接到用户可用的面上：

| 实现 | 位置 | 接入前调用点 |
| --- | --- | --- |
| 注解 SQL（`@Select` / `@Insert` / `@Update` / `@Delete`，含 `<script>`） | `src/core/annotation.rs` | 只有自身单测 |
| iBatis 2.x（`<dynamic>` / `#param#` / `<iterate>` / `<parameterMap>`） | `src/core/ibatis/**` | 只有自身单测 |
| `LambdaQueryWrapper` / `QueryWrapper` 链（文本 → 链 → SQL） | `src/analyze/wrapper.rs` | 自身单测 + `src/execute/parity.rs`（测试夹具） |

## 0 结论

- **不需要新增命令、视图、能力、provider、MCP 工具或依赖。** 三类来源全部汇入已有的 `preview-sql` 命令（`plugin.toml` 里已在 `projectView.context`、`editor.context` 挂好，`Alt+Shift+Q` 键盘映射已有）和已有 MCP 工具 `sqlv_render_sql` 的新 `kind` 参数。
- `plugin.toml` 只有**一处**建议改动（MCP 工具描述，见 §2），不改也能用。
- 新增文案键已按所有权约定追加到 `src/i18n.rs` 的 `TABLE` 末尾；`locales/*.json` 未动（按 `docs/SDK-REF.md` §6，运行时点号键只走 `TABLE`）。

## 1 为什么没有新命令

`src/lib.rs` 的 `run_command` 把命令 id 与分发函数一一硬编码，且它属于主控独占文件。新 id 在 `lib.rs` 没有分支就会落到 `Error::NotFound`，**装上也点不出东西**。所以本车道把所有入口收敛进既有 `preview-sql` 的「来源判定与分派」段（该段是本车道获准修改的唯一区段），按参数形态与文件类型分派，不新增命令 id。这条申请因此只登记「要不要动 `plugin.toml`」。

## 2 需要主控应用的一处改动（可选，建议改）

`[[contributes.mcpTools]]` 中 `sqlv_render_sql` 的 `description` 目前只说 mapper 语句：

```toml
[[contributes.mcpTools]]
name = "sqlv_render_sql"
description = "Render one MyBatis mapper statement to SQL with ? placeholders and bound parameters."
inputSchema = "src/mcp/schemas/sqlv_render_sql.schema.json"
readOnly = true
destructive = false
```

建议替换 `description` 一行（其余行不动）：

```toml
description = "Render one statement to SQL with ? placeholders and bound parameters; kind selects mapper XML (default), a statement annotation, an iBatis 2 sqlMap or a wrapper chain."
```

理由：宿主把这条描述直接给 agent 看，`kind` 在 `inputSchema` 里虽有描述，但 agent 选工具时先读到的是 description。`src/mcp/mod.rs` 的 `TOOLS` 已同步成同一句；manifest 本车道不可改，故留此条。

`[[contributes.menus]]`、`[[contributes.keymaps]]`、能力表、`README.md` 都**不需要**改：`preview-sql` 的两处菜单与快捷键早已声明，本车道只是让这个命令认得更多来源。

## 3 入口形态（用户/agent 侧）

| 来源 | `preview-sql` 参数 | MCP `sqlv_render_sql` | 说明 |
| --- | --- | --- | --- |
| 注解 SQL | `<Mapper.java>#<method>` 或 `<Class>#<method>`；无参数时取光标所在方法 | `kind = "annotation"` + `text`（Java 源码）+ `method`（可选） | 单语句文件可不给方法；多语句且无方法/光标不在方法内 → `unresolved` 并列出方法名 |
| iBatis 2.x | `<sqlMap.xml>#<namespace.id>`；文件只有一个语句时可省 `#id` | `kind = "ibatis"` + `xml` + `statement_id` | 按根元素判定，不靠文件名；多语句无 id → `unresolved` 并列出 id |
| Wrapper 链 | 直接贴链文本，或在编辑器里选中链后触发 | `kind = "wrapper"` + `text` + `entity`（表 + 字段→列） | 命令从项目索引找实体类；MCP 无工程读取，必须显式给映射 |
| Mapper XML | 行为不变（`<mapper.xml>#<stmt>` / 裸路径 / 行内 XML） | `kind` 缺省或 `"mapper"` | 完全向后兼容 |

置信度口径（三类一致，未变）：

| 情形 | 置信度 |
| --- | --- |
| 参数齐全、`@TableField`/`@TableName` 明确、链全字面量 | `exact` |
| 有值按 `?` 渲染（如 `#{}` / 运行期变量）、更新包装器只出条件片段、列名/表名由 MyBatis-Plus 默认规则推导 | `partial`，`reason` 里带 messageKey |
| 找不到实体类 / 方法名不存在或重载歧义 / 多语句无 id / 语句 id 不存在 / 循环 include | `unresolved`，**`sql` 为空**，只给原因与可选列表 |

## 4 文案键

以下 36 个 key 已追加到 `src/i18n.rs` 的 `TABLE` 末尾（中英各一条），无需主控动作。它们原本就由 `core::annotation` / `core::ibatis` / `analyze::wrapper` 产出，在接线前不可能被读到：

- 预览与注解（5 条）：`preview.error.annotation_class_unknown`、`annotation.not_found`、`annotation.method.not_found`、`annotation.method.required`、`annotation.method.ambiguous`
- iBatis（16 条）：`ibatis.statement.required`、`ibatis.root_invalid`、`ibatis.parse.failed`、`ibatis.statement.not_found`、`ibatis.statement.duplicate`、`ibatis.include.not_found`、`ibatis.include.cycle`、`ibatis.include.too_deep`、`ibatis.unknown_tag`、`ibatis.iterate.not_collection`、`ibatis.condition.no_compare_target`、`ibatis.dollar.unclosed`、`ibatis.inline.malformed`、`ibatis.parameter_map.missing_id`、`ibatis.parameter_map.not_found`、`ibatis.render.degraded`
- 包装器（11 条）：`wrapper.not_a_wrapper`、`wrapper.table_unknown`、`wrapper.update_fragment_only`、`wrapper.unknown_field`、`wrapper.unsupported_method`、`wrapper.between_arity`、`wrapper.empty_in`、`wrapper.limit_not_literal`、`wrapper.last_not_literal`、`wrapper.param_value`、`wrapper.receiver_mismatch`
- 本车道新增（4 条）：`wrapper.entity.not_found`、`wrapper.column.derived`、`wrapper.render.note`、`wrapper.empty_fragment`

## 5 与原版（IntelliJ 版）的对照：原版有、本版没做

1. **原版没有这两个来源的入口。** `StandaloneSqlSourceResolver.kt` 的判定门只有「XML 语句块 / 纯 SQL / 未识别 / 过长」；注解与包装器在原版走的是 PSI 动作（`EntityClassResolver` 一族）。所以本车道的注解/iBatis/包装器分派是**扩展**，不是移植，没有可逐像素对齐的 UX 基线；参数语法是顺着本仓 `preview-sql` 既有形态长的（`路径#语句` 的同一套分隔符）。
2. **实体解析从 PSI 换成文本。** 本平台没有插件可用的 PSI（`AGENTS.md`「已知平台坑」）。本版用 `src/analyze/entity.rs` 读 `@TableName` / `@TableField` / `@TableId`，缺注解时按 MyBatis-Plus 的默认驼峰规则推导。**这是有意的降级**：项目若关掉 `mapUnderscoreToCamelCase`，推导列名就会错，因此这类渲染标 `partial`（`wrapper.column.derived`），而不是冒充 `exact`。
3. **`@SelectProvider` / 函数式 Provider 不做。** 需要执行用户代码，`core::annotation` 明确不覆盖；传入这类方法得到 `annotation.method.not_found`（`annotation.not_found` 亦然），不会猜一条 SQL。
4. **参数面板（勾选条件 / foreach 重放 / 字段填充）不做。** `RenderOutput` 只有 `sql` + `params`，没有条件模型；三类来源都只渲染 `{}` 参数下的静态形态，与既有 mapper 预览口径一致。
5. **iBatis/MyBatis 不「双链择优」。** 原版对不确定的片段两条链都跑、取能成的；本版按根元素精确判定（`<sqlMap>` → iBatis，`<mapper` → MyBatis），根元素是可判事实，不需要择优。
6. **iBatis 语句选择器不做。** 预览页的语句选择器是 `KIND_XML`（mapper）专属，且它的 `preview-statement` op 走 `MapperSet`（`src/views/workbench.rs`，不属本车道）。因此 `<sqlMap>` 多语句时要求显式 `#id`，宁可 `unresolved` 也不做「取第一个」。

## 6 交界：三处语义收紧（已记录，便于回归）

`preview-sql` 的来源判定有三处相对旧行为的收紧，都只在旧行为会产出「明显不是 SQL 的东西」时生效：

1. 形如 `Class#method`（无空白、类名含小写字母、方法名是合法标识符）的参数现在按注解来源处理，不再当纯 SQL 文本贴进预览。`SELECT#1` 这类全大写片段不受影响（类名部分没有小写字母即拒绝）。
2. 整份 `.java` 文档走注解渲染（`<Class>#<method>` 或光标定位），不再把 Java 源码当 SQL 贴进预览；文件里没有注解但含包装器链时走包装器。
3. `.xml` 文件既不是 `<mapper>` 也不是 `<sqlMap>` 时按文本来源处理（`kind = sql`），不再尝试当语句解析。

另外两条来自既有 manifest 事实，不是本车道的选择：

- `editor.context` 菜单会把**文件路径**作为参数传入，所以从右键菜单进来时走「整份文件」判定，不看光标；光标定位只在无参数调用（`Alt+Shift+Q` / Find Action）时生效。
- 整份 Java 文档的包装器判定基于文本里是否出现链式调用，文件里若有多条链会被合并解析——只建议用于「一个方法一条链」的选区或无参数场景；复杂情况请选中链再触发。

## 7 改动清单与所有权

改动（本车道工作树内）：

| 文件 | 改动 |
| --- | --- |
| `src/analyze/entity.rs` | 新增：实体类文本 → 字段/列投影（`@TableName` / `@TableField` / `@TableId` + 默认驼峰规则），静态/瞬态/集合/`exist=false` 跳过并登记原因 |
| `src/analyze/sources.rs` | 新增：来源判定（包装器链、`<sqlMap>`、注解方法/光标定位、iBatis 语句 id 解析）与 `WrapperOutput → RenderOutput` 适配 |
| `src/analyze/mod.rs` | 只加两行模块声明 + 模块文档一句 |
| `src/commands/preview_sql.rs` | 只改「来源判定与分派」段：新增 `Ibatis` / `Annotation` / `Wrapper` 三种输入与其模型构建；dialog/reveal 段未动 |
| `src/mcp/handlers/render_sql.rs` | 新增 `kind`（`mapper` 缺省）/`text`/`method`/`entity`/`dialect` 参数与三条来源分支 |
| `src/mcp/schemas/sqlv_render_sql.schema.json` | 新增同名属性 + `allOf`/`if`/`then` 分种类必填；`mapper` 分支保持原 `oneOf` 语义 |
| `src/mcp/mod.rs` | `sqlv_render_sql` 的描述一句（与 §2 建议的 manifest 文案一致） |
| `src/mcp/tests.rs` | 新增 4 条走工具面的来源用例 |
| `src/i18n.rs` | 只在 `TABLE` 末尾追加 36 个键 |

未改：`plugin.toml`、`src/lib.rs`、`Cargo.toml`、`Cargo.lock`、`README.md`、`locales/*.json`、`.github/`、`web/**`、`src/views/**`、`engine/ENGINE_VERSION`、`src/sidecar/**`、`src/core/annotation.rs`、`src/core/ibatis/**`、`src/analyze/wrapper.rs`。

引擎没有参与：三类来源都是本仓 Rust 侧实现，`ENGINE_VERSION` 与 sidecar 未动。

## 8 门禁读数

见车道报告的「门禁」一节（`cargo fmt --all --check` + 定向测试）。本文件不复制读数，避免两处数字漂移。
