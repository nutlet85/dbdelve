//! What the structure editor writes, per engine, and whether the gate that
//! admits it agrees.
//!
//! Every engine's batch is recorded beside the structure gate's verdict on each
//! statement, so a spelling the gate stops accepting fails here rather than in
//! front of someone editing a table.

use super::check;
use crate::db::{
    ColumnDefinition, Engine, Index, IndexKind, ParameterMode, RelationKind, Routine, RoutineKind,
    RoutineParameter, Syntax, Target, alter,
};
use crate::sql::{self, StructureScope};

const SCHEMA: &str = "public";
const TABLE: &str = "accounts";

fn column(name: &str, data_type: &str, nullable: bool, default: Option<&str>) -> ColumnDefinition {
    ColumnDefinition {
        name: name.into(),
        data_type: data_type.into(),
        nullable,
        default: default.map(str::to_string),
    }
}

fn index(name: &str, kind: IndexKind, columns: &[&str]) -> Index {
    Index {
        name: name.into(),
        kind,
        columns: columns.iter().map(|column| column.to_string()).collect(),
        predicate: None,
        definition: String::new(),
    }
}

fn kept(original: ColumnDefinition) -> alter::ColumnEdit {
    alter::ColumnEdit {
        name: original.name.clone(),
        data_type: original.data_type.clone(),
        nullable: original.nullable,
        default: original.default.clone().unwrap_or_default(),
        original: Some(original),
        using: String::new(),
        dropped: false,
    }
}

/// One edit carrying the operations every engine that edits structure writes:
/// a column added, one dropped, one renamed, and an index made and dropped.
/// The index rows are left out where the engine has none to draw.
fn common(engine: Engine) -> alter::TableEdit {
    alter::TableEdit {
        schema: SCHEMA.into(),
        table: TABLE.into(),
        relation_kind: RelationKind::Table,
        columns: vec![
            kept(column("id", "integer", false, None)),
            alter::ColumnEdit {
                original: Some(column("old_name", "integer", true, None)),
                name: "new_name".into(),
                data_type: "integer".into(),
                nullable: true,
                default: String::new(),
                using: String::new(),
                dropped: false,
            },
            alter::ColumnEdit {
                original: Some(column("gone", "text", true, None)),
                name: "gone".into(),
                data_type: "text".into(),
                nullable: true,
                default: String::new(),
                using: String::new(),
                dropped: true,
            },
            alter::ColumnEdit {
                original: None,
                name: "added".into(),
                data_type: "text".into(),
                nullable: false,
                default: "''".into(),
                using: String::new(),
                dropped: false,
            },
        ],
        indexes: match Target::built_in(engine).edits_indexes() {
            true => vec![
                alter::IndexEdit {
                    original: Some(index("drop_me", IndexKind::Plain, &["label"])),
                    name: "drop_me".into(),
                    unique: false,
                    columns: vec!["label".into()],
                    predicate: String::new(),
                    dropped: true,
                },
                alter::IndexEdit {
                    original: None,
                    name: "new_index".into(),
                    unique: true,
                    columns: vec!["added".into()],
                    predicate: String::new(),
                    dropped: false,
                },
            ],
            false => Vec::new(),
        },
    }
}

/// The changes that are not universal: a column's type, nullability and
/// default, and an index renamed and rekeyed.
fn changes(engine: Engine) -> alter::TableEdit {
    alter::TableEdit {
        schema: SCHEMA.into(),
        table: TABLE.into(),
        relation_kind: RelationKind::Table,
        columns: vec![alter::ColumnEdit {
            original: Some(column("label", "text", true, None)),
            name: "label".into(),
            data_type: "varchar(40)".into(),
            nullable: false,
            default: "'none'".into(),
            using: "\"label\"::varchar(40)".into(),
            dropped: false,
        }],
        indexes: match Target::built_in(engine).edits_indexes() {
            true => vec![
                alter::IndexEdit {
                    original: Some(index("rename_me", IndexKind::Plain, &["label"])),
                    name: "renamed_index".into(),
                    unique: false,
                    columns: vec!["label".into()],
                    predicate: String::new(),
                    dropped: false,
                },
                alter::IndexEdit {
                    original: Some(index("rekey_me", IndexKind::Plain, &["id"])),
                    name: "rekey_me".into(),
                    unique: true,
                    columns: vec!["id".into(), "label".into()],
                    predicate: String::new(),
                    dropped: false,
                },
            ],
            false => Vec::new(),
        },
    }
}

