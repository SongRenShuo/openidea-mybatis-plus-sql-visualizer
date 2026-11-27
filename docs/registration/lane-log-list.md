# L2 登记：SQL 日志列表 UI 对齐（高亮 / 行视觉 / 显示模式 / 标记 / 弹层）

本车道只改日志面板的展示层：`web/log-panel.html`、
`src/views/log_panel/{mod,rows,logs,state}.rs`、`src/views/icons.rs`（本轮未动），
外加 `src/i18n.rs` 的追加文案。不新增命令、视图、能力、provider、dialog 或新 op。

## 0. 注册事项

| 单点文件 | 是否申请 | 说明 |
| --- | --- | --- |
| `plugin.toml` | 否 | 没有新命令 / 视图 / 能力 / provider。设置 / 交流群 / 帮助复用既有 `settings`、`help`、`community` op。 |
| `src/lib.rs` | 否 | 没有新模块导出。 |
| `Cargo.toml` / `Cargo.lock` | 否 | 没有新依赖。 |
| `README.md` 能力表 | 否 | 没有新能力。 |
| `locales/*.json` | 否 | 文案走 `src/i18n.rs`（`view.logs.*` 只追加）。 |
| `settings/*` | 否 | 本车道只读 `logs.displayMode`、`log.showDuration`、`log.slowThresholdMs`、`logs.crud.style`；键定义归 L3。 |

## 1. 追加的 i18n 键（只追加，不改既有键）

| 键 | zh-CN | en | 消费者 |
| --- | --- | --- | --- |
| `view.logs.sheet.close` | 关闭 | Close | 页内弹层 × 的 title/aria-label |
| `view.logs.settings.display_mode` | SQL 显示模式 | SQL display mode | 设置弹层行标签 |
| `view.logs.settings.show_duration` | 显示执行耗时 | Show execution duration | 同上 |
| `view.logs.settings.slow_threshold` | 慢查询阈值 | Slow query threshold | 同上 |
| `view.logs.settings.crud_style` | 类型颜色标记 | Type colour marker | 同上 |
| `view.logs.settings.path_hint` | 在 IDE 设置 › 工具 › MyBatis SQL Visualizer › SQL 日志 中修改。 | Change these in Settings › Tools › MyBatis SQL Visualizer › SQL Logs. | 设置弹层底部指引 |
| `view.logs.settings.on` / `.off` | 开 / 关 | On / Off | 布尔值展示 |
| `view.logs.display_mode.single` | 单行摘要 | Single line | 显示模式当前值 |
| `view.logs.display_mode.keyword_wrap` | 关键字换行 | Keyword wrap | 同上 |
| `view.logs.display_mode.formatted` | 完整格式化 | Formatted | 同上 |
| `view.logs.crud.bar` | 左侧色条 | Left bar | CRUD 标记当前值 |
| `view.logs.crud.background` | 整行淡背景 | Row background | 同上 |
| `view.logs.crud.off` | 关闭 | Off | 同上 |
| `view.logs.community.qr_placeholder` | 二维码占位 — 交流群二维码见项目主页。 | QR code placeholder — the group QR is published on the project page. | 交流群弹层 |
| `view.logs.badge.group` | 同结构 {n} 条 | {n} same structure | meta 行同结构徽章（`{n}` 由页面填） |
| `view.logs.badge.n_plus_one_suspect` | 疑似 N+1 | Suspected N+1 | meta 行 N+1 徽章 |

既有键 `view.logs.badge.n_plus_one`、`view.logs.badge.group_prefix`、
`view.logs.menu.settings_page` 保留但不再被页面使用，避免动到别的车道的引用。

## 2. state 新增 / 语义变化

| 字段 | 变化 | 消费者 |
| --- | --- | --- |
| `rows[].sql` | 由「按 180 字符截断的原文」改为按 `displayMode` 准备的展示文本（single 摘要 / keyword_wrap 子句行 / formatted 完整格式化） | 行内语法高亮 |
| `rows[].sqlTail` | 新增；keyword_wrap 超过 3 行时的 ` … (+N)`，其余模式为空串 | 行尾灰字 |
| `displayMode` | 归一化取值改为 `single` / `keyword_wrap` / `formatted`；旧 `multi` 映射到 `formatted` | 单行模式 8px 行边距、列表签名 |
| `crudStyle` | 新增；`bar` / `background` / `off`，读 `logs.crud.style`，缺失回落 `bar` | CRUD 标记形态 |
| `labels.groupBadge` 等 | 新增（见 §1） | 弹层与徽章 |

`rows[].sql` 不再截断到 180 字符：single 模式由 160 字摘要封顶，keyword_wrap 模式在 4096 字处
按空白截断，formatted 模式不封顶（与 `log.maxSqlLength` 的 20000 一致）。复制 / 导出 /
右键动作仍读宿主 store 的完整还原 SQL，不受展示文本影响。

## 3. 与原版的已知偏离

1. **近似耗时 `~`**：页面已支持 `row.duration_approx` 前缀，但宿主不产出该字段。原版的
   `DurationKind.APPROXIMATE` 由解析器的「时长来自相邻行」路径决定，落在
   `src/core/log/*`；该目录不在本车道文件所有权内，未改。四档着色不依赖它。
2. **MCP 紫色角标**：meta 行已有 `.meta .mcp` 的配色与渲染分支（`row.mcp` 为真才画），但
   当前行模型不携带语句来源（`src/views/log_panel/mod.rs` 的 `rows_json` 只有 `db: ""`），
   所以不会有行命中。补来源字段需要解析层改动，同样是本车道所有权外的事。
3. **DDL 的关键字换行**：本端口 `core::format` 不拆 DDL 定义列表，所以 `keyword_wrap` 下
   `CREATE/DROP` 只有一行；原版 `formatClauseLine` 会把列定义逐行排开。CRUD 的 DDL 虚线
   色条不受影响。
4. **`[request id]`**：meta 行在 `[文件:行]`、`[类#方法]` 之后仍保留 `[请求 id]`（原版
   header 片段没有这一项）。它是本端口既有的「按请求聚焦」入口，删掉会失去筛选项，故保留。
5. **`logs.displayMode` 的枚举**：`settings.rs` 里仍是 `single | multi | keyword_wrap`
   （本车道不改 `settings/*`）。宿主把 `multi` 归一化为 `formatted` 并接受 `formatted` 值；
   L3 落 `logs.*` 时可以顺手把枚举改成 `single | keyword_wrap | formatted`。
6. **字体**：列表 SQL 行按原版改用 UI 字体；未做真机对比前的兜底结论以报告里的真机验收为准。
   如果真机观感明显变差，改回 `--openidea-font-mono` 只需一行（`.sql` 的 `font-family`）。

## 4. 弹层而不是 dialog

设置 / 交流群 / 帮助都开同一个 `#sheet` 覆盖层（背板 + 卡片 + ×），内容全部由
`createElement` / `textContent` 构建。三个入口仍 `post` 既有 op：`settings`、`help`、
`community` 在宿主侧继续写通知与 Output，页面侧不再只有这一条路径。