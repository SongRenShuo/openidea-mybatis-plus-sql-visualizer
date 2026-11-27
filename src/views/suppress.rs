//! SQL suppression rules (lane E): the reference's "Manage SQL Suppression
//! Rules" manager.
//!
//! The reference keeps this in an application-level XML file
//! (`SqlSuppressRuleManager`) and a modal `DialogWrapper`
//! (`SqlSuppressManagerDialog`). This port keeps the same model on the
//! platform's own key-value store and the same surface as a declared dialog
//! (`[[contributes.dialogs]] id = "sqlv-suppress"`, `web = "web/suppress.html"`).
//!
//! Storage is per plugin, not per project, so every key carries a hash of the
//! project root — the same convention the history tab uses. Rules therefore
//! survive a restart and stay scoped to the project they were made in.
//!
//! Matching has two modes, exactly as the reference:
//! - exact (default): whitespace-normalised SQL with a trailing `;` stripped,
//!   compared by the FNV-1a fingerprint from [`suppress_fingerprint`];
//! - structure (opt-in per rule): string literals and bare numbers folded to
//!   `?` first ([`structure_fingerprint`]), so the same shape with other
//!   parameter values matches too.
//!
//! A rule is a display-time filter over the live buffer. Rows already parsed
//! are hidden while a rule matches them; a row that arrives while the rule is
//! active also counts one interception. Rows that were already in the buffer
//! when the rule was created are hidden but never counted, and a row is counted
//! at most once (the panel keeps a line cursor). Deleting a rule brings back the
//! buffered rows it was hiding — they were never dropped from the buffer. What
//! cannot come back is a row a rule hid before the panel ever parsed it: this
//! port filters at display time, so the raw log text is still there, but the
//! dialog promises the reference's behaviour, where a hit was dropped on
//! capture.
//!
//! The unit tests are at the bottom; `log_panel` owns the store lock they take.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, MutexGuard, OnceLock};

use openidea_plugin::views::ViewEvent;
use openidea_plugin::{Error, Invocation, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::core::log::LogEntry;
use crate::i18n::t;

/// The id declared in `[[contributes.dialogs]]`.
pub const VIEW_ID: &str = "sqlv-suppress";

/// Storage namespace. One key per project: `suppress.<project hash>.rules`.
const BASE: &str = "suppress";

/// The reference's rule cap (`SqlSuppressRuleManager.MAX_RULES`): the oldest
/// rule wins, a new one is refused rather than silently evicting an old rule.
pub const MAX_RULES: usize = 500;

/// Summary length of a rule in the list (the reference's
/// `MAX_PATTERN_SUMMARY_LEN`).
const SUMMARY_CHARS: usize = 80;

/// One suppression rule. Field names follow the reference's
/// `SqlSuppressRule` where they overlap:
/// - `pattern` is the normalised SQL (`sample_sql` trimmed, whitespace folded
///   and a trailing `;` stripped) and is what the list shows;
/// - `sample_sql` is the SQL as the user added it, shown in full in the right
///   pane;
/// - `structure` is the reference's `structureMatch` switch;
/// - `key` is the fingerprint the rule matches by (the "structure fingerprint"
///   once `structure` is on) and is recomputed from `pattern` on load, so a
///   stored rule can never drift from the matcher's definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuppressRule {
    pub id: String,
    pub pattern: String,
    #[serde(default)]
    pub sample_sql: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub hits: u64,
    #[serde(default)]
    pub structure: bool,
    #[serde(default)]
    pub key: String,
}

impl SuppressRule {
    /// The list's summary: the first line of `pattern`, cut at
    /// [`SUMMARY_CHARS`] characters with an ellipsis.
    fn summary(&self) -> String {
        let first = self.pattern.lines().next().unwrap_or("").trim();
        if first.chars().count() <= SUMMARY_CHARS {
            return first.to_string();
        }
        let mut out: String = first.chars().take(SUMMARY_CHARS).collect();
        out.push('…');
        out
    }

    fn recompute_key(&mut self) {
        self.key = if self.structure {
            structure_fingerprint(&self.pattern)
        } else {
            suppress_fingerprint(&self.pattern)
        };
    }
}

/// What the dialog shows as its status line: a message key plus an optional
/// `{n}` parameter (the rule limit) and a severity for the page's ink.
#[derive(Debug, Clone, Default)]
struct Status {
    key: &'static str,
    param: Option<usize>,
    kind: &'static str,
}

impl Status {
    fn info(key: &'static str) -> Self {
        Self {
            key,
            param: None,
            kind: "info",
        }
    }

    fn error(key: &'static str, param: Option<usize>) -> Self {
        Self {
            key,
            param,
            kind: "error",
        }
    }

    fn is_empty(&self) -> bool {
        self.key.is_empty()
    }
}

