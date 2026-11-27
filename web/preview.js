/*
 * The SQL preview workbench, as one ES module with two mount points:
 *
 * - `web/log-panel.html` imports it and mounts it on the fifth tab
 *   (`#panelPreview`), so the preview lives inside the log panel exactly as the
 *   IntelliJ edition's "SQL 预览" tab lives inside the SQL Log tool window;
 * - `web/preview.html` mounts it as a page of its own, the page the API 1.18
 *   dialog (`sqlv-preview`) loads.
 *
 * The layout follows the IntelliJ edition's `SqlPreviewPanel` / `SqlPreviewDialog`:
 * a meta row (dialect / type / table / source) with the text toolbar on its
 * right, the performance-advice line under it, the parameter column beside the
 * SQL editor, and a status line. The dialog adds the reference's bottom action
 * row (Execute / Copy / Copy-and-close / Cancel). The input half is the tab
 * mount's own affordance and is hidden in the dialog, where the reference shows
 * only the rendered statement.
 *
 * `mountPreview(root, deps)` builds the workbench inside `root`, wires every
 * control, and returns an object with `applyTab(tab)` — the render entry the
 * host's state push drives. All selectors are scoped to `root`, so a future page
 * could mount two instances without the ids colliding.
 *
 * The module is strict (modules always are) and touches no storage and no
 * network: the packaged page runs in an `<iframe sandbox="allow-scripts">` with
 * an opaque origin, where browser storage, cookies and network requests do not
 * exist at all. Its only channel is `openidea.postMessage` /
 * `openidea.onMessage`.
 */

/* The workbench markup. One template, both mount points: the panel page and the
   standalone page show exactly the same DOM. Parsed with `DOMParser` and
   imported node by node — the markup never goes through an HTML-string sink,
   so nothing here can become a script or a markup-injection channel. */
const TEMPLATE = `<!-- Input half (tab mount only): the standalone page actions. -->
<div class="pv-pane pv-top" id="pvPaneTop">
  <div class="pv-toolbar pv-input-toolbar">
    <button id="pvResolve" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M13.5 7.13397C14.1667 7.51888 14.1667 8.48112 13.5 8.86602L4.5 14.0622C3.83333 14.4471 3 13.966 3 13.1962L3 2.80385C3 2.03405 3.83333 1.55292 4.5 1.93782L13.5 7.13397Z" fill="none" stroke="var(--openidea-success)"/></svg><span data-btnlabel></span></button>
    <button id="pvFormat" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><rect x="2" y="12" width="12" height="1" rx="0.5" fill="currentColor"/><rect x="6" y="6" width="8" height="1" rx="0.5" fill="currentColor"/><rect x="6" y="9" width="8" height="1" rx="0.5" fill="currentColor"/><rect x="2" y="3" width="12" height="1" rx="0.5" fill="currentColor"/><path d="M2.8 6.1C2.64849 5.98637 2.44579 5.96809 2.27639 6.05279C2.107 6.13749 2 6.31062 2 6.5V9.5C2 9.68939 2.107 9.86252 2.27639 9.94722C2.44579 10.0319 2.64849 10.0136 2.8 9.9L4.8 8.4C4.9259 8.30558 5 8.15738 5 8C5 7.84262 4.9259 7.69443 4.8 7.6L2.8 6.1Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <button id="pvLoadFile" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill-rule="evenodd" clip-rule="evenodd" d="M10.0775 2.44037C10.1656 2.12093 9.97809 1.79054 9.65866 1.70242C9.33922 1.6143 9.00882 1.80182 8.9207 2.12126L5.76529 13.5596C5.67717 13.8791 5.86469 14.2095 6.18412 14.2976C6.50356 14.3857 6.83396 14.1982 6.92208 13.8788L10.0775 2.44037ZM5.41281 4.66699C5.61982 4.92575 5.57786 5.30332 5.31911 5.51033L1.96049 8.19723L5.31911 10.8841C5.57786 11.0911 5.61982 11.4687 5.41281 11.7275C5.20581 11.9862 4.82823 12.0282 4.56947 11.8212L0.625202 8.66575L0.0395508 8.19723L0.625202 7.7287L4.56947 4.57329C4.82823 4.36628 5.20581 4.40823 5.41281 4.66699ZM10.5872 11.7275C10.3802 11.4687 10.4222 11.0911 10.6809 10.8841L14.0395 8.19725L10.6809 5.51035C10.4222 5.30334 10.3802 4.92577 10.5872 4.66701C10.7942 4.40825 11.1718 4.3663 11.4305 4.57331L15.3748 7.72872L15.9605 8.19725L15.3748 8.66577L11.4305 11.8212C11.1718 12.0282 10.7942 11.9862 10.5872 11.7275Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <button id="pvClear" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill-rule="evenodd" clip-rule="evenodd" d="M7 2H9C9.55228 2 10 2.44772 10 3H6C6 2.44772 6.44772 2 7 2ZM5 3C5 1.89543 5.89543 1 7 1H9C10.1046 1 11 1.89543 11 3H13C13.5523 3 14 3.44772 14 4V5V6H13V13C13 14.1046 12.1046 15 11 15H5C3.89543 15 3 14.1046 3 13V6H2V5V4C2 3.44772 2.44772 3 3 3H5ZM11 4H10H6H5H3V5H4H12H13V4H11ZM4 6H12V13C12 13.5523 11.5523 14 11 14H5C4.44772 14 4 13.5523 4 13V6ZM6.5 7C6.22386 7 6 7.22386 6 7.5V11.5C6 11.7761 6.22386 12 6.5 12C6.77614 12 7 11.7761 7 11.5V7.5C7 7.22386 6.77614 7 6.5 7ZM9 7.5C9 7.22386 9.22386 7 9.5 7C9.77614 7 10 7.22386 10 7.5V11.5C10 11.7761 9.77614 12 9.5 12C9.22386 12 9 11.7761 9 11.5V7.5Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <button id="pvCopy" class="btn pv-btn-text" type="button"><span data-btnlabel></span></button>
    <button id="pvOpenDialog" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill-rule="evenodd" clip-rule="evenodd" d="M7.00002 3H12C12.5523 3 13 3.44772 13 4V12C13 12.5523 12.5523 13 12 13H7.00002V3ZM6.00002 2H7.00002H12C13.1046 2 14 2.89543 14 4V12C14 13.1046 13.1046 14 12 14H7.00002H6.00002H4C2.89543 14 2 13.1046 2 12V4C2 2.89543 2.89543 2 4 2H6.00002ZM6.00002 13H4C3.44772 13 3 12.5523 3 12V4C3 3.44772 3.44772 3 4 3H6.00002V13Z" fill="currentColor"/><rect x="8" y="5" width="4" height="1" rx="0.5" fill="currentColor"/><rect x="8" y="7.5" width="4" height="1" rx="0.5" fill="currentColor"/><rect x="8" y="10" width="4" height="1" rx="0.5" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <!-- Lane L48: shown only in the dialog page, where the host offers the
         dialog object; the tab mount leaves it hidden. -->
    <button id="pvClose" class="btn pv-btn-text" type="button" hidden><span data-btnlabel></span></button>
  </div>
  <div class="hint" id="pvInputHint"></div>
  <textarea id="pvInput" spellcheck="false" autocomplete="off"></textarea>
</div>

<div class="pv-splitter" id="pvSplitter" role="separator" aria-orientation="horizontal"
     tabindex="0"></div>

<div class="pv-pane pv-preview">
  <!-- The reference's meta row and toolbar share one line: meta left, nine
       text actions right. -->
  <div class="pv-toprow">
    <div class="pv-meta">
      <span class="pv-chip" id="pvDialectChip"></span>
      <span class="pv-meta-item" id="pvKindWrap"><span class="pv-meta-label" id="pvKindLabel"></span><span id="pvKindValue"></span></span>
      <!-- The reference names the table; this render has no table metadata, so
           the segment stays hidden rather than guessing one from the SQL. -->
      <span class="pv-meta-item" id="pvTableWrap" hidden><span class="pv-meta-label" id="pvTableLabel"></span><span id="pvTableValue"></span></span>
      <!-- Statement picker (lane L45): only a mapper render offers it. -->
      <span class="pv-statement-wrap" id="pvStatementWrap" hidden>
        <label class="muted" id="pvStatementLabel" for="pvStatement"></label>
        <select id="pvStatement" class="btn"></select>
      </span>
      <span class="pv-badge" id="pvConfidence" hidden></span>
      <span class="pv-source muted" id="pvSource" hidden></span>
    </div>
    <div class="pv-toolbar" id="pvToolbar">
      <!-- Statement type (the reference shows it only when more than one
           consumption form exists); kept hidden when there is nothing to
           switch, never faked. -->
      <button id="pvStmtType" class="btn" type="button" hidden><span data-btnlabel></span></button>
      <!-- Error text (the reference shows it only for a render with an error). -->
      <button id="pvCopyError" class="btn" type="button" hidden><span data-btnlabel></span></button>
      <!-- Compact / multi-line: one toggle whose label names the mode a click
           switches to (the reference's modeButton). -->
      <button id="pvMode" class="btn pv-btn-text" type="button" aria-pressed="false"><span data-btnlabel></span></button>
      <button id="pvFormatPreview" class="btn pv-btn-text" type="button"><span data-btnlabel></span></button>
      <button id="pvKeywordWrap" class="btn pv-btn-text" type="button" aria-pressed="false"><span data-btnlabel></span></button>
      <button id="pvVertical" class="btn pv-btn-text" type="button" aria-pressed="false"><span data-btnlabel></span></button>
      <!-- Explain: the reference runs EXPLAIN through the data source; this
           build has no sidecar execution path here, so the button stays
           disabled and explains itself. -->
      <button id="pvExplain" class="btn" type="button" disabled><span data-btnlabel></span></button>
      <button id="pvRefresh" class="btn" type="button"><span data-btnlabel></span></button>
      <span class="pv-more-wrap">
        <button id="pvMore" class="btn" type="button" aria-haspopup="true"
                aria-expanded="false"><span data-btnlabel></span></button>
        <div class="pv-more-menu" id="pvMoreMenu" role="menu" hidden>
          <!-- Ten low-frequency actions in the reference's order. Entries this
               build cannot honour stay disabled with the reason in their
               tooltip/accessible name; none of them pretends to succeed. -->
          <button id="pvRollback" class="pv-more-item" type="button" role="menuitem"
                  disabled></button>
          <button id="pvSwitchDataSource" class="pv-more-item" type="button" role="menuitem"
                  disabled></button>
          <button id="pvImportParams" class="pv-more-item" type="button" role="menuitem"></button>
          <button id="pvDiff" class="pv-more-item" type="button" role="menuitem"></button>
          <button id="pvShare" class="pv-more-item" type="button" role="menuitem"></button>
          <button id="pvExport" class="pv-more-item" type="button" role="menuitem"></button>
          <button id="pvReplayExport" class="pv-more-item" type="button" role="menuitem"
                  disabled></button>
          <button id="pvAiContext" class="pv-more-item" type="button" role="menuitem"
                  disabled></button>
          <button id="pvAiContextIds" class="pv-more-item" type="button" role="menuitem"
                  disabled></button>
          <button id="pvDmlImpact" class="pv-more-item" type="button" role="menuitem"
                  disabled></button>
          <button id="pvQualify" class="pv-more-item" type="button" role="menuitem"
                disabled hidden></button>
        </div>
      </span>
    </div>
  </div>
  <!-- Performance suggestions: one 11 px line under the meta row. -->
  <div class="pv-perf" id="pvPerfRow" hidden><span id="pvAdvice"></span></div>
  <!-- Degradation banner: the reference's honesty strip. -->
  <div class="pv-banner" id="pvBanner" role="status" hidden>
    <span class="pv-banner-text" id="pvBannerText"></span>
    <button class="pv-banner-close" id="pvBannerClose" type="button"></button>
  </div>
  <div class="pv-cols">
    <div class="pv-params">
      <div class="pv-scroll">
        <!-- Preset fold (the reference's HideableTitledPanel): collapsed by
             default. There is no preset store in this build, so the controls
             are disabled with the reason visible on hover. -->
        <details class="pv-preset" id="pvPresetBlock">
          <summary id="pvPresetSummary"></summary>
          <div class="pv-preset-row">
            <select id="pvPresetSelect" class="pv-select"><option id="pvPresetDefault"></option></select>
            <button id="pvPresetManage" class="pv-mini" type="button" disabled><span data-btnlabel></span></button>
          </div>
        </details>
        <!-- Dynamic conditions (C07): one checkbox per "if" / "when" node. -->
        <div class="pv-section" id="pvConditions" hidden>
          <div class="pv-section-head">
            <span class="pv-section-title" id="pvConditionsTitle"></span>
            <span class="pv-section-actions">
              <button id="pvConditionsAll" class="pv-mini" type="button"><span data-btnlabel></span></button>
              <button id="pvConditionsClear" class="pv-mini" type="button"><span data-btnlabel></span></button>
            </span>
          </div>
          <div id="pvConditionsList"></div>
          <div class="hint" id="pvConditionsHint"></div>
        </div>
        <div class="pv-section" id="pvParamsSection">
          <!-- "参数" header appears only when a condition section precedes it,
               exactly as the reference's sectionHeaderRow does. -->
          <div class="pv-section-head" id="pvParamsHead" hidden>
            <span class="pv-section-title" id="pvParamsTitle"></span>
          </div>
          <div id="pvParamsList"></div>
        </div>
        <div class="pv-params-actions">
          <button id="pvRebindBtn" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M13.5 7.13397C14.1667 7.51888 14.1667 8.48112 13.5 8.86602L4.5 14.0622C3.83333 14.4471 3 13.966 3 13.1962L3 2.80385C3 2.03405 3.83333 1.55292 4.5 1.93782L13.5 7.13397Z" fill="none" stroke="var(--openidea-success)"/></svg><span data-btnlabel></span></button>
          <button id="pvImportBtn" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M5.5 6C5.22386 6 5 6.22386 5 6.5C5 6.77614 5.22386 7 5.5 7H10.5C10.7761 7 11 6.77614 11 6.5C11 6.22386 10.7761 6 10.5 6H5.5Z" fill="currentColor"/><path d="M5 8.5C5 8.22386 5.22386 8 5.5 8H10.5C10.7761 8 11 8.22386 11 8.5C11 8.77614 10.7761 9 10.5 9H5.5C5.22386 9 5 8.77614 5 8.5Z" fill="currentColor"/><path d="M5.5 10C5.22386 10 5 10.2239 5 10.5C5 10.7761 5.22386 11 5.5 11H10.5C10.7761 11 11 10.7761 11 10.5C11 10.2239 10.7761 10 10.5 10H5.5Z" fill="currentColor"/><path fill-rule="evenodd" clip-rule="evenodd" d="M12 2H10.9146C10.7087 1.4174 10.1531 1 9.5 1H6.5C5.84689 1 5.29127 1.4174 5.08535 2H4C2.89543 2 2 2.89543 2 4V13C2 14.1046 2.89543 15 4 15H12C13.1046 15 14 14.1046 14 13V4C14 2.89543 13.1046 2 12 2ZM4 3H5.08535C5.29127 3.5826 5.84689 4 6.5 4H9.5C10.1531 4 10.7087 3.5826 10.9146 3H12C12.5523 3 13 3.44772 13 4V13C13 13.5523 12.5523 14 12 14H4C3.44772 14 3 13.5523 3 13V4C3 3.44772 3.44772 3 4 3ZM6.5 2C6.22386 2 6 2.22386 6 2.5C6 2.77614 6.22386 3 6.5 3H9.5C9.77614 3 10 2.77614 10 2.5C10 2.22386 9.77614 2 9.5 2H6.5Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
        </div>
        <div class="hint" id="pvParamsHint"></div>
      </div>
    </div>
    <!-- Draggable split between the parameter column and the SQL editor, the
         reference's horizontal OnePixelSplitter (default 0.3). -->
    <div class="pv-hsplitter" id="pvHSplitter" role="separator"
         aria-orientation="vertical" tabindex="0"></div>
    <div class="pv-sqlside">
      <!-- Conditional tabs: SQL always; 原文 for a render whose source differs
           (mapper XML / HQL); 列信息 when the render carried columns. With only
           SQL the strip hides, as in the reference. -->
      <div class="pv-tabs" id="pvTabs" role="tablist" hidden>
        <button class="pv-tab" type="button" role="tab" data-pvtab="sql" aria-selected="true"></button>
        <button class="pv-tab" type="button" role="tab" data-pvtab="raw" aria-selected="false"></button>
        <button class="pv-tab" type="button" role="tab" data-pvtab="columns" aria-selected="false"></button>
      </div>
      <div class="pv-code" id="pvCode">
        <div class="pv-gutter" id="pvGutter" aria-hidden="true" hidden></div>
        <pre class="pv-tabbody" id="pvTabBody" role="tabpanel"></pre>
      </div>
    </div>
  </div>
  <!-- Status line, plus the dialog's fixed action row (hidden in the tab). -->
  <div class="pv-footer" id="pvFooter" hidden>
    <span id="pvStatus" class="muted"></span>
    <span class="pv-dialog-actions" id="pvDialogActions" hidden>
      <button id="pvExecute" class="btn" type="button" disabled><span data-btnlabel></span></button>
      <button id="pvCopySql" class="btn" type="button"><span data-btnlabel></span></button>
      <button id="pvCopyClose" class="btn pv-primary" type="button"><span data-btnlabel></span></button>
      <button id="pvCancel" class="btn" type="button"><span data-btnlabel></span></button>
    </span>
  </div>
</div>`;

