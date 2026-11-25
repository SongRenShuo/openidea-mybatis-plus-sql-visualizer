# SDK API 速查（子代理必读）

> 权威只有两处：**SDK 源码** 与 **清单参考**。本文只是索引，签名不符时以源码为准。
> 本机路径（cargo 已 fetch 到本地 checkout）：

```
SDK_SRC = C:\Users\USER\.cargo\git\checkouts\openidea-7faf5f9c9e6a9886\30e72ff\sdk\rust\openidea-plugin\src
MANIFEST_REF = C:\Users\USER\.cargo\git\checkouts\openidea-7faf5f9c9e6a9886\30e72ff\plugins\README.md
OFFICIAL_PLUGINS = C:\Users\USER\.cargo\git\checkouts\openidea-7faf5f9c9e6a9886\30e72ff\plugins\official
```

`MANIFEST_REF` 里逐节写了 `plugin.toml` 的每个 `[[contributes.*]]`、能力风险分级、provider kinds、
视图字段、MCP 工具 schema、javaAgent、事件与预算。**写清单前先读对应小节**。

## 1 入口

```rust
pub trait Plugin: 'static {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error> { Ok(()) }
    fn deactivate(&mut self) {}
    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error>;
    fn handle_events(&mut self, _events: &[Event], _ctx: &Invocation) {}
}
```

可选接口（`register_plugin!(Main, views, providers, indexing, mcp, ...)`，由主控加 token）：

| token | trait | 用途 |
| --- | --- | --- |
| `views` | `openidea_plugin::views::Views` | `[[contributes.views]]` 面板 |
| `providers` | `openidea_plugin::providers::Providers` | completion / definition / line_markers |
| `indexing` | `openidea_plugin::indexing::DocumentIndexer` | `documentIndexers` 图 |
| `mcp` | `openidea_plugin::McpProvider` | `[[contributes.mcpTools]]` |
| `diagnostics` | `openidea_plugin::diagnostics::DiagnosticsProvider` | 未保存文档诊断 |
| `injections` | `openidea_plugin::language_features::Injections` | mapper XML 内 SQL 注入 |
| `inlay_hints` | `openidea_plugin::inlay_hints::InlayHints` | inlay 提示 |

## 2 常用 host 函数（`openidea_plugin::host`）

```rust
host::projects() -> Result<Vec<Project>, Error>
host::project_of(path: &str) -> Option<Project>
host::read_file(path) / write_file(path, text)          // 路径级（需 FS 能力）
host::open_editors() -> Vec<OpenEditor>                  // 含 path/selection/text?
host::output_append(channel, text) / output_clear / output_reveal
host::notify(level, title, body) / log(level, msg)
host::copy_to_clipboard(text) / read_clipboard()
host::begin_task(title, cancellable) -> Task  // Task::report(done,total,msg) / is_cancelled()
host::setting(key) / setting_bool / setting_string / setting_number
host::run_process(program, args, stdin) -> ProcessOutput
host::begin_loopback(path) -> Loopback                   // 本地 HTTP 预览页
host::WebSocket::connect(&project, &WebSocketRequest{ url, headers? })
host::subscribe(EventKind) / subscribe_paths(kind, globs)
host::publish_diagnostics(path, items)
host::set_decorations / set_file_icon / set_file_icons
host::reveal_view(view) / refresh_view(view) / post_view_message(view, msg)
host::fetch(&HttpRequest) -> HttpResponse                // 需 net:<host>
host::secret_set / secret_is_set / secret_delete
host::locale() -> String
```

`Project`：

```rust
project.root() -> Result<String>
project.read_file(path) / document(path) / write_file(path, text)
project.symbols(query, limit) -> Vec<SymbolInfo>   // 容器限定查询如 "User." 成立
project.search_text(&TextQuery, limit) -> TextSearch
```

## 3 视图（L5）

```rust
trait Views {
    fn render(&mut self, view: &str, viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error>;
    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> { Ok(()) }
}
```

`View::new(Body)`；`Body::Table` / `Body::sortable_table` / `Body::Fields` / `Body::Html`;
`Button::new(id, label)`、`View::status`、`View::message`。web 视图用 `host::begin_loopback`
起本地页 + `post_view_message`/`refresh_view` 推送（页面侧 `window.openidea`）。CSP 内不得外链。

## 4 导航/注入（L6）

```rust
trait DocumentIndexer { fn index(&mut self, doc: &Document) -> Edges; }   // Edges{definitions,references,aliases,markers}
trait Providers { definition / line_markers / completion / code_actions }
trait Injections { fn injections(&mut self, doc: &Document) -> Vec<Injection>; }
```

`DocumentIndexer::Document` 带 `path/text/symbols`；`Anchor{key, range}`、`Alias{prefix,target}`。
导航默认关：设置键 `nav.enabled=false`，provider 内先读设置再返回空。

## 5 MCP（L9）

```rust
pub trait McpProvider { fn call(&mut self, tool: &str, arguments: &str, ctx: &Invocation) -> Result<String, Error>; }
```

`[[contributes.mcpTools]]`：`name`、`description`、`inputSchema`（包内 JSON Schema 路径）、
`readOnly`、`destructive`。结果 JSON 必须带 `confidence`；`partial` 不得伪装 `exact`。

## 6 置信度与本地化

- 渲染结果一律 `confidence: "exact" | "partial" | "unresolved"`；`unresolved` 不出 SQL。
- 纯逻辑层只返回 `message_key`（如 `perf.leading_wildcard`）与结构化数据，文案由 `src/i18n.rs` 映射。
- 新增文案键写 `docs/registration/<lane>.md`，不要改 `locales/*.json`。

## 7 门禁

```
export PATH="/c/Users/USER/.cargo/bin:$PATH"
cargo fmt --all --check
cargo test <module>            # 只跑自己模块；不跑全量、不跑 clippy、不 clean
# 需要组件校验时只跑一次：
openidea-plugin build . && openidea-plugin check .
```

不做：`cargo clippy --all-targets`（CI 权威）、`cargo test` 全量、`git add -A`、push。