#[derive(Default)]
struct Store {
    /// Hash of the project root the loaded rules belong to.
    scope: String,
    rules: Vec<SuppressRule>,
    /// Highest log line already looked at by [`note_new_entries`]; rows at or
    /// below it arrived before the current rules (or were already counted) and
    /// are never counted twice.
    counted_upto: usize,
    status: Status,
    /// One-shot: the page should keep the add pane open (a rejected add must
    /// not throw away what the user pasted).
    keep_add: bool,
    /// One-shot: the rule the dialog should select after a change.
    select: Option<String>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> MutexGuard<'static, Store> {
    STORE
        .get_or_init(|| Mutex::new(Store::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Fingerprints (shared with the log panel's structure sort)
// ---------------------------------------------------------------------------

/// Whitespace-normalised SQL: trimmed and every whitespace run folded to one
/// space. Case and semicolons are left alone (the reference's
/// `normalizeSqlWhitespace`).
pub fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The exact-match key: normalised text with a trailing `;` stripped, then the
/// FNV-1a fingerprint. The reference's `suppressFingerprint`.
pub fn suppress_fingerprint(sql: &str) -> String {
    fingerprint(normalize_sql(sql).trim_end_matches(';').trim())
}

/// The structure-match key: the exact key's text with every string literal and
/// every bare number folded to `?`. The reference's `structureFingerprint`.
pub fn structure_fingerprint(sql: &str) -> String {
    let normalised = normalize_sql(sql);
    let body = normalised.trim_end_matches(';').trim();
    fingerprint(&parameterise_literals(body))
}

/// FNV-1a 64-bit hex. The reference hashes UTF-16 code units; this port hashes
/// UTF-8 bytes, so the two agree on ASCII SQL and only differ on non-ASCII
/// identifiers. Nothing compares a key across the two plugins.
fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:x}")
}

/// Folds string literals (`'…'`, `''` escapes) and bare numbers to `?`.
///
/// Identifiers keep their digits (`t2024`, `plan_2026001`): a digit folds only
/// when the character before it is not a SQL word character. `IN` lists keep
/// their arity (`IN (?,?)` differs from `IN (?,?,?)`). An unterminated literal
/// folds to one `?` and ends the scan.
fn parameterise_literals(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let chars: Vec<char> = sql.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\'' {
            let mut end = index + 1;
            while end < chars.len() {
                if chars[end] == '\'' {
                    if end + 1 < chars.len() && chars[end + 1] == '\'' {
                        end += 2;
                        continue;
                    }
                    break;
                }
                end += 1;
            }
            out.push('?');
            index = if end < chars.len() {
                end + 1
            } else {
                chars.len()
            };
        } else if ch.is_ascii_digit() && (index == 0 || !is_sql_word_char(chars[index - 1])) {
            let mut end = index;
            while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
                end += 1;
            }
            if end < chars.len() && is_sql_word_char(chars[end]) {
                out.extend(&chars[index..end]);
            } else {
                out.push('?');
            }
            index = end;
        } else {
            out.push(ch);
            index += 1;
        }
    }
    out
}

fn is_sql_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

// ---------------------------------------------------------------------------
// Scope and persistence
// ---------------------------------------------------------------------------

/// Hash of the invoking window's project root. Two projects never share rules.
fn scope_key(ctx: &Invocation) -> String {
    let root = ctx
        .project()
        .and_then(|project| project.root().ok())
        .unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn rules_key(scope: &str) -> String {
    format!("{BASE}.{scope}.rules")
}

/// Points the store at the invoking project, loading its rules when the scope
/// changes. Every entry that carries an [`Invocation`] calls this first.
pub fn set_scope(ctx: &Invocation) {
    let scope = scope_key(ctx);
    let mut guard = store();
    if guard.scope == scope {
        return;
    }
    guard.rules = load(&scope);
    guard.scope = scope;
    guard.counted_upto = 0;
    guard.status = Status::default();
    guard.keep_add = false;
    guard.select = None;
}

#[derive(Deserialize)]
struct StoredRules {
    #[serde(default)]
    rules: Vec<SuppressRule>,
}

fn load(scope: &str) -> Vec<SuppressRule> {
    let Some(text) = host::storage::get(&rules_key(scope)).ok().flatten() else {
        return Vec::new();
    };
    let Ok(stored) = serde_json::from_str::<StoredRules>(&text) else {
        // A damaged value is treated as "no rules" rather than failing the
        // panel: the dialog shows an empty list and the next save overwrites it.
        return Vec::new();
    };
    let mut rules = stored.rules;
    rules.retain(|rule| !rule.id.is_empty() && !rule.pattern.is_empty());
    for rule in &mut rules {
        if rule.sample_sql.is_empty() {
            rule.sample_sql = rule.pattern.clone();
        }
        rule.recompute_key();
    }
    rules
}

fn save(guard: &Store) {
    if guard.scope.is_empty() {
        return;
    }
    let stored = json!({ "version": 1, "rules": guard.rules });
    let _ = host::storage::set(&rules_key(&guard.scope), &stored.to_string());
}

// ---------------------------------------------------------------------------
// Matching and counting
// ---------------------------------------------------------------------------

/// A snapshot of the match keys, so one state push computes each row's
/// fingerprint at most once and never holds the store lock across the list.
#[derive(Debug, Default)]
pub struct Matcher {
    exact: HashSet<String>,
    structure: HashSet<String>,
}

impl Matcher {
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.structure.is_empty()
    }

    /// Whether a rule hides this statement. Exact rules are tried first, then
    /// the structure rules — the reference's gate order.
    ///
    /// A row carries the statement in two forms: `sql` is the restored text
    /// (parameters substituted) and `raw_sql` keeps the `?` placeholders. A
    /// rule can come from either: pasting the console's `Preparing:` line
    /// yields the placeholder form, while copying a list row yields the
    /// restored form. Matching only one of them made a pasted rule never hide
    /// the row it was made from (2026-11-27, live IDE).
    pub fn hidden(&self, sql: &str, raw_sql: &str) -> bool {
        matches_one(self, sql) || matches_one(self, raw_sql)
    }
}

