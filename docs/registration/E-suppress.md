# 车道 E（SQL 屏蔽规则）注册申请

对齐原版「管理 SQL 屏蔽规则」（`SqlSuppressRuleManager` + `SqlSuppressManagerDialog`）：
规则持久化、结构匹配、命中计数与面板过滤，入口是日志面板工具栏的 eye-off 动作，
对话框是本版清单里的 `[[contributes.dialogs]] id = "sqlv-suppress"`。

## 写集

- 新增 `src/views/suppress.rs`（模型、持久化、匹配、计数、对话框协议与测试）
- 覆盖 `web/suppress.html`（主控放的占位页，替换为真正的管理对话框）
- `src/views/log_panel.rs`：屏蔽过滤钩子、`manage-suppress` op、视图事件分派、
  结构指纹改为复用 `suppress` 的实现、eye-off 按钮（原弹层）改为打开对话框
- `src/i18n.rs`：**只在 `TABLE` 末尾追加** 29 个 `view.suppress.*` 键，没有重排
- 本文件

没有改 `plugin.toml`（条目由主控加好）、`src/lib.rs`、`Cargo.toml`、`Cargo.lock`、
`README.md`、`locales/*.json`、`.github/`、`web/preview.*`、`src/views/workbench.rs`。

`src/views/mod.rs` 也没动：`suppress` 模块在 `log_panel.rs` 里以
`#[path = "suppress.rs"] pub mod suppress;` 声明，文件仍在 `src/views/suppress.rs`。

## 规则数据模型与存储键

一条规则（`src/views/suppress.rs::SuppressRule`，字段名对齐原版 `SqlSuppressRule`）：

| 字段 | 含义 |
| --- | --- |
| `id` | 唯一 id（毫秒时间戳 + 冲突后缀，取代原版的 UUID） |
| `pattern` | 代表 SQL：`sample_sql` 去首尾空白、空白折叠、**去掉尾部分号**后的规范化文本；列表摘要来源 |
| `sample_sql` | 用户粘贴的原始 SQL；右侧全文展示 |
| `created_at` | 首次时间（毫秒） |
| `hits` | 拦截次数（本版持久化，见「与原版的差异」） |
| `structure` | 是否结构匹配（原版 `structureMatch`） |
| `key` | 匹配指纹：精确口径为 `suppress_fingerprint(pattern)`，结构口径为 `structure_fingerprint(pattern)`；加载时按 `pattern` 重算，杜绝存量键与匹配器口径漂移 |

存储：

- 键：`suppress.<project-hash>.rules`，`project-hash` = 项目根路径的
  `DefaultHasher` 十六进制（与历史页签同一算法，独立命名空间）。
- 值：`{"version":1,"rules":[…]}`。
- `storage` 是插件级（不是项目级），因此用项目哈希前缀实现「按项目」；
  切换项目时 `set_scope` 重新加载，行计数游标清零。
- 解析失败 / 字段缺失按「空规则」处理，下一次保存覆盖；`id` 或 `pattern`
  为空的条目在加载时丢弃。
- 上限 500 条（原版 `MAX_RULES`），超限拒绝新增并提示，不做 LRU 淘汰。

## 结构匹配判定口径

两个键函数与原版逐条同口径（`LogSqlModels.kt` 的 `suppressFingerprint` /
`structureFingerprint`）：

1. 空白归一：`trim` + 连续空白折叠为一个空格（换行也是空白）；
2. 精确口径：去掉字符串尾部连续的 `;`，再 FNV-1a 64 位哈希（十六进制）；
3. 结构口径：在第 2 步的文本上把字符串字面量（含 `''` 转义）与裸数字折叠为
   `?`，再哈希。双引号/反引号标识符不参数化；标识符内的数字不折叠；
   `IN (?,?)` 与 `IN (?,?,?)` 是不同形状；未闭合字面量按一个 `?` 收尾。

与原版的差异只有一处：哈希输入是本版的 UTF-8 字节，原版是 UTF-16 code unit。
ASCII SQL 完全相同，非 ASCII 标识符才可能不同；两版之间从不比较指纹，所以
不影响任何行为。

与车道 A 的关系：车道 A 的 `log_panel::structure_fingerprint` 原来是一份独立
拷贝（带自己的 `parameterise_literals`）。本车道把实现移到 `suppress.rs`，
`log_panel` 改成调用同一函数，删掉重复实现；车道 A 的
`structure_fingerprint_folds_literals_but_not_identifiers` 用例外形不变且全绿，
所以同结构排序与屏蔽规则现在共用一份口径，不会再各写一份。

