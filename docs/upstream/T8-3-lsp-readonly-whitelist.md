# T8-3 上游提案：语言服务器只读方法白名单

状态：Draft
目标版本：1.15 之后的下一个 minor（接口草案按 `@1.16.0` 写，实际版本由上游排期决定）
提出方：OpenIDEA 端口（openidea-mybatis-plus-sql-visualizer），非上游仓库提交

> 证据基线：上游 checkout `30e72ffa5`（tag `v0.1.51`，SDK `openidea-plugin` 0.1.51）。
> 下文 `wit/…`、`crates/…`、`plugins/README.md` 均相对该 checkout；行号在该提交上核对，
> 升级 SDK 后需复核。本仓路径相对 openidea-mybatis-plus-sql-visualizer。

## 问题

插件的导航目前只能靠文本启发：`src/providers/nav.rs:1` 的 definition / line-markers 自己解析
mapper XML 与 Java。IDE 的语言定义把 Java 交给 `jdtls`（`languages/java.toml:32-34`），但插件
碰不到任何 LSP 标准方法。
1.15 的 `language-servers` 只放行服务器自有方法，所有 LSP 命名空间一律 `invalid`
（`crates/ide-lsp/src/plugin_requests.rs:32-38`、`plugins/README.md:938-943`）。于是
`textDocument/definition`、`hover`、`documentSymbol`、`diagnostic` 这些只读查询也一并被挡，
插件只能重复实现或放弃语义精度。

这个封禁是有理由的：文档同步、补全、格式化、rename 由 IDE 自己驱动，插件介入会打架。但「只读
查询」与「改文档」应该分档。本提案只开只读白名单，不碰同步、生命周期与任何会产生编辑的方法。

## 现状证据

| 事实 | 出处 |
| --- | --- |
| 14 个 LSP 命名空间全部拒绝：`textDocument` / `workspace` / `window` / `notebookDocument` / `callHierarchy` / `typeHierarchy` / `completionItem` / `codeAction` / `codeLens` / `documentLink` / `inlayHint` / `workspaceSymbol` / `client` / `telemetry` | `crates/ide-lsp/src/plugin_requests.rs:11-26` |
| 生命周期方法单独拒绝：`initialize` / `initialized` / `shutdown` / `exit` | `crates/ide-lsp/src/plugin_requests.rs:27` |
| 白名单实现就是「非空、不以 `$/` 开头、首段不在 LSP_NAMESPACES 且非生命周期」 | `crates/ide-lsp/src/plugin_requests.rs:32-38` |
| 宿主在插件边界再拒一次，并要求项目受信任 | `crates/ide-host/src/plugins/api.rs:2447-2449`、`:2452-2455` |
| README 的同一份禁令与理由 | `plugins/README.md:938-943` |
| 现有能力 `lsp:<server>` 是 high 风险：服务器会代插件读写项目文件 | `plugins/README.md:364`、`:914-918` |
| 调用只在本项目已运行的服务器上发生，永不启动服务器 | `plugins/README.md:945-946`、`crates/ide-lsp/src/plugin_requests.rs:54-56` |
| 请求随调用的取消与时间预算结束；method ≤256 字节、params ≤1 MiB；未声明结果上限 | `plugins/README.md:947-948` |
| SDK 包装只暴露 `language_server_request` / `language_server_notify` | `sdk/rust/openidea-plugin/src/host.rs:1168-1181` |
| IDE 侧已认识这些方法及其 server capability 键 | `crates/ide-lsp/src/caps.rs:199-212` |
| Java 默认语言服务器是 `jdtls`（能力声明里的 server 名） | `languages/java.toml:32-34` |
| provider 预算 50 ms 软 / 500 ms 硬；命令 5 s；task 60 s | `plugins/README.md:1506`、`:2649-2653` |
| 语言特性列表上限 10,000 项、单串 64 KiB | `plugins/README.md:1668-1672` |
| 语义 token 导出上限 65,536；hover 内容上限 64 KiB | `wit/semantic.wit:28-30`、`wit/hover.wit:20-21` |

## 提议

### 能力与接口

新增一个独立的只读能力 `lsp.read:<server>`，与现有 high 风险的 `lsp:<server>` 分开：