fn matches_one(matcher: &Matcher, sql: &str) -> bool {
    if sql.is_empty() {
        return false;
    }
    if !matcher.exact.is_empty() && matcher.exact.contains(&suppress_fingerprint(sql)) {
        return true;
    }
    !matcher.structure.is_empty() && matcher.structure.contains(&structure_fingerprint(sql))
}

/// The matcher for the current rule set.
pub fn matcher() -> Matcher {
    let guard = store();
    let mut out = Matcher::default();
    for rule in &guard.rules {
        if rule.structure {
            out.structure.insert(rule.key.clone());
        } else {
            out.exact.insert(rule.key.clone());
        }
    }
    out
}

fn hit_index(rules: &[SuppressRule], sql: &str, raw_sql: &str) -> Option<usize> {
    let exact = suppress_fingerprint(sql);
    let exact_raw = if raw_sql.is_empty() {
        None
    } else {
        Some(suppress_fingerprint(raw_sql))
    };
    if let Some(index) = rules.iter().position(|rule| {
        !rule.structure && (rule.key == exact || exact_raw.as_deref() == Some(rule.key.as_str()))
    }) {
        return Some(index);
    }
    let structure = structure_fingerprint(sql);
    let structure_raw = if raw_sql.is_empty() {
        None
    } else {
        Some(structure_fingerprint(raw_sql))
    };
    rules.iter().position(|rule| {
        rule.structure
            && (rule.key == structure || structure_raw.as_deref() == Some(rule.key.as_str()))
    })
}

/// Counts one interception on the rule a row matches. The caller keeps the
/// store lock.
fn record_hit(rules: &mut [SuppressRule], sql: &str, raw_sql: &str) -> bool {
    let Some(index) = hit_index(rules, sql, raw_sql) else {
        return false;
    };
    rules[index].hits = rules[index].hits.saturating_add(1);
    true
}

/// Counts the newly parsed rows against the active rules, once each.
///
/// `entries` is the panel's whole parse; `line_no` is stable while the text
/// only grows. Rows at or below the cursor are already accounted for, and rows
/// parsed while no rule was active are skipped by advancing the cursor — a rule
/// created afterwards hides them but does not claim interceptions it never made.
pub fn note_new_entries(entries: &[LogEntry]) {
    let mut guard = store();
    let highest = entries.iter().map(|entry| entry.line_no).max().unwrap_or(0);
    if guard.rules.is_empty() {
        guard.counted_upto = guard.counted_upto.max(highest);
        return;
    }
    let base = guard.counted_upto;
    let mut counted = base;
    let mut hits = 0;
    for entry in entries {
        if entry.line_no <= base || !entry.has_sql() {
            continue;
        }
        counted = counted.max(entry.line_no);
        if record_hit(&mut guard.rules, &entry.sql, &entry.raw_sql) {
            hits += 1;
        }
    }
    guard.counted_upto = guard.counted_upto.max(counted);
    if hits > 0 {
        // Persisted at once: the count is part of the rule's stored state, and
        // a crash between two pushes should not lose what was already reported.
        save(&guard);
    }
}

/// Forgets the line cursor. Called when the panel buffer is replaced or
/// cleared: the next parse is a fresh buffer, whose rows are new to the rules.
pub fn reset_cursor() {
    store().counted_upto = 0;
}

// ---------------------------------------------------------------------------
// Rule operations
// ---------------------------------------------------------------------------

/// Outcome of [`add_rule`], used only to pick the dialog's status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    Duplicate,
    Blank,
    LimitReached,
}

