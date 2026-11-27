/*
 * The SQL preview workbench (lane L23), as one ES module with two mount points:
 *
 * - `web/log-panel.html` imports it and mounts it on the fifth tab
 *   (`#panelPreview`), so the preview lives inside the log panel exactly as the
 *   IntelliJ edition's "SQL 预览" tab lives inside the SQL Log tool window;
 * - `web/preview.html` mounts it as a page of its own, the shell the API 1.18
 *   dialog will load.
 *
 * `mountPreview(root, deps)` builds the workbench inside `root`, wires every
 * control, and returns an object with `applyTab(tab)` — the render entry the
 * host's state push drives. Nothing here talks to the page's other tabs: the
 * session's log-panel specifics stay in `web/log-panel.html`, the workbench
 * state stays here. All selectors are scoped to `root`, so a future page could
 * mount two instances without the ids colliding.
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
const TEMPLATE = `<!-- Input half (lane L27): the reference's standalone page actions. -->
<div class="pv-pane pv-top" id="pvPaneTop">
  <div class="pv-toolbar">
    <button id="pvResolve" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M13.5 7.13397C14.1667 7.51888 14.1667 8.48112 13.5 8.86602L4.5 14.0622C3.83333 14.4471 3 13.966 3 13.1962L3 2.80385C3 2.03405 3.83333 1.55292 4.5 1.93782L13.5 7.13397Z" fill="none" stroke="var(--openidea-success)"/></svg><span data-btnlabel></span></button>
    <button id="pvFormat" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><rect x="2" y="12" width="12" height="1" rx="0.5" fill="currentColor"/>
<rect x="6" y="6" width="8" height="1" rx="0.5" fill="currentColor"/>
<rect x="6" y="9" width="8" height="1" rx="0.5" fill="currentColor"/>
<rect x="2" y="3" width="12" height="1" rx="0.5" fill="currentColor"/>
<path d="M2.8 6.1C2.64849 5.98637 2.44579 5.96809 2.27639 6.05279C2.107 6.13749 2 6.31062 2 6.5V9.5C2 9.68939 2.107 9.86252 2.27639 9.94722C2.44579 10.0319 2.64849 10.0136 2.8 9.9L4.8 8.4C4.9259 8.30558 5 8.15738 5 8C5 7.84262 4.9259 7.69443 4.8 7.6L2.8 6.1Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <button id="pvLoadFile" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill-rule="evenodd" clip-rule="evenodd" d="M10.0775 2.44037C10.1656 2.12093 9.97809 1.79054 9.65866 1.70242C9.33922 1.6143 9.00882 1.80182 8.9207 2.12126L5.76529 13.5596C5.67717 13.8791 5.86469 14.2095 6.18412 14.2976C6.50356 14.3857 6.83396 14.1982 6.92208 13.8788L10.0775 2.44037ZM5.41281 4.66699C5.61982 4.92575 5.57786 5.30332 5.31911 5.51033L1.96049 8.19723L5.31911 10.8841C5.57786 11.0911 5.61982 11.4687 5.41281 11.7275C5.20581 11.9862 4.82823 12.0282 4.56947 11.8212L0.625202 8.66575L0.0395508 8.19723L0.625202 7.7287L4.56947 4.57329C4.82823 4.36628 5.20581 4.40823 5.41281 4.66699ZM10.5872 11.7275C10.3802 11.4687 10.4222 11.0911 10.6809 10.8841L14.0395 8.19725L10.6809 5.51035C10.4222 5.30334 10.3802 4.92577 10.5872 4.66701C10.7942 4.40825 11.1718 4.3663 11.4305 4.57331L15.3748 7.72872L15.9605 8.19725L15.3748 8.66577L11.4305 11.8212C11.1718 12.0282 10.7942 11.9862 10.5872 11.7275Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <button id="pvClear" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill-rule="evenodd" clip-rule="evenodd" d="M7 2H9C9.55228 2 10 2.44772 10 3H6C6 2.44772 6.44772 2 7 2ZM5 3C5 1.89543 5.89543 1 7 1H9C10.1046 1 11 1.89543 11 3H13C13.5523 3 14 3.44772 14 4V5V6H13V13C13 14.1046 12.1046 15 11 15H5C3.89543 15 3 14.1046 3 13V6H2V5V4C2 3.44772 2.44772 3 3 3H5ZM11 4H10H6H5H3V5H4H12H13V4H11ZM4 6H12V13C12 13.5523 11.5523 14 11 14H5C4.44772 14 4 13.5523 4 13V6ZM6.5 7C6.22386 7 6 7.22386 6 7.5V11.5C6 11.7761 6.22386 12 6.5 12C6.77614 12 7 11.7761 7 11.5V7.5C7 7.22386 6.77614 7 6.5 7ZM9 7.5C9 7.22386 9.22386 7 9.5 7C9.77614 7 10 7.22386 10 7.5V11.5C10 11.7761 9.77614 12 9.5 12C9.22386 12 9 11.7761 9 11.5V7.5Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <button id="pvOpenDialog" class="btn" type="button" disabled><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path fill-rule="evenodd" clip-rule="evenodd" d="M7.00002 3H12C12.5523 3 13 3.44772 13 4V12C13 12.5523 12.5523 13 12 13H7.00002V3ZM6.00002 2H7.00002H12C13.1046 2 14 2.89543 14 4V12C14 13.1046 13.1046 14 12 14H7.00002H6.00002H4C2.89543 14 2 13.1046 2 12V4C2 2.89543 2.89543 2 4 2H6.00002ZM6.00002 13H4C3.44772 13 3 12.5523 3 12V4C3 3.44772 3.44772 3 4 3H6.00002V13Z" fill="currentColor"/>
<rect x="8" y="5" width="4" height="1" rx="0.5" fill="currentColor"/>
<rect x="8" y="7.5" width="4" height="1" rx="0.5" fill="currentColor"/>
<rect x="8" y="10" width="4" height="1" rx="0.5" fill="currentColor"/></svg><span data-btnlabel></span></button>
  </div>
  <div class="hint" id="pvInputHint"></div>
  <textarea id="pvInput" spellcheck="false" autocomplete="off"></textarea>
</div>

<div class="pv-splitter" id="pvSplitter" role="separator" aria-orientation="horizontal"
     tabindex="0"></div>

<div class="pv-pane pv-preview">
  <!-- Meta row: the dialect chip carries the capability matrix in its
       tooltip (the reference's dialect-capability section). -->
  <div class="pv-meta">
    <span class="pv-chip" id="pvDialectChip"></span>
    <span class="muted" id="pvDialectHint"></span>
    <span class="pv-badge" id="pvConfidence" hidden></span>
    <span class="pv-advice muted" id="pvAdvice"></span>
  </div>
  <!-- Toolbar, in the reference order: copy / format / display mode /
       explain (disabled — this plugin never executes SQL) / refresh. -->
  <div class="pv-toolbar">
    <button id="pvCopy" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><rect x="2.5" y="3.5" width="9" height="10" rx="1.5" stroke="currentColor"/>
<rect x="5" y="6" width="4" height="1" rx="0.5" fill="currentColor"/>
<rect x="5" y="8" width="4" height="1" rx="0.5" fill="currentColor"/>
<rect x="5" y="10" width="4" height="1" rx="0.5" fill="currentColor"/>
<path fill-rule="evenodd" clip-rule="evenodd" d="M11.0017 2H11.5998C12.373 2 12.9998 2.6268 12.9998 3.4V3.91081C13.0011 3.94038 13.0017 3.97011 13.0017 4V11.5482C13.6063 11.1124 13.9998 10.4021 13.9998 9.6V3.4C13.9998 2.07452 12.9253 1 11.5998 1H6.39978C5.59677 1 4.88587 1.39437 4.4502 2H6.39978H11.0017Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <button id="pvFormatPreview" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><rect x="2" y="12" width="12" height="1" rx="0.5" fill="currentColor"/>
<rect x="6" y="6" width="8" height="1" rx="0.5" fill="currentColor"/>
<rect x="6" y="9" width="8" height="1" rx="0.5" fill="currentColor"/>
<rect x="2" y="3" width="12" height="1" rx="0.5" fill="currentColor"/>
<path d="M2.8 6.1C2.64849 5.98637 2.44579 5.96809 2.27639 6.05279C2.107 6.13749 2 6.31062 2 6.5V9.5C2 9.68939 2.107 9.86252 2.27639 9.94722C2.44579 10.0319 2.64849 10.0136 2.8 9.9L4.8 8.4C4.9259 8.30558 5 8.15738 5 8C5 7.84262 4.9259 7.69443 4.8 7.6L2.8 6.1Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
    <label class="muted" id="pvModeLabel" for="pvMode"></label>
    <select id="pvMode" class="btn">
      <option value="normal"></option>
      <option value="formatted"></option>
    </select>
    <!-- Explain: the reference runs EXPLAIN through the data source; this
         build has no sidecar execution path here, so the button stays
         disabled and explains itself (same wording as Execute). -->
    <button id="pvExplain" class="btn" type="button" disabled><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M13.5 7.13397C14.1667 7.51888 14.1667 8.48112 13.5 8.86602L4.5 14.0622C3.83333 14.4471 3 13.966 3 13.1962L3 2.80385C3 2.03405 3.83333 1.55292 4.5 1.93782L13.5 7.13397Z" fill="none" stroke="var(--openidea-success)"/></svg><span data-btnlabel></span></button>
    <button id="pvRefresh" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M2.5 9V8C2.5 4.96243 4.96243 2.5 8 2.5C9.10679 2.5 10.1372 2.82692 11 3.38947" stroke="currentColor" stroke-linecap="round"/>
<path d="M5 12.6105C5.86278 13.1731 6.89321 13.5 8 13.5C11.0376 13.5 13.5 11.0376 13.5 8V7" stroke="currentColor" stroke-linecap="round"/>
<path d="M0.49997 7.50027L2.5 9.5L4.49998 7.50023" stroke="currentColor" stroke-linecap="round"/>
<path d="M11.5 8.49982L13.5 6.5L15.5 8.49982" stroke="currentColor" stroke-linecap="round"/></svg><span data-btnlabel></span></button>
    <span class="pv-more-wrap">
      <button id="pvMore" class="btn" type="button" aria-haspopup="true"
              aria-expanded="false"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><circle cx="3" cy="8" r="1.2" fill="currentColor"/><circle cx="8" cy="8" r="1.2" fill="currentColor"/><circle cx="13" cy="8" r="1.2" fill="currentColor"/></svg><span data-btnlabel></span></button>
      <div class="pv-more-menu" id="pvMoreMenu" role="menu" hidden>
        <button id="pvExport" class="pv-more-item" type="button" role="menuitem"></button>
        <!-- Rollback (L27): the workbench has no table/primary-key
             metadata, so the item stays disabled with the reason in its
             tooltip. -->
        <button id="pvRollback" class="pv-more-item" type="button" role="menuitem"
                disabled></button>
      </div>
    </span>
  </div>
  <!-- Degradation banner (L35b): the reference's honesty strip. Shown
       while the render is unresolved; the reasons carry their message
       keys, and the reader can dismiss it until the next render. -->
  <div class="pv-banner" id="pvBanner" role="status" hidden>
    <span class="pv-banner-text" id="pvBannerText"></span>
    <button class="pv-banner-close" id="pvBannerClose" type="button"></button>
  </div>
  <div class="pv-cols">
    <div class="pv-params">
      <div class="pv-pane-title" id="pvParamsTitle"></div>
      <div class="pv-scroll">
        <div id="pvParamsList"></div>
        <div class="pv-params-actions">
          <button id="pvRebindBtn" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M13.5 7.13397C14.1667 7.51888 14.1667 8.48112 13.5 8.86602L4.5 14.0622C3.83333 14.4471 3 13.966 3 13.1962L3 2.80385C3 2.03405 3.83333 1.55292 4.5 1.93782L13.5 7.13397Z" fill="none" stroke="var(--openidea-success)"/></svg><span data-btnlabel></span></button>
          <button id="pvImportBtn" class="btn" type="button"><svg viewBox="0 0 16 16" aria-hidden="true" focusable="false"><path d="M5.5 6C5.22386 6 5 6.22386 5 6.5C5 6.77614 5.22386 7 5.5 7H10.5C10.7761 7 11 6.77614 11 6.5C11 6.22386 10.7761 6 10.5 6H5.5Z" fill="currentColor"/>
<path d="M5 8.5C5 8.22386 5.22386 8 5.5 8H10.5C10.7761 8 11 8.22386 11 8.5C11 8.77614 10.7761 9 10.5 9H5.5C5.22386 9 5 8.77614 5 8.5Z" fill="currentColor"/>
<path d="M5.5 10C5.22386 10 5 10.2239 5 10.5C5 10.7761 5.22386 11 5.5 11H10.5C10.7761 11 11 10.7761 11 10.5C11 10.2239 10.7761 10 10.5 10H5.5Z" fill="currentColor"/>
<path fill-rule="evenodd" clip-rule="evenodd" d="M12 2H10.9146C10.7087 1.4174 10.1531 1 9.5 1H6.5C5.84689 1 5.29127 1.4174 5.08535 2H4C2.89543 2 2 2.89543 2 4V13C2 14.1046 2.89543 15 4 15H12C13.1046 15 14 14.1046 14 13V4C14 2.89543 13.1046 2 12 2ZM4 3H5.08535C5.29127 3.5826 5.84689 4 6.5 4H9.5C10.1531 4 10.7087 3.5826 10.9146 3H12C12.5523 3 13 3.44772 13 4V13C13 13.5523 12.5523 14 12 14H4C3.44772 14 3 13.5523 3 13V4C3 3.44772 3.44772 3 4 3ZM6.5 2C6.22386 2 6 2.22386 6 2.5C6 2.77614 6.22386 3 6.5 3H9.5C9.77614 3 10 2.77614 10 2.5C10 2.22386 9.77614 2 9.5 2H6.5Z" fill="currentColor"/></svg><span data-btnlabel></span></button>
        </div>
        <div class="hint" id="pvParamsHint"></div>
      </div>
    </div>
    <div class="pv-sqlside">
      <div class="pv-tabs" role="tablist">
        <button class="pv-tab" type="button" role="tab" data-pvtab="sql" aria-selected="true"></button>
        <button class="pv-tab" type="button" role="tab" data-pvtab="raw" aria-selected="false"></button>
        <button class="pv-tab" type="button" role="tab" data-pvtab="columns" aria-selected="false"></button>
      </div>
      <pre class="code pv-tabbody" id="pvTabBody" role="tabpanel"></pre>
    </div>
  </div>
  <!-- Status line only: the reference's resultPanel SOUTH. -->
  <div class="pv-footer">
    <span id="pvStatus" class="muted"></span>
  </div>
</div>`;

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
  pvClear: 'clear', pvOpenDialog: 'openDialog', pvCopy: 'copySql',
  pvFormatPreview: 'format', pvExport: 'export', pvExplain: 'explain',
  pvRefresh: 'refresh', pvMore: 'more',
};
const PV_TAB_KEYS = { sql: 'tabSql', raw: 'tabRaw', columns: 'tabColumns' };
const PV_TAB_ORDER = ['sql', 'raw', 'columns'];

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

  // The tab is a flex child of the panel page, exactly as before; the class
  // carries the workbench's own CSS (web/preview.css) instead of the id.
  if (!root.classList.contains('pv-root')) { root.classList.add('pv-root'); }
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

  const preview = { model: {}, labels: {}, dialect: {}, tab: 'sql', mode: 'normal' };
  if (deps.labels) { preview.labels = deps.labels; }
  // Signature of what the tab body shows: a state push that changes none of it
  // must not re-render (the reader keeps their scroll position).
  let pvSig = null;
  // Params (lane L27): signature of the parameter list (so an unrelated push
  // does not clobber what the user typed) and whether a clipboard read was
  // refused (the capability gates navigator.clipboard in a web view).
  let pvParamsSig = null;
  let clipboardMissing = false;
  // Degradation banner (L35b): the reasons the reader dismissed. A later render
  // with different reasons shows the strip again.
  let pvBannerDismissed = null;

  /** One node of this workbench, by id. */
  function node(id) { return root.querySelector('#' + id); }
  function tabs() { return preview.model.tabs || {}; }
  function tabButtons() { return root.querySelectorAll('.pv-tab'); }
  function paramInputs() { return root.querySelectorAll('.pv-param-input'); }

  function signature() {
    const m = preview.model || {};
    const t = tabs();
    const l = preview.labels || {};
    return [
      m.sql, m.reason, m.confidence, (m.params || []).join(','),
      t.formatted, t.raw, t.columns, preview.tab, preview.mode,
      l.empty, l.columnsEmpty, l.reasonTitle,
    ].join('\u0001');
  }

  function inputText() {
    const editor = node('pvInput');
    return editor ? editor.value : '';
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
        text = preview.mode === 'formatted'
          ? (source.formatted || source.sql || '')
          : (source.sql || '');
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
    body.textContent = text;
    body.className = placeholder ? 'code pv-tabbody muted' : 'code pv-tabbody';
    body.scrollTop = 0;
    pvSig = signature();
  }

  function selectTab(key) {
    if (PV_TAB_ORDER.indexOf(key) < 0) { return; }
    preview.tab = key;
    const buttons = tabButtons();
    for (let i = 0; i < buttons.length; i++) {
      const on = buttons[i].getAttribute('data-pvtab') === key;
      buttons[i].setAttribute('aria-selected', on ? 'true' : 'false');
    }
    renderTab();
  }

  function setMode(mode) {
    preview.mode = mode === 'formatted' ? 'formatted' : 'normal';
    const select = node('pvMode');
    if (select && select.value !== preview.mode) { select.value = preview.mode; }
    renderTab();
  }

  function applyLabels() {
    const labels = preview.labels || {};
    const ids = Object.keys(PV_LABEL_KEYS);
    for (let index = 0; index < ids.length; index++) {
      setText(node(ids[index]), labels[PV_LABEL_KEYS[ids[index]]]);
    }
    const openDialog = node('pvOpenDialog');
    if (openDialog) { openDialog.title = labels.openDialogNote || ''; }
    // Explain stays disabled: this plugin never executes user SQL (the
    // reference runs EXPLAIN through the data source); the tooltip says so
    // and points at the engine side.
    const explainButton = node('pvExplain');
    if (explainButton) {
      explainButton.disabled = true;
      explainButton.title = labels.explainNote || '';
    }
    const moreButton = node('pvMore');
    if (moreButton) { moreButton.title = labels.moreNote || ''; }
    const bannerClose = node('pvBannerClose');
    if (bannerClose) {
      bannerClose.textContent = '\u00d7';
      bannerClose.title = labels.bannerDismiss || '';
      bannerClose.setAttribute('aria-label', labels.bannerDismiss || '');
    }
    setText(node('pvInputHint'), labels.hintInput);
    setText(node('pvParamsTitle'), labels.params);
    setText(node('pvModeLabel'), labels.mode);
    const modeSelect = node('pvMode');
    if (modeSelect && modeSelect.options.length >= 2) {
      modeSelect.options[0].textContent = labels.modeNormal || '';
      modeSelect.options[1].textContent = labels.modeFormatted || '';
    }
    const buttons = tabButtons();
    for (let tabIndex = 0; tabIndex < buttons.length; tabIndex++) {
      const key = buttons[tabIndex].getAttribute('data-pvtab');
      buttons[tabIndex].textContent = labels[PV_TAB_KEYS[key]] || key;
    }
    // Lane L27: the editable-parameter actions and the rollback entry.
    const rebindBtn = node('pvRebindBtn');
    if (rebindBtn) { setText(rebindBtn, labels.rebind || ''); }
    const importBtn = node('pvImportBtn');
    if (importBtn) { setText(importBtn, labels.importClipboard || ''); }
    const rollbackBtn = node('pvRollback');
    if (rollbackBtn) {
      rollbackBtn.textContent = labels.rollback || '';
      // The workbench carries no table/primary-key metadata: a rollback plan
      // can never be built here, so the entry stays disabled and explains
      // itself instead of guessing a table.
      rollbackBtn.disabled = true;
      rollbackBtn.title = labels.rollbackNote || '';
    }
    updateParamsActions();
  }

  // Editable parameters (lane L27) --------------------------------------
  function buildParams() {
    const host = node('pvParamsList');
    if (!host) { return; }
    while (host.firstChild) { host.removeChild(host.firstChild); }
    const params = (preview.model && preview.model.params) || [];
    const labels = preview.labels || {};
    for (let i = 0; i < params.length; i++) {
      const row = doc.createElement('div');
      row.className = 'pv-param-row';
      // Two columns (L35b), the reference's name-beside-value layout. The model
      // carries positions only — the mapper's parameter names are gone by the
      // time the SQL is restored — so the name column shows the ordinal.
      const name = doc.createElement('span');
      name.className = 'pv-param-name';
      name.textContent = (labels.paramsName || '').replace('{0}', String(i + 1));
      name.title = labels.paramsHint || '';
      const input = doc.createElement('input');
      input.type = 'text';
      input.className = 'pv-param-input';
      input.value = params[i];
      input.autocomplete = 'off';
      input.spellcheck = false;
      row.appendChild(name);
      row.appendChild(input);
      host.appendChild(row);
    }
    if (params.length === 0) {
      const empty = doc.createElement('div');
      empty.className = 'pv-params-empty';
      empty.textContent = labels.paramsEmpty || '';
      host.appendChild(empty);
    }
    pvParamsSig = params.join('\u0001');
  }

  // Rebind / Import availability: no values -> nothing to rebind; no clipboard
  // API (the capability gates navigator.clipboard in a web view) -> the import
  // button is greyed with the reason in its tooltip.
  function updateParamsActions() {
    const labels = preview.labels || {};
    const count = ((preview.model && preview.model.params) || []).length;
    const rebindBtn = node('pvRebindBtn');
    if (rebindBtn) {
      rebindBtn.disabled = count === 0;
      rebindBtn.title = count === 0 ? (labels.paramsEmpty || '') : (labels.paramsHint || '');
    }
    const importBtn = node('pvImportBtn');
    if (importBtn) {
      const api = !!(navigator.clipboard && navigator.clipboard.readText);
      const blocked = !api || clipboardMissing;
      importBtn.disabled = blocked || count === 0;
      importBtn.title = blocked
        ? (labels.clipboardMissing || '')
        : (count === 0 ? (labels.paramsEmpty || '') : '');
    }
    setText(node('pvParamsHint'), labels.paramsHint);
  }

  // Degradation banner (L35b): the reference's honesty strip. It appears while
  // the render is `unresolved` (the host sets `degraded`), carries the reasons
  // verbatim — every line starts with its message key — and the reader can
  // dismiss it until the next render replaces the model.
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
    setText(node('pvDialectChip'), (labels.dialect || '') + ': ' + name + suffix);
    const chip = node('pvDialectChip');
    if (chip) { chip.title = dialect.capabilities || ''; }
    setText(node('pvDialectHint'), labels.dialectHint);
    setText(node('pvAdvice'), m.advice);

    const badge = node('pvConfidence');
    if (badge) {
      const level = (m.confidence === 'partial' || m.confidence === 'unresolved')
        ? m.confidence : '';
      badge.className = level ? 'pv-badge ' + level : 'pv-badge';
      badge.textContent = labels.confidenceLabel || '';
      badge.hidden = !labels.confidenceLabel;
    }

    const params = m.params || [];
    // Rebuild the inputs only when the parameter list itself changed: a push
    // that carries the same list must leave the user's edits alone.
    if (params.join('\u0001') !== pvParamsSig) { buildParams(); }
    updateParamsActions();

    const columns = tabs().columns || '';
    const buttons = tabButtons();
    for (let tabIndex = 0; tabIndex < buttons.length; tabIndex++) {
      const key = buttons[tabIndex].getAttribute('data-pvtab');
      buttons[tabIndex].title = key === 'columns' && !columns
        ? (labels.columnsEmpty || '') : '';
    }
    applyBanner();
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
    setText(node('pvStatus'), tab.status ? tab.status.text : '');
    const sig = signature();
    if (sig !== pvSig) { renderTab(); }
  }

  // "更多" fold (L35b): Export and Rollback live behind one button, the way the
  // reference folds its low-frequency actions into `panel.more`.
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
  on('pvCopy', function () { post({ op: 'preview-copy' }); });
  on('pvExport', function () { closeMore(); post({ op: 'preview-export' }); });
  // Format (L35b): the reference formats the SQL the result pane shows
  // (`formatSql`), not the input box on the other half of the splitter; the
  // display-mode select below stays the separate view switch.
  on('pvFormatPreview', function () {
    const shown = tabs().sql || '';
    post({ op: 'preview-format', text: shown || inputText() });
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
    if (event.key === 'Escape') { closeMore(); }
  });

  // Lane L27: rebind the edited values into the SQL (no automatic rebind after
  // an import), and import a comma/newline separated list from the clipboard
  // into the parameter table.
  on('pvRebindBtn', function () {
    const inputs = paramInputs();
    const values = [];
    for (let i = 0; i < inputs.length; i++) { values.push(inputs[i].value); }
    post({ op: 'preview-params', values: values });
  });
  on('pvImportBtn', function () {
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
        return;
      }
      const inputs = paramInputs();
      for (let j = 0; j < inputs.length && j < values.length; j++) {
        inputs[j].value = values[j];
      }
      const note = ((preview.labels || {}).imported || '').replace('{0}', String(values.length));
      setText(node('pvStatus'), note);
    }).catch(function (error) {
      const message = (error && error.message) ? error.message : '';
      if (error && error.name === 'NotAllowedError') { clipboardMissing = true; }
      updateParamsActions();
      setText(node('pvStatus'), ((preview.labels || {}).importFailed || '').replace('{0}', message));
    });
  });
  on('pvRollback', function () { post({ op: 'preview-rollback' }); });

  const modeSelect = node('pvMode');
  if (modeSelect) {
    modeSelect.addEventListener('change', function () {
      setMode(modeSelect.value);
    });
  }

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

  if (deps.labels) { applyLabels(); }

  return {
    /** The mounted element (the tab's host in the panel page). */
    root: root,
    /** Render one `tabs.preview` payload. */
    applyTab: applyTab,
    /** The op sender this mount uses (the panel page's own). */
    post: post,
  };
}

export default mountPreview;
