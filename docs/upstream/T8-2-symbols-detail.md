# T8-2 上游提案：暴露符号 `detail`（方法形参、字段类型、返回类型）

状态：Draft
目标版本：1.15 之后的下一个 minor（接口草案按 `@1.16.0` 写，实际版本由上游排期决定）
提出方：OpenIDEA 端口（openidea-mybatis-plus-sql-visualizer），非上游仓库提交

> 证据基线：上游 checkout `30e72ffa5`（tag `v0.1.51`，SDK `openidea-plugin` 0.1.51）。
> 下文 `wit/…`、`crates/…`、`plugins/README.md` 均相对该 checkout；行号在该提交上核对，
> 升级 SDK 后需复核。本仓路径相对 openidea-mybatis-plus-sql-visualizer。

## 问题

插件做 MyBatis 渲染时，需要把 Java 标识符落到列名、把方法形参落到 SQL 参数。1.15 的
`workspace.symbols` 只返回名字、种类、容器和位置（`wit/workspace.wit:34-40`），没有字段类型、
方法形参名/类型、返回类型。上游明确把类型解析与成员查询推迟到异步的 API 2.0
（`plugins/README.md:873-874`）。结果是插件只能在 wasm 侧自写词法/结构解析，同一份 Java 源被
重复解析，且拿不到方法签名里的形参名。

## 现状证据

| 事实 | 出处 |
| --- | --- |
| `symbol-info` 只有 `name` / `kind` / `container` / `location` | `wit/workspace.wit:34-40` |
| 查询入口是 `symbols(query, limit)`，模糊、只读当前内存索引 | `wit/workspace.wit:105` |
| README 的 API 表同样只承诺「name、camelCase kind、container、UTF-16 location」 | `plugins/README.md:839` |
| 结果可能不完整、位置可能落后于未保存编辑，取消/截止时间随调用 | `plugins/README.md:865-871` |
| 类型解析与成员查询明确推迟到 API 2.0 | `plugins/README.md:873-874` |
| 索引内部 `StoredSymbol` 只存 name / kind / container / range / bases | `crates/ide-index/src/data.rs:104-112` |
| 对外 `SymbolMatch` 不含 detail，也不暴露 bases | `crates/ide-index/src/lib.rs:42-50` |
| 宿主对符号查询的既有上限：query ≤256 UTF-8 字节、limit 夹到 1000 | `crates/ide-host/src/plugins/api.rs:2425`、`:2430` |
| `document-symbol` 记录里有 `detail` 字段，但那是插件导出、且只在 LSP 没有文档符号时使用 | `wit/language-features.wit:71-78`（`detail` 在 `:73`）、`plugins/README.md:1648` |
| 符号查询需要 `project.read` | `plugins/README.md:351` |
| provider 预算 50 ms 软 / 500 ms 硬，命令 5 s，task 60 s | `plugins/README.md:1506`、`:2649-2653` |

本仓的现状是纯文本分析：Wrapper 链由 `src/analyze` 解析，实体列由调用方传入；不在
`EntityColumns` 里的字段直接判 `unresolved`，不出 SQL（`src/analyze/wrapper.rs:13-18`）。
平台没有 PSI/类型解析也是架构前提（`docs/ARCHITECTURE.md:9`）。所以本提案的目标不是「拿到
完整类型系统」，而是把宿主已经拥有的语法级信息暴露出来，减少重复解析与猜测。

## 提议

### 为什么是「语法级 detail」而不是「类型解析」

- 宿主索引由语言语法层产出（`crates/ide-syntax`；Java 的符号提取开关见 `languages/java.toml:59`），
  能给出声明处的语法文本，但做不了跨文件泛型实参、重载决议。提案只承诺 `List<User>` 这样的
  **语法文本**，不承诺 `User` 的最终类型。
- 这与产品的「不猜」纪律一致：`partial` 必须显式标注。插件拿到 `type-text` 后仍可判
  `unresolved`（例如泛型元素类型不在 `EntityColumns` 里）。

### WIT 草案

新增独立接口 `symbol-details`，避免改动已发布的 `symbol-info`（给已发布记录加字段会导致组件
不链接，`plugins/README.md:3031-3039`）。不新增能力：仍用 `project.read`（`plugins/README.md:351`）。