/* ---- Text this module owns ------------------------------------------
 *
 * The host localises every label it knows (`tabs.preview.labels`); the labels
 * this page introduces — the reference's nine toolbar entries, the ten "more"
 * actions, the dialog row and the parameter-panel chrome — live here in both
 * languages, so the page reads correctly before the host tables catch up and
 * never hard-codes one language. `pickLang` follows the host: a localised label
 * containing CJK means the IDE is running in Chinese.
 */
const PV_TEXT = {
  zh: {
    dialect: '方言:',
    kind: '类型:',
    table: '表:',
    kindSql: 'SQL',
    kindXml: 'Mapper XML',
    stmtType: '语句: {0}',
    copyError: '复制错误信息',
    copyErrorEmpty: '本次渲染没有可复制的错误信息',
    copyErrorCopied: '错误信息已复制',
    explain: '执行计划',
    refresh: '刷新',
    compact: '压缩',
    multiline: '多行',
    moreRollback: '回滚 SQL',
    moreSwitchDataSource: '切换数据源',
    moreImportParams: '导入参数',
    moreDiff: '差异对比',
    moreShare: '分享',
    morePortableExport: '导出可移植 SQL',
    moreReplayExport: '导出回放脚本',
    moreAiContext: '复制为 AI 上下文',
    moreAiContextIds: '复制为 AI 上下文（含表列名）',
    moreDmlImpact: '预估影响行数',
    rollbackNote: '本构建不持有表/主键元数据，无法生成回滚计划',
    switchDataSourceNote: '预览页没有数据源绑定后端，无法切换数据源',
    portableExportNote: '按当前方言导出格式化 SQL 与来源头注释；本构建不做可移植形态标注',
    replayExportNote: '本构建没有执行通道，无法导出回放脚本',
    aiContextNote: '本构建没有脱敏与 AI 上下文通道',
    dmlImpactNote: '预估影响行数需要执行 COUNT 查询；本插件从不执行 SQL',
    explainNote: '本插件从不执行用户 SQL，执行计划不可用',
    executeNote: '本插件从不执行用户 SQL；请复制到数据库控制台执行',
    preset: '预设:',
    presetDefault: '默认预设',
    presetManage: '管理预设…',
    presetManageNote: '本构建没有预设存储后端',
    conditions: '动态条件',
    paramsSection: '参数',
    selectAll: '全选',
    selectNone: '全不选',
    clear: '清除',
    clearNote: '清空全部参数值（不改变分支勾选）',
    paramNumber: '数字，如 42',
    paramTemporal: '日期，如 2026-01-01 12:00:00',
    paramCollection: '[1, 2, 3]',
    paramObject: '{"id": 1}',
    paramsEmpty: '无未解析参数',
    copySql: '复制 SQL',
    copySqlAndClose: '复制 SQL 并关闭',
    cancel: '取消',
    copyCloseNote: '复制当前 SQL 并关闭预览',
  },
  en: {
    dialect: 'Dialect:',
    kind: 'Type:',
    table: 'Table:',
    kindSql: 'SQL',
    kindXml: 'Mapper XML',
    stmtType: 'Statement: {0}',
    copyError: 'Copy error',
    copyErrorEmpty: 'This render carries no error text',
    copyErrorCopied: 'Error text copied',
    explain: 'Explain',
    refresh: 'Refresh',
    compact: 'Compact',
    multiline: 'Multi-line',
    moreRollback: 'Rollback SQL',
    moreSwitchDataSource: 'Switch data source',
    moreImportParams: 'Import parameters',
    moreDiff: 'Compare',
    moreShare: 'Share',
    morePortableExport: 'Export portable SQL',
    moreReplayExport: 'Export replay script',
    moreAiContext: 'Copy as AI context',
    moreAiContextIds: 'Copy as AI context (with table and column names)',
    moreDmlImpact: 'Estimate affected rows',
    rollbackNote: 'This build holds no table/primary-key metadata, so no rollback plan can be built',
    switchDataSourceNote: 'The preview has no data-source backend, so it cannot switch sources',
    portableExportNote: 'Exports the formatted SQL with a source header for the current dialect; this build does not annotate portable shapes',
    replayExportNote: 'This build has no execution channel, so no replay script can be produced',
    aiContextNote: 'This build has no redaction or AI-context channel',
    dmlImpactNote: 'Estimating rows needs a COUNT query; this plugin never executes SQL',
    explainNote: 'This plugin never executes user SQL, so EXPLAIN is unavailable',
    executeNote: 'This plugin never executes user SQL; copy the statement into a database console',
    preset: 'Preset:',
    presetDefault: 'Default preset',
    presetManage: 'Manage presets…',
    presetManageNote: 'This build has no preset store',
    conditions: 'Dynamic conditions',
    paramsSection: 'Parameters',
    selectAll: 'Select all',
    selectNone: 'Select none',
    clear: 'Clear',
    clearNote: 'Clear every parameter value (branch switches unchanged)',
    paramNumber: 'Number, e.g. 42',
    paramTemporal: 'Date, e.g. 2026-01-01 12:00:00',
    paramCollection: '[1, 2, 3]',
    paramObject: '{"id": 1}',
    paramsEmpty: 'No unresolved parameter',
    copySql: 'Copy SQL',
    copySqlAndClose: 'Copy SQL and close',
    cancel: 'Cancel',
    copyCloseNote: 'Copy the current SQL and close the preview',
  },
};