规则去重比原版严一点：`pattern` 在比较前也剥掉尾部分号，所以
`SELECT 1` 与 `SELECT 1;` 视为同一条规则（原版会留两条匹配行为完全相同的规则）。

大小写敏感（原版 D2 定案），不做任何大小写折叠。

## 命中计数与显示过滤

- **显示过滤**：`build_state` 在 `display_rows` 之后、关键字筛选与上限裁剪之前
  过滤掉命中规则的行业；`visible_sql`（导出可见）与 `all_sql`（复制全部）同样
  排除。规则是「显示时过滤」而不是「入队时丢弃」，所以删除规则后仍在缓冲里的
  行会重新出现（原版明确承诺的行为）。
- **计数**：`ensure_parsed` 每次解析后调用 `note_new_entries`。面板维护一个
  「已看过到哪一行」的游标（`line_no` 单调），只对游标之后、带 SQL 的新行计一次；
  同一次解析被多个 state push 复用也不会重复计数。规则创建时已在缓冲里的行
  只隐藏、不计拦截（与原版「闸门只拦新条目」一致）。`set_log_text` / `clear`
  重置游标（新缓冲的行都算新行）。
- 每计到一次命中立即落盘，重启后计数还在（见差异说明）。

## 对话框交互清单

页面 `web/suppress.html`，由 `views::reveal("sqlv-suppress")` 打开（本版调用
`host::reveal_view`，即 WIT 的 `views.reveal`；只能从命令或视图事件里调用，
面板的 `manage-suppress` op 就是视图事件）。

| 控件 | 行为 |
| --- | --- |
| 左侧规则列表 | 每行 = SQL 摘要 + `首次时间` + `已拦截 N 次`；结构匹配规则多一个「结构匹配」徽章；点击 / Enter / 空格选中 |
| 右侧全文 | 选中规则的 `sample_sql`，只读、等宽、软换行 |
| 新增 | 进入新增态：右侧变可编辑 textarea，按钮变「确认新增」，出现「取消」 |
| 确认新增 | 把 textarea 文本发给宿主；成功则退出新增态并选中新规则；空白/重复/超限保留输入原地提示 |
| 取消 | 退出新增态，恢复到第一条规则 |
| 删除 | 删除选中规则（原版也不二次确认）；删除后宿主立刻重推面板状态 |
| 清空 | 页面内两步确认（「确定删除全部 N 条屏蔽规则？」+ 确认清空/取消），不弹平台模态框 |
| 结构匹配（单条） | 按选中规则切换；开启时按钮文案变「关闭结构匹配」，列表出现徽章 |
| 全部按结构匹配 | 勾选框；全部规则都开启时才勾选；切换即批量设置 |
| 关闭 | `openidea.dialog.close()`；同时标题栏关闭按钮与 Escape 也可关闭 |
| 空态 | 列表区「还没有屏蔽规则」，删除/清空/结构按钮置灰 |
| 状态行 | 宿主文案（新增/重复/空白/超限/删除/清空/结构开/关），错误用红色 |
| 底部说明 | 「规则即时生效……屏蔽期间新产生的 SQL 已被丢弃、无法恢复」，常驻 |
| Escape | 有清空确认或新增态时先取消（页面 `preventDefault`）；否则交给平台关闭 |

`closed` 事件（`ViewEvent::closed()`）只清瞬态的选中、状态行与「保留新增态」
标志，**不清规则**；随后重推一次面板状态，保证关窗前删掉的规则立刻让缓冲行回来。

面板入口：工具栏 eye-off 按钮（原版第 5 个动作 `ManageSuppressAction`）发
`{"op":"manage-suppress"}`，宿主 `views.reveal` 打开对话框。原来的「已屏蔽方法」
弹层撤掉；`block-method` 右键能力与 `clear-blocked` op 保留，方法屏蔽计数仍在
过滤行的「已屏蔽 N」胶囊里一键恢复。

## 测试读数

本机（worktree `.worktrees/lane-e`）：

- `cargo test --lib views::log_panel` → `111 passed; 0 failed`（含本车道 15 条）
- `cargo test --lib suppress::tests` → `15 passed; 0 failed`（覆盖：增删改、空白/重复/上限、
  精确匹配空白与尾分号归一、结构匹配命中/不命中、只计一次、旧行只隐藏不计数、
  删除后恢复显示、持久化往返（规则 + 计数 + 结构位）、按项目分键、对话框状态字段、
  页面消息驱动规则表、`closed` 后规则仍在、面板入口 reveal、被拒新增保留新增态）
- `cargo test --lib views::` → `178 passed; 0 failed`
- `cargo test --lib i18n::` → `6 passed; 0 failed`（29 个新键的唯一性与中英文非空）
- `cargo test --lib commands::open_logs` → `1 passed`
- `cargo test --lib commands::open_workbench` → `3 passed`
- `cargo test --lib commands::capture_open_log` → `5 passed`