- 现有 `lsp:<server>` 继续只放行服务器自有方法（`phoxite/laravel/routes` 这类），语义不变；
- `lsp.read:<server>` 只放行下面固定白名单里的 LSP 标准只读方法，建议定 medium 风险。

在 `language-servers` 接口新增一个函数（宿主接口可以增长，`plugins/README.md:3040-3043`）：

```wit
package openidea:plugin@1.16.0;

interface language-servers {
    use types.{error};
    use workspace.{project};

    /// Read-only LSP request. Requires `lsp.read:<server>` and a trusted project.
    /// `method` must be in the host's fixed read-only allowlist and advertised by
    /// the server; otherwise invalid. `params` is the method's JSON params.
    /// The call never starts a server; not-found when it is not running.
    /// Results are the server's JSON, capped by the host (1 MiB).
    read: func(project: borrow<project>, server: string, method: string, params: option<string>)
        -> result<string, error>;
}
```

保留原有 `request` / `notify` 不动，老插件行为完全不变。新函数单独放行白名单，避免把
「服务器自有方法」与「LSP 标准方法」混在一个判定里。

### 允许的方法（白名单）

只收「读数据、不改文档、不执行命令」的方法：

| 方法 | 用途 | 宿主结果上限（建议） |
| --- | --- | --- |
| `textDocument/definition` | 跳定义 | 100 个 location |
| `textDocument/declaration` | 跳声明 | 100 个 location |
| `textDocument/typeDefinition` | 跳类型定义 | 100 个 location |
| `textDocument/implementation` | 跳实现 | 100 个 location |
| `textDocument/hover` | 悬停信息 | 64 KiB（对齐 hover provider，`wit/hover.wit:20-21`） |
| `textDocument/documentSymbol` | 文档大纲 | 10,000 项 / 1 MiB |
| `textDocument/diagnostic` | 拉取诊断 | 1,000 条 / 1 MiB |
| `textDocument/signatureHelp` | 签名帮助 | 100 个签名 |
| `textDocument/documentHighlight` | 同符号高亮 | 1,000 项 |
| `textDocument/foldingRange` | 折叠范围 | 10,000 项 |
| `textDocument/selectionRange` | 选区扩展 | 1,000 项 |
| `textDocument/references` | 引用检索 | 10,000 个 location / 1 MiB |
| `textDocument/prepareCallHierarchy` | 调用层级入口 | 100 项 |
| `callHierarchy/incomingCalls` | 入调用 | 100 项 |
| `callHierarchy/outgoingCalls` | 出调用 | 100 项 |
| `textDocument/prepareTypeHierarchy` | 类型层级入口 | 100 项 |
| `typeHierarchy/supertypes` | 父类型 | 100 项 |
| `typeHierarchy/subtypes` | 子类型 | 100 项 |
| `workspace/symbol` | 全项目符号 | 100 项 / 1 MiB（与 `project.symbols` 重叠，可选） |

上限与语言特性列表口径一致（10,000 项、单串 64 KiB，`plugins/README.md:1668-1672`），结果总字节
另设 1 MiB 硬上限。超限一律 `failed("result too large")`，不截断 JSON（截断会破坏调用方对结构的假设）。

### 禁止的方法

| 类别 | 方法 | 原因 |
| --- | --- | --- |
| 重命名 | `textDocument/rename`、`textDocument/prepareRename` | 返回 `WorkspaceEdit`，等价于写项目 |
| 格式化 | `textDocument/formatting`、`textDocument/rangeFormatting`、`textDocument/onTypeFormatting` | 返回编辑；IDE 已有 formatter 选择 |
| 代码动作 | `textDocument/codeAction`、`codeAction/resolve` | 可携带编辑/命令 |
| 补全 | `textDocument/completion`、`completionItem/resolve` | 体量大、IDE 自己驱动；resolve 可触发服务器工作 |
| 命令 | `workspace/executeCommand` | 任意服务器命令，等于把服务器当执行器 |
| 编辑应用 | `workspace/applyEdit` | 本来就是 server→client 方向；插件发它没有语义 |
| 文档同步 | `textDocument/didOpen`、`didChange`、`didClose`、`didSave`、`willSave`、`willSaveWaitUntil` | 会改 IDE 的文档状态 |
| 工作区同步 | `workspace/didChangeConfiguration`、`didChangeWatchedFiles`、`didCreateFiles`、`didRenameFiles`、`didDeleteFiles` | 会改服务器视图 |
| 生命周期 | `initialize`、`initialized`、`shutdown`、`exit`、`$/…` | 服务器由 IDE 管理 |
| 客户端/遥测 | `window/*`、`client/*`、`telemetry/*`、`notebookDocument/*` | 不属于插件 |
| 语义 token / inlay | `textDocument/semanticTokens/*`、`textDocument/inlayHint` | 体量大，IDE 已在渲染；插件没有增量价值 |