impl AddOutcome {
    pub fn is_added(self) -> bool {
        self == AddOutcome::Added
    }
}

/// Adds a rule from SQL the user pasted. Whitespace variants of the same
/// statement are the same rule (the reference's duplicate rule); the store is
/// saved before returning.
pub fn add_rule(sample_sql: &str) -> AddOutcome {
    // The trailing `;` is stripped here too, not just in the fingerprint: two
    // pastes that differ only by a statement terminator are the same rule, and
    // keeping both would give the list two entries that filter identically.
    let normalized = normalize_sql(sample_sql);
    let pattern = normalized.trim_end_matches(';').trim().to_string();
    if pattern.is_empty() {
        let mut guard = store();
        guard.status = Status::error("view.suppress.status.blank", None);
        guard.keep_add = true;
        return AddOutcome::Blank;
    }
    let mut guard = store();
    if guard.rules.iter().any(|rule| rule.pattern == pattern) {
        guard.status = Status::error("view.suppress.status.duplicate", None);
        guard.keep_add = true;
        return AddOutcome::Duplicate;
    }
    if guard.rules.len() >= MAX_RULES {
        guard.status = Status::error("view.suppress.status.limit", Some(MAX_RULES));
        guard.keep_add = true;
        return AddOutcome::LimitReached;
    }
    let id = next_id(&guard.rules);
    let mut rule = SuppressRule {
        id: id.clone(),
        pattern,
        sample_sql: sample_sql.trim().to_string(),
        created_at: now_ms(),
        hits: 0,
        structure: false,
        key: String::new(),
    };
    rule.recompute_key();
    guard.rules.push(rule);
    guard.status = Status::info("view.suppress.status.added");
    guard.keep_add = false;
    guard.select = Some(id);
    save(&guard);
    AddOutcome::Added
}

fn next_id(rules: &[SuppressRule]) -> String {
    let stamp = now_ms();
    let mut candidate = format!("{stamp:x}");
    let mut suffix = 1;
    while rules.iter().any(|rule| rule.id == candidate) {
        suffix += 1;
        candidate = format!("{stamp:x}-{suffix}");
    }
    candidate
}

/// Deletes one rule. Returns whether it existed.
pub fn delete_rule(id: &str) -> bool {
    let mut guard = store();
    let before = guard.rules.len();
    guard.rules.retain(|rule| rule.id != id);
    if guard.rules.len() == before {
        return false;
    }
    guard.status = Status::info("view.suppress.status.deleted");
    guard.keep_add = false;
    if guard.select.as_deref() == Some(id) {
        guard.select = None;
    }
    save(&guard);
    true
}

/// Deletes every rule. Returns whether anything was deleted.
pub fn clear_rules() -> bool {
    let mut guard = store();
    if guard.rules.is_empty() {
        return false;
    }
    guard.rules.clear();
    guard.status = Status::info("view.suppress.status.cleared");
    guard.keep_add = false;
    guard.select = None;
    save(&guard);
    true
}

/// Turns the structure switch of one rule on or off. Returns whether the rule
/// changed; `None` when the id is unknown.
pub fn set_structure(id: &str, enabled: bool) -> Option<bool> {
    let mut guard = store();
    let index = guard.rules.iter().position(|rule| rule.id == id)?;
    if guard.rules[index].structure == enabled {
        return Some(false);
    }
    guard.rules[index].structure = enabled;
    guard.rules[index].recompute_key();
    guard.status = Status::info(if enabled {
        "view.suppress.status.structure_on"
    } else {
        "view.suppress.status.structure_off"
    });
    guard.keep_add = false;
    save(&guard);
    Some(true)
}

/// Turns the structure switch of every rule on or off (the dialog's
/// "structure-match all" checkbox). Returns whether anything changed.
pub fn set_structure_all(enabled: bool) -> bool {
    let mut guard = store();
    let mut changed = false;
    for rule in &mut guard.rules {
        if rule.structure == enabled {
            continue;
        }
        rule.structure = enabled;
        rule.recompute_key();
        changed = true;
    }
    if !changed {
        return false;
    }
    guard.status = Status::info(if enabled {
        "view.suppress.status.structure_on"
    } else {
        "view.suppress.status.structure_off"
    });
    guard.keep_add = false;
    save(&guard);
    true
}

/// Rule snapshot, in creation order (the reference's list order).
pub fn rules() -> Vec<SuppressRule> {
    store().rules.clone()
}

/// The rule the dialog should select next, if any (one-shot).
pub fn take_selection() -> Option<String> {
    store().select.take()
}

// ---------------------------------------------------------------------------
// Dialog
// ---------------------------------------------------------------------------

/// Opens the manager in the invoking window. `views.reveal` is accepted only
/// from a command or a view event, which is where every caller lives.
pub fn reveal(ctx: &Invocation) -> Result<(), Error> {
    set_scope(ctx);
    host::reveal_view(VIEW_ID)
}