const PV_PLACEHOLDER_KEYS = {
  number: 'paramNumber',
  temporal: 'paramTemporal',
  collection: 'paramCollection',
  object: 'paramObject',
};

/** Text buttons carry an inline icon; only the label span is replaced so the
 * icon survives every state push (same rule as the panel page's `setText`). */
function setText(node, text) {
  if (!node) { return; }
  const value = text == null ? '' : text;
  const label = node.querySelector('[data-btnlabel]');
  if (label) {
    // Icon-only button: the text becomes the tooltip / accessible name.
    label.textContent = value;
    node.title = value;
    node.setAttribute('aria-label', value);
  } else {
    node.textContent = value;
  }
}

/** The page's own channel to the host, for a mount point that passes none. */
function defaultPost(message) {
  if (window.openidea && window.openidea.postMessage) {
    window.openidea.postMessage(JSON.stringify(message));
  }
}

const PV_LABEL_KEYS = {
  pvResolve: 'resolve', pvFormat: 'format', pvLoadFile: 'loadFile',
  pvClear: 'clear', pvOpenDialog: 'openDialog', pvClose: 'close',
  pvCopy: 'copySql',
  pvFormatPreview: 'format', pvExplain: 'explain',
  pvRefresh: 'refresh', pvMore: 'more',
};
const PV_TAB_KEYS = { sql: 'tabSql', raw: 'tabRaw', columns: 'tabColumns' };
const PV_TAB_ORDER = ['sql', 'raw', 'columns'];
const PV_MODES = ['normal', 'formatted', 'keyword_wrap', 'vertical'];

/* ---- Display-only SQL shaping (lane L45) --------------------------------
 *
 * Both helpers take the SQL the preview shows and return the text to paint.
 * They never run on the model: the executable SQL the host holds, the Copy /
 * Export paths and the formatted tab are untouched — these switches are
 * presentation, nothing else. Called on every render, so they stay linear and
 * allocate little.
 */

/* The top-level keywords the wrap breaks before. Longest-first so `GROUP BY`
   and `ORDER BY` win over a bare `GROUP` / `ORDER`, and `LEFT JOIN` style
   two-word joins are covered by the trailing JOIN entry. */
const PV_WRAP_KEYWORDS = [
  'LEFT OUTER JOIN', 'RIGHT OUTER JOIN', 'FULL OUTER JOIN',
  'LEFT JOIN', 'RIGHT JOIN', 'FULL JOIN', 'INNER JOIN', 'CROSS JOIN',
  'UNION ALL', 'GROUP BY', 'ORDER BY',
  'SELECT', 'FROM', 'WHERE', 'HAVING', 'LIMIT', 'OFFSET', 'JOIN', 'ON',
  'AND', 'OR', 'UNION', 'INSERT INTO', 'INSERT', 'UPDATE', 'DELETE FROM',
  'DELETE', 'VALUES', 'SET', 'WITH',
];

/* Characters that end a string / quoted identifier / comment; the scanner
   below never breaks inside one. */
function isQuote(ch) { return ch === '\'' || ch === '"' || ch === '`'; }

function isWordStart(ch) { return /[A-Za-z_]/.test(ch); }
function isWordChar(ch) { return /[A-Za-z0-9_$]/.test(ch); }

/**
 * Walks `text` and hands each plain (unquoted, uncommented) character to
 * `visit`; quoted spans, line comments and block comments arrive as one opaque
 * chunk so no rule can fire inside them. Shared by the display shapers.
 */
function walkSql(text, visit) {
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (isQuote(ch)) {
      let j = i + 1;
      while (j < text.length) {
        if (text[j] === ch) {
          if (text[j + 1] === ch) { j += 2; continue; } // doubled-quote escape
          break;
        }
        j += 1;
      }
      const stop = Math.min(j + 1, text.length);
      visit(text.slice(i, stop), i, stop, 'opaque');
      i = stop;
      continue;
    }
    if (ch === '-' && text[i + 1] === '-') {
      const end = text.indexOf('\n', i);
      const stop = end < 0 ? text.length : end + 1;
      visit(text.slice(i, stop), i, stop, 'opaque');
      i = stop;
      continue;
    }
    if (ch === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2);
      const stop = end < 0 ? text.length : end + 2;
      visit(text.slice(i, stop), i, stop, 'opaque');
      i = stop;
      continue;
    }
    visit(ch, i, i + 1, 'plain');
    i += 1;
  }
}

/**
 * Breaks the SQL before each top-level keyword and returns the display text.
 *
 * Only whitespace is inserted at those points: no token is dropped, renamed or
 * reordered, and text inside string literals, quoted identifiers and comments
 * is left alone. Pure: same input, same output, no state.
 */
export function wrapKeywords(sql) {
  const text = String(sql == null ? '' : sql);
  if (!text) { return text; }
  const upper = text.toUpperCase();
  const breaks = [];
  let seenToken = false;
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (isQuote(ch) || (ch === '-' && text[i + 1] === '-')
        || (ch === '/' && text[i + 1] === '*')) {
      // One opaque chunk (string, identifier, comment): never break in it.
      let stop;
      if (isQuote(ch)) {
        let j = i + 1;
        while (j < text.length) {
          if (text[j] === ch) {
            if (text[j + 1] === ch) { j += 2; continue; }
            break;
          }
          j += 1;
        }
        stop = Math.min(j + 1, text.length);
      } else if (ch === '-') {
        const end = text.indexOf('\n', i);
        stop = end < 0 ? text.length : end + 1;
      } else {
        const end = text.indexOf('*/', i + 2);
        stop = end < 0 ? text.length : end + 2;
      }
      seenToken = true;
      i = stop;
      continue;
    }
    if (isWordStart(ch)) {
      let matched = null;
      // Longest-first: the list is ordered that way, so the first hit wins.
      for (let k = 0; k < PV_WRAP_KEYWORDS.length; k++) {
        const keyword = PV_WRAP_KEYWORDS[k];
        if (!upper.startsWith(keyword, i)) { continue; }
        const after = i + keyword.length;
        // `SELECTED` must not match `SELECT`: a word character right after the
        // keyword means this is a longer identifier.
        if (after < text.length && isWordChar(text[after])) { continue; }
        matched = after;
        break;
      }
      if (matched !== null) {
        if (seenToken) { breaks.push(i); }
        seenToken = true;
        i = matched;
        continue;
      }
      // An ordinary word: skip it whole, so a keyword inside it never matches.
      let j = i;
      while (j < text.length && isWordChar(text[j])) { j += 1; }
      seenToken = true;
      i = j;
      continue;
    }
    if (!/\s/.test(ch)) { seenToken = true; }
    i += 1;
  }
  if (breaks.length === 0) { return text; }
  let out = '';
  let cursor = 0;
  for (let index = 0; index < breaks.length; index++) {
    const at = breaks[index];
    // Collapse the whitespace already before the keyword into one newline, so
    // a wrapped line does not keep the original indentation.
    let start = at;
    while (start > cursor && /\s/.test(text[start - 1])) { start -= 1; }
    out += text.slice(cursor, start);
    if (out.length > 0 && out[out.length - 1] !== '\n') { out += '\n'; }
    cursor = at;
  }
  out += text.slice(cursor);
  return out;
}