/// Every name the edits read or write, which is what the gate is given.
fn scope() -> StructureScope {
    StructureScope {
        schema: SCHEMA.into(),
        relation: TABLE.into(),
        names: [
            "id",
            "label",
            "old_name",
            "new_name",
            "gone",
            "added",
            "drop_me",
            "rename_me",
            "renamed_index",
            "rekey_me",
            "new_index",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    }
}

fn sql_engines() -> impl Iterator<Item = Engine> {
    Engine::ALL
        .into_iter()
        .filter(|engine| engine.syntax() == Syntax::Sql)
        .filter(|&engine| Target::built_in(engine).edits_structure(RelationKind::Table))
}

fn refused(out: &mut String, engine: Engine, case: &str, result: Result<Vec<String>, String>) {
    match result {
        Ok(statements) => {
            out.push_str(&format!(
                "\n[{}] {case} ran {} statements\n",
                engine.as_str(),
                statements.len()
            ));
        }
        Err(reason) => out.push_str(&format!("\n[{}] {case}\n  ! {reason}\n", engine.as_str())),
    }
}

fn recorded(out: &mut String, engine: Engine, case: &str, edit: &alter::TableEdit) {
    out.push_str(&format!("\n[{}] {case}\n", engine.as_str()));
    match alter::statements(engine, edit) {
        Ok(statements) => {
            for statement in statements {
                let admitted = sql::is_generated_structure(engine, &statement, &scope());
                out.push_str(&format!("  |{statement}|\n  = admitted:{admitted}\n"));
                // A statement dbdelve wrote and its own gate refuses is a bug,
                // and the one thing this file exists to catch.
                assert!(
                    admitted,
                    "{} refused its own statement: {statement}",
                    engine.as_str()
                );
            }
        }
        Err(reason) => out.push_str(&format!("  ! {reason}\n")),
    }
}

#[test]
fn structure_edits_per_engine() {
    let mut out = String::new();
    for engine in sql_engines() {
        recorded(&mut out, engine, "common operations", &common(engine));
        recorded(&mut out, engine, "changes", &changes(engine));
    }
    check("alters_per_engine", &out);
}

/// The operations the editor offers but an engine cannot write, and what it
/// says instead.
#[test]
fn structure_refusals_per_engine() {
    let mut out = String::new();

    let retyped = alter::TableEdit {
        schema: SCHEMA.into(),
        table: TABLE.into(),
        relation_kind: RelationKind::Table,
        columns: vec![alter::ColumnEdit {
            original: Some(column("label", "text", true, None)),
            name: "label".into(),
            data_type: "integer".into(),
            nullable: true,
            default: String::new(),
            using: String::new(),
            dropped: false,
        }],
        indexes: Vec::new(),
    };
    refused(
        &mut out,
        Engine::Sqlite,
        "retype a column",
        alter::statements(Engine::Sqlite, &retyped),
    );

    let partial = alter::TableEdit {
        schema: SCHEMA.into(),
        table: TABLE.into(),
        relation_kind: RelationKind::Table,
        columns: Vec::new(),
        indexes: vec![alter::IndexEdit {
            original: None,
            name: "partial".into(),
            unique: false,
            columns: vec!["label".into()],
            predicate: "label IS NOT NULL".into(),
            dropped: false,
        }],
    };
    for engine in [Engine::MySql, Engine::MariaDb] {
        refused(
            &mut out,
            engine,
            "a partial index",
            alter::statements(engine, &partial),
        );
    }

    let defaulted = alter::TableEdit {
        schema: SCHEMA.into(),
        table: TABLE.into(),
        relation_kind: RelationKind::Table,
        columns: vec![alter::ColumnEdit {
            original: Some(column("label", "text", true, Some("'old'"))),
            name: "label".into(),
            data_type: "text".into(),
            nullable: true,
            default: "'new'".into(),
            using: String::new(),
            dropped: false,
        }],
        indexes: Vec::new(),
    };
    refused(
        &mut out,
        Engine::SqlServer,
        "replace a default",
        alter::statements(Engine::SqlServer, &defaulted),
    );

    let nulled = alter::TableEdit {
        schema: SCHEMA.into(),
        table: TABLE.into(),
        relation_kind: RelationKind::Table,
        columns: vec![alter::ColumnEdit {
            original: Some(column("label", "text", true, None)),
            name: "label".into(),
            data_type: "text".into(),
            nullable: false,
            default: String::new(),
            using: String::new(),
            dropped: false,
        }],
        indexes: Vec::new(),
    };
    refused(
        &mut out,
        Engine::ClickHouse,
        "change nullability",
        alter::statements(Engine::ClickHouse, &nulled),
    );

    let primary = alter::TableEdit {
        schema: SCHEMA.into(),
        table: TABLE.into(),
        relation_kind: RelationKind::Table,
        columns: Vec::new(),
        indexes: vec![alter::IndexEdit {
            original: Some(index("accounts_pkey", IndexKind::Primary, &["id"])),
            name: "accounts_pkey".into(),
            unique: true,
            columns: vec!["id".into()],
            predicate: String::new(),
            dropped: true,
        }],
    };
    for engine in [Engine::Postgres, Engine::MySql, Engine::SqlServer] {
        refused(
            &mut out,
            engine,
            "drop the primary key's index",
            alter::statements(engine, &primary),
        );
    }

    check("alters_refusals", &out);
}

/// A routine's call, in the four engines that offer one, so the statement the
/// Run dialog writes is on record too.
#[test]
fn routine_calls_per_engine() {
    let routine = |kind: RoutineKind, result_type: &str| Routine {
        name: "total".into(),
        kind,
        identity_arguments: "integer, text".into(),
        result_type: result_type.into(),
        language: "sql".into(),
        definition: String::new(),
    };
    let arguments = vec![
        (
            RoutineParameter {
                name: "a".into(),
                data_type: "integer".into(),
                mode: ParameterMode::In,
            },
            "7".into(),
        ),
        (
            RoutineParameter {
                name: "b".into(),
                data_type: "text".into(),
                mode: ParameterMode::InOut,
            },
            "it's".into(),
        ),
    ];
    let mut out = String::new();
    for (engine, kind, result_type, case) in [
        (
            Engine::Postgres,
            RoutineKind::Function,
            "bigint",
            "function",
        ),
        (Engine::Postgres, RoutineKind::Procedure, "", "procedure"),
        (Engine::MySql, RoutineKind::Function, "bigint", "function"),
        (Engine::MySql, RoutineKind::Procedure, "", "procedure"),
        (
            Engine::SqlServer,
            RoutineKind::Function,
            "TABLE",
            "table function",
        ),
        (Engine::SqlServer, RoutineKind::Procedure, "", "procedure"),
        (
            Engine::Snowflake,
            RoutineKind::Function,
            "NUMBER",
            "function",
        ),
        (Engine::Snowflake, RoutineKind::Procedure, "", "procedure"),
    ] {
        let text = alter::call_statement(engine, SCHEMA, &routine(kind, result_type), &arguments);
        out.push_str(&format!("[{}] {case}\n  |{text}|\n", engine.as_str()));
    }
    check("alters_routine_calls", &out);
}
