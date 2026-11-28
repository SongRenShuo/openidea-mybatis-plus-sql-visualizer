//! SQL log panel (T2-1, lane L17).
//!
//! Data is pushed by the capture lane and by commands: [`set_log_text`] and
//! [`append_text`] fill a module-level buffer, and the panel parses it lazily.
//! The panel UI is the packaged page `web/log-panel.html`
//! (`web = "web/log-panel.html"` in `plugin.toml`, applied by the controller):
//! the page owns the DOM and posts user actions (`ready`, `filter`, `copy-line`,
//! `open-line`, `capture-start` / `capture-stop` / `capture-retry`, `clear`,
//! `sort`, `export`), and the plugin answers with the full state through
//! [`host::post_view_message`]. No HTML is built in Rust any more, so the page
//! always renders text with `textContent` (no injection).
//!
//! The store lives in a `OnceLock<Mutex<_>>` because `Main` belongs to the
//! controller (wasm is single-threaded; the mutex only serialises test threads).
//!
//! Lane W2-0 split the file by concern so the second-wave lanes can each work on
//! one file without conflicts: [`state`] holds the filter/sort model and the
//! process-wide store, [`rows`] the row assembly/filtering/sorting, [`logs`] the
//! log list's ops, [`tabs`] the two input tabs plus the preview routing, and
//! [`history`] the snapshot list. Nothing public changed: the items below keep
//! their old `log_panel::…` paths through the re-exports.

use openidea_plugin::host;
use openidea_plugin::views::{Body, View, ViewEvent, Views};
use openidea_plugin::{Error, Invocation};
use serde_json::{Value, json};

use crate::i18n::t;
use crate::indexer;
use crate::settings;

mod history;
mod logs;
mod rows;
mod state;
mod tabs;

pub use self::history::history_capture;
pub use self::logs::visible_sql;
pub use self::rows::{
    PanelRow, RowCategory, SqlType, build_rows, build_rows_with, filter_row_refs, filter_rows,
    regex_is_invalid, row_matches,
};
#[cfg(test)]
pub(crate) use self::state::reset_for_tests;
pub use self::state::{
    Filter, Focus, FocusKind, SortMode, TypeFilter, append_text, buffered_len, clear, display_mode,
    is_paused, set_filter, set_log_text, set_paused, snapshot, toggle_pause,
};

use self::history::history_state;
use self::logs::all_sql;
use self::rows::{
    apply_sort, cached_list_display_sql, dedupe_key_token, display_rows, java_target,
    n_plus_one_key_token, xml_target,
};
use self::state::{
    CRUD_STYLE_SETTING, DISPLAY_MODE_SETTING, SHOW_DURATION_SETTING, buffer_cap, crud_style,
    dedupe_active, ensure_parsed, slow_threshold, store,
};
use self::tabs::{log_to_sql_state, sql_to_java_state};

// Lane E's SQL suppression rules live in `src/views/suppress.rs`. The `#[path]`
// keeps `views/mod.rs` (not this lane's file) untouched; the module is declared
// here because this file is the panel whose rows it filters. The path is
// relative to `views/log_panel/` since lane W2-0 moved this file into a
// directory.
#[path = "../suppress.rs"]
pub mod suppress;

/// The id declared in `[[contributes.views]]`.
pub const VIEW_ID: &str = "sqlv-logs";

/// Default refresh interval for pushed rows (also the parse throttle window).
pub const DEFAULT_REFRESH_MS: u64 = 250;
/// SQL displayed before truncation (the full statement is kept in the store and
/// copied/exported in full).
pub(super) const SQL_DISPLAY_CHARS: usize = 180;

// ---------------------------------------------------------------------------
// State push
// ---------------------------------------------------------------------------

/// The agent's newest heartbeat counters, as the page reads them.
///
/// `null` before the first heartbeat: the socket being up while the agent has
/// said nothing is a different state from an agent reporting zero woven classes.
fn agent_health_json() -> Value {
    match crate::capture::bridge::agent_health() {
        Some(health) => json!({
            "woven": health.woven,
            "matched": health.matched,
            "failed": health.failed,
            "unmodifiable": health.unmodifiable,
            "skippedByVersion": health.skipped_by_version,
            "catchAll": health.catch_all,
            "gate": health.gate,
            "dropped": health.dropped,
        }),
        None => Value::Null,
    }
}

/// Builds the full push state (see the lane contract in `docs/registration/L17.md`).
fn build_state() -> Value {
    let mut state = store();
    ensure_parsed(&mut state);

    // Lane L2: the list display mode and the configured dialect decide how a
    // row's statement text is prepared. Both are read before the rows are
    // serialised, so the page only renders what the host resolved (the folding
    // and the formatter's casing never disagree between push and render).
    let display = display_mode(&settings::get_string(DISPLAY_MODE_SETTING, "keyword_wrap"));
    let (list_dialect, _) = crate::views::workbench::configured_dialect();

    let slow_threshold_ms = slow_threshold();
    let dedupe = dedupe_active(&state);
    let rows = display_rows(
        &state.analysis,
        &state.sources,
        &state.removed,
        &state.blocked,
        dedupe,
        slow_threshold_ms,
        &state.expanded,
    );
    // Lane E: a suppression rule hides the rows it matches. This is a display
    // filter, not a buffer drop — deleting the rule brings these rows back.
    let suppression = suppress::matcher();
    let rows: Vec<PanelRow> = if suppression.is_empty() {
        rows
    } else {
        rows.into_iter()
            .filter(|row| !suppression.hidden(&row.sql, &row.raw_sql))
            .collect()
    };

    let filter = state.filter.clone();
    let mut filtered = filter_row_refs(&rows, &filter, slow_threshold_ms);
    let total = filtered.len();
    let slow = filtered
        .iter()
        .filter(|row| row.duration_ms.is_some_and(|ms| ms >= slow_threshold_ms))
        .count();
    let n_plus_one = filtered.iter().filter(|row| row.n_plus_one).count();
    let blocked = state.blocked.len();

    // The buffer cap keeps the newest `cap` rows in capture order, then the
    // display order sorts that window. Sorting before the cap would let a
    // descending mode drop the very rows it puts first (the old duration toggle
    // kept the fastest rows of a longest-first list).
    let cap = buffer_cap();
    let start = total.saturating_sub(cap);
    let mut shown: Vec<&PanelRow> = filtered.drain(start..).collect();
    apply_sort(&mut shown, state.sort);
    let truncated = total > shown.len();

    // The two location indexes are queried once per push, on the rows actually
    // sent, and only through memory: each lookup is a hash lookup over an index
    // the host filled in its own background batches. No file is read here.
    let java_targets: Vec<Option<(String, u32)>> = {
        let java = indexer::java_methods();
        shown.iter().map(|row| java_target(row, &java)).collect()
    };
    let xml_targets: Vec<Option<(String, u32)>> = {
        let xml = indexer::xml_statements();
        shown.iter().map(|row| xml_target(row, &xml)).collect()
    };

    let rows_json: Vec<Value> = shown
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let jump = java_targets.get(index).and_then(Option::as_ref);
            let xml = xml_targets.get(index).and_then(Option::as_ref);
            // `logs.displayMode`: the display text for this row and the muted
            // `(+N)` tail (empty outside the folded keyword-wrap mode). The full
            // statement stays in the store — copy, export and the row actions
            // never read this field.
            let (sql_text, sql_tail) = cached_list_display_sql(&row.sql, display, &list_dialect);
            json!({
                "line": row.line_no,
                "seq": start + index + 1,
                "time": row.time_text(),
                "duration_ms": row.duration_ms,
                "db": "",
                "rows_affected": row.affected_rows,
                "dedupe_count": row.dedupe_count,
                "group_count": row.group_count,
                "n_plus_one": row.n_plus_one,
                // Provenance (lane L28). `class_simple` + `method` are what the
                // row header shows; `focus_key` is what a method chip filters by.
                "class_name": row.class_name(),
                "class_simple": row.class_simple(),
                "method": row.method(),
                "focus_key": row.focus_key(),
                "thread": row.thread(),
                "request_id": row.request_id(),
                // Only a unique resolution is offered; a multi-candidate row
                // states nothing rather than picking one (1-based lines).
                "jump_file": jump.map(|(file, _)| file.clone()),
                "jump_line": jump.map(|(_, line)| line + 1),
                "xml_file": xml.map(|(file, _)| file.clone()),
                "xml_line": xml.map(|(_, line)| line + 1),
                "sql": sql_text,
                "sqlTail": sql_tail,
                "error_text": row.error_text(),
                "category": row.category.token(),
                "sql_type": row.sql_type.token(),
                // Group expansion (lane L26): the keys the ⨉N / ×N badges
                // toggle, and whether this row belongs to an expanded group.
                "dedupe_key": row.dedupe_key,
                "n_plus_one_key": row.n_plus_one_key,
                "expanded": state.expanded.contains(&dedupe_key_token(&row.dedupe_key))
                    || state.expanded.contains(&n_plus_one_key_token(&row.n_plus_one_key)),
            })
        })
        .collect();

    let console_lines = state.text.lines().count() as u64;
    let paused = state.paused;
    let follow = state.follow;
    let sort = state.sort;
    let blocked_keys: Vec<String> = state.blocked.iter().cloned().collect();
    let log_to_sql = log_to_sql_state(&mut state.log_to_sql);
    let sql_to_java = sql_to_java_state(&mut state.sql_to_java);
    let history = history_state(&state.history);
    let send_to_tab = state.send_to_tab.clone();
    drop(state);

    // Lane L34: the duration segment switch. Read on every push, so a change
    // lands with the next state and needs no restart.
    let show_duration = settings::get_bool(SHOW_DURATION_SETTING, true);
    // Lane L2: `logs.crud.style` — bar / background / off. The settings lane
    // declares the key; an absent key resolves to the default, so this lane
    // never waits on it.
    let crud = crud_style(&settings::get_string(CRUD_STYLE_SETTING, "bar"));

    // The fifth tab (lane L23) is the preview workbench: the model and the
    // ops live in `views::workbench`, the state, the page and the push channel
    // are this panel's. `selectTab` is `null` unless something asked for it
    // (`open-workbench`, a double-click on a row) and stays set until the page
    // reports the tab it shows.
    let preview = crate::views::workbench::preview_tab();
    let select_tab = crate::views::workbench::select_tab();
    // The engine half of the row's database badge (lane L32). It is the same
    // configured dialect the preview tab states (`list_dialect`, resolved
    // before the rows), so the two never disagree; a row that states no
    // database still gets the engine only — this build captures no connection
    // name and never invents one.
    let dialect_name = list_dialect.clone();
    let send_to_tab_json = match &send_to_tab {
        Some(send) => json!({
            "tab": send.tab,
            "text": send.text,
            "selectTab": send.select_tab,
        }),
        None => Value::Null,
    };

    let stats = crate::capture::bridge::Stats::current();
    // A capture start whose dial was refused (the target application is not up
    // yet) is a waiting state, not an error: the page ticks `capture-retry`
    // every 2 s while this is set.
    let waiting = crate::capture::bridge::waiting();
    let capture_waiting = waiting
        .as_ref()
        .map(|waiting| {
            json!({
                "port": waiting.port,
                "attempts": waiting.attempts,
                "lastError": waiting.last_error,
            })
        })
        .unwrap_or(Value::Null);

    json!({
        "op": "state",
        "rows": rows_json,
        "selectTab": select_tab,
        "dialect": dialect_name,
        "sendToTab": send_to_tab_json,
        "tabs": {
            "logToSql": log_to_sql,
            "sqlToJava": sql_to_java,
            "history": history,
            "preview": preview,
        },
        "filter": {
            "types": filter.types.selected_tokens(),
            "types_mode": filter.types.mode(),
            "slow_only": filter.slow_only,
            "keyword": filter.keyword,
            "match_case": filter.match_case,
            "regex": filter.regex,
            "dedupe": dedupe,
            "regex_error": regex_is_invalid(&filter),
            // The active focus chip; the page only draws it (the filtering is
            // this module's).
            "focus": match &filter.focus {
                Some(focus) => json!({"kind": focus.kind.token(), "value": focus.value}),
                None => Value::Null,
            },
            // The blocked keys themselves (lane L26: the eye-off popup lists
            // them); the count stays in `counts.blocked`.
            "blocked": blocked_keys,
        },
        "counts": {
            "total": total,
            "shown": shown.len(),
            "slow": slow,
            "n_plus_one": n_plus_one,
            "truncated": truncated,
            "cap": cap,
            "blocked": blocked,
        },
        "status": {
            "console_lines": console_lines,
            "agent_connected": stats.connected,
            "agent_events": stats.events,
            "paused": paused,
            "follow": follow,
            // Non-null while a refused capture start waits for the target
            // application; `null` once it connected or capture stopped.
            "capture_waiting": capture_waiting,
            // The agent's own counters from its heartbeat (lane W2-1): the only
            // way to tell "hooks never fired" from "the agent stopped itself"
            // when the panel is empty while the socket is up.
            "agent_health": agent_health_json(),
        },
        "sort": sort.token(),
        "slow_threshold_ms": slow_threshold_ms,
        // Lane L34: `logs.displayMode` / `log.showDuration`.
        "displayMode": display,
        "showDuration": show_duration,
        // Lane L2: `logs.crud.style` (bar / background / off).
        "crudStyle": crud,
        "labels": labels_json(),
    })
}