/**
 * Puts each top-level comma-separated item on its own line.
 *
 * Commas inside parentheses (function arguments, `IN (…)`, subqueries) and
 * inside strings / comments stay put: only the commas at depth 0 break the
 * line, which is the "one item per line" reading of a SELECT list. Pure.
 */
export function verticalItems(sql) {
  const text = String(sql == null ? '' : sql);
  if (!text) { return text; }
  let out = '';
  let depth = 0;
  let pendingSkip = 0;
  walkSql(text, function (chunk, start, stop, kind) {
    if (kind === 'opaque') { out += chunk; return; }
    const ch = chunk;
    if (ch === '(') { depth += 1; out += ch; return; }
    if (ch === ')') { depth = Math.max(0, depth - 1); out += ch; return; }
    if (ch === ',' && depth === 0) {
      out += ',\n';
      // Drop the indentation that used to follow the comma.
      pendingSkip = 1;
      return;
    }
    if (pendingSkip && (ch === ' ' || ch === '\t')) { return; }
    pendingSkip = 0;
    out += ch;
  });
  return out;
}

/**
 * The text the SQL tab paints: the chosen display shaping plus the compact
 * switch. `options` is `{ mode, compact }`; `normal` with compact off returns
 * the input unchanged. Pure.
 */
export function shapeForDisplay(sql, options) {
  const opts = options || {};
  let text = String(sql == null ? '' : sql);
  if (opts.mode === 'keyword_wrap') { text = wrapKeywords(text); }
  if (opts.mode === 'vertical') { text = verticalItems(text); }
  if (opts.compact) { text = compactSql(text); }
  return text;
}

/**
 * Collapses runs of whitespace to one space, except inside quoted spans and
 * comments and never across a line-comment terminator (dropping that newline
 * would swallow the statement that follows into the comment). Pure.
 */
export function compactSql(sql) {
  const text = String(sql == null ? '' : sql);
  if (!text) { return text; }
  let out = '';
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (isQuote(ch)) {
      let j = i + 1;
      while (j < text.length) {
        if (text[j] === ch) {
          if (text[j + 1] === ch) { j += 2; continue; }
          break;
        }
        j += 1;
      }
      const stop = Math.min(j + 1, text.length);
      out += text.slice(i, stop);
      i = stop;
      continue;
    }
    if (ch === '-' && text[i + 1] === '-') {
      const end = text.indexOf('\n', i);
      const stop = end < 0 ? text.length : end + 1;
      // Keep the terminating newline; the next line is not a comment.
      out += text.slice(i, stop);
      i = stop;
      continue;
    }
    if (ch === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2);
      const stop = end < 0 ? text.length : end + 2;
      out += text.slice(i, stop);
      i = stop;
      continue;
    }
    if (/\s/.test(ch)) {
      let j = i + 1;
      while (j < text.length && /\s/.test(text[j])
             && !(text[j] === '-' && text[j + 1] === '-')) { j += 1; }
      out += ' ';
      i = j;
      continue;
    }
    out += ch;
    i += 1;
  }
  return out.trim();
}

/**
 * Builds the workbench inside `root`.
 *
 * `deps.post(message)` sends one op to the host (the panel page passes its own,
 * which owns the page's single `openidea` handle); `deps.labels` seeds the copy
 * before the first state push (a standalone page then is not blank while it
 * waits).
 */
