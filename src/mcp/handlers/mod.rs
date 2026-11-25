//! One module per declared tool; each `run` takes the raw argument JSON and
//! returns the response JSON (see `super::contract` for the envelopes).

pub(crate) mod check_sql;
pub(crate) mod crud_sql;
pub(crate) mod explain;
pub(crate) mod history_search;
pub(crate) mod index_stats;
pub(crate) mod mapper_check;
pub(crate) mod perf_advice;
pub(crate) mod render_logs;
pub(crate) mod render_sql;
pub(crate) mod reverse_sql;
pub(crate) mod rollback_plan;
pub(crate) mod schema_context;