持久化往返用例 `rules_and_hits_survive_a_restart` 的实际断言：新增规则 → 开结构匹配 →
追加一条命中行并 push → `hits == 1`；清空进程内 store 后 `set_scope` 从
`suppress.<hash>.rules` 重新加载，得到 1 条规则、`hits == 1`、`structure == true`，
且结构键命中 `SELECT * FROM heartbeat WHERE id = 7`。

## 门禁读数

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过（`FMT_EXIT=0`） |
| `node scripts/check-web-syntax.mjs` | `ok: 4 web script(s) parse`（含新页面） |
| `cargo test --lib views::log_panel` | `111 passed; 0 failed` |
| `cargo test --lib views::` | `178 passed; 0 failed` |
| `cargo test --lib i18n::` | `6 passed; 0 failed` |
| `cargo test --lib commands::open_logs` / `open_workbench` / `capture_open_log` | `1 / 3 / 5 passed` |

`openidea-plugin build/check` 没跑：本机 CLI 仍是 0.1.51，遇到 `engines.openidea = "^1.18"`
直接报错（环境问题，不是代码问题）；CI 用 0.1.54 校验。

## 与原版的差异 / LIMITS 候选

1. **屏蔽是显示时过滤，不是入队丢弃**。原版在 `SqlLogEntryBuffer` 入队闸门直接
   丢弃，命中行不进去重计数、不占序号、也不会进历史快照；本版的行仍在缓冲区
   文本里，只是不显示、不计入 `counts`、不进导出与「复制全部」。因此
   「删除规则后缓冲行重新显示」成立，但「屏蔽期间新产生的 SQL 已被丢弃、
   无法恢复」这句在**历史快照**上不成立：SQL 历史页签保存的仍是原始日志文本，
   重新打开历史文件仍能看到被规则屏蔽过的语句。要做到真正丢弃，需要把过滤
   前移到 capture 入队（`capture::bridge` → `log_panel::append_text` 之间），
   超出本车道写集。
2. **拦截次数持久化**。原版明确只做会话内计数（重启归零）；本车道的任务契约
   要求「拦截次数」是持久化字段，故按契约落盘。副作用：同一规则跨重启累计。
3. **规则按项目**。原版是 application 级服务（跨项目共享，理由：心跳 SQL 跨项目
   通用）；本版按任务契约按项目存（`suppress.<project-hash>.rules`）。
4. **对话框标题栏是清单里的英文串**（`title = "Manage SQL Suppression Rules"`）。
   `plugin.toml` 不在本车道写集，`locales/*.json` 也在主控手上；若平台按插件
   catalog 翻译清单标题，请主控补一条对应键（平台 README 称清单标题走插件
   翻译表）。页面内所有文案都走 `view.suppress.*`，中英已齐。
5. **没有「屏蔽此 SQL」右键动作 / 多选屏蔽 / balloon**。原版可以从列表右键
   「屏蔽此 SQL」直接成规则（还支持多选、成功后弹 balloon 带「管理屏蔽规则…」
   入口）；本版规则只从对话框的「新增 → 粘贴 SQL → 确认新增」创建。行级入口
   会动 `log_panel` 的右键菜单与选择模型，超出「只改屏蔽相关过滤钩子与那个
   op」的边界；需要时另开一条车道。
6. **清空确认是页面内两步确认**，不是原版的 `Messages.showYesNoDialog`。web 页面
   没有平台模态框 API（`host.confirm` 只能从命令/事件调，且是另一个交互模型），
   页面内两步确认能让用户看清 N 条规则将被删除。
7. **时间是 UTC**（`YYYY-MM-DD HH:MM`）。平台没有时区 API，面板的时间列同样
   按 UTC 展示；不是本车道特有。
8. **面板未打开时的捕获不吃规则**。规则集在首个带 `Invocation` 的面板消息
   （`ready`）里按项目加载；从 Find Action 直接 `capture-start` 且面板从未打开
   时，捕获会先堆文本，等面板打开后规则才生效，且此时首次解析会把已缓冲的
   命中行按「新行」计一次拦截。正常入口（面板上的捕获按钮）不会出现这个顺序。
9. **「全部按结构匹配」是批量写状态**，没有原版的派生态灰显细节（原版也是
   全开才勾选，本版一致）；勾选框在新增态或空列表时禁用。
10. **没有平台级尺寸记忆的额外实现**：对话框尺寸由平台 `Dialog` 自己记住
    （README 的 API 1.18 行为），本页面不重复存。
