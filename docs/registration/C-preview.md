# C 注册片段：SQL 预览核心对齐（车道 C）

本车道只改 `web/preview.js`、`web/preview.css`、`src/views/workbench.rs`，**未改**
`plugin.toml`、`src/lib.rs`、`Cargo.toml`、`Cargo.lock`、`README.md`、`locales/*.json`、
`.github/`、`web/log-panel.html`，也**未改** `src/i18n.rs`（文案键交主控在合并点补，
见 §2）。没有新增依赖、命令、能力、provider 或 MCP 工具。

## 1 本车道已落地的宿主/页面改动（无需主控动作）

| 文件 | 改动 |
| --- | --- |
| `web/preview.js` | 参数列与结果页签之间加可拖拽竖向分隔条（`#pvHSplitter`，默认 30%，鼠标/方向键）；性能建议从 meta 行移到独立 `#pvPerfRow`（原版 `perfRow` 位置）；meta 行加 `#pvSource`（渲染来源：语句 id / 文件）；显示模式从 `<select>` 改成原版式切换按钮（按钮文案 = 目标模式）；「关键字换行」「竖向排列」从「更多」移到工具栏直显（原版位置，保留 L45 的显示态语义）；「更多」重排为 回滚 / 差异 / 分享 / 导出 / 含表列名；SQL 页签加轻量语法着色（`tokenizeSql`，纯展示，拼接后与原文逐字符一致）。 |
| `web/preview.css` | `.pv-perf` / `.pv-hsplitter` / `.pv-source` / `.pv-btn-text` / `.pv-tok-*` 规则；参数列默认宽度 34% → 30%。 |
| `src/views/workbench.rs` | `WorkbenchModel::copy_text()` 改为按原版 `SqlCommentStripper` 口径剥离 `--` / `/* */` 注释（保留字符串与引号标识符、丢弃纯注释行、逐行去尾空白）；新增 `strip_sql_comments` / `skip_quoted_span` / `filter_commented_lines` 与三条单测；contract 断言同步新布局/工具栏顺序/高亮入口。 |

这些改动不改变 op 协议、不新增设置键、不改变模型 JSON 字段，旧页面/旧夹具照常解码。

## 2 需要主控补的文案键（只落 `src/i18n.rs` 的 `TABLE`）

`src/i18n.rs` 是车道公共文件，本车道按所有权约定未改。以下键对应的控件**尚未接线**
（页面/host 一侧按原版位置留好或待补），主控补键后再接：

| key | en | zh-CN | 用途 |
| --- | --- | --- | --- |
| `view.workbench.action.copy_error` | Copy error | 复制错误 | 仅当 `model.reason` 非空时显示；把原因全文放剪贴板（原版 `panel.copyError`） |
| `view.workbench.status.copy_error_empty` | Nothing to copy: this render carries no warning. | 没有可复制的内容：本次渲染没有警示。 | 上项的拒绝态 |
| `view.workbench.status.copy_error_copied` | Error text copied. | 警示文本已复制。 | 上项的成功态 |
| `view.workbench.mode.compact` | Compressed | 压缩 | 原版 `panel.mode.singleline`：单行压缩态 |
| `view.workbench.mode.multiline` | Multi-line | 多行 | 原版 `panel.mode.multiline` |
| `view.workbench.action.import_params` | Import Params | 导入参数 | 原版放在「更多」；本版目前只有参数区的「从剪贴板导入」 |
| `view.workbench.action.replay_export` | Export Replay Script | 导出回放脚本 | 原版与「可移植导出」并列；本版 `Export` 已导出可执行 SQL，是否分两键由主控定 |
| `view.workbench.action.ai_context` | Copy as AI Context | 复制为 AI 上下文 | 需脱敏器，见 §3 |
| `view.workbench.action.ai_context_identifiers` | Copy as AI Context (include table/column names) | 复制为 AI 上下文（含表列名） | 同上 |
| `view.workbench.action.dml_impact` | Estimate Impact | 影响面预演 | 需数据库只读 COUNT，见 §3 |
| `view.workbench.params.source` | (运行期值，无需文案) | | 参数名应显示 `#{name}` / `${name}` 来源，见 §3 |