`codeLens` 本身是只读的，但结果可携带命令对象，且对插件价值低，v1 一并排除；未来若需要，按
「新方法加白名单」处理，不改 WIT 签名。

### 宿主侧校验（安全）

1. 方法必须在固定白名单里；服务器没声明该 capability 时 `invalid`（IDE 已有 capability 表，
   `crates/ide-lsp/src/caps.rs:199-212`）。
2. 服务器必须已为本项目运行；永不启动服务器（`plugins/README.md:945-946`）。
3. 项目必须受信任（`crates/ide-host/src/plugins/api.rs:2452-2455`）。
4. `params` 必须是 JSON 对象、≤1 MiB；其中所有 `textDocument.uri` / `rootUri` / `workspaceFolders`
   路径必须解析到项目根内，拒绝 `file://` 越界、`untitled:`、符号链接逃逸。
5. 防御性拒绝编辑类键：`newName`、`edit`、`edits`、`changes`、`documentChanges`、`arguments`；
   即使方法在允许列表里，出现这些键也按 `invalid` 处理。
6. 宿主只把服务器返回的 JSON 原样交给插件，从不解析并应用其中的 `WorkspaceEdit`。
7. 结果总字节 >1 MiB 直接 `failed`；超时/取消沿用调用的 token 与 deadline。
8. 错误文本做脱敏（不含项目外路径、服务器启动参数、环境变量）。

### 配额与缓存

- 每项目 + 每服务器：30 次/分钟、4 个并发；超限 `failed("quota")`（或 `cancelled`，需上游定）。
- 缓存键：`(项目根, 服务器名, 服务器进程代号, 方法, 文档路径, 文档版本, position/range, params 哈希)`。
  进程重启或文档版本变化即失效；`workspace/symbol` 用 `(项目根, 服务器名, 进程代号, 方法, params 哈希)`。
- 缓存容量建议 256 条 / 4 MiB，TTL 30 s，LRU 淘汰。缓存只存最近结果，不做跨会话持久化。

### 预算与 provider 的关系

- 现有 provider 走 50 ms 软 / 500 ms 硬（`plugins/README.md:1506`、`:2649-2653`）。一次 LSP 往返
  通常超过 50 ms，**provider 不得同步等待 LSP**；只有缓存命中时可以在 provider 内使用结果。
- 命令、视图动作、MCP 调用放进 task（60 s，`plugins/README.md:815`）；LSP 调用随 task 的预算结束。
- 本仓的 definition / line-markers 继续作为基线（`src/providers/nav.rs:1`）；LSP 结果作为补充，
  合并时去重，LSP 不可用时回落到现有实现。不能把「LSP 没返回」显示成「没有定义」——要区分
  `unavailable` 与空结果。

### 与现有 `lsp:<server>` 的关系

- 服务器自有方法（`rust-analyzer/expandMacro`、框架路由等）继续走老函数与老能力。
- 只读 LSP 方法走新函数与新能力；两类授权分开，用户可以在不批准「服务器代插件改文件」的情况下
  批准只读查询。
- 若上游认为两个能力太碎，可以把 `lsp.read:<server>` 作为 `lsp:<server>` 的一个隐含子集；但那样
  用户看到的仍是 high 风险文案，与「只读」语义不符，需要在同意页单独说明。

## 影响面

