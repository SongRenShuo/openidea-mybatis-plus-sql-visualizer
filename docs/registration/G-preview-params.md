# 车道 W2-2 登记（预览参数名 / 条件开关 / MP 拦截器语义）

本车道改 `src/core/xml/{mod,render,param,intercept}.rs`、`src/views/workbench.rs`、
`web/preview.js`、`web/preview.css`、`settings/sql-preview.json`、`src/settings.rs`、
`src/i18n.rs`（只在 TABLE 末尾追加）。**未改** `plugin.toml`、`src/lib.rs`、
`Cargo.toml`、`Cargo.lock`、`README.md`、`locales/*.json`、`.github/**`、
`web/log-panel.html`、`src/views/log_panel*`。没有新增依赖、命令、能力、provider 或
MCP 工具，op 协议只加了页面 → 插件的一个 `preview-conditions`（不是 `[[contributes]]`
声明面）。

## 1 需要主控做的事

### 1.1 `locales/*.json` 同步（唯一必须）

`src/i18n.rs` 的 TABLE 已落 17 条新键；`locales/*.json` 在主线，请同步追加（追加即可，
不要重排既有条目）：

| key | en | zh-CN |
| --- | --- | --- |
| `view.workbench.conditions.title` | `Dynamic conditions` | `动态条件` |
| `view.workbench.conditions.hint` | `Untick a condition to leave that branch out of the render.` | `取消勾选后，该分支不参与本次渲染。` |
| `view.workbench.conditions.all` | `All` | `全选` |
| `view.workbench.conditions.none` | `None` | `全不选` |
| `view.workbench.param.empty.number` | `number, e.g. 1` | `数字，例如 1` |
| `view.workbench.param.empty.temporal` | `date, e.g. 2026-11-27` | `日期，例如 2026-11-27` |
| `view.workbench.param.empty.collection` | `list, e.g. 1,2,3` | `集合，例如 1,2,3` |
| `view.workbench.param.empty.object` | `object property` | `对象属性` |
| `view.workbench.param.warn.number` | `This value is not a number` | `该值不是数字` |
| `view.workbench.param.warn.temporal` | `This value is not a date` | `该值不是日期` |
| `preview.intercept.logicDelete` | `Simulated logic delete: {0}` | `已模拟逻辑删除：{0}` |
| `preview.intercept.tenant` | `Simulated tenant line: {0}` | `已模拟多租户：{0}` |
| `preview.intercept.pagination` | `Simulated pagination: {0}` | `已模拟分页：{0}` |
| `preview.intercept.tablePrefix` | `Simulated table prefix: {0}` | `已模拟表名前缀：{0}` |
| `preview.intercept.skipExisting` | `Not injected, the statement already mentions {0}` | `未注入：语句里已经有 {0}` |
| `preview.intercept.tableNotPrefixed` | `Table left unprefixed: {0}` | `未加表前缀：{0}` |
| `preview.intercept.paginationOrderBy` | `SQL Server OFFSET/FETCH needs an ORDER BY` | `SQL Server 的 OFFSET/FETCH 需要 ORDER BY` |

`{0}` 由调用方 `String::replace` 填入，不是 `format!` 占位符。

### 1.2 `src/commands/preview_sql.rs`（另一条车道的文件，本车道无权改）

gutter / 命令入口的预览模型（`output_model` 与 mapper 分支）只从 `RenderOutput`
拷 `params`，因此**命令路径**的面板仍显示「序号 + 文本框」、没有条件列表。让 C06/C07
在命令路径生效只需两行：

```rust
// 原：let out = set.render(&id, &json!({}));
let detailed = set.render_detailed(&id, &json!({}), ConditionSwitches::Defaults);
// … 仍用 detailed.output 设 sql/confidence/reason；
model.params = detailed.output.params.iter().map(param_text).collect();
model.parameters = /* 与 workbench::apply_parameters 同源 */;
model.conditions = detailed.conditions.iter().map(/* 同 workbench::condition_view */).collect();
```

`workbench::apply_parameters` / `condition_view` 目前是私有函数（同 crate 内可
`pub(crate)` 后复用）。拦截器语义（`workbench::apply_intercepts`）同理。**本车道不
改这些文件**，工作树内命令路径保持原样（面板自动退回序号渲染，不会崩）。

### 1.3 文档

设置页 SQL 预览多了一组「MyBatis Plus」10 项（见 §3）。若 `README.md` / 设置说明列了
键清单，请补这 10 项；能力表不需要动（无新能力）。

## 2 行为变化（合并点需要知道）