其中 `copy_error` 只依赖已有的 `model.reason`，主机侧可直接实现；`mode.compact` 若要
真正对齐原版「压缩/多行」两态，需要 `workbench` 增加一个压缩显示态（把 SQL 空白压成单行），
这是纯展示、无依赖，可随文案一起交下一轮。

**没有新增 settings 键**，`settings/*.json` 无需改动。

## 3 平台/引擎边界（建议主控并入 `docs/LIMITS.md`）

以下差异**不是本车道漏做**，是当前平台或引擎没有对应能力；本车道保持「不猜」：

1. **参数名（`#{name}` / `${name}`）**：`core::xml` 的 `RenderOutput.params` 只有值
   （`Vec<Value>`），不携带占位符来源名；`WorkbenchModel.params` 因此只能按序号显示
   「参数 N」。要显示原版那种参数名，需引擎 core 在渲染结果里带上参数来源。
2. **XML 动态条件勾选 / foreach 次数 / 批量列 / 字段填充**：原版左栏有可勾选条件与
   `__frch_` 批量值框，本版 `RenderOutput` 没有 condition / collection 视图，
   `render_mapper` 只做一次 `{}` 参数的静态渲染。需引擎暴露条件模型后才能做。
3. **含表列名（F145 / LIMITS 第 10 条）**：保持禁用，理由不变（渲染器必须知道每列来自哪张表）。
4. **EXPLAIN / 执行 SQL**：保持禁用；平台 `database` 不执行 SQL，执行面只在 sidecar，
   本版预览页不持数据源（LIMITS 第 3 条）。
5. **切换数据源（LIMITS 第 11 条）**：预览页无数据源 API，不设入口。
6. **AI 上下文（LIMITS 第 12 条）**：本版无脱敏器与宿主 AI 会话接口；入口待脱敏能力到位后再加。
7. **DML 影响面预演**：需要连库执行只读 `COUNT`，与「预览不连库」的边界冲突，未做。
8. **分享（LIMITS 第 13 条）**：仍退化为本地复制。
9. **导出落盘（原版「导出可移植 SQL / 回放脚本」会弹保存框写任意路径）**：平台
   `fs.write:<path>` 只按 manifest 里写死的路径授权，装不下运行期用户选的路径；本版保持
   写到 Output 面板，由用户自行保存。
10. **复制错误 / 参数名 / 条件勾选** 三项属于「待文案 / 待引擎」，不是平台做不到。

## 4 平台参考仓核对（2026-11-27，只读）

- `F:\code\Java\Github\other\openidea`（`3d1769ef`，API 1.18）：
  `docs/en/plugins/manifest.mdx:121` 明确 `dialogs` 是 **API 1.18**；本仓钉的 SDK
  `v0.1.51` 基线是 `^1.15`，所以 LIMITS 第 22 条的「没有插件模态对话框」结论不变，
  预览继续落在工具窗页签。
- 同仓 `wit/database.wit` 只有 `upsert-source` / `remove-source` / `sources` /
  `set-credential` / `connect`，**没有 query / execute**；EXPLAIN 与 DML 影响面只能走
  sidecar 或不做，LIMITS 第 3 / 11 条结论不变。
- `F:\code\Java\Github\other\openidea-plugins`（`f3c4a1c`）里 `jiusi.mybatis-log` /
  `ritsu.sqltoy-navigator` / `qtvbwfn.mybatis-duo` 三个同领域插件均为工具窗/面板形态，
  没有可借鉴的「插件模态预览弹窗」先例。

## 5 与 L45 的关系

L45 的「关键字换行 / 竖向排列」是**显示态**（不改模型、不影响复制）。本车道只把这两个
开关从「更多」菜单移到工具栏直显（原版位置），语义与 `preview-display` op 完全不变；
L45 的 contract 断言（`pvKeywordWrap` / `pvVertical` id、`wrapKeywords` / `verticalItems` /
`shapeForDisplay` 调用）继续成立。