/// Posts the full state to the packaged page.
pub fn push_state() -> Result<(), Error> {
    // Every push consumes the workbench's one-shot `replace_input`/`handoff`
    // flags, so a test that pushes without the lock steals another test's flag
    // and fails it later, somewhere else. This assert turns that into a
    // deterministic failure at the offending call (test builds only).
    #[cfg(test)]
    crate::views::assert_state_lock_held("log_panel::push_state");
    let state = build_state();
    host::post_view_message(VIEW_ID, &state.to_string())
}
/// The label map the page binds (see [`labels_base`] and [`labels_actions`]).
///
/// The two halves are separate `json!` objects because one object literal of
/// this size exceeds the macro's recursion limit; merging keeps the page's
/// single `labels` object.
fn labels_json() -> Value {
    let mut base = labels_base();
    for extra in [labels_actions(), labels_tabs(), labels_sheets()] {
        if let (Some(target), Value::Object(extra)) = (base.as_object_mut(), extra) {
            target.extend(extra);
        }
    }
    base
}

fn labels_base() -> Value {
    json!({
        "typesTitle": t("view.logs.filter.types_title"),
        "typesAll": t("view.logs.filter.all"),
        "typesNone": t("view.logs.filter.none"),
        "typesCountSuffix": t("view.logs.filter.count_suffix"),
        "selectAll": t("view.logs.filter.select_all"),
        "selectNone": t("view.logs.filter.select_none"),
        "slowOnly": t("view.logs.filter.slow_only"),
        "search": t("view.logs.filter.search"),
        // Row fragments (lane L32): the duration unit and the affected-row
        // suffix the reference prints as `12ms` and `(3 rows)`.
        "durationUnit": t("view.logs.row.duration_suffix"),
        "rowsSuffix": t("view.logs.row.rows_suffix"),
        // The page draws its own action row: a packaged view gets no host toolbar.
        "pause": t("view.logs.button.pause"),
        "resume": t("view.logs.button.resume"),
        "clear": t("view.logs.button.clear"),
        "copyAll": t("view.logs.button.copy"),
        "dedupe": t("view.logs.filter.dedupe"),
        "keywordPlaceholder": t("view.logs.filter.keyword"),
        "matchCase": t("view.logs.filter.match_case"),
        "matchCaseTitle": t("view.logs.filter.match_case_title"),
        "regex": t("view.logs.filter.regex"),
        "regexTitle": t("view.logs.filter.regex_title"),
        "hitsSuffix": t("view.logs.filter.hits_suffix"),
        "regexError": t("view.logs.filter.regex_error"),
        "slowPrefix": t("view.logs.slow_prefix"),
        "empty": t("view.logs.empty"),
        // Lane A: the reference's empty state is a two-line guidance block in
        // the platform's muted `StatusText` ink (`log.panel.empty`), not a
        // one-line note.
        "emptyHint": t("view.logs.empty.hint"),
        "paused": t("view.logs.paused"),
        "statusConsolePrefix": t("view.logs.status.console_prefix"),
        "statusConsoleSuffix": t("view.logs.status.console_suffix"),
        "statusAgentOff": t("view.logs.status.agent_off"),
        // Lane A: the status line names a refused capture start while the
        // plugin keeps dialling; `{port}` is filled in by the page.
        "statusCaptureWaiting": t("view.logs.status.capture_waiting"),
        "statusAgentEventsPrefix": t("view.logs.status.agent_events_prefix"),
        // Why the enhanced channel is empty while the socket is up: the agent's
        // heartbeat counters (lane W2-1).
        "statusAgentGate": t("view.logs.status.agent_gate"),
        "statusAgentNoHooks": t("view.logs.status.agent_no_hooks"),
        "statusAgentIdle": t("view.logs.status.agent_idle"),
        "totalPrefix": t("view.logs.status.total_prefix"),
        "totalSuffix": t("view.logs.status.total_suffix"),
        "truncatedPrefix": t("view.logs.status.truncated_prefix"),
        "nPlusOne": t("view.logs.badge.n_plus_one_suspect"),
        "dedupeBadgePrefix": t("view.logs.badge.dedupe_prefix"),
        "groupBadgePrefix": t("view.logs.badge.group_prefix"),
        "menuCopySql": t("view.logs.menu.copy_sql"),
        "menuCopyTime": t("view.logs.menu.copy_time"),
        "menuExport": t("view.logs.menu.export"),
        // Row actions (lane L28).
        "menuJumpJava": t("view.logs.menu.jump_java"),
        "menuJumpXml": t("view.logs.menu.jump_xml"),
        "menuFocusMethod": t("view.logs.menu.focus_method"),
        "menuFocusRequest": t("view.logs.menu.focus_request"),
        "menuFocusTemplate": t("view.logs.menu.focus_template"),
        "menuDelete": t("view.logs.menu.delete"),
        "menuBlockMethod": t("view.logs.menu.block_method"),
        "menuSuppress": t("view.logs.menu.suppress"),
        // Lane L32: the two "发送到" hand-offs into the input tabs.
        "menuSendSqlToJava": t("view.logs.menu.send_sql_to_java"),
        "menuSendLogToSql": t("view.logs.menu.send_log_to_sql"),
        "focusMethodPrefix": t("view.logs.focus.method_prefix"),
        "focusRequestPrefix": t("view.logs.focus.request_prefix"),
        "focusTemplatePrefix": t("view.logs.focus.template_prefix"),
        "focusClear": t("view.logs.focus.clear"),
        "blockedPrefix": t("view.logs.blocked.prefix"),
        "blockedClear": t("view.logs.blocked.clear"),
        "typeSelect": t("view.logs.type.select"),
        "typeInsert": t("view.logs.type.insert"),
        "typeUpdate": t("view.logs.type.update"),
        "typeDelete": t("view.logs.type.delete"),
        "typeCreate": t("view.logs.type.create"),
        "typeDrop": t("view.logs.type.drop"),
    })
}

/// In-panel sheets and the phrase badges (lane L2), split out for the same
/// macro-budget reason as [`labels_tabs`]: the settings/community/help
/// overlays and the `同结构 N 条` phrase added enough entries to outgrow one
/// `json!` call.
fn labels_sheets() -> Value {
    json!({
        // The reference prints the template count as a phrase (`同结构 N 条`),
        // with `{n}` filled by the page.
        "groupBadge": t("view.logs.badge.group"),
        // The settings sheet names the four settings this panel consumes and
        // the page they live on; help reuses the cheat-sheet lines.
        "sheetClose": t("view.logs.sheet.close"),
        "settingsDisplayMode": t("view.logs.settings.display_mode"),
        "settingsShowDuration": t("view.logs.settings.show_duration"),
        "settingsSlowThreshold": t("view.logs.settings.slow_threshold"),
        "settingsCrudStyle": t("view.logs.settings.crud_style"),
        "settingsPathHint": t("view.logs.settings.path_hint"),
        "settingsOn": t("view.logs.settings.on"),
        "settingsOff": t("view.logs.settings.off"),
        "displayModeSingle": t("view.logs.display_mode.single"),
        "displayModeKeywordWrap": t("view.logs.display_mode.keyword_wrap"),
        "displayModeFormatted": t("view.logs.display_mode.formatted"),
        "crudStyleBar": t("view.logs.crud.bar"),
        "crudStyleBackground": t("view.logs.crud.background"),
        "crudStyleOff": t("view.logs.crud.off"),
        "helpTitle": t("view.logs.help.title"),
        "helpSearch": t("view.logs.help.search"),
        "helpTab": t("view.logs.help.tab"),
        "helpDblclick": t("view.logs.help.dblclick"),
        "helpMenu": t("view.logs.help.menu"),
        "helpScroll": t("view.logs.help.scroll"),
        "helpSort": t("view.logs.help.sort"),
        "helpBlocked": t("view.logs.help.blocked"),
        "communityAuthor": t("view.logs.community.author"),
        "communityAuthorDesc": t("view.logs.community.author_desc"),
        "communityGroup": t("view.logs.community.group"),
        "communityGroupDesc": t("view.logs.community.group_desc"),
    })
}

/// The seven sort options in the reference's `sortOptions()` order.
fn sort_option_labels() -> Value {
    Value::Array(
        SortMode::ALL
            .iter()
            .map(|mode| json!({ "token": mode.token(), "label": sort_label(*mode) }))
            .collect(),
    )
}

/// One sort-mode label (`log.sort.*` in the reference bundle).
fn sort_label(mode: SortMode) -> String {
    let key = match mode {
        SortMode::TimeAsc => "view.logs.sort.time_asc",
        SortMode::TimeDesc => "view.logs.sort.time_desc",
        SortMode::DurationAsc => "view.logs.sort.duration_asc",
        SortMode::DurationDesc => "view.logs.sort.duration_desc",
        SortMode::Sequence => "view.logs.sort.sequence",
        SortMode::Structure => "view.logs.sort.structure",
        SortMode::Database => "view.logs.sort.database",
    };
    t(key)
}

/// Tab strip and per-tab captions (lanes L22/L23), split out of
/// [`labels_base`] so no single `json!` call outgrows the macro's recursion
/// budget — the merge in [`labels_json`] puts every entry in one object.
fn labels_tabs() -> Value {
    json!({
        // Tabs (lane L22).
        "tabLogs": t("view.logs.tab.logs"),
        "tabLogToSql": t("view.logs.tab.log_to_sql"),
        "tabSqlToJava": t("view.logs.tab.sql_to_java"),
        "tabHistory": t("view.logs.tab.history"),
        // Fifth tab, the preview workbench (lane L23).
        "tabPreview": t("view.logs.tab.preview"),
        // The two input tabs (lane L33): each is a standalone pane with an
        // input editor on top, a result editor below and a status line.
        "tabLogToSqlConvert": t("view.logs.tab.log_to_sql.convert"),
        "tabLogToSqlReadClipboard": t("view.logs.tab.log_to_sql.read_clipboard"),
        "tabLogToSqlClearInput": t("view.logs.tab.log_to_sql.clear_input"),
        "tabLogToSqlHintInput": t("view.logs.tab.log_to_sql.hint_input"),
        "tabLogToSqlHintResult": t("view.logs.tab.log_to_sql.hint_result"),
        "tabLogToSqlCopy": t("view.logs.tab.log_to_sql.copy"),
        "tabLogToSqlToggleFormat": t("view.logs.tab.log_to_sql.toggle_format"),
        "tabLogToSqlClearResult": t("view.logs.tab.log_to_sql.clear_result"),
        "tabLogToSqlEmpty": t("view.logs.tab.log_to_sql.empty"),
        "tabLogToSqlNoEntry": t("view.logs.tab.log_to_sql.no_entry"),
        "tabLogToSqlConverted": t("view.logs.tab.log_to_sql.converted"),
        "tabLogToSqlUnresolved": t("view.logs.tab.log_to_sql.unresolved"),
        "tabLogToSqlCopied": t("view.logs.tab.log_to_sql.copied"),
        "tabSqlToJavaConvert": t("view.logs.tab.sql_to_java.convert"),
        "tabSqlToJavaFormat": t("view.logs.tab.sql_to_java.format"),
        "tabSqlToJavaClearInput": t("view.logs.tab.sql_to_java.clear_input"),
        "tabSqlToJavaHintInput": t("view.logs.tab.sql_to_java.hint_input"),
        "tabSqlToJavaHintResult": t("view.logs.tab.sql_to_java.hint_result"),
        "tabSqlToJavaCopy": t("view.logs.tab.sql_to_java.copy"),
        "tabSqlToJavaClearResult": t("view.logs.tab.sql_to_java.clear_result"),
        "tabSqlToJavaFormatLabel": t("view.logs.tab.sql_to_java.format_label"),
        "tabSqlToJavaFormatLambda": t("view.logs.tab.sql_to_java.format_lambda"),
        "tabSqlToJavaFormatString": t("view.logs.tab.sql_to_java.format_string"),
        "tabSqlToJavaEntityLabel": t("view.logs.tab.sql_to_java.entity_label"),
        "tabSqlToJavaEntityHint": t("view.logs.tab.sql_to_java.entity_hint"),
        "tabSqlToJavaCallerLabel": t("view.logs.tab.sql_to_java.caller_label"),
        "tabSqlToJavaCallerHint": t("view.logs.tab.sql_to_java.caller_hint"),
        "tabSqlToJavaReturnLabel": t("view.logs.tab.sql_to_java.return_label"),
        "tabSqlToJavaReturnHint": t("view.logs.tab.sql_to_java.return_hint"),
        "tabSqlToJavaEmpty": t("view.logs.tab.sql_to_java.empty"),
        "tabSqlToJavaGenerated": t("view.logs.tab.sql_to_java.generated"),
        "tabSqlToJavaFixedLambda": t("view.logs.tab.sql_to_java.fixed_lambda"),
        "tabSqlToJavaNoCode": t("view.logs.tab.sql_to_java.no_code"),
        "tabSqlToJavaCopied": t("view.logs.tab.sql_to_java.copied"),
        "tabHistoryKeyword": t("view.logs.tab.history.keyword"),
        "tabHistorySearch": t("view.logs.tab.history.search"),
        "tabHistoryEmpty": t("view.logs.tab.history.empty"),
        "tabHistoryNoMatch": t("view.logs.tab.history.no_match"),
        "tabHistoryCountSuffix": t("view.logs.tab.history.count_suffix"),
        "tabHistorySource": t("view.logs.tab.history.source"),
        "tabHistoryTruncated": t("view.logs.tab.history.truncated"),
        "tabHistoryFilesEmpty": t("view.logs.tab.history.files_empty"),
        "tabHistoryEntriesEmpty": t("view.logs.tab.history.entries_empty"),
        "tabHistorySearchFiles": t("view.logs.tab.history.search_files"),
        "tabHistorySearchEntries": t("view.logs.tab.history.search_entries"),
        "tabHistoryFileCountSuffix": t("view.logs.tab.history.file_count_suffix"),
        "tabHistoryFilesCountSuffix": t("view.logs.tab.history.files_count_suffix"),
        "tabHistoryBack": t("view.logs.tab.history.back"),
        "tabHistoryRefresh": t("view.logs.tab.history.refresh"),
        "tabHistoryScrollEnd": t("view.logs.tab.history.scroll_end"),
        "tabHistoryPreviewToggle": t("view.logs.tab.history.preview_toggle"),
        "tabHistoryPreviewEmpty": t("view.logs.tab.history.preview_empty"),
        "tabHistoryCopy": t("view.logs.tab.history.copy"),
        "tabHistoryJumpJava": t("view.logs.tab.history.jump_java"),
        "tabHistoryJumpXml": t("view.logs.tab.history.jump_xml"),
        "tabHistoryOpenPreview": t("view.logs.tab.history.open_preview"),
    })
}