1. **条件默认开**（只影响预览页）：`<if test="id != null">` 在预览里默认勾选，预览显示
   「语句形状」（参数为 null 时是 `?` 占位），与参考版 `defaultTruth` 一致；`== null`
   一类默认不勾。**命令 / MCP / provider 的渲染不变**——`MapperSet::render` 与
   `render_statement` 走新增的 `ConditionSwitches::Evaluate`，仍按 OGNL 求值（这条是
   硬约束：共享 API 不能静默改语义，已用 `update_with_one_branch_still_drops_the_comma`
   等既有 118 条 core::xml 测试钉住）。
2. 拦截器**默认全关**（与参考版一致的是「默认设置不破坏既有 SQL」的口径）：参考版靠
   实体 `@TableLogic` / 方法 `Page` 形参判断「这个项目用不用拦截器」，本仓预览期没有
   实体与 PSI，猜就会给不用拦截器的项目凭空加 `AND deleted = 0`，所以四个开关都是 opt-in。
3. 表名前缀改写命中时渲染降为 `partial` 并给 `preview.intercept.tablePrefix` 说明——表名
   位置是从渲染文本里认出来的（启发式），按产品纪律不能算 `exact`。
4. 条件 id 是**树内路径**（`1`、`2.w0`、`2.w1`…），不是参考版的 test 表达式原文：同一
   test 出现两次时参考版会撞 id，路径不会。页面只显示 `label`（test 原文），id 仅协议内用。
5. 一次渲染的条件状态会冻结进 store（`PreviewStore.conditions`），Refresh / 改方言时沿用；
   重新点「解析」（`preview-render`）、加载文件、切语句都回到该文档/语句自己的默认。

## 3 设置键（`settings/sql-preview.json` + `src/settings.rs` 双向一致）

| key | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `preview.injectLogicDelete` | bool | `false` | 模拟逻辑删除（SELECT/DELETE） |
| `preview.logicDeleteColumn` | string | `deleted` | 逻辑删除列 |
| `preview.logicDeleteValue` | string | `0` | 逻辑删除值（字面量） |
| `preview.injectTenantLine` | bool | `false` | 模拟多租户（SELECT/UPDATE/DELETE） |
| `preview.tenantColumnName` | string | `tenant_id` | 租户列 |
| `preview.tenantColumnDefault` | string | `1` | 租户值（渲染为 `?` 的参数值） |
| `preview.paginationSim` | bool | `false` | 模拟分页（仅 SELECT） |
| `preview.paginationCurrent` | integer | `1` | 页码（`Page.current`） |
| `preview.paginationSize` | integer | `10` | 每页条数（`Page.size`） |
| `preview.tablePrefix` | string | `""` | 表名前缀（空前缀 = 关） |

`DEFINITIONS` 由 46 项增至 56 项，新组 `MyBatis Plus`；schema 与代码的一致性测试
（`schema_json_matches_the_code_definitions` / `schema_has_no_extra_properties`）已过。

## 4 模型与协议（增量，向后兼容）

`WorkbenchModel` 新增两个字段，均 `#[serde(default)]`：

```rust
pub struct ParamView {
    pub name: String,        // `#{}` 名；拦截器追加的是列名 / page.offset / page.size
    pub kind: String,        // text|number|temporal|collection|object|unknown
    pub kind_source: String, // declared|inferred|value|none
    pub placeholder: String, // 已本地化的空框提示
    pub value: String,       // null → 空串
    pub warning: String,     // 已本地化的类型不符提示（提示不是闸门）
    pub injected: bool,      // true = 拦截器追加的 `?`
}
pub struct ConditionView {
    pub id: String,      // 树内路径
    pub label: String,   // test 原文
    pub enabled: bool,
    pub kind: String,    // if|when
    pub group: String,   // 所属 <choose> 的路径（if 为空）
}
```

页面 → 插件新增一个 op：`{op:"preview-conditions", conditions:{id:bool}}`（只接受宿主
已知 id；输入框不重写，复选框不参与重渲染的 `replaceInput`）。

## 5 已知限制（登记，不在本车道扩）

- `#{}` 类型只来自三处：token 自带的 `jdbcType`/`javaType`、集合命名启发式、绑定值的
  JSON 类型。参考版从 mapper 方法签名取类型，本仓无 PSI/类型解析 → 多数参数是 `unknown`
  （不显示徽章、不告警），已按参考版 UNKNOWN 口径处理。
- `<trim>/<where>/<set>` 是结构节点不是条件节点，不可单独开合（参考版同口径，强开会让
  SQL 结构失效）；`<choose>` 只逐个开合它的 `<when>`，`<otherwise>` 不参与（无 test）。
- 多租户 INSERT 列追加未做：参考版经实体字段展开列清单，本仓纯文本无法可靠对齐「列清单
  ↔ 值清单」。
- `<foreach>` 的参数名：预览用空参数渲染，集合为空时不产生 `?`，因此面板上没有展开项行；
  有值时名字显示为 `<item>`（`__frch_x_0` → `x`）——已单测覆盖。