/// Handles an event of the dialog page. Closing clears only the transient
/// dialog state (status and pending selection); the rules stay.
pub fn handle_event(event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
    if event.closed() {
        let mut guard = store();
        guard.select = None;
        guard.keep_add = false;
        guard.status = Status::default();
        drop(guard);
        // Deleting a rule must bring the buffered rows back *now*, not when the
        // panel is next revealed; a close after no change is a cheap re-push.
        // A closed panel is not an error for the dialog's own close.
        let _ = crate::views::log_panel::push_state();
        return Ok(());
    }
    match event {
        ViewEvent::Message(text) => handle_message(&text, ctx),
        _ => Ok(()),
    }
}

/// Handles one JSON message posted by the page.
pub fn handle_message(text: &str, ctx: &Invocation) -> Result<(), Error> {
    let message: Value =
        serde_json::from_str(text).map_err(|error| Error::Invalid(error.to_string()))?;
    set_scope(ctx);
    let op = message
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut changed = false;
    match op {
        // The page asks for the state once its script is ready (`ready`) and
        // then on every op's reply. Opening the dialog starts from a clean
        // status line, like the reference's `refreshRuleList` on construction.
        "ready" | "page" => {
            let mut guard = store();
            guard.status = Status::default();
            guard.keep_add = false;
            guard.select = None;
        }
        "add" => {
            let sql = message
                .get("sql")
                .and_then(Value::as_str)
                .unwrap_or_default();
            changed = add_rule(sql).is_added();
        }
        "delete" => {
            if let Some(id) = message.get("id").and_then(Value::as_str) {
                changed = delete_rule(id);
            }
        }
        "clear" => {
            changed = clear_rules();
        }
        // Cancelling the add form clears the status line a failed add left
        // behind (the red "paste SQL first" stayed after the form closed).
        "cancel-add" => {
            let mut guard = store();
            guard.status = Status::default();
            guard.keep_add = false;
        }
        "structure" => {
            let enabled = message
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(id) = message.get("id").and_then(Value::as_str) {
                changed = set_structure(id, enabled) == Some(true);
            }
        }
        "structure-all" => {
            let enabled = message
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            changed = set_structure_all(enabled);
        }
        _ => {}
    }
    post_state()?;
    if changed {
        // The panel's rows are a live filter over the same rules: a new rule
        // hides matching buffered rows, a deleted one shows them again. A panel
        // that is not open simply has nothing to refresh.
        let _ = crate::views::log_panel::push_state();
    }
    Ok(())
}

/// Posts the dialog's whole state to the page.
pub fn post_state() -> Result<(), Error> {
    host::post_view_message(VIEW_ID, &state().to_string())
}

/// The JSON the dialog page renders.
pub fn state() -> Value {
    let mut guard = store();
    let keep_add = std::mem::take(&mut guard.keep_add);
    let select = guard.select.take();
    let count = guard.rules.len();
    let all_structure = count > 0 && guard.rules.iter().all(|rule| rule.structure);
    let rules: Vec<Value> = guard
        .rules
        .iter()
        .map(|rule| {
            json!({
                "id": rule.id,
                "summary": rule.summary(),
                "sql": rule.sample_sql,
                "created": rule.created_at,
                "createdText": format_time(rule.created_at),
                "hits": rule.hits,
                "hitsText": t("view.suppress.hit_count").replace("{n}", &rule.hits.to_string()),
                "structure": rule.structure,
            })
        })
        .collect();
    let status = if guard.status.is_empty() {
        Value::Null
    } else {
        let mut text = t(guard.status.key);
        if let Some(param) = guard.status.param {
            text = text.replace("{n}", &param.to_string());
        }
        json!({ "kind": guard.status.kind, "text": text })
    };
    drop(guard);
    json!({
        "op": "state",
        "rules": rules,
        "empty": count == 0,
        "allStructure": all_structure,
        "select": select,
        "keepAdd": keep_add,
        "status": status,
        "labels": labels(),
    })
}

fn labels() -> Value {
    let count = store().rules.len();
    json!({
        "title": t("view.suppress.title"),
        "listTitle": t("view.suppress.list_title"),
        "add": t("view.suppress.add"),
        "addConfirm": t("view.suppress.add_confirm"),
        "cancelAdd": t("view.suppress.cancel_add"),
        "delete": t("view.suppress.delete"),
        "clear": t("view.suppress.clear"),
        "close": t("view.suppress.close"),
        "empty": t("view.suppress.empty"),
        "addHint": t("view.suppress.add_hint"),
        "structureOn": t("view.suppress.structure_on"),
        "structureOff": t("view.suppress.structure_off"),
        "structureAll": t("view.suppress.structure_all"),
        "structureBadge": t("view.suppress.structure_badge"),
        "clearConfirm": t("view.suppress.clear_confirm").replace("{n}", &count.to_string()),
        "clearYes": t("view.suppress.clear_yes"),
        "clearNo": t("view.suppress.clear_no"),
        "note": t("view.suppress.note"),
        "selectedHint": t("view.suppress.selected_hint"),
    })
}

