//! Parameter type classes for the preview pane (C06).
//!
//! The IntelliJ edition reads a parameter's class off the mapper interface's
//! method signature (`XmlPreviewModel.kindFor(source)`); this build has no PSI
//! and no type resolution, so the class comes from what the statement itself
//! says, in this order:
//!
//! 1. **declared** — a `#{id,javaType=java.lang.Long}` / `jdbcType=BIGINT`
//!    attribute on the placeholder. This is the statement speaking, so the page
//!    may warn about a value that does not fit;
//! 2. **inferred** — the reference's collection-name heuristic
//!    (`looksLikeCollectionName`): `list` / `ids` / `array` / `collection` in
//!    the name, or a plural `s` ending. A hint only, never a warning;
//! 3. **value** — the JSON type of the value the render bound (a log row hands
//!    real values over; a mapper render binds nothing).
//!
//! Everything else is `Unknown`, exactly the reference's `ParamKind.UNKNOWN`
//! behaviour: the box has no placeholder and the row carries no warning.
//!
//! Both warning paths are hints, not gates — the reference is explicit about
//! this (`T-PT-02`): the value is rendered as typed, and the render's
//! confidence never changes because a parameter looks odd.

use serde_json::Value;

/// The class of one parameter value (the reference's `ParamKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Text,
    Number,
    Temporal,
    Collection,
    Object,
    Unknown,
}

impl ParamKind {
    /// The token the page and the model carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Temporal => "temporal",
            Self::Collection => "collection",
            Self::Object => "object",
            Self::Unknown => "unknown",
        }
    }

    /// The class a model token names (the inverse of [`ParamKind::as_str`]).
    pub fn parse(name: &str) -> Self {
        match name {
            "text" => Self::Text,
            "number" => Self::Number,
            "temporal" => Self::Temporal,
            "collection" => Self::Collection,
            "object" => Self::Object,
            _ => Self::Unknown,
        }
    }
}

/// Where a [`ParamKind`] came from, so the page can say "declared" apart from
/// "guessed" (and the report can be honest about which is which).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamSource {
    /// The `#{}` token declared it (`javaType` / `jdbcType`).
    Declared,
    /// The name heuristic said so.
    Inferred,
    /// The bound value's JSON type said so.
    Value,
    /// Nothing said so.
    None,
}

impl ParamSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Inferred => "inferred",
            Self::Value => "value",
            Self::None => "none",
        }
    }
}

/// One classified parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classified {
    pub kind: ParamKind,
    pub source: ParamSource,
}

/// Classifies one `#{}` placeholder by its name and declared types.
///
/// `jdbc_type` / `java_type` are what the token carried (`None` = it said
/// nothing). The `javaType` wins when both are present: it is the parameter's
/// own type, while `jdbcType` is the SQL column type the value is sent as.
pub fn classify(name: &str, jdbc_type: Option<&str>, java_type: Option<&str>) -> Classified {
    if let Some(java) = java_type
        && let Some(kind) = kind_from_java_type(java)
    {
        return Classified {
            kind,
            source: ParamSource::Declared,
        };
    }
    if let Some(jdbc) = jdbc_type
        && let Some(kind) = kind_from_jdbc_type(jdbc)
    {
        return Classified {
            kind,
            source: ParamSource::Declared,
        };
    }
    // `name[0]` / `name.field`: the placeholder addresses a member, not a
    // scalar, so the only honest class is "object path".
    if is_object_path(name) {
        return Classified {
            kind: ParamKind::Object,
            source: ParamSource::Inferred,
        };
    }
    if looks_like_collection(name) {
        return Classified {
            kind: ParamKind::Collection,
            source: ParamSource::Inferred,
        };
    }
    Classified {
        kind: ParamKind::Unknown,
        source: ParamSource::None,
    }
}

/// Falls back to the bound value's JSON type when nothing declared the class.
///
/// A declared / inferred class always wins: the value only fills the gap. Null
/// and booleans stay `Unknown` — the reference maps a boolean parameter to
/// `UNKNOWN` on purpose (there is no boolean class in `ParamKind`).
pub fn refine_with_value(classified: Classified, value: &Value) -> Classified {
    if classified.kind != ParamKind::Unknown {
        return classified;
    }
    let kind = match value {
        Value::String(text) if !text.is_empty() => ParamKind::Text,
        Value::Number(_) => ParamKind::Number,
        Value::Array(_) => ParamKind::Collection,
        Value::Object(_) => ParamKind::Object,
        _ => return classified,
    };
    Classified {
        kind,
        source: ParamSource::Value,
    }
}

/// The reference's collection-name heuristic (`looksLikeCollectionName`).
///
/// A name ending in `s` counts unless it ends in `ss` or is one of the singular
/// words that happen to end in `s` (`status`, `address`, …); compound camel
/// words are checked by suffix, so `locationStatus` is singular too.
pub fn looks_like_collection(name: &str) -> bool {
    // The leaf of an object path: `user.tags` → `tags`, `ids[0]` → `ids`.
    let leaf = name.rsplit('.').next().unwrap_or(name);
    let leaf = leaf.split('[').next().unwrap_or(leaf);
    let leaf = leaf.to_ascii_lowercase();
    if SINGULAR_WORDS
        .iter()
        .any(|word| leaf == *word || leaf.ends_with(word))
    {
        return false;
    }
    if leaf.contains("list")
        || leaf.contains("ids")
        || leaf.contains("array")
        || leaf.contains("collection")
    {
        return true;
    }
    leaf.ends_with('s') && !leaf.ends_with("ss")
}