/// Toolbar actions and interactions (lane L26).
fn labels_actions() -> Value {
    json!({
        "captureStart": t("view.logs.button.capture_start"),
        "captureStop": t("view.logs.button.capture_stop"),
        "scrollEnd": t("view.logs.button.scroll_end"),
        // Sort menu (lane A): the reference's seven `sortOptions()` entries in
        // menu order, and the `Sort: <current mode>` tooltip prefix. The page
        // builds the popup from this list, so the order and the wording have a
        // single source on the host side.
        "sortLabel": t("view.logs.sort.label"),
        "sortOptions": sort_option_labels(),
        "blocked": t("view.logs.button.blocked"),
        "blockedTitle": t("view.logs.blocked.title"),
        "blockedEmpty": t("view.logs.blocked.empty"),
        // Lane E: the eye-off toolbar action now opens the suppression-rule
        // manager (the reference's fifth action), so its tooltip names that.
        "manageSuppress": t("view.suppress.manage"),
        "settings": t("view.logs.button.settings"),
        "menuSettingsPage": t("view.logs.menu.settings_page"),
        "help": t("view.logs.button.help"),
        // Lane L32: the two toolbar entries the reference has and this build
        // gained — quick preview and the community page.
        "preview": t("view.logs.button.preview"),
        "community": t("view.logs.button.community"),
        "followOn": t("view.logs.status.follow_on"),
        "followOff": t("view.logs.status.follow_off"),
        "groupExpand": t("view.logs.group.expand"),
        "groupCollapse": t("view.logs.group.collapse"),
    })
}

// ---------------------------------------------------------------------------
// Rendering (toolbar + status chrome; the body is the packaged page)
// ---------------------------------------------------------------------------

/// Builds the panel view for a `web =` view.
///
/// The page is the body **and** the chrome: it draws the reference's action row,
/// its footer and its status line itself. The host renders `View::toolbar` as a
/// separate strip above the body (`features/plugins/views/PluginView.tsx`), so
/// anything pushed here appears *in addition to* the page's own controls —
/// that is what put a stray second row above the panel. The one-shot state push
/// stays: the live capture path (`refresh_view`) has no other way to reach the
/// page.
pub fn render_panel() -> View {
    if !store().paused {
        let _ = push_state();
    }
    View::new(Body::Html(String::new()))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Handles toolbar buttons and messages posted by the packaged page.
pub fn handle_panel_event(event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
    match event {
        ViewEvent::Button(id) if id == "toggle-pause" => {
            toggle_pause();
            let _ = push_state();
            host::refresh_view(VIEW_ID);
        }
        ViewEvent::Button(id) if id == "clear" => {
            clear();
            let _ = push_state();
            host::refresh_view(VIEW_ID);
        }
        ViewEvent::Button(id) if id == "copy-all" => {
            host::copy_to_clipboard(&all_sql())?;
        }
        ViewEvent::Message(text) => handle_message(&text, ctx)?,
        _ => {}
    }
    Ok(())
}
fn handle_message(text: &str, ctx: &Invocation) -> Result<(), Error> {
    let message: Value =
        serde_json::from_str(text).map_err(|error| Error::Invalid(error.to_string()))?;
    let op = message
        .get("op")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    // History storage is keyed by project; every history op states which
    // project it belongs to before touching storage.
    if op.starts_with("history") {
        history::set_history_scope(ctx);
    }
    // Suppression rules are per project too (lane E): point the rule store at
    // this window's project before any op reads or writes it.
    suppress::set_scope(ctx);
    // One arm per op; the work itself lives in the submodule named on the right
    // (lane W2-0), so a later lane edits that file without touching the
    // dispatch.
    match op {
        // The packaged page asks for the state once its script is ready.
        "ready" | "page" => {
            let _ = push_state();
        }
        "filter" => logs::filter(&message)?,
        "toggle-pause" => logs::toggle_pause_op()?,
        "clear" => logs::clear_op()?,
        "copy-all" => logs::copy_all()?,
        "copy-line" => logs::copy_line(&message)?,
        "copy-time" => logs::copy_time(&message)?,
        "export" => logs::export()?,
        "open-line" => logs::open_line(&message, ctx)?,
        // Tab strip: the page reports the tab it now shows, which closes a
        // pending "switch to the preview tab" request and any pending
        // "send to a tab" hand-off (lane L32).
        "tab" => {
            crate::views::workbench::note_page_tab();
            store().send_to_tab = None;
            // Lane L34: reporting the tab is also the point where a hand-off
            // (open-workbench / open-line / history-open / preview-sql) has
            // landed, so `render.formatOnPreview` is applied here. A changed
            // model needs its own push: the page is already showing the tab and
            // would otherwise keep the unformatted statement.
            if crate::views::workbench::format_handoff_sql() {
                let _ = push_state();
            }
        }
        // Lane L32: "发送到 SQL→Java" / "发送到 日志→SQL" (see `tabs`).
        "send-to-tab" => tabs::send_to_tab(&message)?,
        // "Log → SQL" tab (lane L33): the tab's own input pane converts the
        // pasted log text; the list's selection plays no part any more.
        "log-to-sql-convert" => tabs::log_to_sql_convert(&message)?,
        "log-to-sql-clear" => tabs::log_to_sql_clear()?,
        "log-to-sql-read-clipboard" => tabs::log_to_sql_read_clipboard()?,
        // "SQL → Java" tab (lane L33): the input and the option row come from
        // the page, the conversion is the `sqlv_reverse_sql` handler.
        "sql-to-java-convert" => tabs::sql_to_java_convert(&message)?,
        "sql-to-java-format" => tabs::sql_to_java_format(&message)?,
        "sql-to-java-clear" => tabs::sql_to_java_clear()?,
        // Copy from either tab's result pane (see `tabs::copy_text`).
        "copy-text" => tabs::copy_text(&message)?,
        // "SQL history": a file list, then the statements of the opened file.
        "history-list" => history::list()?,
        "history-open-file" => history::open_file(&message)?,
        "history-back" => history::back()?,
        "history-refresh" => history::refresh()?,
        "history" => history::run(&message)?,
        "history-open" => history::open(&message, ctx)?,
        "history-copy" => history::copy(&message)?,
        "history-jump-java" => history::jump_java(&message)?,
        "history-jump-xml" => history::jump_xml(&message)?,
        "history-preview" => history::preview(&message)?,
        "history-preview-toggle" => history::preview_toggle(&message)?,
        // Row actions (lane L28): jump to the method / to the mapper XML, drop
        // the line, block the method. Each is a no-op when the row does not
        // state the dimension — the page disables those items, the host never
        // guesses a target.
        "jump-java" => logs::jump_java_op(&message)?,
        "jump-xml" => logs::jump_xml_op(&message)?,
        "delete-line" => logs::delete_line(&message)?,
        "block-method" => logs::block_method(&message)?,
        // The reference's row action of the same name: a rule from the row's
        // own text, no dialog round-trip (lane E deferred it; added 2026-11-27
        // when a rule pasted by hand failed to hide its own row).
        "suppress-sql" => logs::suppress_sql(&message)?,
        "clear-blocked" => logs::clear_blocked()?,
        // The eye-off toolbar action (the reference's fifth action,
        // `ManageSuppressAction`) opens the suppression-rule manager (lane E).
        // A dialog is revealed from a command or a view event — this is one.
        "manage-suppress" => logs::manage_suppress(ctx)?,
        // Capture controls (lane L26): the toolbar buttons forward to the
        // existing commands, which write their own Output lines (connected /
        // already connected / the failure reason); the panel only refreshes the
        // state so the status line follows the connection.
        "capture-start" => logs::capture_start(ctx)?,
        "capture-stop" => logs::capture_stop(ctx)?,
        // The page retries a refused capture start every 2 s (lane A).
        "capture-retry" => logs::capture_retry(ctx)?,
        // The page also ticks this while capture is on: the host's
        // `websocket-ready` fires per arriving frame, so a wake lost while the
        // plugin was idle would leave staged frames unread.
        "capture-poll" => logs::capture_poll()?,
        // Follow (lane L26): the page reports a flip when the user scrolls
        // away from the bottom; "scroll to bottom" restores it.
        "follow" => logs::follow(&message)?,
        "scroll-end" => logs::scroll_end()?,
        // Display order (lane A): the page's sort popup posts one of the seven
        // tokens; an unknown or legacy token is ignored rather than guessed.
        "sort" => logs::sort(&message)?,
        // Group expansion (lane L26): the ⨉N / ×N badges toggle their group.
        "toggle-group" => logs::toggle_group_op(&message)?,
        // The platform has no API to open the settings page, so the button
        // points the user there through a notification and the Output channel
        // instead of doing nothing.
        "settings" => logs::settings()?,
        // Help: a one-screen shortcut/interaction cheat sheet in the Output
        // channel.
        "help" => logs::help()?,
        // Community (L32): the toolbar's 交流群 entry, pointed at the project's
        // community page instead of doing nothing.
        "community" => logs::community()?,
        // The preview tab (lane L23): the page prefixes its ops so they cannot
        // collide with this panel's own `clear` / `export` / `copy`; the model
        // and the op handling stay in `views::workbench`, the push is this
        // panel's state.
        _ if op.starts_with("preview") => tabs::preview_message(&message)?,
        _ => {}
    }
    Ok(())
}

pub(super) fn message_line(message: &Value) -> Option<usize> {
    message
        .get("line")
        .and_then(|value| value.as_u64())
        .map(|line| line as usize)
}

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

/// `now` in milliseconds since the Unix epoch (WASI clocks on wasm).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM:SS.mmm` UTC for an epoch timestamp. Logs rarely carry a
/// zone; the panel shows the raw wall clock rather than guessing one.
pub(super) fn format_epoch_ms(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millis:03}")
}

/// Howard Hinnant's civil-from-days (UTC).
pub(super) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

pub(super) fn format_ms(ms: f64) -> String {
    if (ms - ms.round()).abs() < 0.001 {
        format!("{ms:.0}")
    } else {
        format!("{ms:.1}")
    }
}

pub(super) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

// ---------------------------------------------------------------------------
// View trait
// ---------------------------------------------------------------------------

impl Views for crate::Main {
    fn render(
        &mut self,
        view: &str,
        _viewport: Option<openidea_plugin::views::Viewport>,
        _ctx: &Invocation,
    ) -> Result<View, Error> {
        match view {
            VIEW_ID => Ok(render_panel()),
            // Lane L48: the preview dialog. Its page comes from
            // `[[contributes.dialogs]]`, and the host still asks for the view,
            // so the plugin answers with empty chrome (`web/preview.html`
            // builds every control).
            crate::views::workbench::DIALOG_VIEW_ID => Ok(crate::views::workbench::render_dialog()),
            // The suppression manager's page comes from `[[contributes.dialogs]]`
            // too, and the host still asks for the view: a `NotFound` here would
            // leave the dialog blank. Empty chrome, like the preview dialog.
            suppress::VIEW_ID => Ok(View::new(Body::Html(String::new()))),
            other => Err(Error::NotFound(format!("view {other}"))),
        }
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match view {
            VIEW_ID => handle_panel_event(event, ctx),
            // Lane L48: the dialog's own messages and its `closed` event. The
            // workbench owns both; this panel only routes them, because the
            // `Views` impl lives here.
            crate::views::workbench::DIALOG_VIEW_ID => {
                crate::views::workbench::handle_dialog_event(event, ctx)
            }
            // The suppression manager is a declared dialog (lane E): its page's
            // messages and `closed` event arrive here, under its own id.
            suppress::VIEW_ID => suppress::handle_event(event, ctx),
            other => Err(Error::NotFound(format!("view {other}"))),
        }
    }
}
#[cfg(test)]
pub(crate) mod test_support {
    //! Fixtures shared by the panel's per-module test modules (lane W2-0).
    //!
    //! The store and the workbench model are process-wide, so every module's
    //! tests take the same lock and start from the same baseline.

    use openidea_plugin::mock::MockHost;
    use serde_json::Value;

    use crate::core::log::{LogAnalysis, LogEntry, ParseOptions, parse_with_sources};

    use super::*;

    /// The row store and the workbench model are shared by the whole process;
    /// the lock is the one both panels' tests and the `commands` tests take.
    pub(crate) fn test_host() -> (MockHost, super::super::StateLockGuard) {
        let guard = super::super::test_state_lock();
        let host = MockHost::install();
        // One shared reset: the same baseline the `commands` tests use, so a
        // leftover from whichever test held the lock last cannot leak in.
        super::reset_for_tests();
        set_filter(Filter::default());
        set_paused(false);
        // The location indexes are process-wide too (lane L28).
        crate::indexer::clear_indexes();
        // The capture session and its waiting state are process-wide (lane A):
        // a leftover wait would make `status.capture_waiting` non-null.
        crate::capture::bridge::reset_capture_for_tests();
        // The preview tab's model is process-wide too (lane L23).
        crate::views::workbench::clear();
        crate::views::workbench::note_page_tab();
        (host, guard)
    }