```wit
package openidea:plugin@1.16.0;

interface symbol-details {
    use types.{error, location};
    use workspace.{project};

    record parameter {
        name: string,
        /// Syntactic type text as written (`List<User>`, `int[]`); none when not parsed.
        type-text: option<string>,
        /// Variadic / rest parameter; false also when the language has no such concept.
        variadic: bool,
    }

    record symbol-detail {
        name: string,
        kind: string,
        container: option<string>,
        location: location,
        /// Syntactic type of a field / local / parameter; never cross-file resolved.
        type-text: option<string>,
        /// Declared return type text for methods and functions.
        return-type: option<string>,
        /// none = unknown (not parsed); an empty list = declared with no parameters.
        parameters: option<list<parameter>>,
        /// Base type names written in the declaration; empty for non-types.
        bases: list<string>,
        /// Declaration text normalized to one line, host-capped (64 KiB).
        signature: option<string>,
        /// "index" when every returned field came from the index; "syntax" when a file parse ran.
        source: string,
        /// Requested field names that could not be returned; empty when all were.
        missing: list<string>,
        /// Why fields are missing: language-unsupported | parse-failed | deadline |
        /// symbol-not-indexed | unsaved-edits. none when `missing` is empty.
        reason: option<string>,
    }

    record member-list {
        items: list<symbol-detail>,
        has-more: bool,
    }

    /// Detail of the innermost indexed symbol at `at`. A range whose start equals
    /// its end is treated as a position; none when no symbol contains it.
    /// `fields` accepts "type-text", "return-type", "parameters", "bases",
    /// "signature"; unknown names are ignored (the set may grow).
    /// Requires project.read.
    detail-at: func(project: borrow<project>, at: location, fields: list<string>)
        -> result<option<symbol-detail>, error>;

    /// Members whose `container` equals the given string (as returned by
    /// `symbol-info.container`), optionally filtered by kind (empty = every kind).
    /// At most limit (cap 1000). Requires project.read.
    members: func(project: borrow<project>, container: string, kinds: list<string>, fields: list<string>, limit: u32)
        -> result<member-list, error>;
}
```

`fields` 用字符串列表而不是强类型位域：上游已把会增长的种类集合定成字符串，正是为了后续加值
不改签名（`plugins/README.md:3035-3037`）。新增 detail 字段时，老插件忽略即可。

### 可选字段与缺失语义

| 字段 | `none` | 空值 | 说明 |
| --- | --- | --- | --- |
| `type-text` | 未解析或语言不支持 | 不适用 | 字段/局部变量/形参的语法类型 |
| `return-type` | 未解析 | 不适用 | 方法/函数声明处的返回类型文本 |
| `parameters` | 不知道 | `[]` = 明确没有形参 | 两者必须区分，否则「无参」会被误判成「没解析」 |
| `bases` | 不适用（索引总有值） | `[]` = 无基类 | 索引已有该字段（`crates/ide-index/src/data.rs:111-112`） |
| `signature` | 未解析或过长被裁 | 不适用 | 一行化、截断的声明文本 |
| `missing` / `reason` | 全部字段都拿到 | `missing=[]` | 部分缺失时逐字段列出，避免调用方猜 |

### 预算：哪些字段能同步给，哪些必须进 task

| 阶段 | 字段 | 依据 |
| --- | --- | --- |
| 索引快照，可同步（provider 50 ms 内） | `name` / `kind` / `container` / `location` / `bases` | 已在 `StoredSymbol` 里（`crates/ide-index/src/data.rs:104-112`） |
| 单文件语法解析，需要余量 | `type-text` / `return-type` / `parameters` / `signature` | 要重新走 tree-sitter；语言定义已开启符号提取（`languages/java.toml:59`），但索引没存签名 |
| 批量成员详情 | `members` 的语法字段 | 1000 个成员逐个解析会超出 50 ms，必须进 task（60 s，`plugins/README.md:815`） |

实现建议：

- provider 路径只请求索引字段；若请求了语法字段而当前调用没有余量，立即返回 `missing=[...]`、
  `reason="deadline"`，不要阻塞到 500 ms 硬中断。
- command/task 路径可以拿全量；宿主按 `path + 内容哈希` 做 LRU 文本解析缓存（建议 32 MiB），
  保存/关闭文档时失效。
- 索引与未保存编辑不一致时返回 `reason="unsaved-edits"`，不回退到磁盘文本冒充索引结果。索引本身
  已知会落后于未保存编辑（`plugins/README.md:867-869`）。

### 查询方式：为什么用 location 而不是 symbol id

`symbol-info` 没有 id，给已发布记录加字段会破坏链接（`plugins/README.md:3031-3039`）。`location`
（`path` + `range`，`wit/types.wit:50-53`）是 `symbol-info` 已经带的东西，因此：

- 调用方把 `symbol-info.location` 原样传回 `detail-at`；
- 光标查询传一个空 range（start == end），宿主取包含该点的最内层符号；
- 容器限定用 `members`，`container` 字符串取自 `symbol-info.container`，避免另造一套命名规则。

### 宿主落点

- `crates/ide-host/src/plugins/api.rs`：解析 `project.read` 与项目范围，复用现有 `symbols` 的
  查询/截止时间/排除规则（`crates/ide-host/src/plugins/api.rs:2416-2432`），把结果交给新增的
  detail 解析层。
