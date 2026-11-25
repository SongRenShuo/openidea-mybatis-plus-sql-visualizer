//! `sqlv_schema_context`: compact schema text for agent prompts.
//!
//! The caller supplies the schema snapshot (tables, columns, indexes); this
//! tool only formats it, so the result is `exact`. Getting the snapshot is the
//! caller's job because the platform's `database` surface does not execute SQL
//! or introspect.

use serde::Deserialize;
use serde_json::json;

use crate::mcp::contract::{invalid, parse_args, success};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaArg {
    tables: Vec<TableSchema>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TableSchema {
    name: String,
    #[serde(default)]
    schema: Option<String>,
    columns: Vec<ColumnSchema>,
    #[serde(default)]
    indexes: Vec<IndexSchema>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ColumnSchema {
    name: String,
    #[serde(rename = "type", default)]
    data_type: Option<String>,
    #[serde(default)]
    nullable: Option<bool>,
    #[serde(default)]
    primary_key: bool,
    #[serde(default)]
    auto_increment: bool,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexSchema {
    name: String,
    columns: Vec<String>,
    #[serde(default)]
    unique: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    schema: SchemaArg,
}

pub fn run(arguments: &str) -> String {
    let args = match parse_args::<Args>(arguments) {
        Ok(args) => args,
        Err(failure) => return failure,
    };
    if let Err(reason) = validate(&args.schema) {
        return invalid(reason);
    }
    let (text, columns, indexes) = format_schema(&args.schema);
    success(
        "exact",
        json!({
            "text": text,
            "tables": args.schema.tables.len(),
            "columns": columns,
            "indexes": indexes,
        }),
    )
}

fn validate(schema: &SchemaArg) -> Result<(), String> {
    if schema.tables.is_empty() {
        return Err("schema.tables must contain at least one table".to_string());
    }
    for table in &schema.tables {
        if table.name.trim().is_empty() {
            return Err("table name must not be empty".to_string());
        }
        if table.columns.is_empty() {
            return Err(format!("table {} needs at least one column", table.name));
        }
        for column in &table.columns {
            if column.name.trim().is_empty() {
                return Err(format!("table {} has a column without a name", table.name));
            }
        }
        for index in &table.indexes {
            if index.name.trim().is_empty() {
                return Err(format!("table {} has an index without a name", table.name));
            }
            if index.columns.is_empty() || index.columns.iter().any(|c| c.trim().is_empty()) {
                return Err(format!(
                    "index {} needs at least one non-empty column",
                    index.name
                ));
            }
        }
    }
    Ok(())
}

fn format_schema(schema: &SchemaArg) -> (String, usize, usize) {
    let mut text = String::new();
    let mut columns = 0usize;
    let mut indexes = 0usize;
    for table in &schema.tables {
        if !text.is_empty() {
            text.push('\n');
        }
        let qualified = match &table.schema {
            Some(schema) => format!("{schema}.{}", table.name),
            None => table.name.clone(),
        };
        text.push_str(&format!(
            "TABLE {qualified} ({} columns)\n",
            table.columns.len()
        ));
        for column in &table.columns {
            text.push_str("  ");
            text.push_str(&column.name);
            if let Some(data_type) = &column.data_type {
                text.push(' ');
                text.push_str(data_type);
            }
            match column.nullable {
                Some(true) => text.push_str(" NULL"),
                Some(false) => text.push_str(" NOT NULL"),
                None => {}
            }
            if column.primary_key {
                text.push_str(" PK");
            }
            if column.auto_increment {
                text.push_str(" AUTO");
            }
            if let Some(comment) = &column.comment {
                text.push_str(" -- ");
                text.push_str(comment);
            }
            text.push('\n');
            columns += 1;
        }
        for index in &table.indexes {
            text.push_str(&format!(
                "  INDEX {} ({}){}\n",
                index.name,
                index.columns.join(", "),
                if index.unique { " UNIQUE" } else { "" }
            ));
            indexes += 1;
        }
    }
    (text.trim_end().to_string(), columns, indexes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_columns_and_indexes() {
        let schema = SchemaArg {
            tables: vec![TableSchema {
                name: "user".into(),
                schema: Some("app".into()),
                columns: vec![
                    ColumnSchema {
                        name: "id".into(),
                        data_type: Some("bigint".into()),
                        nullable: Some(false),
                        primary_key: true,
                        auto_increment: true,
                        comment: None,
                    },
                    ColumnSchema {
                        name: "name".into(),
                        data_type: Some("varchar(64)".into()),
                        nullable: Some(true),
                        primary_key: false,
                        auto_increment: false,
                        comment: Some("display name".into()),
                    },
                ],
                indexes: vec![IndexSchema {
                    name: "idx_user_name".into(),
                    columns: vec!["name".into()],
                    unique: false,
                }],
            }],
        };
        let (text, columns, indexes) = format_schema(&schema);
        assert!(text.starts_with("TABLE app.user (2 columns)"));
        assert!(text.contains("id bigint NOT NULL PK AUTO"));
        assert!(text.contains("name varchar(64) NULL -- display name"));
        assert!(text.contains("INDEX idx_user_name (name)"));
        assert_eq!(columns, 2);
        assert_eq!(indexes, 1);
    }
}
