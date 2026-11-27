# 车道 A（日志面板对齐）注册申请

本车道改了 `web/log-panel.html`、`src/views/log_panel.rs`、`src/capture/bridge.rs`、
`src/commands/capture_start.rs`、`src/i18n.rs`。

**不需要改 `plugin.toml` / `src/lib.rs` / `Cargo.toml` / `README.md`**：

- 没有新命令：排序沿用已有的页面 op `sort`；重连用新的页面 op `capture-retry`（页面 →
  插件消息，不是 `[[contributes.commands]]`）。
- 没有新能力：仍然只走已有的 WebSocket 客户端连接，没有新增 `net:` / `process:` 声明。
- 没有新依赖。
- 工具栏图标**没有**新增清单引用：原版 24 族 SVG 仍按 L19 的结论留在 `icons/toolbar/`
  但不进安装包，本车道把用到的 10 个图形**逐字内联进页面**（同一份 Apache-2.0 路径数据，
  颜色改走 `--openidea-*` 宿主变量）。因此不需要 `[[contributes.markerIcons]]` 之类的声明。

## locales/*.json 同步（唯一需要主控做的事）

`src/i18n.rs` 的 `TABLE` 已经落好下面这些键（面板文案的权威来源）；`locales/*.json`
在主控手上，若要保持两份一致，请同步追加：

| key | en | zh-CN |
| --- | --- | --- |
| `view.logs.sort.label` | `Sort:` | `排序：` |
| `view.logs.sort.time_asc` | `Time ↑` | `时间升序` |
| `view.logs.sort.time_desc` | `Time ↓` | `时间降序` |
| `view.logs.sort.duration_asc` | `Duration ↑` | `耗时升序` |
| `view.logs.sort.duration_desc` | `Duration ↓` | `耗时降序` |
| `view.logs.sort.sequence` | `Row #` | `行号` |
| `view.logs.sort.structure` | `Structure clusters` | `同结构聚类` |
| `view.logs.sort.database` | `Group by database` | `按目标库分组` |
| `view.logs.empty.hint` | `SQL is captured automatically once a console with SQL logging runs (Capture). You can also paste logs manually in the "Log to SQL" tab; for real parameters and exact timings, enable Enhanced Capture in Settings.` | `点「捕捉」并运行带 SQL 日志的控制台后自动捕获；也可从「日志转 SQL」页签手动粘贴还原；想要参数实参与精确耗时，可在设置页开启增强捕获。` |
| `view.logs.status.capture_waiting` | `Waiting for the target application (port {port})` | `等待目标应用启动（端口 {port}）` |
| `capture.start.waiting` | `Capture started; waiting for the target application to start (port {port})` | `已开始捕获，等待目标应用启动（端口 {port}）` |
| `capture.start.connected` | `Connected to the target application (port {port})` | `已接入目标应用（端口 {port}）` |

`{port}` 由调用方 `String::replace` 填入，不是 `format!` 占位符。

## 面板行为变化（供合并点核对）

1. 工具栏去掉暂停按钮（原版 `AgentPauseAction.update()` 无条件
   `isVisible = false`，见 `SqlLogPanel.kt` 1528–1542 行）。`toggle-pause` 的 op 与
   `render_panel()` 的宿主按钮都已撤下；Rust 侧 `toggle-pause` 消息处理保留（能力还在，
   只是没有入口，与原版一致）。
2. 排序按钮从「时间/耗时」两态开关改成 7 项单选菜单；`SortMode` 七值、菜单顺序与原版
   `sortOptions()` 一致。
3. `capture-start` 连不上不再报错：记录等待态并输出
   `已开始捕获，等待目标应用启动（端口 N）[capture.start.waiting] (原因)`；页面每 2 s 发
   `capture-retry`，连上后输出 `已接入…[capture.start.connected]`；`capture-stop` 清等待态
   （清理由 `bridge::disconnect()` 统一做，面板因此停止重连）。注意：等待态下点停止，
   `capture_stop` 的文案仍是 `capture.stop.not_connected`（「no capture WebSocket is open」），
   因为 `capture_stop.rs` 不在本车道写集；要改成「已停止等待」需要主控放行该文件。

## LIMITS 候选（平台做不到 / 本版口径）

1. **「按目标库分组」在当前数据下等价于按时间**：原版按 `databaseName` 分组，本版不采集
   连接名（行徽章只显示方言，`docs/LIMITS.md` 第 5 条「本版实现边界」已记），所有行都落在
   「未知库」组，组内按时间升序——与原版对无库名条目的行为一致。要真正分组需要 agent 侧
   上报连接名。
2. **原版空态带一个可点链接**（跳到设置页「增强捕获」行）：本版空态是两行纯文字，没有链接
   （面板是打包页面，没有跨视图打开设置页的入口）。文案已对齐原版的两行结构。
3. **原版排序菜单是 `JPopupMenu` + `JRadioButtonMenuItem`**：本版是页面内的 `div.menu`
   单选列表，键盘导航只有 Esc 关闭，没有方向键单选（页面没有原生菜单控件）。