/// `now` in milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM` UTC for the list's time line.
fn format_time(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    // Labelled `UTC`: the wasm has no timezone API, while the log rows show the
    // application's own wall clock. An unlabelled UTC time read as "the rule was
    // created at a wrong hour" next to rows stamped in local time.
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

/// Howard Hinnant's civil-from-days (UTC).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
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

/// Puts the store back to a fresh baseline. The rules live in process-wide
/// state, so the panel tests (which share the store lock) must not inherit
/// another test's rules; storage itself is per test host.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    *store() = Store::default();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    use crate::views::StateLockGuard;
    use crate::views::log_panel::{handle_panel_event, set_log_text};

    fn test_host() -> (MockHost, StateLockGuard) {
        let guard = crate::views::test_state_lock();
        let host = MockHost::install();
        reset_for_tests();
        crate::views::log_panel::reset_for_tests();
        crate::indexer::clear_indexes();
        crate::capture::bridge::reset_capture_for_tests();
        crate::views::workbench::clear();
        set_scope(&host.invocation());
        (host, guard)
    }

    fn state_of(host: &MockHost) -> Value {
        let messages = host.view_messages();
        let (_, text) = messages
            .iter()
            .rev()
            .find(|(view, _)| view == VIEW_ID)
            .expect("the dialog state was posted");
        serde_json::from_str(text).expect("valid JSON")
    }

    fn log_state(host: &MockHost) -> Value {
        let messages = host.view_messages();
        let (_, text) = messages
            .iter()
            .rev()
            .find(|(view, _)| view == crate::views::log_panel::VIEW_ID)
            .expect("a panel state was posted");
        serde_json::from_str(text).expect("valid JSON")
    }

    fn panel_op(host: &MockHost, op: &str) -> Value {
        handle_panel_event(
            ViewEvent::Message(json!({ "op": op }).to_string()),
            &host.invocation(),
        )
        .unwrap();
        log_state(host)
    }

    const TEX_ONE: &str =
        "==>  Preparing: SELECT * FROM heartbeat WHERE id = ?\n==> Parameters: 1(Long)\n";

    // Rule model ------------------------------------------------------------

    #[test]
    fn add_normalises_whitespace_and_rejects_duplicates() {
        let (host, _guard) = test_host();
        assert_eq!(add_rule("SELECT  *\n  FROM t"), AddOutcome::Added);
        assert_eq!(add_rule(" SELECT * FROM t ; "), AddOutcome::Duplicate);
        let list = rules();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].pattern, "SELECT * FROM t");
        assert_eq!(list[0].sample_sql, "SELECT  *\n  FROM t");
        assert!(!list[0].key.is_empty());
        assert!(!list[0].structure);
        // The rule was persisted for the invoking scope.
        set_scope(&host.invocation());
        assert_eq!(rules().len(), 1);
    }

    #[test]
    fn blank_input_never_becomes_a_rule() {
        let (_host, _guard) = test_host();
        assert_eq!(add_rule("   \n "), AddOutcome::Blank);
        assert!(rules().is_empty());
    }

    #[test]
    fn delete_and_clear_report_changes() {
        let (_host, _guard) = test_host();
        add_rule("SELECT 1");
        add_rule("SELECT 2");
        let first = rules()[0].id.clone();
        assert!(delete_rule(&first));
        assert!(!delete_rule(&first));
        assert_eq!(rules().len(), 1);
        assert!(clear_rules());
        assert!(!clear_rules());
        assert!(rules().is_empty());
    }

    // Matching --------------------------------------------------------------

    #[test]
    fn exact_matching_folds_whitespace_and_a_trailing_semicolon() {
        let (_host, _guard) = test_host();
        add_rule("SELECT * FROM t WHERE id = 1;");
        let matcher = matcher();
        assert!(matcher.hidden("SELECT  *  FROM t\nWHERE id = 1", ""));
        assert!(!matcher.hidden("SELECT * FROM t WHERE id = 2", ""));
        assert!(!matcher.hidden("select * from t where id = 1", ""));
    }

    #[test]
    fn a_rule_pasted_from_the_placeholder_form_hides_the_restored_row() {
        // The console prints `Preparing: … id = ?` and the row keeps the
        // restored `… id = 1`. A rule made from the console line must still
        // hide the row (2026-11-27, live IDE: it did not).
        let (_host, _guard) = test_host();
        add_rule("SELECT * FROM heartbeat WHERE id = ?");
        assert!(matcher().hidden(
            "SELECT * FROM heartbeat WHERE id = 1",
            "SELECT * FROM heartbeat WHERE id = ?"
        ));
    }

    #[test]
    fn a_rule_copied_from_the_restored_row_hides_the_placeholder_row() {
        let (_host, _guard) = test_host();
        add_rule("SELECT * FROM heartbeat WHERE id = 5");
        assert!(matcher().hidden(
            "SELECT * FROM heartbeat WHERE id = 5",
            "SELECT * FROM heartbeat WHERE id = ?"
        ));
        // Exact matching still keeps two different values apart.
        assert!(!matcher().hidden(
            "SELECT * FROM heartbeat WHERE id = 6",
            "SELECT * FROM heartbeat WHERE id = ?"
        ));
    }

    #[test]
    fn structure_matching_hits_the_same_shape_with_other_values() {
        let (_host, _guard) = test_host();
        add_rule("SELECT * FROM t WHERE id = 1 AND name = 'a'");
        let id = rules()[0].id.clone();
        assert!(!matcher().hidden("SELECT * FROM t WHERE id = 2 AND name = 'b'", ""));

        assert_eq!(set_structure(&id, true), Some(true));
        let structure = matcher();
        assert!(structure.hidden("SELECT * FROM t WHERE id = 2 AND name = 'b'", ""));
        assert!(structure.hidden("SELECT * FROM t WHERE id = 99 AND name = 'other'", ""));
        // A different table/column is a different shape.
        assert!(!structure.hidden("SELECT * FROM other WHERE id = 2 AND name = 'b'", ""));

        assert_eq!(set_structure(&id, false), Some(true));
        assert!(!matcher().hidden("SELECT * FROM t WHERE id = 2 AND name = 'b'", ""));
    }

    #[test]
    fn structure_all_switches_every_rule() {
        let (_host, _guard) = test_host();
        add_rule("SELECT 1");
        add_rule("SELECT 2");
        assert!(set_structure_all(true));
        assert!(rules().iter().all(|rule| rule.structure));
        assert!(!set_structure_all(true), "no change, no save");
        assert!(set_structure_all(false));
        assert!(rules().iter().all(|rule| !rule.structure));
    }

    // Counting and the display filter ---------------------------------------

    #[test]
    fn a_new_row_counts_once_and_old_rows_are_hidden_but_not_counted() {
        let (host, _guard) = test_host();
        set_log_text(TEX_ONE);
        // Parse the buffer first: these rows exist before the rule does.
        panel_op(&host, "ready");
        add_rule("SELECT * FROM heartbeat WHERE id = 1");
        assert_eq!(rules()[0].hits, 0);

        // The buffered row is hidden. It was not counted: it was already there.
        let state = panel_op(&host, "ready");
        assert_eq!(state["rows"].as_array().unwrap().len(), 0);
        assert_eq!(rules()[0].hits, 0);

        // A row that arrives while the rule is active is counted exactly once.
        crate::views::log_panel::append_text(TEX_ONE);
        panel_op(&host, "ready");
        assert_eq!(rules()[0].hits, 1);
        panel_op(&host, "ready");
        assert_eq!(
            rules()[0].hits,
            1,
            "a push must not count the same row again"
        );
    }

    #[test]
    fn the_suppress_row_action_hides_the_row_it_was_used_on() {
        // The whole chain, with the real fixture: the console line keeps `?`,
        // the row keeps the restored SQL. The row action must create the rule
        // from the placeholder form and the row must vanish (2026-11-27).
        let (host, _guard) = test_host();
        set_log_text(TEX_ONE);
        let state = panel_op(&host, "ready");
        let line = state["rows"][0]["line"].as_u64().expect("one row");
        handle_panel_event(
            ViewEvent::Message(json!({ "op": "suppress-sql", "line": line }).to_string()),
            &host.invocation(),
        )
        .unwrap();
        let state = log_state(&host);
        assert_eq!(
            state["rows"].as_array().unwrap().len(),
            0,
            "the row it was used on is hidden: {state}"
        );
        assert_eq!(
            rules()[0].pattern,
            "SELECT * FROM heartbeat WHERE id = ?",
            "the rule takes the placeholder form"
        );
    }

    #[test]
    fn deleting_a_rule_brings_the_buffered_rows_back() {
        let (host, _guard) = test_host();
        set_log_text(TEX_ONE);
        panel_op(&host, "ready");
        add_rule("SELECT * FROM heartbeat WHERE id = 1");
        let state = panel_op(&host, "ready");
        assert_eq!(state["rows"].as_array().unwrap().len(), 0);

        let id = rules()[0].id.clone();
        assert!(delete_rule(&id));
        let state = panel_op(&host, "ready");
        assert_eq!(state["rows"].as_array().unwrap().len(), 1, "{state}");
    }

    // Persistence -----------------------------------------------------------

    #[test]
    fn rules_and_hits_survive_a_restart() {
        let (host, _guard) = test_host();
        set_log_text(TEX_ONE);
        panel_op(&host, "ready");
        add_rule("SELECT * FROM heartbeat WHERE id = 1");
        let id = rules()[0].id.clone();
        set_structure(&id, true);
        crate::views::log_panel::append_text(TEX_ONE);
        panel_op(&host, "ready");
        assert_eq!(rules()[0].hits, 1);

        // A restart drops the in-memory store; the next scope change loads the
        // stored rules, hits and structure flag included.
        reset_for_tests();
        set_scope(&host.invocation());
        let rules = rules();
        assert_eq!(rules.len(), 1, "the rule came back from storage");
        assert_eq!(rules[0].hits, 1);
        assert!(rules[0].structure);
        assert!(matcher().hidden("SELECT * FROM heartbeat WHERE id = 7", ""));
    }

    #[test]
    fn storage_is_keyed_per_project() {
        let (host, _guard) = test_host();
        add_rule("SELECT 1");
        let key = rules_key(&scope_key(&host.invocation()));
        let stored = host::storage::get(&key).unwrap().expect("stored");
        assert!(stored.contains("SELECT 1"), "{stored}");
        // Every other scope starts empty: the prefix is the project hash.
        assert!(
            !scope_key(&host.invocation()).is_empty(),
            "the invoking project always has a scope"
        );
    }

    // Dialog protocol -------------------------------------------------------

    #[test]
    fn the_dialog_state_carries_every_field_the_page_draws() {
        let (host, _guard) = test_host();
        add_rule("SELECT * FROM t WHERE id = 1");
        set_scope(&host.invocation());
        post_state().unwrap();
        let state = state_of(&host);
        let rules = state["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["sql"], "SELECT * FROM t WHERE id = 1");
        assert!(rules[0]["hitsText"].as_str().unwrap().contains('0'));
        assert_eq!(rules[0]["structure"], false);
        assert_eq!(state["empty"], false);
        assert_eq!(state["allStructure"], false);
        assert!(!state["labels"]["add"].as_str().unwrap().is_empty());
        assert!(!state["labels"]["note"].as_str().unwrap().is_empty());
    }

    #[test]
    fn the_page_messages_drive_the_rule_list() {
        let (host, _guard) = test_host();
        let ctx = host.invocation();
        handle_message("{\"op\":\"ready\"}", &ctx).unwrap();
        assert_eq!(state_of(&host)["empty"], true);

        handle_message("{\"op\":\"add\",\"sql\":\"SELECT 1\"}", &ctx).unwrap();
        let state = state_of(&host);
        assert_eq!(state["rules"].as_array().unwrap().len(), 1);
        assert!(
            state["rules"][0]["createdText"]
                .as_str()
                .is_some_and(|text| text.ends_with(" UTC")),
            "the created time states its zone: {state}"
        );
        let id = state["rules"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(state["select"], json!(id));

        handle_message(
            &format!("{{\"op\":\"structure\",\"id\":\"{id}\",\"enabled\":true}}"),
            &ctx,
        )
        .unwrap();
        assert_eq!(state_of(&host)["rules"][0]["structure"], true);
        assert_eq!(state_of(&host)["allStructure"], true);

        handle_message(&format!("{{\"op\":\"delete\",\"id\":\"{id}\"}}"), &ctx).unwrap();
        assert_eq!(state_of(&host)["empty"], true);
    }

    #[test]
    fn closing_the_dialog_keeps_the_rules_and_drops_the_transient_state() {
        let (host, _guard) = test_host();
        add_rule("SELECT 1");
        post_state().unwrap();
        let ctx = host.invocation();
        handle_event(
            ViewEvent::Other {
                kind: "closed".into(),
                data: String::new(),
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(rules().len(), 1, "closing does not delete rules");
        post_state().unwrap();
        let state = state_of(&host);
        assert_eq!(state["status"], Value::Null);
        assert_eq!(state["select"], Value::Null);
    }

    #[test]
    fn the_panel_entry_opens_the_declared_dialog() {
        let (host, _guard) = test_host();
        handle_panel_event(
            ViewEvent::Message(json!({ "op": "manage-suppress" }).to_string()),
            &host.invocation(),
        )
        .unwrap();
        assert_eq!(host.revealed_views(), [VIEW_ID.to_string()]);
    }

    #[test]
    fn a_rejected_add_keeps_the_add_pane_open() {
        let (host, _guard) = test_host();
        let ctx = host.invocation();
        add_rule("SELECT 1");
        handle_message("{\"op\":\"add\",\"sql\":\"SELECT 1\"}", &ctx).unwrap();
        let state = state_of(&host);
        assert_eq!(state["keepAdd"], true);
        assert_eq!(state["status"]["kind"], "error");
    }
}
