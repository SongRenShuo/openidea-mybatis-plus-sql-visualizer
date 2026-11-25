//! `sqlv_render_logs`: parse caller-supplied log text into statements.
//!
//! Nothing is read from the IDE console (the platform exposes no console API);
//! the caller passes the text. The output is the core `LogAnalysis` unchanged.
//! The envelope confidence summarises the whole analysis:
//! - `unresolved` when no entry carried SQL (error-only or empty result);
//! - `partial` when any entry is partial/unresolved or a line stayed unparsed;
//! - `exact` when every entry is exact and nothing was left unparsed.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::log::{Confidence as LogConfidence, LogAnalysis, ParseOptions, parse};
use crate::mcp::contract::{invalid, parse_args, success, success_with};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionsArg {
    #[serde(default)]
    slow_threshold_ms: Option<f64>,
    #[serde(default)]
    n_plus_one_threshold: Option<usize>,
    #[serde(default)]
    ignore_params_in_dedupe: Option<bool>,
    #[serde(default)]
    dedupe_window_ms: Option<i64>,
    #[serde(default)]
    max_sql_len: Option<usize>,
    #[serde(default)]
    timestamp_offset_minutes: Option<i32>,
    #[serde(default)]
    recognize_bare_sql: Option<bool>,
}

impl OptionsArg {
    fn into_options(self) -> Result<ParseOptions, String> {
        let mut options = ParseOptions::default();
        if let Some(value) = self.slow_threshold_ms {
            if !value.is_finite() || value < 0.0 {
                return Err(invalid(
                    "options.slow_threshold_ms must be a finite number >= 0",
                ));
            }
            options.slow_threshold_ms = value;
        }
        if let Some(value) = self.n_plus_one_threshold {
            if value < 2 {
                return Err(invalid("options.n_plus_one_threshold must be at least 2"));
            }
            options.n_plus_one_threshold = value;
        }
        if let Some(value) = self.ignore_params_in_dedupe {
            options.ignore_params_in_dedupe = value;
        }
        if let Some(value) = self.dedupe_window_ms {
            if value < 0 {
                return Err(invalid("options.dedupe_window_ms must be >= 0"));
            }
            options.dedupe_window_ms = Some(value);
        }
        if let Some(value) = self.max_sql_len {
            if value < 100 {
                return Err(invalid("options.max_sql_len must be at least 100"));
            }
            options.max_sql_len = value;
        }
        if let Some(value) = self.timestamp_offset_minutes {
            options.timestamp_offset_minutes = value;
        }
        if let Some(value) = self.recognize_bare_sql {
            options.recognize_bare_sql = value;
        }
        Ok(options)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    text: String,
    #[serde(default)]
    options: Option<OptionsArg>,
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    if args.text.trim().is_empty() {
        return invalid("text must not be empty");
    }
    let options = match args.options {
        Some(options) => match options.into_options() {
            Ok(options) => options,
            Err(failure) => return failure,
        },
        None => ParseOptions::default(),
    };
    let analysis = parse(&args.text, &options);
    let confidence = overall_confidence(&analysis);
    let data = serde_json::to_value(&analysis).unwrap_or(Value::Null);
    if analysis.entries.is_empty() {
        success_with(
            confidence,
            data,
            &[("message_keys", json!(["log.parse.no_entries"]))],
        )
    } else {
        success(confidence, data)
    }
}

fn overall_confidence(analysis: &LogAnalysis) -> &'static str {
    if analysis.entries.is_empty()
        || analysis
            .entries
            .iter()
            .all(|entry| entry.confidence == LogConfidence::Unresolved)
    {
        return "unresolved";
    }
    let degraded = analysis
        .entries
        .iter()
        .any(|entry| entry.confidence != LogConfidence::Exact)
        || !analysis.unparsed_lines.is_empty();
    if degraded { "partial" } else { "exact" }
}