- `crates/ide-index`：v1 不改持久化格式。`StoredSymbol` 没有签名字段，强行加字段会触发索引格式
  迁移与全量重建；先按需解析（`crates/ide-index/src/data.rs:104-112`）。后续若命中率足够高，再在
  格式版本升级时把 `signature`/`parameters` 落到索引。
- `crates/ide-syntax`：用与索引相同的 tree-sitter grammar 解析声明节点，只取语法文本；未知语言
  直接 `language-unsupported`，不要用正则猜。
- Java/Kotlin 优先；其他语言先返回 `language-unsupported`，按语言定义逐步接入。

## 影响面

| 位置 | 改动 |
| --- | --- |
| `wit/workspace.wit`（及 SDK `wit/` 副本） | 新接口 + package minor 升级 |
| `crates/ide-plugin/src/wit_host.rs`、`runtime.rs` | 绑定与 `HostApi` 方法 |
| `crates/ide-host/src/plugins/api.rs` | 实现：定位、字段请求、预算、未保存编辑判定 |
| `crates/ide-index` / `crates/ide-syntax` | 按需语法解析与缓存（持久格式暂不变） |
| SDK `src/host.rs`、`src/types.rs`、mock `imp` | 包装与测试桩 |
| `tests/api/released/`、`tests/wit_coverage.rs` | 快照与覆盖 |
| `plugins/README.md` API 表 | 新函数与字段语义 |

不新增能力，`plugin.toml` 的能力声明不变；但组件导入了新接口后，宿主 minor 必须足够新
（`plugins/README.md:3045-3049`）。

## 兼容与降级

- 无 detail 的宿主：本仓继续用现有文本启发（`src/analyze` + 调用方提供的 `EntityColumns`），
  未知字段判 `unresolved`，不出 SQL（`src/analyze/wrapper.rs:13-18`）。这与现在的行为一致，
  提案落地只是把「能确定的语法信息」从猜变成读。
- 语法类型不等于解析类型：`List<User>` 只说明源码里这么写；跨文件泛型、继承来的字段仍可能是
  `partial`。插件不得因为拿到 `type-text` 就把结果升级成 `exact`（`docs/ARCHITECTURE.md:45-53`）。
- 未保存编辑：索引落后时返回 `unsaved-edits`；调用方可以提示保存后重查，而不是静默用旧数据。
- 性能回退：detail 解析失败/超时必须降级成 `missing` + `reason`，不能抛错让整个 provider 失败。

## 测试设计

夹具工程（Java + Kotlin）：

1. 类字段：基本类型、数组、泛型（`List<User>`）、静态/常量、注解字段。
2. 方法：无参、多参、可变参、泛型形参、泛型返回值、构造器、重载（同名不同参）。
3. 类型声明：class / interface / enum / record / 内部类 / 匿名类；`extends`/`implements` 多个基类型。
4. Kotlin：data class 默认参数、`vararg`、可空类型（`String?`）、扩展函数。

断言：

- `detail-at` 取 `symbol-info.location` 能返回同名同 kind 的 detail；空 range 落在方法体内取最内层符号。
- 形参名与类型文本逐字匹配；`parameters=[]` 与 `none` 区分；`bases` 与源码一致。
- `members("User.")` 返回字段/方法，`has-more` 在超过 limit 时为 true；跨容器不串。
- 未知语言 → `missing` 非空且 `reason="language-unsupported"`；损坏文件 → `parse-failed`。
- 未保存编辑（改字段名但不保存）→ `reason="unsaved-edits"`（或明确返回索引旧值并标注，二选一，
  需在实现前定死）。
- 预算：800 行 Java 上，索引字段 p95 < 5 ms；全量语法字段在 task 内完成；provider 截止时间前
  返回 `deadline` 而不是硬中断。
- 内存：1000 个成员的 detail 列表不超过既有列表上限与 64 KiB 单串上限（`plugins/README.md:1668-1672`）。

## 开放问题

1. 身份：继续用 `location`，还是在未来的 `symbol-info` 版本里加稳定 id？后者要等一次 minor 的
   记录扩展窗口，不能给已发布记录加字段。
2. 索引格式：是否值得把 `signature`/`parameters` 持久化？收益是 provider 免解析，成本是全量重建
   与格式迁移。
3. 语义边界：`type-text` 是否应该返回注解、泛型约束、Kotlin 的可空标记？建议先只返回声明原文，
   由调用方决定怎么解释。
4. `members` 的 `container` 精确匹配规则：`symbol-info.container` 目前是显示串还是限定名？需要在
   上游确认（本提案按「原样回传」设计，避免自造规则）。
5. 文档注释（Javadoc/KDoc）是否属于 detail？它可能很大，且涉及 64 KiB 上限，建议后置。
6. 是否要为 detail 增加新的能力（如 `project.detail`）？本提案认为不需要，`project.read` 已覆盖
   读项目内容；若上游认为解析成本需要单独授权，可以再加。