| 位置 | 改动 |
| --- | --- |
| `wit/host.wit`（及 SDK `wit/` 副本） | 新函数 + package minor 升级 |
| `crates/ide-lsp/src/plugin_requests.rs` | 新白名单常量与 `plugin_read_method_allowed`；现有 `plugin_method_allowed` 不动 |
| `crates/ide-host/src/plugins/api.rs` | 能力检查、URI/参数校验、结果上限、脱敏 |
| `crates/ide-plugin/src/wit_host.rs`、`runtime.rs` | 绑定与 `HostApi` 方法 |
| `crates/ide-extension/src/capability.rs` + 同意 UI | 新能力 `lsp.read:<server>`（medium） |
| SDK `src/host.rs`、mock `imp` | 公开包装与测试桩 |
| `tests/api/released/`、`tests/wit_coverage.rs` | 快照与覆盖 |
| `plugins/README.md` | 能力表、LSP 小节、API 表 |

声明形态：

```toml
engines.openidea = "^1.16"

[capabilities]
optional = ["lsp.read:jdtls"]
```

`<server>` 的名字来自语言定义里的 `[[language-servers]]`（`plugins/README.md:916-918`）；客户端
不能自定义服务器，也不能传入进程参数。

## 兼容与降级

- 版本门：导入新函数要求宿主 minor 足够新；旧 IDE 会拒绝安装/`check`（`plugins/README.md:3045-3049`）。
  optional 只控制权限，不解决 API 不存在的问题。
- 权限被拒或服务器未运行：`denied` / `not-found`；插件保持现有文本导航，不报错、不空转。
- 服务器不支持某能力：`invalid` 或服务器的 `MethodNotFound`（`failed`）；插件按「该项不可用」降级。
- 结果超限/超时：`failed`；调用方可以缩小范围重试（例如 references 限定 range），不得把超限结果
  当完整结果使用。
- 服务器重启：缓存按进程代号整体失效，避免拿到上一个进程的旧结果。

## 测试设计

1. 白名单矩阵：上表每个允许方法各一条 mock 应答；每个禁止方法各一条，断言 `invalid` 且没有
   触达服务器（用 mock 的请求计数）。
2. URI 逃逸：`file:///etc/passwd`、`../../outside`、`untitled:Untitled-1`、符号链接指向项目外，
   全部 `invalid`；项目内相对路径/绝对路径通过。
3. 编辑键注入：在允许方法的 params 里塞 `newName` / `edits` / `arguments`，断言 `invalid`。
4. 结果上限：返回 2 MiB JSON → `failed("result too large")`；返回 900 KiB → 原样通过。
5. 超时/取消：mock 延迟超过 task 预算 → `cancelled`；不留下悬挂请求。
6. 信任与生命周期：未信任项目 `denied`；未运行服务器 `not-found` 且断言没有启动进程；
   `initialize` / `shutdown` / `$/` 仍 `invalid`。
7. 配额与缓存：31 次/分钟被限；同键第二次命中缓存；文档版本变化、服务器重启后缓存失效。
8. provider 回退：`lsp.read` 被拒时，`Providers::definition` 仍返回现有文本结果；LSP 有结果时
   合并去重。
9. 真机矩阵：至少 JDK language server（definition/hover/documentSymbol/diagnostic）与一个
   非 Java 服务器（如 rust-analyzer 的 definition）各跑一遍，确认 capability 映射与结果形状。

## 开放问题

1. 能力粒度：`lsp.read:<server>` 独立，还是复用 `lsp:<server>`？本提案倾向前者，理由是同意文案
   要能区分「只读查询」与「服务器代插件改文件」。
2. 结果上限：1 MiB 是否合适？`workspace/symbol`、`references` 可能天然很大，或许需要按方法分档。
3. 超配额返回什么：`failed("quota")` 还是 `cancelled`？前者可诊断，后者更像「稍后再试」。
4. 是否允许 `workspace/symbol`：它与 `project.symbols` 重叠，但能拿到 LSP 的语义排序；若不需要，
   可以从 v1 白名单删掉。
5. 异步化：1.x 同步调用使 provider 无法等待 LSP；真正的 hover/definition 合并需要 2.0 的异步模型，
   本提案只解决「能在命令/task 里查」。
6. 服务器自有的只读方法（非 LSP 命名空间）是否也应纳入统一白名单？现状是老函数全放行，
   本提案不动它，避免行为变化。