    /// Two statements with different kinds, one slow, one error.
    pub(crate) const MIXED_LOG: &str = "\
Preparing: SELECT id FROM users WHERE id = ?
Parameters: 1(Long)
Preparing: DELETE FROM users WHERE id = ?
Parameters: 2(Long)
Preparing: UPDATE account SET balance = ?
Parameters: 3(Long)
java.sql.SQLException: Table 'demo.user' doesn't exist
";

    pub(crate) fn analysis_of(text: &str) -> LogAnalysis {
        // The panel parses with the provenance pass (lane L28); the helpers keep
        // the same pass so a fixture's sources are present too.
        parse_with_sources(text, &ParseOptions::default()).analysis
    }

    pub(crate) fn rows_of(text: &str) -> Vec<PanelRow> {
        build_rows(&analysis_of(text), true, 1000.0)
    }

    pub(crate) fn state_of(host: &MockHost) -> Value {
        let messages = host.view_messages();
        let last = messages.last().expect("a state message").clone();
        serde_json::from_str(&last.1).expect("state is JSON")
    }

    /// The table each pushed row selects from (`FROM <table>`), in display
    /// order — a short, stable stand-in for the whole statement.
    pub(crate) fn row_tables(state: &Value) -> Vec<String> {
        state["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| {
                let sql = row["sql"].as_str().unwrap_or("");
                sql.split_whitespace()
                    .skip_while(|word| !word.eq_ignore_ascii_case("FROM"))
                    .nth(1)
                    .unwrap_or("")
                    .to_string()
            })
            .collect()
    }

    /// Two MyBatis statements under a logback prefix, the second one with a
    /// correlation id: the shape Spring Boot prints and the only place a
    /// class/method can be read from.
    pub(crate) const SOURCED_LOG: &str = "\
2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT id FROM users WHERE id = ?
2026-11-25 21:00:01.002 DEBUG 4242 --- [nio-8080-exec-1] c.e.m.mapper.UserMapper.selectById : ==> Parameters: 1(Long)
2026-11-25 21:00:02.001 DEBUG 4242 --- [nio-8080-exec-2] traceId=abc123 c.e.m.mapper.OrderMapper.findByUser : ==>  Preparing: SELECT id FROM orders WHERE uid = ?
2026-11-25 21:00:02.002 DEBUG 4242 --- [nio-8080-exec-2] traceId=abc123 c.e.m.mapper.OrderMapper.findByUser : ==> Parameters: 1(Long)
";

    pub(crate) const USER_MAPPER_JAVA: &str = "\
package com.example.myapp.mapper;

public interface UserMapper {
    int selectById(Long id);
}
";

    pub(crate) const USER_MAPPER_XML: &str = r#"<mapper namespace="com.example.myapp.mapper.UserMapper">
  <select id="selectById">select 1</select>
