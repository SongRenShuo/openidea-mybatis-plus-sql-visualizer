//! MCP tool implementations (T6-1~T6-3).
//!
//! Lane L9 owns this directory; see `docs/TASKS.md` P6 and
//! `docs/registration/L9.md` for the manifest entries.
//!
//! The surface is deliberately thin: [`TOOLS`] declares the twelve tools,
//! [`call_tool`] dispatches by local name, and each handler lives in
//! [`handlers`] as a plain function over the raw argument JSON. No handler
//! calls `host::`, so every case is unit-testable; the `McpProvider`
//! implementation below is only the shell the platform calls.
//!
//! Every result carries `confidence`. `partial` is never reported as `exact`,
//! and an `unresolved` result never carries SQL (the core guarantees it and the
//! render handlers re-check it). Nothing in this lane writes a database:
//! `read_only` is true for every tool and `destructive` is false for every
//! tool, including the generators, which only produce text.

mod contract;
pub mod handlers;
#[cfg(test)]
mod tests;

use openidea_plugin::{Error, Invocation};

pub use contract::ToolSpec;

/// The twelve declared tools, in registration order.
///
/// Local names are `sqlv_`-prefixed because the registered name is
/// `songrenshuo_mybatis_plus_sql_visualizer__<name>` and the whole thing must
/// fit 64 bytes. The longest local name here is 19 bytes.
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "sqlv_render_sql",
        description: "Render one statement to SQL with ? placeholders and bound parameters; kind selects mapper XML (default), a statement annotation, an iBatis 2 sqlMap or a wrapper chain.",
        schema_path: "src/mcp/schemas/sqlv_render_sql.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_render_logs",
        description: "Parse MyBatis, p6spy, Hibernate or JdbcTemplate log text into statements, duplicates and N+1 groups.",
        schema_path: "src/mcp/schemas/sqlv_render_logs.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_check_sql",
        description: "Classify each SQL statement in the text, flag unbounded UPDATE/DELETE and destructive DDL across all of them, and list performance findings.",
        schema_path: "src/mcp/schemas/sqlv_check_sql.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_perf_advice",
        description: "Run performance rules, a 0-100 complexity score and index advice on each SQL statement of the text.",
        schema_path: "src/mcp/schemas/sqlv_perf_advice.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_crud_sql",
        description: "Generate select/insert/update/delete/batch-insert SQL for a table description.",
        schema_path: "src/mcp/schemas/sqlv_crud_sql.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_rollback_plan",
        description: "Build a rollback plan (snapshot plus reverse statements) for an UPDATE or DELETE; refused without a primary key.",
        schema_path: "src/mcp/schemas/sqlv_rollback_plan.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_reverse_sql",
        description: "Translate a single-table SELECT into a MyBatis-Plus LambdaQueryWrapper chain; unsupported shapes are reported as unresolved, never guessed.",
        schema_path: "src/mcp/schemas/sqlv_reverse_sql.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_mapper_check",
        description: "Load mapper XML documents and report duplicate ids, dangling includes, missing resultMaps and unparseable files.",
        schema_path: "src/mcp/schemas/sqlv_mapper_check.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_history_search",
        description: "Search the log text supplied in this call for lines containing a query; there is no IDE history access.",
        schema_path: "src/mcp/schemas/sqlv_history_search.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_schema_context",
        description: "Format a caller-supplied schema (tables, columns, indexes) as compact DDL-like text for an agent prompt.",
        schema_path: "src/mcp/schemas/sqlv_schema_context.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_explain",
        description: "Format an EXPLAIN result supplied by the caller as a tree or table; this tool never executes SQL.",
        schema_path: "src/mcp/schemas/sqlv_explain.schema.json",
        read_only: true,
        destructive: false,
    },
    ToolSpec {
        name: "sqlv_index_stats",
        description: "Report what the plugin's own Java/XML indexes hold and what indexing them cost (documents, bytes, total and slowest document).",
        schema_path: "src/mcp/schemas/sqlv_index_stats.schema.json",
        read_only: true,
        destructive: false,
    },
];

/// Calls one tool by local name.
///
/// Argument and domain errors are returned as the shared failure envelope
/// (`{"ok":false,…}`) so the agent always gets a readable body; the `Result`
/// only exists to match the `McpProvider` signature.
pub fn call_tool(tool: &str, arguments: &str) -> Result<String, Error> {
    Ok(dispatch(tool, arguments))
}

fn dispatch(tool: &str, arguments: &str) -> String {
    match tool {
        "sqlv_render_sql" => handlers::render_sql::run(arguments),
        "sqlv_render_logs" => handlers::render_logs::run(arguments),
        "sqlv_check_sql" => handlers::check_sql::run(arguments),
        "sqlv_perf_advice" => handlers::perf_advice::run(arguments),
        "sqlv_crud_sql" => handlers::crud_sql::run(arguments),
        "sqlv_rollback_plan" => handlers::rollback_plan::run(arguments),
        "sqlv_reverse_sql" => handlers::reverse_sql::run(arguments),
        "sqlv_mapper_check" => handlers::mapper_check::run(arguments),
        "sqlv_history_search" => handlers::history_search::run(arguments),
        "sqlv_schema_context" => handlers::schema_context::run(arguments),
        "sqlv_explain" => handlers::explain::run(arguments),
        "sqlv_index_stats" => handlers::index_stats::run(arguments),
        other => contract::failure(
            contract::CODE_UNKNOWN_TOOL,
            contract::KEY_UNKNOWN_TOOL,
            serde_json::json!({
                "tool": other,
                "available": TOOLS.iter().map(|spec| spec.name).collect::<Vec<_>>(),
            }),
        ),
    }
}

/// The platform-facing shell. All work happens in [`call_tool`]; this does not
/// touch the invocation context because no tool needs project or editor data
/// in this version (the caller passes everything as arguments).
impl openidea_plugin::McpProvider for crate::Main {
    fn call(&mut self, tool: &str, arguments: &str, _ctx: &Invocation) -> Result<String, Error> {
        call_tool(tool, arguments)
    }
}