/// Words that end in `s` but are singular; a name ending in one of them must
/// not get the collection placeholder.
const SINGULAR_WORDS: [&str; 8] = [
    "status", "address", "alias", "class", "business", "process", "access", "analysis",
];

/// The placeholder message key for a kind; empty for `Text` / `Unknown`, which
/// is the reference's empty `emptyText` (`paramEmptyText`).
pub fn placeholder_key(kind: ParamKind) -> &'static str {
    match kind {
        ParamKind::Number => "view.workbench.param.empty.number",
        ParamKind::Temporal => "view.workbench.param.empty.temporal",
        ParamKind::Collection => "view.workbench.param.empty.collection",
        ParamKind::Object => "view.workbench.param.empty.object",
        ParamKind::Text | ParamKind::Unknown => "",
    }
}

/// The warning message key for a value that does not fit a declared class
/// (`paramWarningOf`), or `None` when the value is empty or fine.
pub fn warning_key(kind: ParamKind, value: &str) -> Option<&'static str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    match kind {
        ParamKind::Number if !is_number(trimmed) => Some("view.workbench.param.warn.number"),
        ParamKind::Temporal if !is_temporal(trimmed) => Some("view.workbench.param.warn.temporal"),
        _ => None,
    }
}

fn is_object_path(name: &str) -> bool {
    name.contains('.') || name.contains('[')
}

fn kind_from_java_type(java: &str) -> Option<ParamKind> {
    let name = java.trim();
    if name.is_empty() {
        return None;
    }
    if name.ends_with("[]") {
        return Some(ParamKind::Collection);
    }
    let leaf = name.rsplit('.').next().unwrap_or(name);
    match leaf {
        "String" | "CharSequence" | "StringBuilder" | "StringBuffer" | "UUID" => {
            Some(ParamKind::Text)
        }
        "int" | "long" | "short" | "byte" | "double" | "float" | "Integer" | "Long" | "Short"
        | "Byte" | "Double" | "Float" | "BigDecimal" | "BigInteger" | "Number" => {
            Some(ParamKind::Number)
        }
        "Date" | "LocalDate" | "LocalDateTime" | "LocalTime" | "Instant" | "OffsetDateTime"
        | "ZonedDateTime" | "Timestamp" | "Time" | "Year" | "YearMonth" => {
            Some(ParamKind::Temporal)
        }
        "List" | "ArrayList" | "LinkedList" | "Set" | "HashSet" | "LinkedHashSet" | "TreeSet"
        | "Collection" | "Iterable" | "Queue" | "Deque" => Some(ParamKind::Collection),
        "Map" | "HashMap" | "LinkedHashMap" | "TreeMap" => Some(ParamKind::Object),
        // A named class (a DTO / entity) is an object path holder.
        _ => Some(ParamKind::Object),
    }
}

fn kind_from_jdbc_type(jdbc: &str) -> Option<ParamKind> {
    let name = jdbc.trim().to_ascii_uppercase();
    match name.as_str() {
        "DATE" | "TIME" | "TIMESTAMP" | "TIMESTAMP_WITH_TIMEZONE" | "TIME_WITH_TIMEZONE" => {
            Some(ParamKind::Temporal)
        }
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "FLOAT" | "REAL" | "DOUBLE" | "NUMERIC"
        | "DECIMAL" => Some(ParamKind::Number),
        "CHAR" | "VARCHAR" | "LONGVARCHAR" | "NCHAR" | "NVARCHAR" | "LONGNVARCHAR" | "CLOB"
        | "NCLOB" => Some(ParamKind::Text),
        "ARRAY" => Some(ParamKind::Collection),
        "OTHER" | "STRUCT" | "JAVA_OBJECT" => Some(ParamKind::Object),
        _ => None,
    }
}

