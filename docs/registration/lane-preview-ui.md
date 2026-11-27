# L1 注册申请（feat/preview-ui，预览 UI 对齐）

本车道只改 `web/preview.css`、`web/preview.js`、`src/views/workbench.rs`，**未改**
`plugin.toml`、`src/lib.rs`、`Cargo.toml` / `Cargo.lock`、`README.md`、`locales/*.json`、
`src/i18n.rs`，也未改 `web/log-panel.html` 与 `src/views/log_panel/mod.rs`。没有新增
依赖、命令、能力、provider 或 MCP 工具。

## plugin.toml

无。`[[contributes.dialogs]] sqlv-preview` 的 900×640 保持原样（本任务允许改 900×600，
但 manifest 属禁改文件，差异留待主控在合并点决定）。

## capabilities

无新增。页面仍只走 `openidea.postMessage` / `onMessage` 与 `openidea.dialog.close()`。

## src/lib.rs / Cargo.toml

无。

## op 协议变化（页面 → 宿主）

| op / 字段 | 方向 | 说明 |
| --- | --- | --- |
| `preview-copy-error` | 新增 op | 复制 `model.reason` 原文到剪贴板；原因空时只回状态，不伪造成功。处理在 `workbench::handle_preview_message`，无新命令。 |
| `preview-display` 的 `mode` | 新增字段 | `normal` / `formatted` / `keyword_wrap` / `vertical`，四档显示模式。 |
| `preview-display` 的 `compact` | 新增字段 | 压缩（单行）开关。 |
| `preview-display` 的 `keywordWrap` / `vertical` | 保留 | 旧页面仍可发送；宿主映射到 `mode`，未知值回落 `normal`。 |
| `tabs.preview.display` | 扩展字段 | 新增 `mode` / `compact`，并保留由 mode 派生的 `keywordWrap` / `vertical` 供旧页读取。 |

模型 JSON 字段没有变化（`sql / params / dialect / confidence / source / kind / reason /
advice / statements / statement / parameters / conditions` 原样）。

## 需要主控补的文案键（本车道先用页面内双语表）

`web/preview.js` 的 `PV_TEXT` 已带 zh / en 两份，按宿主 `labels` 里是否含 CJK 选语言，
所以合并后即使主控暂不补键也能正确显示。要进 `src/i18n.rs` 的键：

| key | zh | en | 用途 |
| --- | --- | --- | --- |
| `view.workbench.meta.dialect` | 方言: | Dialect: | meta 行 |
| `view.workbench.meta.kind` | 类型: | Type: | meta 行 |
| `view.workbench.meta.table` | 表: | Table: | meta 行（本渲染无表元数据时隐藏） |
| `view.workbench.kind.sql` | SQL | SQL | 类型值 |
| `view.workbench.kind.xml` | Mapper XML | Mapper XML | 类型值 |
| `view.workbench.action.statement_type` | 语句: {0} | Statement: {0} | 多消费档位时显示（本构建恒隐藏） |
| `view.workbench.action.copy_error` | 复制错误信息 | Copy error | 有 reason 时显示 |
| `view.workbench.mode.compact` | 压缩 | Compact | 压缩/多行翻转按钮 |
| `view.workbench.mode.multiline` | 多行 | Multi-line | 同上 |
| `view.workbench.action.rollback` | 回滚 SQL | Rollback SQL | 「更多」首项（本构建禁用） |
| `view.workbench.action.switch_datasource` | 切换数据源 | Switch data source | 「更多」第 2 项（禁用） |
| `view.workbench.action.import_params` | 导入参数 | Import parameters | 「更多」第 3 项（接剪贴板导入） |
| `view.workbench.action.compare` | 差异对比 | Compare | 「更多」第 4 项（接 `preview-diff`） |
| `view.workbench.action.portable_export` | 导出可移植 SQL | Export portable SQL | 「更多」第 6 项（接 `preview-export`，导出格式化 SQL 与来源头注释，不做形态标注） |
| `view.workbench.action.replay_export` | 导出回放脚本 | Export replay script | 第 7 项（禁用：无执行通道） |
| `view.workbench.action.ai_context` | 复制为 AI 上下文 | Copy as AI context | 第 8 项（禁用：无脱敏通道） |
| `view.workbench.action.ai_context_identifiers` | 复制为 AI 上下文（含表列名） | Copy as AI context (with identifiers) | 第 9 项（禁用） |
| `view.workbench.action.dml_impact` | 预估影响行数 | Estimate affected rows | 第 10 项（禁用：从不执行 SQL） |
| `view.workbench.preset.label` | 预设: | Preset: | 折叠块标题 |
| `view.workbench.preset.default` | 默认预设 | Default preset | 下拉唯一项 |
| `view.workbench.preset.manage` | 管理预设… | Manage presets… | 管理按钮（禁用：无存储后端） |
| `view.workbench.condition.clear` | 清除 | Clear | 条件分区右侧（清空参数值，不动勾选） |
| `view.workbench.param.placeholder.number` | 数字，如 42 | Number, e.g. 42 | 类型 placeholder |
| `view.workbench.param.placeholder.temporal` | 日期，如 2026-01-01 12:00:00 | Date, e.g. 2026-01-01 12:00:00 | 同上 |
| `view.workbench.param.placeholder.collection` | [1, 2, 3] | [1, 2, 3] | 同上 |
| `view.workbench.param.placeholder.object` | {"id": 1} | {"id": 1} | 同上 |
| `view.workbench.action.copy_sql_and_close` | 复制 SQL 并关闭 | Copy SQL and close | 弹窗主按钮 |
| `view.workbench.action.cancel` | 取消 | Cancel | 弹窗取消 |

禁用原因文案（`rollbackNote / switchDataSourceNote / portableExportNote / replayExportNote /
aiContextNote / dmlImpactNote / executeNote / explainNote / presetManageNote / clearNote`）
同样在 `PV_TEXT` 里；主控补 i18n 时应一并落盘，否则页面回退到页面内文本。

## README 能力表

无新增能力。

## 未做项与边界（不是漏做）

1. **表名（meta 的「表」段）**：`WorkbenchModel` 没有表元数据；按「不猜」原则隐藏，不从
   SQL 反推。
2. **语句类型按钮**：原版在存在多消费档位（Lambda / Wrapper 等）时显示；本构建没有消费
   档位模型，按钮恒隐藏。现有 mapper 语句切换仍走 meta 行的 `#pvStatement` 下拉。
3. **回滚 SQL / 切换数据源 / 导出回放脚本 / AI 上下文 / 预估影响行数**：分别缺主键元数据、
   数据源绑定、执行通道、脱敏通道、只读 COUNT；入口置灰并在 title / aria-label 说明。
4. **导出可移植 SQL**：接到现有 `preview-export`（导出格式化 SQL + 来源头注释），不做原版
   的方言形态标注；tooltip 已写明这一差异。
5. **预设存储 / 条件值输入**：没有预设后端与逐条件值模型；预设折叠块收起、控件置灰并说明，
   条件行只有勾选框。
6. **弹窗尺寸 900×600**：`plugin.toml` 禁止本车道改动，保持 900×640。

## 本地门禁读数（2026-11-27）

```
node scripts/check-web-syntax.mjs     # ok: 5 web script(s) parse
cargo fmt --all --check               # 通过
cargo test views:: --lib              # 见合并报告（workbench 90 passed）
```