export function mountPreview(root, deps) {
  if (!root) { throw new Error('mountPreview: a root element is required'); }
  deps = deps || {};
  const doc = root.ownerDocument || document;
  const post = typeof deps.post === 'function' ? deps.post : defaultPost;
  // Lane L48: only a plugin dialog's page gets `openidea.dialog` (API 1.18).
  // Everything that closes the dialog is behind this one check, so the same
  // module mounted in the log panel's fifth tab binds nothing and throws
  // nothing — there is no element or listener that depends on a missing host.
  const dialogBridge =
    (typeof window !== 'undefined' && window.openidea && window.openidea.dialog)
      ? window.openidea.dialog
      : null;

  // The tab is a flex child of the panel page, exactly as before; the class
  // carries the workbench's own CSS (web/preview.css) instead of the id.
  if (!root.classList.contains('pv-root')) { root.classList.add('pv-root'); }
  // The dialog hides the input half and shows the fixed bottom action row;
  // the tab does the opposite. One class drives both, so no element is created
  // conditionally.
  root.classList.toggle('pv-dialog-mode', !!dialogBridge);
  const parsed = new DOMParser().parseFromString(TEMPLATE, 'text/html');
  // The template has three top-level children (input half, splitter, preview
  // half): import all of them, in order, or the splitter and the whole result
  // pane would be dropped.
  const staged = doc.createDocumentFragment();
  while (parsed.body.firstChild) {
    staged.appendChild(doc.importNode(parsed.body.firstChild, true));
    parsed.body.removeChild(parsed.body.firstChild);
  }
  root.appendChild(staged);

  const preview = {
    model: {}, labels: {}, dialect: {}, tab: 'sql', mode: 'normal',
    compact: false,
    // The display state in the host's shape: the mode (`normal` / `formatted`
    // / `keyword_wrap` / `vertical`) and the compact switch. It rides every
    // push so a remount restores what the reader chose.
    display: { mode: 'normal', compact: false },
  };
  if (deps.labels) { preview.labels = deps.labels; }
  let pvLang = 'zh';
  // A push that changes none of the painted text must not re-render (the
  // reader keeps their scroll position).
  let pvSig = null;
  // Params (lane L27): signature of the parameter list (so an unrelated push
  // does not clobber what the user typed) and whether a clipboard read was
  // refused (the capability gates navigator.clipboard in a web view).
  let pvParamsSig = null;
  // C07: the condition list's id signature, so a push with the same nodes only
  // syncs the checkboxes instead of rebuilding the list under the cursor.
  let pvConditionsSig = null;
  let clipboardMissing = false;
  // Degradation banner (L35b): the reasons the reader dismissed. A later render
  // with different reasons shows the strip again.
  let pvBannerDismissed = null;

  /** One node of this workbench, by id. */
  function node(id) { return root.querySelector('#' + id); }

  /** The module-owned text, in the host's language. */
  function L(key) {
    const table = PV_TEXT[pvLang] || PV_TEXT.zh;
    return table[key] != null ? table[key] : (PV_TEXT.zh[key] || key);
  }

  /** A host label when it exists, else this module's own text. */
  function labelOf(key, fallbackKey) {
    const labels = preview.labels || {};
    if (labels[key]) { return labels[key]; }
    return L(fallbackKey || key);
  }

  function tabs() { return preview.model.tabs || {}; }
  function tabButtons() { return root.querySelectorAll('.pv-tab'); }
  function paramInputs() { return root.querySelectorAll('.pv-param-input'); }

  function signature() {
    const m = preview.model || {};
    const t = tabs();
    const l = preview.labels || {};
    return [
      m.sql, m.reason, m.confidence, (m.params || []).join(','),
      m.advice, m.source, m.kind,
      t.formatted, t.raw, t.columns, preview.tab, preview.mode,
      preview.compact ? 'c' : '',
      l.empty, l.columnsEmpty, l.reasonTitle, pvLang,
    ].join('\u0001');
  }

  function inputText() {
    const editor = node('pvInput');
    return editor ? editor.value : '';
  }

  /** The input hint is guidance for an empty box: once there is text it goes
   *  away (the reference hides its `inputHintLabel` on the same condition). */
  function applyInputHint() {
    const hint = node('pvInputHint');
    const editor = node('pvInput');
    if (!hint || !editor) { return; }
    hint.hidden = editor.value.trim() !== '';
  }

  /** The footer is the status line plus the dialog's action row; with an empty
   *  status and no dialog row it takes no height at all. */
  function updateFooter() {
    const status = node('pvStatus');
    const actions = node('pvDialogActions');
    const footer = node('pvFooter');
    if (actions) { actions.hidden = !dialogBridge; }
    if (footer) {
      const hasStatus = !!(status && status.textContent.trim());
      footer.hidden = !hasStatus && !dialogBridge;
    }
  }

  /** Paints one SQL text as coloured runs. The token texts are the exact input,
   *  so selecting or copying the rendered page yields the SQL. */
  function paintSql(body, text) {
    body.textContent = text;
  }

  /** Line numbers, one per painted line. The code pane never wraps, so the
   *  counts always agree. */
  function paintGutter(text) {
    const gutter = node('pvGutter');
    if (!gutter) { return; }
    const lines = text ? text.split('\n') : [];
    let out = '';
    for (let i = 0; i < lines.length; i++) {
      out += (i > 0 ? '\n' : '') + String(i + 1);
    }
    gutter.textContent = out;
  }

  function renderTab() {
    const body = node('pvTabBody');
    if (!body) { return; }
    const m = preview.model || {};
    const labels = preview.labels || {};
    let text = '';
    let placeholder = false;
    if (preview.tab === 'sql') {
      if (!m.sql && m.reason) {
        text = (labels.reasonTitle || '') + '\n' + m.reason;
      } else if (!m.sql) {
        text = labels.empty || '';
        placeholder = true;
      } else {
        const source = tabs();
        text = preview.display.mode === 'formatted'
          ? (source.formatted || source.sql || '')
          : (source.sql || '');
        text = shapeForDisplay(text, preview.display);
        if (m.reason) {
          // A partial render keeps its SQL and explains itself below it the
          // way `render-xml` prints reasons as comments.
          text += '\n\n-- ' + (labels.reasonTitle || '');
          const reasons = m.reason.split('\n');
          for (let line = 0; line < reasons.length; line++) {
            text += '\n-- ' + reasons[line];
          }
        }
      }
    } else if (preview.tab === 'raw') {
      text = tabs().raw || '';
      placeholder = !text;
    } else {
      text = tabs().columns || '';
      if (!text) {
        text = labels.columnsEmpty || '';
        placeholder = true;
      }
    }
    body.className = placeholder ? 'pv-tabbody pv-placeholder' : 'pv-tabbody';
    paintSql(body, text);
    paintGutter(placeholder ? '' : text);
    body.scrollTop = 0;
    pvSig = signature();
  }

  function selectTab(key) {
    if (PV_TAB_ORDER.indexOf(key) < 0) { return; }
    const buttons = tabButtons();
    let available = false;
    for (let i = 0; i < buttons.length; i++) {
      const tab = buttons[i].getAttribute('data-pvtab');
      if (tab === key && !buttons[i].hidden) { available = true; }
    }
    preview.tab = available ? key : 'sql';
    for (let i = 0; i < buttons.length; i++) {
      const on = buttons[i].getAttribute('data-pvtab') === preview.tab;
      buttons[i].setAttribute('aria-selected', on ? 'true' : 'false');
    }
    renderTab();
  }

  // Compact / multi-line (the reference's `modeButton`): the button names the
  // mode a click switches to.
  function updateCompactControl() {
    const button = node('pvMode');
    if (!button) { return; }
    setText(button, preview.compact ? L('multiline') : L('compact'));
    button.setAttribute('aria-pressed', preview.compact ? 'true' : 'false');
    button.classList.toggle('on', preview.compact);
  }

  function toggleCompact() {
    preview.compact = !preview.compact;
    preview.display.compact = preview.compact;
    updateCompactControl();
    renderTab();
    postDisplay();
  }

  // The four display modes (normal / formatted / keyword_wrap / vertical).
  function modeOf(state) {
    return PV_MODES.indexOf(state) >= 0 ? state : 'normal';
  }

  function updateModeControls() {
    const pairs = [
      ['pvFormatPreview', 'formatted'],
      ['pvKeywordWrap', 'keyword_wrap'],
      ['pvVertical', 'vertical'],
    ];
    for (let i = 0; i < pairs.length; i++) {
      const button = node(pairs[i][0]);
      if (!button) { continue; }
      const on = preview.mode === pairs[i][1];
      button.setAttribute('aria-pressed', on ? 'true' : 'false');
      button.classList.toggle('on', on);
    }
  }

  function setMode(mode) {
    preview.mode = modeOf(mode);
    preview.display.mode = preview.mode;
    updateModeControls();
    renderTab();
    postDisplay();
  }

  function postDisplay() {
    post({
      op: 'preview-display',
      mode: preview.mode,
      compact: preview.compact,
      // The pre-mode host keys, kept so a host that only knows the two
      // switches still remembers what the reader chose.
      keywordWrap: preview.mode === 'keyword_wrap',
      vertical: preview.mode === 'vertical',
    });
  }

  // Statement picker (lane L45) -----------------------------------------
  function applyStatementPicker() {
    const wrap = node('pvStatementWrap');
    const select = node('pvStatement');
    if (!wrap || !select) { return; }
    const labels = preview.labels || {};
    const m = preview.model || {};
    const ids = Array.isArray(m.statements) ? m.statements : [];
    setText(node('pvStatementLabel'), labels.statementLabel || '');
    if (ids.length === 0) {
      wrap.hidden = true;
      return;
    }
    wrap.hidden = false;
    // Rebuild only when the option set changed: a push that carries the same
    // list must not reset the reader's focus or emit a change event.
    const current = [];
    for (let i = 0; i < select.options.length; i++) { current.push(select.options[i].value); }
    if (current.join('\u0001') !== ids.join('\u0001')) {
      while (select.firstChild) { select.removeChild(select.firstChild); }
      for (let i = 0; i < ids.length; i++) {
        const option = doc.createElement('option');
        option.value = ids[i];
        option.textContent = ids[i];
        select.appendChild(option);
      }
    }
    const chosen = ids.indexOf(m.statement) >= 0 ? m.statement : ids[0];
    if (select.value !== chosen) { select.value = chosen; }
    select.disabled = ids.length < 2;
    select.title = ids.length < 2
      ? (labels.statementNone || '')
      : (labels.statementNote || '');
  }

  function applyLabels() {
    const labels = preview.labels || {};
    pvLang = pickLang(labels);
    const ids = Object.keys(PV_LABEL_KEYS);
    for (let index = 0; index < ids.length; index++) {
      setText(node(ids[index]), labels[PV_LABEL_KEYS[ids[index]]]);
    }
    const openDialog = node('pvOpenDialog');
    if (openDialog) {
      // Lane L48: the tab mount can pop the preview out into the dialog; inside
      // the dialog the entry means nothing, so it hides rather than opening
      // itself again.
      openDialog.hidden = !!dialogBridge;
      openDialog.title = labels.openDialogNote || '';
    }
    const closeDialog = node('pvClose');
    if (closeDialog) {
      closeDialog.hidden = true;
      closeDialog.title = labels.dialogNote || '';
    }
    // The nine button labels (the host knows none of them; `PV_TEXT` does).
    setText(node('pvMode'), preview.compact ? L('multiline') : L('compact'));
    setText(node('pvStmtType'), L('stmtType').replace('{0}', ''));
    setText(node('pvCopyError'), L('copyError'));
    setText(node('pvExplain'), labels.explain || L('explain'));
    setText(node('pvRefresh'), labels.refresh || L('refresh'));
    const explainButton = node('pvExplain');
    if (explainButton) {
      explainButton.disabled = true;
      explainButton.title = labels.explainNote || L('explainNote');
    }
    const moreButton = node('pvMore');
    if (moreButton) { moreButton.title = labels.moreNote || ''; }
    // Meta labels: this module's own, in the host's language.
    setText(node('pvKindLabel'), L('kind'));
    setText(node('pvTableLabel'), L('table'));
    // Preset fold.
    setText(node('pvPresetSummary'), L('preset'));
    setText(node('pvPresetDefault'), L('presetDefault'));
    const presetSelect = node('pvPresetSelect');
    const presetManage = node('pvPresetManage');
    if (presetSelect) {
      presetSelect.disabled = true;
      presetSelect.title = L('presetManageNote');
    }
    if (presetManage) {
      presetManage.disabled = true;
      presetManage.title = L('presetManageNote');
      presetManage.setAttribute('aria-label', L('presetManageNote'));
    }
    // Condition section.
    setText(node('pvConditionsTitle'), labels.conditions || L('conditions'));
    setText(node('pvConditionsHint'), labels.conditionsHint || '');
    setText(node('pvConditionsClear'), L('clear'));
    const clearButton = node('pvConditionsClear');
    if (clearButton) {
      clearButton.title = L('clearNote');
      clearButton.setAttribute('aria-label', L('clearNote'));
    }
    updateConditionsToggle();
    // Parameter section.
    setText(node('pvParamsTitle'), labels.params || L('paramsSection'));
    setText(node('pvParamsHint'), labels.paramsHint || '');
    // The "more" fold's ten entries.
    const moreItems = [
      ['pvRollback', labels.rollback || L('moreRollback'), labels.rollbackNote || L('rollbackNote')],
      ['pvSwitchDataSource', L('moreSwitchDataSource'), L('switchDataSourceNote')],
      ['pvImportParams', labels.importClipboard || L('moreImportParams'), ''],
      ['pvDiff', labels.diff || L('moreDiff'), labels.diffNote || ''],
      ['pvShare', labels.share || L('moreShare'), labels.shareNote || ''],
      ['pvExport', L('morePortableExport'), L('portableExportNote')],
      ['pvReplayExport', L('moreReplayExport'), L('replayExportNote')],
      ['pvAiContext', L('moreAiContext'), L('aiContextNote')],
      ['pvAiContextIds', L('moreAiContextIds'), L('aiContextNote')],
      ['pvDmlImpact', L('moreDmlImpact'), L('dmlImpactNote')],
    ];
    for (let i = 0; i < moreItems.length; i++) {
      const button = node(moreItems[i][0]);
      if (!button) { continue; }
      button.textContent = moreItems[i][1];
      button.title = moreItems[i][2];
      button.setAttribute('aria-label', moreItems[i][2] || moreItems[i][1]);
    }
    const rollbackBtn = node('pvRollback');
    if (rollbackBtn) {
      rollbackBtn.disabled = true;
      rollbackBtn.title = labels.rollbackNote || L('rollbackNote');
    }
    const qualifyBtn = node('pvQualify');
    if (qualifyBtn) {
      qualifyBtn.textContent = labels.qualify || L('qualifyNote');
      qualifyBtn.disabled = true;
      qualifyBtn.title = labels.qualifyNote || '';
    }
    // The dialog's fixed action row.
    setText(node('pvExecute'), labels.execute || L('executeNote'));
    const executeButton = node('pvExecute');
    if (executeButton) {
      executeButton.disabled = true;
      executeButton.title = labels.executeNote || L('executeNote');
    }
    setText(node('pvCopySql'), labels.copySql || L('copySql'));
    setText(node('pvCopyClose'), L('copySqlAndClose'));
    const copyClose = node('pvCopyClose');
    if (copyClose) {
      copyClose.title = L('copyCloseNote');
      copyClose.setAttribute('aria-label', L('copyCloseNote'));
    }
    setText(node('pvCancel'), L('cancel'));
    const bannerClose = node('pvBannerClose');
    if (bannerClose) {
      bannerClose.textContent = '\u00d7';
      bannerClose.title = labels.bannerDismiss || '';
      bannerClose.setAttribute('aria-label', labels.bannerDismiss || '');
    }
    setText(node('pvInputHint'), labels.hintInput);
    setText(node('pvRebindBtn'), labels.rebind || '');
    setText(node('pvImportBtn'), labels.importClipboard || '');
    const buttons = tabButtons();
    for (let tabIndex = 0; tabIndex < buttons.length; tabIndex++) {
      const key = buttons[tabIndex].getAttribute('data-pvtab');
      buttons[tabIndex].textContent = labels[PV_TAB_KEYS[key]] || key;
    }
    updateCompactControl();
    updateModeControls();
    updateParamsActions();
  }

  // Editable parameters (lane L27 / C06) --------------------------------
  // One row per `?` the render produced. `model.parameters` carries the
  // `#{}` name, its class, the placeholder an empty box shows and the value the
  // render bound; a model without it (an older host, a command-built model)
  // falls back to the ordinal rows this page shipped before.
  function paramSignature(parameters, params) {
    if (parameters.length) {
      const parts = [];
      for (let i = 0; i < parameters.length; i++) {
        const p = parameters[i] || {};
        parts.push([p.name, p.kind, p.value, p.placeholder, p.warning,
          p.injected ? '1' : '0'].join('\u0002'));
      }
      return parts.join('\u0001');
    }
    return params.join('\u0001');
  }

  /** Middle-ellipsis for the name column: the head and tail both identify the
   *  parameter, so a very long `#{...}` keeps both ends; the full name is the
   *  tooltip. */
  function middleEllipsis(text, limit) {
    const value = String(text == null ? '' : text);
    const max = limit || 64;
    if (value.length <= max) { return value; }
    const head = Math.ceil((max - 1) / 2);
    const tail = max - 1 - head;
    return value.slice(0, head) + '\u2026' + value.slice(value.length - tail);
  }

  function placeholderFor(meta) {
    if (meta && meta.placeholder) { return meta.placeholder; }
    const key = meta ? PV_PLACEHOLDER_KEYS[meta.kind] : '';
    return key ? L(key) : '';
  }

  function buildParams() {
    const host = node('pvParamsList');
    if (!host) { return; }
    while (host.firstChild) { host.removeChild(host.firstChild); }
    const m = preview.model || {};
    const params = m.params || [];
    const parameters = Array.isArray(m.parameters) ? m.parameters : [];
    const labels = preview.labels || {};
    const count = parameters.length || params.length;
    // The reference budgeted one label width for the whole column: the widest
    // name (after the middle ellipsis) clamped to 80..480 px, never one width
    // per row.
    let widest = 0;
    for (let i = 0; i < count; i++) {
      const meta = parameters[i] || null;
      const full = meta && meta.name
        ? '#' + meta.name
        : (labels.paramsName || '').replace('{0}', String(i + 1));
      widest = Math.max(widest, middleEllipsis(full, 64).length);
    }
    host.style.setProperty(
      '--pv-name-width',
      Math.round(Math.min(480, Math.max(80, widest * 6.9 + 14))) + 'px',
    );
    for (let i = 0; i < count; i++) {
      const meta = parameters[i] || null;
      const row = doc.createElement('div');
      row.className = 'pv-param-row' + (meta && meta.injected ? ' pv-param-injected' : '');
      // The reference's `#source` label beside the value box; the ordinal is
      // the fallback for a model that carries values only.
      const fullName = meta && meta.name
        ? '#' + meta.name
        : (labels.paramsName || '').replace('{0}', String(i + 1));
      const name = doc.createElement('span');
      name.className = 'pv-param-name';
      name.textContent = middleEllipsis(fullName, 64);
      name.title = fullName + (labels.paramsHint ? ' \u2014 ' + labels.paramsHint : '');
      const cell = doc.createElement('span');
      cell.className = 'pv-param-cell';
      const input = doc.createElement('input');
      input.type = 'text';
      input.className = 'pv-param-input';
      input.value = meta ? (meta.value || '') : params[i];
      const placeholder = placeholderFor(meta);
      if (placeholder) { input.placeholder = placeholder; }
      input.autocomplete = 'off';
      input.spellcheck = false;
      // The reference's box tooltip names the source parameter; the label may
      // have been shortened by the middle ellipsis, so the box carries the
      // full name too.
      input.title = fullName;
      input.setAttribute('aria-label', fullName);
      cell.appendChild(input);
      // The warning mark appears only when the value does not fit the type the
      // statement declared (or inferred); it is a hint, never a gate.
      if (meta && meta.warning) {
        const warn = doc.createElement('span');
        warn.className = 'pv-param-warn';
        warn.textContent = '\u26a0';
        warn.title = meta.warning;
        warn.setAttribute('aria-label', meta.warning);
        cell.appendChild(warn);
      }
      row.appendChild(name);
      row.appendChild(cell);
      host.appendChild(row);
    }
    const conditions = Array.isArray(m.conditions) ? m.conditions : [];
    if (count === 0 && conditions.length === 0) {
      const empty = doc.createElement('div');
      empty.className = 'pv-params-empty';
      empty.textContent = labels.paramsEmpty || L('paramsEmpty');
      host.appendChild(empty);
    }
    pvParamsSig = paramSignature(parameters, params);
  }

  // Dynamic conditions (C07) --------------------------------------------
  function conditionSignature(conditions) {
    const parts = [];
    for (let i = 0; i < conditions.length; i++) {
      parts.push(conditions[i] && conditions[i].id ? conditions[i].id : '');
    }
    return parts.join('\u0001');
  }

  function conditionBoxes() {
    const host = node('pvConditionsList');
    return host ? host.querySelectorAll('.pv-cond-box') : [];
  }

  function postConditions(map) {
    post({ op: 'preview-conditions', conditions: map });
  }

  /** The header's single toggle names the action a click performs: with every
   *  branch on it reads "全不选", otherwise "全选" (the reference's T-SelToggle). */
  function updateConditionsToggle() {
    const button = node('pvConditionsAll');
    if (!button) { return; }
    const conditions = Array.isArray((preview.model || {}).conditions)
      ? preview.model.conditions : [];
    const boxes = conditionBoxes();
    let all = conditions.length > 0 && boxes.length > 0;
    for (let i = 0; i < boxes.length; i++) {
      if (!boxes[i].checked) { all = false; }
    }
    setText(button, all ? L('selectNone') : L('selectAll'));
  }

  function buildConditions() {
    const block = node('pvConditions');
    if (!block) { return; }
    const labels = preview.labels || {};
    const conditions = Array.isArray((preview.model || {}).conditions)
      ? preview.model.conditions : [];
    setText(node('pvConditionsTitle'), labels.conditions || L('conditions'));
    setText(node('pvConditionsHint'), labels.conditionsHint || '');
    if (conditions.length === 0) {
      block.hidden = true;
      pvConditionsSig = null;
      updateConditionsToggle();
      return;
    }
    block.hidden = false;
    const host = node('pvConditionsList');
    if (!host) { return; }
    const sig = conditionSignature(conditions);
    if (sig === pvConditionsSig) {
      // Same nodes: only the flags follow the push, so a checkbox keeps focus.
      const boxes = conditionBoxes();
      for (let i = 0; i < boxes.length; i++) {
        const condition = conditions[i] || {};
        boxes[i].checked = !!condition.enabled;
      }
      updateConditionsToggle();
      return;
    }
    while (host.firstChild) { host.removeChild(host.firstChild); }
    for (let i = 0; i < conditions.length; i++) {
      const condition = conditions[i] || {};
      const row = doc.createElement('label');
      row.className = 'pv-cond-row';
      row.setAttribute('data-cond-kind', condition.kind || 'if');
      if (condition.group) { row.setAttribute('data-cond-group', condition.group); }
      const box = doc.createElement('input');
      box.type = 'checkbox';
      box.className = 'pv-cond-box';
      box.checked = !!condition.enabled;
      box.setAttribute('data-cond-id', condition.id || '');
      box.addEventListener('change', function () {
        updateConditionsToggle();
        const id = box.getAttribute('data-cond-id');
        if (!id) { return; }
        const map = {};
        map[id] = !!box.checked;
        postConditions(map);
      });
      const text = doc.createElement('span');
      text.className = 'pv-cond-label';
      text.textContent = condition.label || condition.id || '';
      text.title = condition.label || condition.id || '';
      row.appendChild(box);
      row.appendChild(text);
      host.appendChild(row);
    }
    pvConditionsSig = sig;
    updateConditionsToggle();
  }

  // Rebind / Import availability: no values -> nothing to rebind; no clipboard
  // API (the capability gates navigator.clipboard in a web view) -> the import
  // button is greyed with the reason in its tooltip.
  function updateParamsActions() {
    const labels = preview.labels || {};
    const m = preview.model || {};
    const count = Array.isArray(m.parameters) && m.parameters.length
      ? m.parameters.length
      : (m.params || []).length;
    const rebindBtn = node('pvRebindBtn');
    if (rebindBtn) {
      rebindBtn.disabled = count === 0;
      rebindBtn.title = count === 0
        ? (labels.paramsEmpty || L('paramsEmpty'))
        : (labels.paramsHint || '');
    }
    const importBtn = node('pvImportBtn');
    if (importBtn) {
      const api = !!(navigator.clipboard && navigator.clipboard.readText);
      const blocked = !api || clipboardMissing;
      importBtn.disabled = blocked || count === 0;
      importBtn.title = blocked
        ? (labels.clipboardMissing || '')
        : (count === 0 ? (labels.paramsEmpty || L('paramsEmpty')) : '');
    }
    const moreImport = node('pvImportParams');
    if (moreImport) {
      const api = !!(navigator.clipboard && navigator.clipboard.readText);
      moreImport.disabled = count === 0 || !api || clipboardMissing;
    }
    setText(node('pvParamsHint'), labels.paramsHint);
  }

  // Degradation banner (L35b): the reference's honesty strip.
  function applyBanner() {
    const banner = node('pvBanner');
    if (!banner) { return; }
    const m = preview.model || {};
    const labels = preview.labels || {};
    if (!m.degraded) {
      banner.hidden = true;
      pvBannerDismissed = null;
      return;
    }
    const reason = m.reason || '';
    if (pvBannerDismissed === reason) { banner.hidden = true; return; }
    const text = node('pvBannerText');
    if (text) {
      text.textContent = reason
        ? (labels.reasonTitle || '') + '\n' + reason
        : (labels.reasonTitle || '');
    }
    banner.hidden = false;
  }

  function applyModel() {
    const m = preview.model || {};
    const dialect = preview.dialect || {};
    const labels = preview.labels || {};
    const name = dialect.name || m.dialect || '';
    const suffix = dialect.isDefault ? (labels.dialectDefault || '') : '';
    setText(node('pvDialectChip'), (labels.dialect || L('dialect')) + ' ' + name + suffix);
    const chip = node('pvDialectChip');
    if (chip) { chip.title = dialect.capabilities || ''; }
    // Type: the input kind the render came from (SQL / mapper XML).
    const kindText = m.kind === 'sql' ? L('kindSql')
      : m.kind === 'xml' ? L('kindXml') : (m.kind || '');
    setText(node('pvKindValue'), kindText);
    const kindWrap = node('pvKindWrap');
    if (kindWrap) { kindWrap.hidden = !kindText; }
    // The table segment has no source in this render; it stays hidden rather
    // than guessing a table out of the statement.
    const table = m.table || '';
    setText(node('pvTableValue'), table);
    const tableWrap = node('pvTableWrap');
    if (tableWrap) { tableWrap.hidden = !table; }
    // The source line (statement id / file) follows the reference's meta row.
    const source = node('pvSource');
    if (source) {
      source.textContent = m.source || '';
      source.hidden = !m.source;
      source.title = m.source || '';
    }
    // Performance advice has its own row under the meta line.
    const perfRow = node('pvPerfRow');
    const advice = node('pvAdvice');
    if (perfRow) { perfRow.hidden = !m.advice; }
    if (advice) { advice.textContent = m.advice || ''; }
    // The error-copy entry exists only while there is error text to copy.
    const errorButton = node('pvCopyError');
    if (errorButton) {
      errorButton.hidden = !(m.reason && m.reason.trim());
    }

    const badge = node('pvConfidence');
    if (badge) {
      const level = (m.confidence === 'partial' || m.confidence === 'unresolved')
        ? m.confidence : '';
      badge.className = level ? 'pv-badge ' + level : 'pv-badge';
      badge.textContent = labels.confidenceLabel || '';
      badge.hidden = !labels.confidenceLabel;
    }

    const params = m.params || [];
    const parameters = Array.isArray(m.parameters) ? m.parameters : [];
    // Rebuild the inputs only when the parameter list itself changed: a push
    // that carries the same list must leave the user's edits alone.
    if (paramSignature(parameters, params) !== pvParamsSig) { buildParams(); }
    updateParamsActions();
    buildConditions();
    // The "参数" header follows the condition section (the reference only adds
    // it when another section precedes it), and the preset fold exists only
    // while there is a statement to save values for.
    const conditions = Array.isArray(m.conditions) ? m.conditions : [];
    const paramCount = parameters.length || params.length;
    const paramsHead = node('pvParamsHead');
    if (paramsHead) { paramsHead.hidden = conditions.length === 0 || paramCount === 0; }
    const preset = node('pvPresetBlock');
    if (preset) { preset.hidden = !m.sql; }

    // Conditional tabs: SQL always, 原文 when the source differs, 列信息 when
    // the render supplied columns.
    const raw = tabs().raw || '';
    const sql = tabs().sql || '';
    const columns = tabs().columns || '';
    const tabStrip = node('pvTabs');
    const showRaw = !!raw && raw !== sql;
    const showColumns = !!columns;
    const buttons = tabButtons();
    let availableCount = 0;
    for (let tabIndex = 0; tabIndex < buttons.length; tabIndex++) {
      const key = buttons[tabIndex].getAttribute('data-pvtab');
      const show = key === 'sql' || (key === 'raw' && showRaw)
        || (key === 'columns' && showColumns);
      buttons[tabIndex].hidden = !show;
      if (show) { availableCount += 1; }
      buttons[tabIndex].title = key === 'columns' && !columns
        ? (labels.columnsEmpty || '') : '';
    }
    if (tabStrip) { tabStrip.hidden = availableCount <= 1; }
    if (preview.tab !== 'sql') {
      const current = root.querySelector('.pv-tab[data-pvtab="' + preview.tab + '"]');
      if (!current || current.hidden) { selectTab('sql'); }
    }
    applyBanner();
    applyStatementPicker();
    updateCompactControl();
    updateModeControls();
  }

  /**
   * Renders one `tabs.preview` payload (the shape `workbench::preview_tab`
   * builds): the model, the labels, the read-only dialect block, the status
   * line, and whether this push rewrites the input box.
   */
  function applyTab(tab) {
    if (!tab) { return; }
    preview.model = tab.model || {};
    preview.labels = tab.labels || {};
    preview.dialect = tab.dialect || {};
    // The display state travels with every push. Absent (an older host, the
    // standalone page's first paint) keeps what the page has, so a missing
    // field never resets a choice the reader made.
    if (tab.display) {
      // The mode shape is current; the two legacy switches (an older host)
      // still map onto it so nothing the reader chose is lost.
      let mode = tab.display.mode;
      if (!mode) {
        mode = tab.display.vertical ? 'vertical'
          : tab.display.keywordWrap ? 'keyword_wrap' : 'normal';
      }
      preview.mode = modeOf(mode);
      preview.compact = !!tab.display.compact;
      preview.display = { mode: preview.mode, compact: preview.compact };
    }
    applyLabels();
    // A render that rewrote the input also re-arms the degradation strip: a
    // dismissed banner must come back when the render changed.
    if (tab.replaceInput) { pvBannerDismissed = null; }
    applyModel();
    // Only an op that rewrote the model asks for the input box to be
    // overwritten; every other state push leaves what the user typed.
    if (tab.replaceInput) {
      const editor = node('pvInput');
      if (editor) { editor.value = tabs().raw || ''; }
    }
    applyInputHint();
    setText(node('pvStatus'), tab.status ? tab.status.text : '');
    updateFooter();
    const sig = signature();
    if (sig !== pvSig) { renderTab(); }
  }

  // "更多" fold: the reference's low-frequency actions behind one button.
  function closeMore() {
    const menu = node('pvMoreMenu');
    const more = node('pvMore');
    if (menu) { menu.hidden = true; }
    if (more) { more.setAttribute('aria-expanded', 'false'); }
  }

  function on(id, handler) {
    const target = node(id);
    if (target) { target.addEventListener('click', handler); }
  }

  // Actions -------------------------------------------------------------
  on('pvResolve', function () {
    post({ op: 'preview-render', text: inputText(), kind: '' });
  });
  on('pvFormat', function () {
    post({ op: 'preview-format', text: inputText() });
  });
  on('pvLoadFile', function () { post({ op: 'preview-load-file' }); });
  on('pvClear', function () { post({ op: 'preview-clear' }); });
  // Lane L48: the dialog shell. In the tab this entry asks the host to reveal
  // the dialog (`views::reveal` runs in that message's context); in the dialog
  // itself the entry is hidden and this handler closes the dialog instead.
  function closeDialog() {
    if (!dialogBridge || typeof dialogBridge.close !== 'function') { return; }
    try {
      // `openidea.dialog.close()` resolves after the host closed the dialog. A
      // rejection (already closed, host gone) must not surface as an unhandled
      // one: this page has no error surface of its own.
      const result = dialogBridge.close();
      if (result && typeof result.catch === 'function') { result.catch(function () {}); }
    } catch (error) { /* the dialog is already gone */ }
  }
  on('pvOpenDialog', function () {
    if (dialogBridge) { closeDialog(); return; }
    post({ op: 'preview-open-dialog' });
  });
  on('pvClose', closeDialog);
  on('pvCopy', function () { post({ op: 'preview-copy' }); });
  // The dialog's fixed action row.
  on('pvCopySql', function () { post({ op: 'preview-copy' }); });
  on('pvCopyClose', function () {
    post({ op: 'preview-copy' });
    closeDialog();
  });
  on('pvCancel', closeDialog);
  on('pvCopyError', function () { closeMore(); post({ op: 'preview-copy-error' }); });
  on('pvExport', function () { closeMore(); post({ op: 'preview-export' }); });
  // Diff / Share (lane L44): both close the fold first, so the menu never sits
  // over the diff tab the host opens.
  on('pvDiff', function () { closeMore(); post({ op: 'preview-diff' }); });
  on('pvShare', function () { closeMore(); post({ op: 'preview-share' }); });
  // Format in the result toolbar selects the reference's display mode; it never
  // rewrites the source text on the input half.
  on('pvFormatPreview', function () {
    setMode(preview.mode === 'formatted' ? 'normal' : 'formatted');
  });
  // Refresh (L35b): the reference re-runs the render on the pane's current
  // input; the host re-parses it (and falls back to the model's own text when
  // the box is empty), so a stale render is one click away from fresh.
  on('pvRefresh', function () {
    post({ op: 'preview-refresh', text: inputText(), kind: '' });
  });
  on('pvMore', function (event) {
    if (event) { event.stopPropagation(); }
    const menu = node('pvMoreMenu');
    const more = node('pvMore');
    if (!menu) { return; }
    menu.hidden = !menu.hidden;
    if (more) { more.setAttribute('aria-expanded', menu.hidden ? 'false' : 'true'); }
  });
  on('pvBannerClose', function () {
    pvBannerDismissed = (preview.model || {}).reason || '';
    const banner = node('pvBanner');
    if (banner) { banner.hidden = true; }
  });
  doc.addEventListener('click', function (event) {
    const menu = node('pvMoreMenu');
    if (!menu || menu.hidden) { return; }
    const more = node('pvMore');
    if (event.target !== menu && !menu.contains(event.target) && event.target !== more) {
      closeMore();
    }
  });
  doc.addEventListener('keydown', function (event) {
    if (event.key !== 'Escape') { return; }
    const menu = node('pvMoreMenu');
    const menuOpen = !!(menu && !menu.hidden);
    closeMore();
    // Lane L48: in a dialog, Escape closes it — the platform hands the key to
    // the page first (a page that calls `preventDefault()` keeps it). The one
    // key the page keeps for itself is the first Escape with the "More" fold
    // open; the next one closes the dialog. In the tab mount `dialogBridge` is
    // null, so nothing is prevented and the panel's own behaviour is unchanged.
    if (dialogBridge && !menuOpen) {
      event.preventDefault();
      closeDialog();
    }
  });

  // Lane L27: rebind the edited values into the SQL.
  on('pvRebindBtn', function () {
    const inputs = paramInputs();
    const values = [];
    for (let i = 0; i < inputs.length; i++) { values.push(inputs[i].value); }
    post({ op: 'preview-params', values: values });
  });
  // C07: the section header's toggle flips every branch at once; the map is
  // complete, so the host applies exactly what the page shows.
  function setAllConditions(value) {
    const conditions = (preview.model || {}).conditions || [];
    if (!conditions.length) { return; }
    const map = {};
    for (let i = 0; i < conditions.length; i++) {
      map[conditions[i].id] = value;
    }
    const boxes = conditionBoxes();
    for (let i = 0; i < boxes.length; i++) { boxes[i].checked = value; }
    updateConditionsToggle();
    postConditions(map);
  }
  on('pvConditionsAll', function () {
    const conditions = (preview.model || {}).conditions || [];
    if (!conditions.length) { return; }
    const boxes = conditionBoxes();
    let all = boxes.length > 0;
    for (let i = 0; i < boxes.length; i++) {
      if (!boxes[i].checked) { all = false; }
    }
    setAllConditions(!all);
  });
  // Clear the parameter values; the reference's `panel.param.clear` does the
  // same in its condition header. The branch switches are untouched.
  on('pvConditionsClear', function () {
    const inputs = paramInputs();
    for (let i = 0; i < inputs.length; i++) { inputs[i].value = ''; }
    const values = [];
    for (let i = 0; i < inputs.length; i++) { values.push(''); }
    post({ op: 'preview-params', values: values });
  });
  on('pvImportBtn', function () { importFromClipboard(); });
  on('pvImportParams', function () { closeMore(); importFromClipboard(); });
  function importFromClipboard() {
    if (!(navigator.clipboard && navigator.clipboard.readText)) {
      clipboardMissing = true;
      updateParamsActions();
      return;
    }
    navigator.clipboard.readText().then(function (text) {
      const parts = String(text || '').split(/[\r\n,]+/);
      const values = [];
      for (let i = 0; i < parts.length; i++) {
        const value = parts[i].trim();
        if (value !== '') { values.push(value); }
      }
      if (values.length === 0) {
        setText(node('pvStatus'), (preview.labels || {}).importFailed || '');
        updateFooter();
        return;
      }
      const inputs = paramInputs();
      for (let j = 0; j < inputs.length && j < values.length; j++) {
        inputs[j].value = values[j];
      }
      const note = ((preview.labels || {}).imported || '').replace('{0}', String(values.length));
      setText(node('pvStatus'), note);
      updateFooter();
    }).catch(function (error) {
      const message = (error && error.message) ? error.message : '';
      if (error && error.name === 'NotAllowedError') { clipboardMissing = true; }
      updateParamsActions();
      setText(node('pvStatus'), ((preview.labels || {}).importFailed || '').replace('{0}', message));
      updateFooter();
    });
  }
  on('pvRollback', function () { post({ op: 'preview-rollback' }); });

  // Statement picker (lane L45): the host re-renders the chosen id out of the
  // mapper document it kept, and the push that follows carries the new SQL.
  const statementSelect = node('pvStatement');
  if (statementSelect) {
    statementSelect.addEventListener('change', function () {
      post({ op: 'preview-statement', id: statementSelect.value });
    });
  }
  // Display controls (lane L45 + this lane's mode buttons).
  on('pvKeywordWrap', function () {
    setMode(preview.mode === 'keyword_wrap' ? 'normal' : 'keyword_wrap');
  });
  on('pvVertical', function () {
    setMode(preview.mode === 'vertical' ? 'normal' : 'vertical');
  });
  on('pvMode', function () { toggleCompact(); });

  const innerTabs = tabButtons();
  for (let index = 0; index < innerTabs.length; index++) {
    (function (element) {
      element.addEventListener('click', function () {
        selectTab(element.getAttribute('data-pvtab'));
      });
    })(innerTabs[index]);
  }

  const editor = node('pvInput');
  if (editor) {
    editor.addEventListener('input', applyInputHint);
    editor.addEventListener('keydown', function (event) {
      if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
        event.preventDefault();
        post({ op: 'preview-render', text: editor.value, kind: '' });
      }
    });
  }

  // Splitter ------------------------------------------------------------
  const paneTop = node('pvPaneTop');
  const splitter = node('pvSplitter');
  let dragging = false;

  function panelRect() { return root.getBoundingClientRect(); }
  function setTopFraction(fraction) {
    if (!paneTop) { return; }
    const clamped = Math.min(0.8, Math.max(0.15, fraction));
    paneTop.style.flexBasis = (clamped * 100) + '%';
  }
  function topFraction() {
    const rect = panelRect();
    if (!paneTop || !rect || rect.height <= 0) { return 0.4; }
    return paneTop.getBoundingClientRect().height / rect.height;
  }

  if (splitter) {
    splitter.addEventListener('pointerdown', function (event) {
      dragging = true;
      splitter.classList.add('dragging');
      if (splitter.setPointerCapture) {
        splitter.setPointerCapture(event.pointerId);
      }
      event.preventDefault();
    });
    splitter.addEventListener('pointermove', function (event) {
      if (!dragging) { return; }
      const rect = panelRect();
      if (rect && rect.height > 0) {
        setTopFraction((event.clientY - rect.top) / rect.height);
      }
    });
    const stopDrag = function (event) {
      if (!dragging) { return; }
      dragging = false;
      splitter.classList.remove('dragging');
      if (splitter.releasePointerCapture && event.pointerId !== undefined) {
        try {
          splitter.releasePointerCapture(event.pointerId);
        } catch (error) {
          /* the capture was already released */
        }
      }
    };
    splitter.addEventListener('pointerup', stopDrag);
    splitter.addEventListener('pointercancel', stopDrag);
    splitter.addEventListener('keydown', function (event) {
      if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') { return; }
      setTopFraction(topFraction() + (event.key === 'ArrowDown' ? 0.02 : -0.02));
      event.preventDefault();
    });
  }

  // Horizontal splitter (parameters | SQL editor): the reference's
  // `OnePixelSplitter(false, 0.3f)`. Same pointer + keyboard handling.
  const paramsPane = root.querySelector('.pv-params');
  const hsplitter = node('pvHSplitter');
  let hDragging = false;

  function colsRect() {
    const cols = root.querySelector('.pv-cols');
    return cols ? cols.getBoundingClientRect() : null;
  }
  function setParamsFraction(fraction) {
    if (!paramsPane) { return; }
    const clamped = Math.min(0.7, Math.max(0.15, fraction));
    paramsPane.style.flexBasis = (clamped * 100) + '%';
  }
  function paramsFraction() {
    const rect = colsRect();
    if (!paramsPane || !rect || rect.width <= 0) { return 0.3; }
    return paramsPane.getBoundingClientRect().width / rect.width;
  }

  if (hsplitter) {
    hsplitter.addEventListener('pointerdown', function (event) {
      hDragging = true;
      hsplitter.classList.add('dragging');
      if (hsplitter.setPointerCapture) {
        hsplitter.setPointerCapture(event.pointerId);
      }
      event.preventDefault();
    });
    hsplitter.addEventListener('pointermove', function (event) {
      if (!hDragging) { return; }
      const rect = colsRect();
      if (rect && rect.width > 0) {
        setParamsFraction((event.clientX - rect.left) / rect.width);
      }
    });
    const stopHDrag = function (event) {
      if (!hDragging) { return; }
      hDragging = false;
      hsplitter.classList.remove('dragging');
      if (hsplitter.releasePointerCapture && event.pointerId !== undefined) {
        try {
          hsplitter.releasePointerCapture(event.pointerId);
        } catch (error) {
          /* the capture was already released */
        }
      }
    };
    hsplitter.addEventListener('pointerup', stopHDrag);
    hsplitter.addEventListener('pointercancel', stopHDrag);
    hsplitter.addEventListener('keydown', function (event) {
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') { return; }
      setParamsFraction(paramsFraction() + (event.key === 'ArrowRight' ? 0.02 : -0.02));
      event.preventDefault();
    });
  }

  if (deps.labels) { applyLabels(); }
  applyInputHint();
  updateFooter();

  return {
    /** The mounted element (the tab's host in the panel page). */
    root: root,
    /** Render one `tabs.preview` payload. */
    applyTab: applyTab,
    /** The op sender this mount uses (the panel page's own). */
    post: post,
  };
}

/** The host's language, judged by its own localised labels. */
function pickLang(labels) {
  const values = Object.keys(labels || {});
  for (let i = 0; i < values.length; i++) {
    const value = labels[values[i]];
    if (typeof value === 'string' && /[\u4e00-\u9fff]/.test(value)) { return 'zh'; }
  }
  return 'en';
}

export default mountPreview;