</mapper>"#;

    pub(crate) fn row_of_line(state: &Value, line: usize) -> &Value {
        state["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["line"] == line)
            .expect("the row of that line")
    }

    pub(crate) fn first_line_with(entries: &[LogEntry], needle: &str) -> usize {
        entries
            .iter()
            .find(|entry| entry.sql.contains(needle))
            .expect("a statement")
            .line_no
    }

    pub(crate) fn handle_panel_message(host: &MockHost, message: &str) -> Result<(), Error> {
        handle_panel_event(ViewEvent::Message(message.to_string()), &host.invocation())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::log_panel::test_support::{
        MIXED_LOG, SOURCED_LOG, first_line_with, handle_panel_message, row_of_line, state_of,
        test_host,
    };

    /// The packaged page and the preview module it loads. The assertions check
    /// the page's wiring against the state this module pushes.
    const PAGE: &str = include_str!("../../../web/log-panel.html");
    const PREVIEW_JS: &str = include_str!("../../../web/preview.js");
    const PREVIEW_CSS: &str = include_str!("../../../web/preview.css");

    #[test]
    fn ready_message_pushes_rows_and_labels_without_html() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["op"], "state");
        let rows = state["rows"].as_array().expect("rows array");
        assert!(!rows.is_empty());
        assert!(state["labels"]["empty"].as_str().is_some());
        // No pre-rendered HTML leaks into the state.
        let raw = state.to_string();
        assert!(!raw.contains("<div"), "no html markup in state");
        assert!(!raw.contains("<tr"), "no html markup in state");
    }

    #[test]
    fn the_state_carries_the_display_mode() {
        // The page owns the folding and branches on the pushed token, so the
        // host only normalises the setting; every push carries it.
        for (stored, mode) in [
            ("single", "single"),
            ("keyword_wrap", "keyword_wrap"),
            ("formatted", "formatted"),
            // The pre-lane token for "no fold" still resolves to the mode that
            // means the same thing.
            ("multi", "formatted"),
        ] {
            let (host, _guard) = test_host();
            host.set_setting("logs.displayMode", &format!("\"{stored}\""));
            set_log_text(MIXED_LOG);
            handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
            assert_eq!(state_of(&host)["displayMode"], mode, "{stored}");
        }
    }

    #[test]
    fn the_state_carries_the_duration_flag() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(
            state_of(&host)["showDuration"],
            true,
            "the reference default shows the duration"
        );
        host.set_setting("log.showDuration", "false");
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["showDuration"], false);
    }

    #[test]
    fn the_state_carries_the_crud_marker_style() {
        // `logs.crud.*` belongs to the settings lane; this panel only consumes
        // it, so an absent key resolves to the default instead of blocking.
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["crudStyle"], "bar");
        for stored in ["background", "off"] {
            host.set_setting("logs.crud.style", &format!("\"{stored}\""));
            handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
            assert_eq!(state_of(&host)["crudStyle"], stored);
        }
        // An unknown stored value falls back to the bar, never to "no marker".
        host.set_setting("logs.crud.style", "\"weird\"");
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["crudStyle"], "bar");
        let page = PAGE;
        // The three styles are list-level switches; the type colour rides the
        // row's custom property and DDL is drawn as a dash pattern.
        assert!(page.contains("list.dataset.crud = state.crudStyle || 'bar';"));
        assert!(page.contains(".list[data-crud=\"background\"] .row:not(.sel)"));
        assert!(page.contains(".list[data-crud=\"off\"] .row .bar { display: none; }"));
        assert!(page.contains("el.style.setProperty('--row-type'"));
        assert!(page.contains("bar.classList.add('ddl');"));
        assert!(
            page.contains("repeating-linear-gradient("),
            "DDL uses a dashed bar"
        );
    }

    #[test]
    fn the_page_applies_the_display_mode_and_the_duration_flag() {
        let (_host, _guard) = test_host();
        let page = PAGE;
        // Lane L2: the host prepares the display text; the page only colours
        // `row.sql` and appends the muted `row.sqlTail`. There is no second
        // folding implementation in the page.
        assert!(
            page.contains("list.dataset.mode = state.displayMode || 'keyword_wrap';"),
            "the cell padding follows the mode"
        );
        assert!(
            page.contains("highlightInto(sql, row.sql);"),
            "the prepared statement is what gets coloured"
        );
        assert!(
            page.contains("if (row.sqlTail) {"),
            "the host states the hidden-line count"
        );
        assert!(
            !page.contains("function foldSql("),
            "the page must not fold a second time"
        );
        assert!(
            !page.contains("modeLineLimit"),
            "the line cap moved to the host"
        );
        // The duration segment (and its tint) is dropped when the flag is off;
        // the rest of the header stays.
        assert!(page.contains("showDuration = state.showDuration !== false;"));
        // A duration attached on a later push must repaint the row: without it
        // the tier (and the `—`) would stay at whatever the first push drew.
        assert!(page.contains("(r.duration_ms == null ? '' : r.duration_ms)"));
        // The segment is only built when the flag is on, so hiding it cannot
        // leave an empty `.dur` node behind.
        let guard = page
            .find("if (showDuration) {")
            .expect("the duration is conditional");
        assert!(page[guard..].contains("meta.appendChild(dur);"));
        // The settings are part of the list identity, so a change re-renders
        // the rows even when no new row was captured.
        assert!(page.contains("function displayKey(state)"));
        assert!(page.contains("+ '#' + (state.dialect || '') + '#' + displayKey(state);"));
    }

    #[test]
    fn the_packaged_page_draws_its_own_action_row() {
        // A `web =` view gets no host toolbar, so the page draws the reference's
        // toolbar actions itself, in the reference order
        // (`SqlLogPanel.toolbarGroup`): capture, capture-pause, clear, settings,
        // block list, search, quick preview, scroll to end, sort, community,
        // help. Each is an IDE icon button: a 24×24 hit area, an inline SVG, and
        // the label in `title` + `aria-label` instead of visible text.
        let (_host, _guard) = test_host();
        let page = PAGE;
        let bar_start = page.find("class=\"actionbar\"").expect("action row");
        let bar_end = page.find("class=\"filterbar\"").expect("filter row");
        let actionbar = &page[bar_start..bar_end];

        // The reference's `agentPauseAction` sits in the group but its `update()`
        // hides it unconditionally (方案 B), so the port draws no pause button.
        let order = [
            "captureBtn",
            "clearBtn",
            "settingsBtn",
            "blockedBtn",
            "searchBtn",
            "previewBtn",
            "scrollEndBtn",
            "sortBtn",
            "communityBtn",
            "helpBtn",
        ];
        let mut previous = 0;
        for id in order {
            let at = actionbar
                .find(&format!("id=\"{id}\" class=\"iconbtn\""))
                .unwrap_or_else(|| panic!("missing icon button {id}"));
            assert!(at >= previous, "{id} is out of the reference order");
            previous = at;
        }
        assert_eq!(
            actionbar.matches("class=\"iconbtn\"").count(),
            order.len(),
            "the action row holds exactly these buttons: {order:?}"
        );
        assert!(
            !actionbar.contains("class=\"btn\""),
            "the action row has no text buttons"
        );
        assert!(
            !page.contains("id=\"pauseBtn\""),
            "the reference hides its capture-pause action; no pause button"
        );
        // The ops the entries post. The settings gear opens a popup whose items
        // post the L26 `settings` / `help` ops; "copy SQL" is not a toolbar
        // action in the reference (it lives in the row menu).
        for op in [
            "'capture-start'",
            "'capture-stop'",
            "'clear'",
            "'settings'",
            "'clear-blocked'",
            "'open-line'",
            "'scroll-end'",
            "'sort'",
            "'community'",
            "'help'",
        ] {
            assert!(page.contains(op), "missing op {op}");
        }
        // The gear opens the in-panel settings sheet (L2); the old two-item popup
        // is gone, so the entry explains itself in place instead of only
        // writing to the Output channel. The sheet is one overlay reused by the
        // community and help entries too.
        assert!(page.contains("id=\"sheet\""), "missing the in-panel sheet");
        assert!(
            !page.contains("id=\"settingsMenu\""),
            "the gear opens the sheet, not a popup menu"
        );
        for kind in ["settings", "community", "help"] {
            assert!(
                page.contains(&format!("openSheet('{kind}')")),
                "missing sheet {kind}"
            );
        }
        for id in ["sheetBackdrop", "sheetClose", "sheetTitle", "sheetBody"] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // Both icons of the state-dependent capture entry are in the page, and
        // the glyph follows "capture is on" — connected *or* waiting for the
        // target application — so a refused start still flips to the stop glyph
        // instead of looking like the click did nothing.
        for id in ["captureRunIcon", "captureStopIcon"] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        assert!(
            page.contains("status.agent_connected || status.capture_waiting"),
            "the glyph has to answer to the waiting state too"
        );
        // The sort entry is a popup opener (the reference's `sortModeAction`
        // shows a radio menu), so it is a menu anchor with its own menu node.
        assert!(page.contains("id=\"sortMenu\""), "missing sortMenu");
        assert!(page.contains("openSortMenu($('sortMenu').hidden)"));
        // The right edge, in the reference order (`SqlLogPanel.kt`): the slow
        // count, then the SQL-type selector, then the dedupe box.
        let right = actionbar.find("class=\"rightgroup\"").expect("right group");
        let group = &actionbar[right..];
        let slow = group.find("id=\"slowBadge\"").expect("slow count");
        let types = group.find("id=\"typeBtn\"").expect("type selector");
        let dedupe = group.find("id=\"dedupe\"").expect("dedupe box");
        assert!(slow < types, "the slow count comes first");
        assert!(types < dedupe, "the dedupe box comes last");
        // The label binds through `title` and `aria-label`, both fed by the
        // state's `labels`.
        assert!(
            page.contains("node.setAttribute('aria-label', label || '')"),
            "aria-label binding"
        );
        assert!(page.contains("node.title = label || ''"), "title binding");
        let labels = labels_json();
        for key in [
            "clear",
            "captureStart",
            "captureStop",
            "scrollEnd",
            "sortLabel",
            "sortOptions",
            "blocked",
            "settings",
            "menuSettingsPage",
            "help",
            "preview",
            "community",
            "followOn",
            "followOff",
            "groupExpand",
            "groupCollapse",
            // In-panel sheets (L2): settings values, mode/style labels and the
            // three sheet bodies.
            "sheetClose",
            "settingsDisplayMode",
            "settingsShowDuration",
            "settingsSlowThreshold",
            "settingsCrudStyle",
            "settingsPathHint",
            "settingsOn",
            "settingsOff",
            "displayModeSingle",
            "displayModeKeywordWrap",
            "displayModeFormatted",
            "crudStyleBar",
            "crudStyleBackground",
            "crudStyleOff",
            "helpTitle",
            "helpSearch",
            "helpTab",
            "helpDblclick",
            "helpMenu",
            "helpScroll",
            "helpSort",
            "helpBlocked",
            "communityAuthor",
            "communityAuthorDesc",
            "communityGroup",
            "communityGroupDesc",
            "groupBadge",
        ] {
            assert!(labels.get(key).is_some(), "missing label {key}");
        }
        // The seven sort options, in the reference's menu order.
        let options = labels["sortOptions"].as_array().expect("sort options");
        let tokens: Vec<&str> = options
            .iter()
            .map(|option| option["token"].as_str().unwrap())
            .collect();
        assert_eq!(
            tokens,
            [
                "time_asc",
                "time_desc",
                "duration_asc",
                "duration_desc",
                "sequence",
                "structure",
                "database"
            ]
        );
        assert!(
            options.iter().all(|option| option["label"]
                .as_str()
                .is_some_and(|label| !label.is_empty())),
            "every sort option has a label: {options:?}"
        );
    }

    #[test]
    fn the_settings_community_and_help_entries_render_in_panel_sheets() {
        // L2: the three fallback entries now explain themselves inside the
        // panel. The settings sheet lists the values this panel actually
        // consumes (mode, duration flag, slow threshold, CRUD style) and names
        // the settings page; help reuses the cheat-sheet lines; community draws
        // a QR placeholder (no asset ships with the plugin).
        let (_host, _guard) = test_host();
        let page = PAGE;
        for needle in [
            "function openSheet(kind)",
            "renderSettingsSheet(body)",
            "renderHelpSheet(body)",
            "renderCommunitySheet(body)",
            "displayModeLabel(state.displayMode || 'keyword_wrap')",
            "state.showDuration === false",
            "state.slow_threshold_ms",
            "crudStyleLabel(state.crudStyle || 'bar')",
            "L.settingsPathHint",
            "L.helpMenu",
            "L.communityAuthor",
            "AUTHOR_QR_SRC",
            "GROUP_QR_SRC",
            "while (body.firstChild) { body.removeChild(body.firstChild); }",
        ] {
            assert!(page.contains(needle), "missing sheet wiring {needle}");
        }
        // The popped-up two-item settings menu is gone; nothing references it.
        for stale in ["settingsPageItem", "settingsHelpItem", "openSettingsMenu("] {
            assert!(!page.contains(stale), "the old settings popup left {stale}");
        }
        // Wording: the guide names the settings page, the group badge states the
        // reference's phrase, and every sheet label is present.
        use crate::i18n::t_in;
        assert!(t_in("zh-CN", "view.logs.settings.path_hint").contains("SQL 日志"));
        assert!(t_in("en", "view.logs.settings.path_hint").contains("Settings"));
        assert_eq!(t_in("zh-CN", "view.logs.badge.group"), "同结构 {n} 条");
        assert_eq!(
            t_in("zh-CN", "view.logs.badge.n_plus_one_suspect"),
            "疑似 N+1"
        );
        let labels = labels_json();
        for key in [
            "settingsDisplayMode",
            "settingsPathHint",
            "communityAuthor",
            "communityGroupDesc",
        ] {
            assert!(
                labels[key].as_str().is_some_and(|text| !text.is_empty()),
                "empty sheet label {key}"
            );
        }
    }

    #[test]
    fn buffer_cap_truncates_rows_but_keeps_the_total() {
        let (host, _guard) = test_host();
        host.set_setting("logs.pageSize", "2");
        let mut text = String::new();
        for index in 0..10 {
            text.push_str(&format!(
                "==>  Preparing: SELECT c{index} FROM t\n==> Parameters: {index}(Integer)\n"
            ));
        }
        set_log_text(&text);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(state["rows"].as_array().unwrap().len(), 2, "capped to 2");
        assert_eq!(state["counts"]["total"], 10);
        assert_eq!(state["counts"]["truncated"], true);
    }

    #[test]
    fn render_pushes_state_unless_paused() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        let _ = render_panel();
        assert_eq!(host.view_messages().len(), 1, "render pushes the state");
        set_paused(true);
        let _ = render_panel();
        assert_eq!(
            host.view_messages().len(),
            1,
            "render does not push while paused"
        );
    }

    /// The page is the chrome. The host draws `View::toolbar` as a strip above
    /// the body, so a button pushed here shows up *next to* the page's own
    /// action row: a second "clear" and a text "Copy" that the reference does
    /// not have. The page also prints the paused note in its own footer, so the
    /// host status is a duplicate too.
    #[test]
    fn the_view_carries_no_host_drawn_chrome() {
        let (_host, _guard) = test_host();
        let view = render_panel();
        assert!(view.toolbar.is_empty(), "the page owns the action row");
        assert!(view.status.is_none(), "the page owns its footer");
    }

    #[test]
    fn tab_message_is_accepted_without_pushing_state() {
        // Switching tabs is DOM-only: the host never answers `tab`, so the
        // page keeps the log list (and its scroll position) untouched.
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let before = host.view_messages().len();
        handle_panel_message(&host, "{\"op\":\"tab\",\"tab\":\"history\"}").unwrap();
        assert_eq!(host.view_messages().len(), before, "tab pushes nothing");
        handle_panel_message(&host, "{\"op\":\"tab\"}").unwrap();
        assert_eq!(host.view_messages().len(), before);
    }

    #[test]
    fn the_page_has_five_tabs_and_keeps_the_log_list() {
        let (_host, _guard) = test_host();
        let page = PAGE;
        for id in [
            "tab-logs",
            "tab-logToSql",
            "tab-sqlToJava",
            "tab-history",
            "tab-preview",
            "panelLogs",
            "panelLogToSql",
            "panelSqlToJava",
            "panelHistory",
            "panelPreview",
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // The log tab's DOM (list + filters + status) must stay in place.
        for id in ["list", "kw", "typeBtn", "statusLeft", "sortBtn"] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // Switching is `hidden` toggling only, and a state push with unchanged
        // rows must not rebuild the list.
        assert!(
            page.contains("panel.hidden = !on"),
            "tab switch hides panels"
        );
        assert!(page.contains("rowsSig"), "rows are signature guarded");
        assert!(page.contains("event.key === 'Tab'"), "Ctrl+Tab cycling");
        // Tab strip (L32): the new-UI **underline** tabs — every tab is a
        // borderless label on the strip's rule and the selected one wears a
        // 2px accent underline instead of a raised card. A narrow strip still
        // scrolls the active tab into view (the host can switch to the fifth).
        assert!(
            page.contains(".tab.active"),
            "the selected tab has its own state class"
        );
        let active_start = page.find(".tab.active {").expect("the active rule");
        let active_end = active_start + page[active_start..].find('}').expect("the rule ends");
        let active_rule = &page[active_start..active_end];
        assert!(
            active_rule.contains("border-bottom-color: var(--openidea-accent)"),
            "the selected tab wears the accent underline: {active_rule}"
        );
        assert!(
            active_rule.contains("color: var(--openidea-fg-strong)"),
            "the selected label uses the strong foreground: {active_rule}"
        );
        assert!(
            !page.contains("border-radius: 4px 4px 0 0"),
            "the card tabs are gone"
        );
        assert!(
            !page.contains(".tab.sel"),
            "tab selection stays on `.active`"
        );
        assert!(
            page.contains("height: 32px; padding: 0 10px"),
            "32px tabs with 10px side padding"
        );
        assert!(
            page.contains("scrollIntoView({ inline: 'nearest', block: 'nearest' })"),
            "the active tab is scrolled into view"
        );
        // The two input tabs' ops (lane L33): each tab converts its own pane,
        // clears its own result, and copies exactly what the page shows.
        for op in [
            "log-to-sql-convert",
            "log-to-sql-clear",
            "log-to-sql-read-clipboard",
            "sql-to-java-convert",
            "sql-to-java-format",
            "sql-to-java-clear",
            "copy-text",
        ] {
            assert!(page.contains(op), "missing op {op}");
        }
        assert!(page.contains("op: 'history'"), "history op");
        for op in [
            "history-list",
            "history-open-file",
            "history-back",
            "history-refresh",
            "history-copy",
            "history-preview-toggle",
            "history-jump-java",
            "history-jump-xml",
        ] {
            assert!(page.contains(op), "missing history op {op}");
        }
        // The preview tab's ops carry the `preview-` prefix (lane L23), so the
        // log panel can route them without colliding with its own names. Lane
        // L42 moved the workbench into `web/preview.js`, so most of them live
        // there; the panel page adds the cross-view `preview-send` (its quick
        // preview button) on top of its own ops.
        let module = PREVIEW_JS;
        for op in [
            "preview-render",
            "preview-format",
            "preview-load-file",
            "preview-clear",
            "preview-copy",
            "preview-export",
            "preview-params",
            "preview-refresh",
            "preview-rollback",
            // Lane L44: the two entries L42 left to this lane.
            "preview-diff",
            "preview-share",
        ] {
            assert!(module.contains(op), "missing op {op} in the module");
        }
        assert!(
            page.contains("preview-send"),
            "the panel posts the cross-view hand-off"
        );

        let labels = labels_json();
        for key in [
            "tabLogs",
            "tabLogToSql",
            "tabSqlToJava",
            "tabHistory",
            "tabPreview",
            "tabLogToSqlEmpty",
            "tabSqlToJavaEmpty",
            "tabHistoryEmpty",
            "tabLogToSqlConvert",
            "tabLogToSqlReadClipboard",
            "tabLogToSqlToggleFormat",
            "tabLogToSqlNoEntry",
            "tabLogToSqlCopied",
            "tabSqlToJavaConvert",
            "tabSqlToJavaFormat",
            "tabSqlToJavaFormatLabel",
            "tabSqlToJavaEntityHint",
            "tabSqlToJavaFixedLambda",
            "tabSqlToJavaCopied",
        ] {
            assert!(labels.get(key).is_some(), "missing label {key}");
        }
    }

    #[test]
    fn the_two_input_tabs_have_two_editors_and_a_status_line() {
        // Lane L33: the reference's LogToSqlPanel / SqlToJavaPanel are two
        // editor panes stacked with a splitter between them and one status
        // line under both; each pane carries its own toolbar.
        let (_host, _guard) = test_host();
        let page = PAGE;

        // 日志 → SQL: input pane (convert / clipboard / clear) over the result
        // pane (copy / single↔multi line / clear).
        for id in [
            "l2sInput",
            "l2sOutput",
            "l2sConvert",
            "l2sReadClipboard",
            "l2sClearInput",
            "l2sCopy",
            "l2sToggleFormat",
            "l2sClearResult",
            "l2sStatus",
            "l2sSplitter",
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // SQL → Java: the same two-pane shape, plus its option row (wrapper
        // form, entity, caller, result type) as a row of its own.
        for id in [
            "s2jInput",
            "s2jOutput",
            "s2jConvert",
            "s2jFormatSql",
            "s2jClearInput",
            "s2jCopy",
            "s2jClearResult",
            "s2jStatus",
            "s2jSplitter",
            "s2jFormatMode",
            "s2jEntity",
            "s2jCaller",
            "s2jReturnType",
        ] {
            assert!(page.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // The option row offers both wrapper forms.
        assert!(page.contains("value=\"lambda\""));
        assert!(page.contains("value=\"stringColumns\""));
        // Its layout is the reference's GridBag shape: the form combo and the
        // entity field span the row, caller and result type sit half each, and
        // the labels live in the placeholders rather than eating width.
        assert!(page.contains("class=\"optselect optwide\""));
        assert!(page.contains("class=\"optinput optfull\""));
        assert_eq!(page.matches("class=\"optinput opthalf\"").count(), 2);
        assert!(page.contains(
            "#panelSqlToJava .optwide,\n    #panelSqlToJava .optfull { grid-column: 1 / -1;"
        ));
        assert!(page.contains("#panelSqlToJava .optionsrow .optlabel { display: none; }"));

        // Red / grey status line: one class, toggled by `statusKind`.
        assert!(
            page.contains(".statusline.error"),
            "the error state has a rule"
        );
        assert!(
            page.contains("node.classList.toggle('error', tab.statusKind === 'error')"),
            "the status line follows statusKind"
        );

        // 压缩↔多行 is a pure display switch: its handler must not post.
        let start = page
            .find("$('l2sToggleFormat').addEventListener")
            .expect("the toggle handler");
        let end = start + page[start..].find("});").expect("the handler ends");
        let handler = &page[start..end];
        assert!(
            !handler.contains("post("),
            "the single/multi-line toggle never asks the host: {handler}"
        );
        assert!(
            handler.contains("renderLogOutput"),
            "it re-renders from the pushed output: {handler}"
        );
        assert!(
            page.contains("function compactSql("),
            "the compact form is derived on the page"
        );

        // "发送到 …" reaches the input panes (L32 hand-off), not just the tab.
        assert!(
            page.contains("$('l2sInput').value = sendText"),
            "the log hand-off fills the log input pane"
        );
        assert!(
            page.contains("$('s2jInput').value = sendText"),
            "the SQL hand-off fills the SQL input pane"
        );
    }

    #[test]
    fn the_log_list_follows_the_jblist_row_style() {
        // L2: the rows read like a JBList cell — a 3px type bar, a muted
        // metadata line, the statement, and a 1px rule between rows. The
        // reference has no alternating-row fill and no hover plate; only the
        // selected row is painted.
        let (_host, _guard) = test_host();
        let page = PAGE;
        for needle in [
            "min-height: 40px",
            ".row.sel { background: var(--openidea-bg-selected); }",
            ".row .bar {",
            "width: 3px; flex: 0 0 3px;",
            "background: var(--row-type, var(--openidea-fg-muted));",
            "padding: 12px 9px 12px 5px;",
            ".list[data-mode=\"single\"] .row .content { padding-top: 8px; padding-bottom: 8px; }",
            "list.dataset.mode = state.displayMode || 'keyword_wrap';",
        ] {
            assert!(page.contains(needle), "missing row style {needle}");
        }
        assert!(
            !page.contains(".row:hover"),
            "the reference list has no hover plate"
        );
        // Metadata is 11px muted with monospace sequence/time/duration; the
        // statement is the IDE's own UI font (the reference inherits the JBList
        // font, not the editor font — `--openidea-font-mono` falls back to
        // Courier New in a frame, which read badly) and wraps.
        for needle in [
            "align-items: center; justify-content: flex-start;",
            "gap: 8px; font-size: 11px;",
            ".meta .time, .meta .dur { font-family: var(--openidea-font-mono, monospace); }",
            "font-family: var(--openidea-font-ui, system-ui, sans-serif);",
            "font-size: var(--openidea-font-size, 13px); line-height: 1.45;",
            "white-space: pre-wrap; word-break: break-word;",
            "margin-top: 3px;",
        ] {
            assert!(page.contains(needle), "missing text style {needle}");
        }
        // The statement fragment is tokenised with the shared highlighter, and
        // a selected row projects every fragment onto the selection foreground
        // (the reference drops syntax colours while a row is selected).
        assert!(
            page.contains(
                "import { into as highlightInto, installStyles } from './sql-highlight.js';"
            ),
            "the list rows use the shared lexer"
        );
        assert!(
            page.contains("installStyles();"),
            "the token classes are installed"
        );
        assert!(
            page.contains("highlightInto(sql, row.sql);"),
            "the SQL is coloured"
        );
        assert!(
            page.contains(".row.sel .content span { color: inherit; }"),
            "selection hides the syntax colours"
        );
        // Badges are 8px-radius 10px capsules tinted 16% with the semantic
        // host variable; the slow duration and the error line take the
        // warning/danger ink.
        assert!(
            page.contains("border-radius: 8px; font-size: 10px;"),
            "capsule badges"
        );
        for tint in ["fg-muted", "warning", "danger"] {
            let rule = format!("color-mix(in srgb, var(--openidea-{tint}) 16%, transparent)");
            assert!(page.contains(&rule), "missing {tint} tint");
        }
        // Duration ink has four tiers, keyed off `log.slowThresholdMs`: red at the
        // threshold, orange at half of it, green below, grey when unknown.
        for tier in ["slow", "warn", "ok", "none"] {
            assert!(
                page.contains(&format!(".meta .dur.{tier} {{ color:")),
                "missing duration tier {tier}"
            );
        }
        assert!(
            page.contains("function durationTier(ms, threshold)"),
            "the tiers are decided once"
        );
        assert!(
            !page.contains(".meta .dur.hot"),
            "the old one-tier tint is gone"
        );
        // The database badge, an MCP row and the affected-row count carry the
        // reference's fragment inks, all through host variables.
        for rule in [
            ".meta .db { color: var(--openidea-info); }",
            ".meta .mcp { color: color-mix(in srgb, var(--openidea-link) 50%, var(--openidea-danger)); }",
            ".meta .rows { color: color-mix(in srgb, var(--openidea-link) 55%, var(--openidea-fg-muted)); }",
        ] {
            assert!(page.contains(rule), "missing meta ink {rule}");
        }
        assert!(
            page.contains("err.textContent = '⛔ ' + row.error_text;"),
            "a failed statement carries the warning prefix"
        );
        assert!(
            page.contains("id=\"slowBadge\" class=\"pill danger\""),
            "the slow count is a right-side capsule"
        );
    }

    #[test]
    fn the_row_metadata_is_a_left_aligned_flow() {
        // The reference row header is one left-to-right flow:
        // #seq time duration [db] (rows) ⨉N ×N N+1 [file:line] [Class#method].
        // The duration is *not* a right-aligned corner badge; the only
        // right-aligned element of the panel is the slow capsule.
        let (_host, _guard) = test_host();
        let page = PAGE;
        let meta_start = page.find("    .meta {").expect("the meta rule");
        let meta_end = page.find(".meta .seq").expect("the next meta rule");
        let meta = &page[meta_start..meta_end];
        assert!(meta.contains("justify-content: flex-start;"), "{meta}");
        assert!(!meta.contains("space-between"), "{meta}");
        assert!(!meta.contains("margin-left: auto"), "{meta}");
        assert!(
            !page.contains(".meta .dur { margin-left"),
            "the duration stays in the flow"
        );
        // The slow count is no longer pushed right inside a filter row: it is the
        // first control of the toolbar's right edge (lane L32).
        assert!(
            !page.contains("#slowBadge { margin-left: auto; }"),
            "the slow count lives in the toolbar's right group"
        );

        let build_start = page
            .find("function buildRow(row, slowThreshold, isMember, dialect)")
            .unwrap();
        let build_end = page.find("function formatMs(ms)").unwrap();
        let build = &page[build_start..build_end];
        let seq = build.find("makePart('seq'").expect("seq");
        let time = build.find("makePart('part time'").expect("time");
        let dur = build.find("dur.className = 'dur'").expect("duration");
        let badge = build.find("makePart('badge dedupe'").expect("a badge");
        let source = build.find("makePart('part src'").expect("source");
        let method = build
            .find("makePart('part method focusable'")
            .expect("method");
        assert!(seq < time, "seq before time");
        assert!(time < dur, "duration right after the time");
        assert!(dur < badge, "badges after the duration");
        assert!(badge < source, "the source comes after the badges");
        assert!(source < method, "[file:line] before [Class#method]");
    }

    #[test]
    fn the_row_header_matches_the_reference_shape() {
        // `SqlEntryPresentation.headerFragments` (lane L32): the sequence bare,
        // the timestamp bracketed, the duration with its unit, then the database
        // badge; the statement folds to three lines with `… (+N)` on the third.
        let (_host, _guard) = test_host();
        let page = PAGE;
        assert!(
            page.contains("makePart('seq', '#' + row.seq)"),
            "the sequence is bare"
        );
        assert!(
            page.contains("makePart('part time', '[' + (row.time || '-') + ']')"),
            "the timestamp is bracketed"
        );
        // `12ms`: the unit comes from the locale, and an unknown duration draws
        // the reference's em dash.
        assert!(
            page.contains("formatMs(row.duration_ms) + (L.durationUnit || 'ms')"),
            "the duration carries its unit"
        );
        assert!(page.contains("? '—'"), "unknown duration is an em dash");
        // The database badge states the configured engine and appends the
        // database only when the row carries one — never a guessed connection.
        assert!(page.contains("var dbText = dbBadge(row.db, dialect);"));
        assert!(
            page.contains("var name = db || '';"),
            "no invented database"
        );
        // Folding: three clause lines with a muted `… (+N)` tail, prepared by the
        // host (`rows::fold_keyword_wrap`); the page renders the pair as-is.
        assert!(
            page.contains("if (row.sqlTail) {"),
            "the page appends the host's tail"
        );
        assert_eq!(
            super::rows::fold_keyword_wrap("a\nb\nc\nd\ne"),
            ("a\nb\nc".to_string(), " … (+2)".to_string()),
            "three lines and the hidden count"
        );
        assert!(
            page.contains("sql.title = row.sql;"),
            "the prepared statement stays in the tooltip"
        );
        assert!(
            page.contains(".sql .sqlTail { color: var(--openidea-fg-muted); }"),
            "the tail is muted"
        );
        // `(N rows)`, the reference's affected-row fragment.
        assert!(page.contains("'(' + row.rows_affected + ' ' + (L.rowsSuffix || 'rows') + ')'"));
    }

    #[test]
    fn the_status_line_uses_the_reference_wording() {
        // `CaptureStatusBar` (lane L32): the left segment is the console channel
        // (`控制台：已扫描 N 行`) and the right one the enhanced channel
        // (`增强：未开启` while it is off). The attached-console count that the
        // reference prints first is not available to this build, so it is not
        // printed rather than guessed.
        let (_host, _guard) = test_host();
        use crate::i18n::t_in;
        assert_eq!(
            t_in("zh-CN", "view.logs.status.console_prefix"),
            "控制台：已扫描"
        );
        assert_eq!(
            t_in("en", "view.logs.status.console_prefix"),
            "Console: scanned"
        );
        assert_eq!(t_in("zh-CN", "view.logs.status.agent_off"), "增强：未开启");
        assert_eq!(
            t_in("en", "view.logs.status.agent_events_prefix"),
            "Enhanced: received"
        );
        let labels = labels_json();
        for key in [
            "statusConsolePrefix",
            "statusConsoleSuffix",
            "statusAgentOff",
            "statusAgentEventsPrefix",
            "statusAgentGate",
            "statusAgentNoHooks",
            "statusAgentIdle",
        ] {
            assert!(labels.get(key).is_some(), "missing label {key}");
        }
        let page = PAGE;
        assert!(
            page.contains("(L.statusConsolePrefix || '') + ' ' + (s.console_lines || 0)"),
            "the console segment is prefix + scanned count"
        );
    }

    #[test]
    fn the_row_header_shows_the_class_and_method_and_filters_on_click() {
        let (_host, _guard) = test_host();
        let page = PAGE;
        // The chip container sits in the filter row, before the slow capsule.
        assert!(
            page.contains("id=\"focusChips\" class=\"chips\""),
            "the chip container is in the filter row"
        );
        // The row header draws [Class#method] from class_simple + method, and a
        // row that states none draws nothing.
        assert!(page.contains("row.class_simple && row.method"));
        assert!(page.contains("'[' + row.class_simple + '#' + row.method + ']'"));
        assert!(page.contains("setFocus('method', row.focus_key)"));
        assert!(page.contains("setFocus('request', row.request_id)"));
        // A chip carries an × and rides the same `filter` op.
        assert!(page.contains("close.className = 'fclose'"));
        assert!(page.contains("close.textContent = '×'"));
        assert!(page.contains("function clearFocus()"));
        assert!(
            page.contains("focus: ui.focus"),
            "the chip rides the filter op"
        );

        let labels = labels_json();
        for key in [
            "menuJumpJava",
            "menuJumpXml",
            "menuFocusMethod",
            "menuFocusRequest",
            "menuDelete",
            "menuBlockMethod",
            "menuSuppress",
            "focusMethodPrefix",
            "focusRequestPrefix",
            "focusClear",
            "blockedPrefix",
            "blockedClear",
        ] {
            assert!(labels.get(key).is_some(), "missing label {key}");
        }
    }

    #[test]
    fn the_context_menu_offers_the_row_actions_and_disables_what_the_row_lacks() {
        let (_host, _guard) = test_host();
        let page = PAGE;
        for act in [
            "jump-java",
            "jump-xml",
            "focus-method",
            "focus-request",
            "focus-template",
            "delete-line",
            "block-method",
            "suppress-sql",
            "copy-sql",
            "copy-time",
            "send-sql-to-java",
            "send-log-to-sql",
            "export",
        ] {
            assert!(
                page.contains(&format!("data-act=\"{act}\"")),
                "missing menu item {act}"
            );
        }
        // The reference's twelve row actions in its order
        // (`SqlLogPanelActions.showContextMenu`), plus this port's
        // mute-method item (the `blocked` feature has no reference row action).
        let menu_start = page.find("id=\"ctxMenu\"").expect("context menu");
        let menu_end = menu_start + page[menu_start..].find("</div>").expect("menu ends");
        let menu = &page[menu_start..menu_end];
        let mut previous = 0;
        for act in [
            "copy-sql",
            "copy-time",
            "delete-line",
            "block-method",
            "suppress-sql",
            "send-sql-to-java",
            "send-log-to-sql",
            "jump-java",
            "jump-xml",
            "focus-method",
            "focus-request",
            "focus-template",
            "export",
        ] {
            let at = menu
                .find(&format!("data-act=\"{act}\""))
                .unwrap_or_else(|| panic!("missing {act}"));
            assert!(at >= previous, "{act} is out of the reference order");
            previous = at;
        }
        assert_eq!(
            menu.matches("<button").count(),
            13,
            "twelve row actions + mute"
        );
        // An item is disabled exactly when the row does not state the dimension.
        assert!(page.contains("enableItem('jump-java', !!(row && row.jump_file))"));
        assert!(page.contains("enableItem('jump-xml', !!(row && row.xml_file))"));
        assert!(page.contains("enableItem('focus-method', !!(row && row.focus_key))"));
        assert!(page.contains("enableItem('focus-request', !!(row && row.request_id))"));
        assert!(page.contains("enableItem('block-method', !!(row && row.focus_key))"));
        assert!(page.contains("enableItem('suppress-sql', !!(row && row.sql))"));
        assert!(page.contains(".ctxmenu button[disabled]"));
        // The host ops the items post.
        for op in [
            "jump-java",
            "jump-xml",
            "delete-line",
            "block-method",
            "suppress-sql",
        ] {
            assert!(page.contains(&format!("op: '{op}'")), "missing op {op}");
        }
    }

    #[test]
    fn the_log_page_uses_no_literal_colours() {
        // Everything visual rides on the host's `--openidea-*` variables, so a
        // theme switch recolours the panel without touching the page.
        let (_host, _guard) = test_host();
        let page = PAGE;
        for needle in ["#6C707E", "#DB3B4B", "#208A3C", "#F2FCF3", "rgb(", "hsl("] {
            assert!(!page.contains(needle), "literal colour {needle}");
        }
    }

    #[test]
    fn the_preview_tab_carries_the_workbench_ui() {
        let (_host, _guard) = test_host();
        let page = PAGE;
        // Lane L42: the workbench is one module (`web/preview.js` plus its
        // `web/preview.css`) mounted on the fifth tab, so the DOM assertions
        // follow it there and the panel page keeps only the mount host.
        let module = PREVIEW_JS;
        let css = PREVIEW_CSS;
        assert!(
            page.contains("id=\"panelPreview\""),
            "the fifth tab keeps its mount host"
        );
        assert!(
            page.contains("import { mountPreview } from './preview.js'"),
            "the panel page imports the module"
        );
        assert!(
            page.contains("mountPreview($('panelPreview'), { post: post })"),
            "the module mounts on the fifth tab"
        );
        assert!(
            page.contains("href=\"./preview.css\""),
            "the panel page loads the shared stylesheet"
        );
        // The input pane: statement box plus the four actions the IntelliJ
        // panel's top half has.
        for id in [
            "pvInput",
            "pvResolve",
            "pvFormat",
            "pvLoadFile",
            "pvClear",
            "pvOpenDialog",
        ] {
            assert!(module.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        // The preview pane (lane L35b follows the reference order): meta row,
        // then Copy SQL / Format / Display mode / Explain / Refresh with Export
        // and Rollback behind "More", then the degradation strip, the two
        // columns and the status-only footer.
        for id in [
            "pvDialectChip",
            "pvCopy",
            "pvFormatPreview",
            "pvMode",
            "pvExplain",
            "pvRefresh",
            "pvMore",
            "pvMoreMenu",
            "pvExport",
            "pvDiff",
            "pvShare",
            "pvQualify",
            "pvRollback",
            "pvBanner",
            "pvBannerText",
            "pvBannerClose",
            "pvParamsList",
            "pvTabActions",
            "pvTabExecute",
            "pvTabBody",
            "pvStatus",
        ] {
            assert!(module.contains(&format!("id=\"{id}\"")), "missing {id}");
        }
        for tab in ["sql", "raw", "columns"] {
            assert!(
                module.contains(&format!("data-pvtab=\"{tab}\"")),
                "missing inner tab {tab}"
            );
        }
        // Lane L27: the parameter column is editable, rebinds through the host
        // and imports a comma/newline separated list from the clipboard; when
        // the clipboard API is absent the import button is greyed with the
        // reason, and the rollback entry explains why it is disabled.
        assert!(css.contains("pv-param-input"), "editable parameter inputs");
        assert!(module.contains("op: 'preview-params'"), "the rebind op");
        assert!(
            module.contains("navigator.clipboard.readText()"),
            "the clipboard read"
        );
        // The import entry is the "more" menu item: with no parameters (or no
        // clipboard API) it is greyed and its tooltip says why.
        assert!(module.contains("moreImport.disabled"), "the degrade branch");
        assert!(
            module.contains("split(/[\\r\\n,]+/)"),
            "comma/newline parsing"
        );
        assert!(module.contains("op: 'preview-rollback'"), "the rollback op");
        assert!(module.contains("labels.rollbackNote"), "the rollback note");
        // Lane L44: Diff and Share post their ops; Qualify stays disabled with
        // the renderer reason in its tooltip (no SQL rewriting on this side).
        assert!(module.contains("op: 'preview-diff'"), "the diff op");
        assert!(module.contains("op: 'preview-share'"), "the share op");
        assert!(module.contains("labels.qualifyNote"), "the qualify note");
        assert!(
            module.contains("qualifyBtn.disabled = true"),
            "Qualify stays disabled: the renderer owns the flag"
        );
        assert!(
            !module.contains("op: 'preview-qualify'"),
            "the page never asks for qualify, so the entry cannot look usable"
        );
        // Explain stays disabled with the reason in its tooltip; the page has
        // no external link, no storage and builds no markup from data.
        assert!(
            module.contains("pvSig"),
            "the tab body is signature guarded"
        );
        assert!(module.contains("id=\"pvExplain\" class=\"btn\" type=\"button\" disabled"));
        assert!(
            module.contains("id=\"pvBanner\" role=\"status\" hidden"),
            "the degradation strip starts hidden"
        );
        assert!(
            module.contains("buttons[i].getAttribute('data-pvtab')"),
            "the three result tabs stay module-side"
        );
        assert!(
            page.contains("openidea.postMessage"),
            "the page talks to the plugin only"
        );
        for (what, text) in [
            ("panel page", page),
            ("preview module", module),
            ("preview stylesheet", css),
        ] {
            for needle in [
                "http://",
                "https://",
                "localStorage",
                "sessionStorage",
                "fetch(",
                "XMLHttpRequest",
                "innerHTML",
                "outerHTML",
                "insertAdjacentHTML",
                "eval(",
            ] {
                assert!(!text.contains(needle), "{what} must not contain {needle}");
            }
        }
    }

    #[test]
    fn the_select_tab_request_survives_until_the_page_reports_its_tab() {
        let (host, _guard) = test_host();
        crate::views::workbench::request_tab();
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["selectTab"], "preview");
        // Every push keeps the request until the page confirms.
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(state_of(&host)["selectTab"], "preview");
        handle_panel_message(&host, "{\"op\":\"tab\",\"tab\":\"preview\"}").unwrap();
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert!(
            state_of(&host)["selectTab"].is_null(),
            "the page confirmed its tab, so the request is done"
        );
    }

    #[test]
    fn a_tab_report_formats_a_handed_off_statement() {
        // Lane L34: `open-workbench` / `open-line` / `preview-sql` set the
        // model and ask for the preview tab; the page's `tab` report is where
        // the hand-off lands, and `render.formatOnPreview` (default on)
        // formats it before the push.
        let (host, _guard) = test_host();
        let input = "select id from t where a = 1";
        crate::views::workbench::set_model(crate::views::workbench::WorkbenchModel::new(
            input,
            Vec::new(),
            "MySQL",
        ));
        crate::views::workbench::request_input_replace();
        handle_panel_message(&host, "{\"op\":\"tab\",\"tab\":\"preview\"}").unwrap();
        let preview = &state_of(&host)["tabs"]["preview"];
        let sql = preview["model"]["sql"].as_str().expect("the model sql");
        assert_ne!(sql, input, "the handed-off statement is formatted");
        assert!(sql.to_uppercase().contains("SELECT"), "{sql}");
        assert_eq!(
            preview["replaceInput"], true,
            "the input box is rewritten with the formatted statement too"
        );
    }

    #[test]
    fn a_tab_report_keeps_the_handed_off_statement_when_formatting_is_off() {
        let (host, _guard) = test_host();
        host.set_setting("render.formatOnPreview", "false");
        let input = "  select id\nfrom t where a=1  ";
        crate::views::workbench::set_model(crate::views::workbench::WorkbenchModel::new(
            input,
            Vec::new(),
            "MySQL",
        ));
        crate::views::workbench::request_input_replace();
        handle_panel_message(&host, "{\"op\":\"tab\",\"tab\":\"preview\"}").unwrap();
        // Nothing to format means no extra push; the model is read directly and
        // the next state (a `ready`, a filter, a capture refresh) carries it.
        assert_eq!(
            crate::views::workbench::model().sql,
            input,
            "with the setting off the statement is byte-identical"
        );
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        assert_eq!(
            state_of(&host)["tabs"]["preview"]["model"]["sql"],
            input,
            "the pushed state keeps every byte too"
        );
    }

    #[test]
    fn the_bare_ops_of_the_log_panel_do_not_touch_the_preview() {
        let (host, _guard) = test_host();
        set_log_text(MIXED_LOG);
        crate::views::workbench::set_model(crate::views::workbench::WorkbenchModel::new(
            "select keep",
            Vec::new(),
            "MySQL",
        ));
        handle_panel_message(
            &host,
            "{\"op\":\"preview-render\",\"text\":\"select keep\"}",
        )
        .unwrap();
        // `clear` empties the log buffer; the preview model stays.
        handle_panel_message(&host, "{\"op\":\"clear\"}").unwrap();
        assert_eq!(crate::views::workbench::model().sql, "select keep");
        assert_eq!(
            state_of(&host)["tabs"]["preview"]["model"]["sql"],
            "select keep"
        );
    }

    #[test]
    fn the_state_rows_carry_the_logger_source_thread_and_request() {
        let (host, _guard) = test_host();
        set_log_text(SOURCED_LOG);
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);

        let line = first_line_with(&snapshot().entries, "FROM users");
        let row = row_of_line(&state, line);
        assert_eq!(row["class_simple"], "UserMapper");
        assert_eq!(row["method"], "selectById");
        assert_eq!(row["class_name"], "c.e.m.mapper.UserMapper");
        assert_eq!(row["focus_key"], "c.e.m.mapper.UserMapper#selectById");
        assert_eq!(row["thread"], "nio-8080-exec-1");
        assert_eq!(row["request_id"], "", "this row states no id");

        let line = first_line_with(&snapshot().entries, "FROM orders");
        let row = row_of_line(&state, line);
        assert_eq!(row["class_simple"], "OrderMapper");
        assert_eq!(row["request_id"], "abc123");
    }

    #[test]
    fn a_five_thousand_row_push_stays_within_the_budget() {
        let (host, _guard) = test_host();
        host.set_setting("logs.pageSize", "5000");
        let mut text = String::new();
        for index in 0..5000 {
            text.push_str(&format!(
                "2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] \
                 c.e.m.mapper.UserMapper.selectById : ==>  Preparing: SELECT c{index} FROM t WHERE id = ?\n"
            ));
            text.push_str(&format!(
                "2026-11-25 21:00:01.001 DEBUG 4242 --- [nio-8080-exec-1] \
                 c.e.m.mapper.UserMapper.selectById : ==> Parameters: {index}(Integer)\n"
            ));
        }
        set_log_text(&text);
        // Warm the parse once (a real session parses on the first push), then
        // measure a full push of 5k rows: parse + rows + focus + index queries
        // + JSON. The index is empty here, which is the common case; a filled
        // index only replaces empty hash lookups with small ones.
        let _ = push_state();
        // Three pushes, keep the fastest: the full suite runs tests in parallel,
        // and a single reading on a loaded machine can cross the bound without
        // the work itself getting slower.
        let mut best = std::time::Duration::MAX;
        let mut state = Value::Null;
        for _ in 0..3 {
            let started = std::time::Instant::now();
            state = build_state();
            best = best.min(started.elapsed());
        }
        println!("5k-row push (best of 3): {best:?}");
        assert_eq!(state["rows"].as_array().unwrap().len(), 5000);
        // Measured ~180 ms for 5000 shown rows in a debug build (the shipped
        // wasm is optimised); with the IDE open and the suite running in
        // parallel the best of three sits at ~400 ms, so the bound is 800 ms.
        // It still catches a quadratic regression (seconds), which is what this
        // guards; the default cap is 50 rows, two orders of magnitude cheaper.
        assert!(
            best < std::time::Duration::from_millis(800),
            "5k-row push took {best:?}"
        );
    }

    #[test]
    fn the_page_draws_the_l26_actions_and_the_interaction_state() {
        let (_host, _guard) = test_host();
        let page = PAGE;
        // The two pause concepts stay separate: refresh pause (existing
        // `paused`) and follow pause (`atBottom` / `follow`).
        assert!(page.contains("scrollTop + list.clientHeight >= list.scrollHeight - 4"));
        assert!(page.contains("var follow = true;"));
        assert!(page.contains("post({ op: 'follow', follow: false })"));
        assert!(page.contains("post({ op: 'scroll-end' })"));
        // The sort entry is a radio popup over the host's seven modes (lane A),
        // not the old two-way toggle: the popup is rebuilt from
        // `labels.sortOptions`, marks the current mode, and posts the token.
        assert!(page.contains("var sortMode = 'time_asc';"));
        assert!(page.contains("var mode = state.sort || 'time_asc';"));
        assert!(page.contains("function renderSortMenu()"));
        assert!(page.contains("L.sortOptions"));
        assert!(page.contains("item.setAttribute('data-sort', option.token)"));
        assert!(page.contains("post({ op: 'sort', sort: sortMode })"));
        assert!(page.contains("L.sortLabel"));
        // Icon-only (the reference's toolbar): no visible label next to the
        // icon; the menu carries the check mark and the tooltip names the
        // active option.
        assert!(!page.contains("sortBtnText"));
        // The eye-off toolbar action opens the suppression manager (lane E) —
        // it used to drop a muted-methods popup, replaced by the dialog.
        assert!(page.contains("id=\"blockedBtn\""));
        assert!(page.contains("post({ op: 'manage-suppress' })"));
        assert!(!page.contains("id=\"blockedMenu\""), "the popup is gone");
        // The muted-methods chip still offers a restore-all action.
        assert!(page.contains("post({ op: 'clear-blocked' })"));
        // The group badges are clickable and post toggle-group with the kind.
        assert!(page.contains("badgeButton(dedupeText, 'dedupe', dedupeTitle"));
        assert!(page.contains("badgeButton(groupText, 'group', groupTitle"));
        assert!(page.contains("kind: 'dedupe'"));
        assert!(page.contains("kind: 'group'"));
        assert!(page.contains("content.classList.add('member')"));
        // The capture button is one icon button whose glyph follows the state:
        // "capture is on" is connected *or* waiting for the target application,
        // here and in `updateLabels` alike.
        assert!(page.contains("post({ op: active ? 'capture-stop' : 'capture-start' })"));
        assert!(page.contains("var active = !!(s.agent_connected || s.capture_waiting);"));
        assert!(page.contains("setHidden($('captureRunIcon'), active)"));
        // `SVGElement` has no `hidden` IDL property: `node.hidden = true` sets an
        // expando and leaves the attribute (and the glyph) alone, which is why
        // the entry sat on the play triangle forever. The page must toggle the
        // attribute, never assign the property.
        assert!(page.contains("node.setAttribute('hidden', '')"));
        assert!(
            !page.contains("Icon').hidden"),
            "an SVG's `hidden` is not a property: toggle the attribute"
        );
        // Lane A: a refused capture start waits for the target application and
        // the page retries every 2 s (`on-schedule` has a 60 s floor).
        assert!(page.contains("function syncCaptureRetry(state)"));
        assert!(page.contains("post({ op: 'capture-retry' })"));
        assert!(page.contains("}, 2000);"));
        assert!(page.contains("captureRetryTimer"));
        assert!(page.contains("s.capture_waiting"));
        // The drain tick: the host's `websocket-ready` is one-shot per arriving
        // frame, so a lost wake would leave frames staged forever. The page ticks
        // a drain while the enhanced channel is on.
        assert!(page.contains("post({ op: 'capture-poll' })"));
        assert!(page.contains("}, 500);"));
        assert!(page.contains("syncCapturePoll(state)"));
        // Settings and help have real feedback paths, not dead buttons.
        assert!(page.contains("post({ op: 'settings' })"));
        assert!(page.contains("post({ op: 'help' })"));
        // The status line names the follow state.
        assert!(page.contains("right = L.followOff || ''"));
        // The scroll-to-bottom tooltip names the follow state too.
        assert!(page.contains("$('scrollEndBtn').title = follow ? (L.followOn"));
    }

    /// 命中针的那一行必须是**活代码**，而不是注释。
    ///
    /// 直接 `contains` 会把 `// post({ op: 'toggle-group', … })` 也算命中：
    /// 把接线注释掉、测试照样绿，等于没拦（反向验证时就踩到了这个假绿）。
    /// 这里要求命中行本身不是注释行，且针之前没有 `//`、`<!--`。
    fn live_line(source: &str, needle: &str) -> bool {
        source.lines().any(|line| {
            let Some(at) = line.find(needle) else {
                return false;
            };
            let head = &line[..at];
            !head.contains("//") && !head.contains("<!--") && !line.trim_start().starts_with('*')
        })
    }

    /// 分发匹配臂的写法。针在运行时拼出来，不会以字面量出现在
    /// `log_panel.rs` 源码里，`live_line(source, …)` 才不会自己命中自己。
    fn dispatch_arm(op: &str) -> String {
        format!("\"{op}\" =>")
    }

    #[test]
    fn b4_the_group_badges_stay_wired_to_toggle_group() {
        // B4：`×N` 同结构徽章点了要能展开/折叠。拦的是「删掉徽章的 click 回调
        // 或它的 post」——删了之后徽章还在、看着能点，用户点下去毫无反应，
        // 而后端 `toggle-group` 的测试照样全绿。
        let (_host, _guard) = test_host();
        let page = PAGE;

        // 两个徽章各自把一个 `post({ op: 'toggle-group', … })` 交给
        // `badgeButton` 当 click 回调；回调与 post 两行都要在，缺一行就断开。
        for (badge, kind) in [
            (
                "badgeButton(dedupeText, 'dedupe', dedupeTitle, function () {",
                "dedupe",
            ),
            (
                "badgeButton(groupText, 'group', groupTitle, function () {",
                "group",
            ),
        ] {
            assert!(
                live_line(page, badge),
                "the {kind} badge lost its click callback"
            );
            assert!(
                live_line(
                    page,
                    &format!("post({{ op: 'toggle-group', line: row.line, kind: '{kind}' }});")
                ),
                "the {kind} badge no longer posts toggle-group"
            );
        }

        // 后端分支：删掉这个匹配臂，op 会落进 `_ => {}`，徽章变成死按钮。
        let source = include_str!("mod.rs");
        assert!(
            live_line(source, &dispatch_arm("toggle-group")),
            "the host no longer dispatches toggle-group"
        );
    }

    #[test]
    fn b5_the_blocked_entry_points_stay_wired() {
        // B5：屏蔽方法有两个入口（右键菜单项 + 工具栏 eye-off 的管理入口）。拦的是
        // 「删掉菜单项 / 删掉按行能力的启用条件 / 删掉后端分支」——任何一处被
        // 删，入口就变成常亮或点了没反应的死按钮。eye-off 现在打开屏蔽规则
        // 管理对话框（车道 E），不再弹方法清单。
        let (_host, _guard) = test_host();
        let page = PAGE;
        assert!(
            live_line(page, "data-act=\"block-method\""),
            "the row menu lost its block-method item"
        );
        // 没有 focus_key 的行没有方法可屏蔽，菜单项必须按行能力置灰；
        // 这条丢了就变成「点了才失败」而不是「一眼看出不可用」。
        assert!(
            live_line(page, "enableItem('block-method', !!(row && row.focus_key))"),
            "the block-method item no longer follows the row's focus_key"
        );
        assert!(
            live_line(page, "post({ op: 'manage-suppress' })"),
            "the eye-off action no longer opens the suppression manager"
        );
        let source = include_str!("mod.rs");
        assert!(
            live_line(source, &dispatch_arm("block-method")),
            "the host no longer dispatches block-method"
        );
        assert!(
            live_line(source, &dispatch_arm("manage-suppress")),
            "the host no longer dispatches manage-suppress"
        );
    }

    #[test]
    fn b6_the_slow_badge_reads_the_state_count() {
        // B6：工具栏右侧的慢查询计数胶囊。拦的是「删掉元素、或删掉它跟
        // `counts.slow` 的绑定」——删了之后慢查询还在拦截，但用户看不出有几条。
        let (host, _guard) = test_host();
        let page = PAGE;
        assert!(
            live_line(page, "id=\"slowBadge\" class=\"pill danger\" hidden"),
            "the slow count capsule is gone (or no longer starts hidden)"
        );
        // 元素还在不等于接线还在：计数要来自推送状态的 `counts.slow`，并且
        // 文案真的写进胶囊。
        for needle in [
            "var c = state.counts || {};",
            "if (c.slow > 0) {",
            "setText(slowBadge, (L.slowPrefix || '') + ' ' + c.slow);",
        ] {
            assert!(
                live_line(page, needle),
                "the capsule no longer reads counts.slow: missing {needle}"
            );
        }
        // 状态里确实带着这个字段：一条 1200ms 的慢查询（阈值 1000ms）要计入。
        set_log_text("1723456789014|1200|statement|SELECT * FROM c\n");
        handle_panel_message(&host, "{\"op\":\"ready\"}").unwrap();
        let state = state_of(&host);
        assert_eq!(
            state["counts"]["slow"], 1,
            "the state carries counts.slow: {state}"
        );
    }

    #[test]
    fn b7_the_settings_help_and_community_buttons_stay_wired() {
        // B7：设置 / 帮助 / 交流群三个降级入口。拦的是「按钮留着但 event
        // listener 被删」——点了什么都不发生，用户以为插件卡住。
        let (_host, _guard) = test_host();
        let page = PAGE;
        for id in ["settingsBtn", "helpBtn", "communityBtn"] {
            assert!(
                live_line(page, &format!("id=\"{id}\" class=\"iconbtn\"")),
                "the toolbar lost {id}"
            );
        }
        // 三个入口都开同一个页内弹层（L2），并保留各自的 op 兜底：点了既能看到
        // 当前值与修改路径，Output 里也仍有可追溯的一行。
        assert!(
            live_line(
                page,
                "$('settingsBtn').addEventListener('click', function (event) {"
            ),
            "the settings button lost its click handler"
        );
        assert!(
            live_line(page, "openSheet('settings');"),
            "the settings entry no longer opens the sheet"
        );
        assert!(
            live_line(page, "post({ op: 'settings' })"),
            "the settings entry no longer posts its op"
        );
        assert!(
            live_line(page, "$('helpBtn').addEventListener('click', function () {"),
            "the help button lost its click handler"
        );
        assert!(
            live_line(page, "openSheet('help');"),
            "the help entry no longer opens the sheet"
        );
        assert!(
            live_line(page, "post({ op: 'help' })"),
            "the help entry no longer posts its op"
        );
        assert!(
            live_line(
                page,
                "$('communityBtn').addEventListener('click', function () {"
            ),
            "the community button lost its click handler"
        );
        assert!(
            live_line(page, "openSheet('community');"),
            "the community entry no longer opens the sheet"
        );
        assert!(
            live_line(page, "post({ op: 'community' })"),
            "the community entry no longer posts its op"
        );
        // 层要能关：× 与遮罩都走 closeSheet，Esc 先关层再关菜单。
        assert!(
            live_line(
                page,
                "$('sheetClose').addEventListener('click', closeSheet);"
            ),
            "the close button lost its handler"
        );
        assert!(
            live_line(
                page,
                "$('sheetBackdrop').addEventListener('click', closeSheet);"
            ),
            "the backdrop lost its handler"
        );
        assert!(
            live_line(page, "if (!$('sheet').hidden) { closeSheet(); return; }"),
            "Escape closes the sheet first"
        );
    }

    #[test]
    fn b8_the_parameter_column_stays_editable_and_rebindable() {
        // B8：工作台参数列可编辑并写入 SQL（L42 起在 `web/preview.js`）。原版
        // 没有「重绑定」按钮——`buildParamRow` 的 DocumentListener 每击键都调
        // `scheduleRender`（150 ms 防抖）。这里拦的是同一件事：输入框还在、
        // 输入事件还绑着、停手后仍会把值 post 给宿主。
        let (_host, _guard) = test_host();
        let module = PREVIEW_JS;
        assert!(
            live_line(module, "input.className = 'pv-param-input';"),
            "the parameter inputs are no longer created"
        );
        assert!(
            live_line(module, "const PARAM_DEBOUNCE_MS = 150;"),
            "the reference's 150 ms render debounce is gone"
        );
        assert!(
            live_line(
                module,
                "paramsHost.addEventListener('input', onParamInput);"
            ),
            "the parameter inputs no longer report edits"
        );
        // 防抖回调体里既要读输入框，又要把它 post 出去；只剩一行就断链。
        assert!(
            live_line(module, "const inputs = paramInputs();"),
            "the rebind no longer reads the parameter inputs"
        );
        assert!(
            live_line(module, "post({ op: 'preview-params', values: values });"),
            "the rebind no longer posts the edited values"
        );
        assert!(
            !module.contains("pvRebindBtn"),
            "the parameter column must not grow buttons of its own"
        );
    }

    #[test]
    fn epoch_timestamps_render_as_utc() {
        assert_eq!(format_epoch_ms(0), "1970-01-01 00:00:00.000");
        assert_eq!(
            format_epoch_ms(1_705_314_645_123),
            "2024-01-15 10:30:45.123"
        );
    }

    #[test]
    fn the_preview_dialog_is_routed_to_the_workbench() {
        // Lane L48: the dialog (`sqlv-preview`) is a view of its own, and the
        // `Views` impl that the host calls lives in this module, so the router
        // is the one thing that can break the dialog silently: a missing arm
        // means `NotFound`, an empty dialog and no `closed` event.
        let (host, _guard) = test_host();
        let mut plugin = crate::Main;

        // `render` answers with empty chrome: the page comes from
        // `[[contributes.dialogs]]` and builds every control itself.
        let view = plugin
            .render(
                crate::views::workbench::DIALOG_VIEW_ID,
                None,
                &host.invocation(),
            )
            .expect("the dialog has a view");
        assert!(view.toolbar.is_empty(), "the platform draws the chrome");
        assert!(matches!(view.body, Body::Html(ref html) if html.is_empty()));

        // A message reaches the workbench and is answered on the dialog view.
        crate::views::workbench::set_model(crate::views::workbench::WorkbenchModel::new(
            "SELECT 1",
            Vec::new(),
            "MySQL",
        ));
        plugin
            .handle(
                crate::views::workbench::DIALOG_VIEW_ID,
                ViewEvent::Message("{\"op\":\"preview-ready\"}".to_string()),
                &host.invocation(),
            )
            .expect("the handshake is answered");
        assert!(
            host.view_messages()
                .iter()
                .any(|(view, _)| view == crate::views::workbench::DIALOG_VIEW_ID),
            "the dialog gets its own payload"
        );

        // `closed` clears the transient flags and keeps the model. The host
        // delivers it as the newer event kind, which reaches the plugin as an
        // `Other` event (`ViewEvent::closed`).
        crate::views::workbench::request_tab();
        plugin
            .handle(
                crate::views::workbench::DIALOG_VIEW_ID,
                ViewEvent::Other {
                    kind: "closed".to_string(),
                    data: "{}".to_string(),
                },
                &host.invocation(),
            )
            .expect("closed is answered");
        assert!(
            crate::views::workbench::select_tab().is_none(),
            "the open marker is cleared on close"
        );
        assert_eq!(crate::views::workbench::model().sql, "SELECT 1");

        // Any other view id is still an error, so the router did not become a
        // catch-all.
        assert!(matches!(
            plugin.render("no-such-view", None, &host.invocation()),
            Err(Error::NotFound(_))
        ));
    }
}