/// A decimal number: optional sign, at most one dot, at least one digit.
fn is_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    if bytes
        .first()
        .is_some_and(|byte| *byte == b'+' || *byte == b'-')
    {
        index = 1;
    }
    let rest = &bytes[index..];
    if rest.is_empty() {
        return false;
    }
    let mut dots = 0;
    let mut digits = 0;
    for byte in rest {
        match byte {
            b'0'..=b'9' => digits += 1,
            b'.' => {
                dots += 1;
                if dots > 1 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    digits > 0
}

/// `yyyy-MM-dd`, `yyyy/MM/dd`, or a date followed by a time (`T` or space),
/// which is what the reference's `DATE_REGEX` accepts.
fn is_temporal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let date_len = 10;
    if bytes.len() < date_len {
        return false;
    }
    let separator = bytes[4];
    if separator != b'-' && separator != b'/' {
        return false;
    }
    if bytes[7] != separator {
        return false;
    }
    for (index, byte) in bytes[..date_len].iter().enumerate() {
        if index == 4 || index == 7 {
            continue;
        }
        if !byte.is_ascii_digit() {
            return false;
        }
    }
    if bytes.len() == date_len {
        return true;
    }
    // A time part: `T` / space, then at least `HH:mm`.
    if bytes[date_len] != b'T' && bytes[date_len] != b' ' {
        return false;
    }
    let rest = &bytes[date_len + 1..];
    rest.len() >= 5
        && rest[..2].iter().all(u8::is_ascii_digit)
        && rest[2] == b':'
        && rest[3..5].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_declared_java_type_wins_and_tags_the_source() {
        let classified = classify("id", Some("BIGINT"), Some("java.lang.Long"));
        assert_eq!(classified.kind, ParamKind::Number);
        assert_eq!(classified.source, ParamSource::Declared);
        let classified = classify("name", None, Some("java.lang.String"));
        assert_eq!(classified.kind, ParamKind::Text);
        let classified = classify("when", None, Some("java.time.LocalDate"));
        assert_eq!(classified.kind, ParamKind::Temporal);
        let classified = classify("ids", None, Some("java.util.List"));
        assert_eq!(classified.kind, ParamKind::Collection);
        let classified = classify("query", None, Some("com.acme.Query"));
        assert_eq!(classified.kind, ParamKind::Object);
    }

    #[test]
    fn a_declared_jdbc_type_is_used_when_java_type_is_absent() {
        assert_eq!(classify("id", Some("BIGINT"), None).kind, ParamKind::Number);
        assert_eq!(
            classify("created", Some("TIMESTAMP"), None).kind,
            ParamKind::Temporal
        );
        assert_eq!(
            classify("name", Some("VARCHAR"), None).kind,
            ParamKind::Text
        );
        // An unknown jdbc type is not a guess.
        assert_eq!(
            classify("blob", Some("SOMETHING"), None).kind,
            ParamKind::Unknown
        );
    }

    #[test]
    fn the_collection_name_heuristic_matches_the_reference() {
        assert!(looks_like_collection("ids"));
        assert!(looks_like_collection("userIdList"));
        assert!(looks_like_collection("deviceIds"));
        assert!(looks_like_collection("tags"));
        assert!(!looks_like_collection("status"));
        assert!(!looks_like_collection("locationStatus"));
        assert!(!looks_like_collection("address"));
        assert!(!looks_like_collection("class"));
        assert!(!looks_like_collection("glass"));
        let classified = classify("tags", None, None);
        assert_eq!(classified.kind, ParamKind::Collection);
        assert_eq!(classified.source, ParamSource::Inferred);
    }

    #[test]
    fn an_object_path_is_an_object_and_an_unknown_name_stays_unknown() {
        let classified = classify("user.name", None, None);
        assert_eq!(classified.kind, ParamKind::Object);
        assert_eq!(classified.source, ParamSource::Inferred);
        let classified = classify("id", None, None);
        assert_eq!(classified.kind, ParamKind::Unknown);
        assert_eq!(classified.source, ParamSource::None);
    }

    #[test]
    fn a_value_fills_only_an_unknown_class() {
        let unknown = classify("id", None, None);
        let refined = refine_with_value(unknown, &json!(3));
        assert_eq!(refined.kind, ParamKind::Number);
        assert_eq!(refined.source, ParamSource::Value);
        // A declared class is never overridden by the value.
        let declared = classify("id", Some("VARCHAR"), None);
        assert_eq!(refine_with_value(declared, &json!(3)).kind, ParamKind::Text);
        // Null and booleans keep the class unknown (the reference has no
        // boolean kind).
        assert_eq!(
            refine_with_value(unknown, &Value::Null).kind,
            ParamKind::Unknown
        );
        assert_eq!(
            refine_with_value(unknown, &json!(true)).kind,
            ParamKind::Unknown
        );
    }

    #[test]
    fn placeholders_cover_the_classes_the_reference_names() {
        assert_eq!(
            placeholder_key(ParamKind::Number),
            "view.workbench.param.empty.number"
        );
        assert_eq!(placeholder_key(ParamKind::Text), "");
        assert_eq!(placeholder_key(ParamKind::Unknown), "");
    }

    #[test]
    fn warnings_fire_only_for_a_value_that_does_not_fit() {
        assert_eq!(
            warning_key(ParamKind::Number, "abc"),
            Some("view.workbench.param.warn.number")
        );
        assert_eq!(warning_key(ParamKind::Number, "-2.5"), None);
        assert_eq!(
            warning_key(ParamKind::Temporal, "yesterday"),
            Some("view.workbench.param.warn.temporal")
        );
        assert_eq!(warning_key(ParamKind::Temporal, "2026-11-27"), None);
        assert_eq!(
            warning_key(ParamKind::Temporal, "2026-11-27 13:20:14"),
            None
        );
        assert_eq!(warning_key(ParamKind::Text, "anything"), None);
        assert_eq!(warning_key(ParamKind::Number, "  "), None);
    }
}
