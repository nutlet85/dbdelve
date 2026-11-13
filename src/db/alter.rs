//! The SQL a structure edit is written as, and the statement a routine is
//! called with.
//!
//! Every spelling lives here, beside the drivers, so that no view has to match
//! on an engine (hard rule 4). Each function is pure: it takes what the editor
//! holds and returns text, or the sentence saying why this engine cannot write
//! it -- which the editor shows and disables Apply for. Nothing here runs
//! anything: the batch goes through the mode gate like any other statement.
//!
//! Two things are deliberate. The batch is ordered so that each statement's
//! dependencies already hold -- an index goes before the column it names, a
//! rename before the alteration that uses the new name -- and it is split into
//! separate statements rather than one multi-operation `ALTER TABLE`, because
//! MySQL's and SQLite's `ALTER` take one operation each.

use rust_i18n::t;

use super::{
    ColumnDefinition, Engine, Index, IndexKind, RelationKind, Routine, RoutineKind,
    RoutineParameter, Syntax,
};

/// One column as the editor holds it: the row the server gave, if there is one,
/// and what the fields now say.
pub struct ColumnEdit {
    /// `None` for a column being added.
    pub original: Option<ColumnDefinition>,
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    /// Empty for no default.
    pub default: String,
    /// The `USING` expression a type change carries, empty for none. Only
    /// Postgres and DuckDB read one.
    pub using: String,
    /// A removed row stays on screen, greyed, so the removal can be taken back
    /// without reloading the table.
    pub dropped: bool,
}

/// One index as the editor holds it.
pub struct IndexEdit {
    /// `None` for an index being created.
    pub original: Option<Index>,
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
    /// A partial index's condition, empty for none.
    pub predicate: String,
    pub dropped: bool,
}

/// Everything one Apply covers.
pub struct TableEdit {
    pub schema: String,
    pub table: String,
    pub relation_kind: RelationKind,
    pub columns: Vec<ColumnEdit>,
    pub indexes: Vec<IndexEdit>,
}

/// The statements one Apply runs, already `;`-terminated and in the order they
/// must run. `Err` is the reason this engine cannot write the edit as it
/// stands, in the words the dialog shows.
pub fn statements(engine: Engine, edit: &TableEdit) -> Result<Vec<String>, String> {
    if engine.syntax() != Syntax::Sql {
        return Err(t!("structure.refuse_not_sql").to_string());
    }
    if !matches!(
        edit.relation_kind,
        RelationKind::Table | RelationKind::PartitionedTable
    ) {
        return Err(t!("structure.refuse_not_table").to_string());
    }
    let mut batch = Vec::new();
    let table = engine.qualified(&edit.schema, &edit.table);

    for index in &edit.indexes {
        if index.removed() {
            batch.push(index_drop(engine, edit, index)?);
        }
    }
    for column in &edit.columns {
        if column.renamed() {
            batch.push(column_rename(engine, &table, column)?);
        }
    }
    for column in &edit.columns {
        if column.added() {
            batch.push(column_add(engine, &table, column)?);
        }
    }
    for column in &edit.columns {
        if column.altered() {
            batch.extend(column_alter(engine, &table, column)?);
        }
    }
    for column in &edit.columns {
        if column.removed() {
            batch.push(format!(
                "ALTER TABLE {table} DROP COLUMN {};",
                engine.quote_identifier(&column.original_name())
            ));
        }
    }
    for index in &edit.indexes {
        if index.renamed() {
            batch.extend(index_rename(engine, edit, &table, index)?);
        }
    }
    for index in &edit.indexes {
        if index.created() {
            batch.push(index_create(engine, edit, &table, index)?);
        } else if index.rebuilt() {
            batch.extend(index_rebuild(engine, edit, &table, index)?);
        }
    }
    Ok(batch)
}

/// What running the batch means for the data, where the engine cannot promise
/// the batch is atomic. `None` where there is nothing to say.
pub fn execution_note(engine: Engine) -> Option<String> {
    match engine {
        // One simple-query submission is one implicit transaction there.
        Engine::Postgres => Some(t!("structure.note_atomic").to_string()),
        _ => Some(t!("structure.note_separate").to_string()),
    }
}

/// A type split where its parameter list begins: `varchar(80)` is
/// `("varchar", "(80)")`, `Nullable(String)` is `("Nullable", "(String)")`,
/// and a type with no list comes back whole.
///
/// The split is at the first `(`, which is what the editor's picker offers and
/// its parameter field holds. Nothing here reads the type: the server is the
/// one that says whether the pair means anything.
pub fn split_type(data_type: &str) -> (String, String) {
    match data_type.find('(') {
        Some(open) => (
            data_type[..open].trim().to_string(),
            data_type[open..].trim().to_string(),
        ),
        None => (data_type.trim().to_string(), String::new()),
    }
}

/// [`split_type`]'s inverse.
pub fn join_type(base: &str, params: &str) -> String {
    format!("{}{}", base.trim(), params.trim())
}

impl ColumnEdit {
    fn original_name(&self) -> String {
        self.original
            .as_ref()
            .map(|column| column.name.clone())
            .unwrap_or_else(|| self.name.clone())
    }

    fn added(&self) -> bool {
        self.original.is_none() && !self.dropped
    }

    fn renamed(&self) -> bool {
        !self.dropped
            && self
                .original
                .as_ref()
                .is_some_and(|column| column.name != self.name)
    }

    fn removed(&self) -> bool {
        self.dropped && self.original.is_some()
    }

    fn type_changed(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|column| column.data_type != self.data_type)
    }

    fn nullability_changed(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|column| column.nullable != self.nullable)
    }

    fn original_default(&self) -> &str {
        self.original
            .as_ref()
            .and_then(|column| column.default.as_deref())
            .unwrap_or("")
    }

    fn default_changed(&self) -> bool {
        self.original.is_some() && self.original_default() != self.default
    }

    fn altered(&self) -> bool {
        !self.dropped
            && self.original.is_some()
            && (self.type_changed() || self.nullability_changed() || self.default_changed())
    }
}

impl IndexEdit {
    fn original_name(&self) -> String {
        self.original
            .as_ref()
            .map(|index| index.name.clone())
            .unwrap_or_else(|| self.name.clone())
    }

    fn created(&self) -> bool {
        self.original.is_none() && !self.dropped
    }

    fn removed(&self) -> bool {
        self.dropped && self.original.is_some()
    }

    fn renamed(&self) -> bool {
        !self.dropped
            && self
                .original
                .as_ref()
                .is_some_and(|index| index.name != self.name)
    }

    /// Whether the key or the uniqueness changed under the same name, which no
    /// engine alters in place: `ALTER INDEX … RENAME` renames and nothing else.
    fn rebuilt(&self) -> bool {
        let Some(original) = self.original.as_ref().filter(|_| !self.dropped) else {
            return false;
        };
        if self.renamed() {
            return false;
        }
        (original.kind == IndexKind::Unique) != self.unique
            || index_key(&original.columns) != index_key(&self.columns)
    }

    /// Why this index is not the editor's to change, when it is one the server
    /// made for a constraint.
    fn refusal(&self) -> Option<String> {
        let primary = self
            .original
            .as_ref()
            .is_some_and(|index| index.kind == IndexKind::Primary);
        (primary && (self.dropped || self.renamed() || self.rebuilt()))
            .then(|| t!("structure.refuse_primary").to_string())
    }
}

/// An index's key compared the way the editor holds it: one entry per column,
/// in order.
fn index_key(columns: &[String]) -> Vec<String> {
    columns
        .iter()
        .map(|column| column.trim().to_string())
        .collect()
}

fn index_columns(index: &IndexEdit) -> String {
    index_key(&index.columns).join(", ")
}

fn column_add(engine: Engine, table: &str, column: &ColumnEdit) -> Result<String, String> {
    let name = engine.quote_identifier(&column.name);
    let data_type = &column.data_type;
    let keyword = match engine {
        Engine::SqlServer => "ADD",
        _ => "ADD COLUMN",
    };
    let default = match column.default.is_empty() {
        true => String::new(),
        false => format!(" DEFAULT {}", column.default),
    };
    let null = match column.nullable {
        true => "",
        false => " NOT NULL",
    };
    let clause = match engine {
        Engine::Postgres | Engine::DuckDb | Engine::Snowflake | Engine::Sqlite => {
            format!("{name} {data_type}{default}{null}")
        }
        Engine::MySql | Engine::MariaDb | Engine::SqlServer => {
            format!("{name} {data_type}{null}{default}")
        }
        Engine::ClickHouse => format!("{name} {data_type}{default}"),
        _ => return Err(t!("structure.refuse_columns").to_string()),
    };
    if engine == Engine::Sqlite && !column.nullable && column.default.is_empty() {
        // SQLite rejects it because the rows already in the table have no value
        // for the new column.
        return Err(t!("structure.refuse_sqlite_add").to_string());
    }
    Ok(format!("ALTER TABLE {table} {keyword} {clause};"))
}

fn column_rename(engine: Engine, table: &str, column: &ColumnEdit) -> Result<String, String> {
    let old = engine.quote_identifier(&column.original_name());
    let new = engine.quote_identifier(&column.name);
    Ok(match engine {
        Engine::SqlServer => format!(
            "EXEC sp_rename {}, {}, {};",
            engine.quote_literal(&member_path(table, &column.original_name())),
            engine.quote_literal(&column.name),
            engine.quote_literal("COLUMN"),
        ),
        _ => format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new};"),
    })
}

/// The `object` argument `sp_rename` takes: the relation as it must be written
/// there, then the member's own name.
fn member_path(table: &str, member: &str) -> String {
    format!(
        "{}.{member}",
        table.replace('"', "").replace(['[', ']'], "")
    )
}

fn column_alter(engine: Engine, table: &str, column: &ColumnEdit) -> Result<Vec<String>, String> {
    let name = engine.quote_identifier(&column.name);
    let data_type = &column.data_type;
    let mut batch = Vec::new();
    match engine {
        Engine::Postgres | Engine::DuckDb => {
            if column.type_changed() {
                let using = match column.using.trim() {
                    "" => String::new(),
                    expression => format!(" USING {expression}"),
                };
                batch.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {name} TYPE {data_type}{using};"
                ));
            }
            if column.nullability_changed() {
                let op = match column.nullable {
                    true => "DROP NOT NULL",
                    false => "SET NOT NULL",
                };
                batch.push(format!("ALTER TABLE {table} ALTER COLUMN {name} {op};"));
            }
            if column.default_changed() {
                let op = match column.default.is_empty() {
                    true => "DROP DEFAULT".to_string(),
                    false => format!("SET DEFAULT {}", column.default),
                };
                batch.push(format!("ALTER TABLE {table} ALTER COLUMN {name} {op};"));
            }
        }
        Engine::MySql | Engine::MariaDb => {
            // One statement, because `MODIFY COLUMN` restates the whole
            // definition: the parts that did not change are written back as
            // they were read.
            let null = match column.nullable {
                true => "",
                false => " NOT NULL",
            };
            let default = match column.default.is_empty() {
                true => String::new(),
                false => format!(" DEFAULT {}", column.default),
            };
            batch.push(format!(
                "ALTER TABLE {table} MODIFY COLUMN {name} {data_type}{null}{default};"
            ));
        }
        Engine::Sqlite => {
            return Err(t!("structure.refuse_sqlite_change").to_string());
        }
        Engine::SqlServer => {
            if column.type_changed() || column.nullability_changed() {
                let null = match column.nullable {
                    true => " NULL",
                    false => " NOT NULL",
                };
                batch.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {name} {data_type}{null};"
                ));
            }
            if column.default_changed() {
                // A default there is a constraint with a name of its own, and
                // the catalog hands back the expression rather than the name it
                // is bound to, so an existing one cannot be replaced or dropped.
                if !column.original_default().is_empty() {
                    return Err(t!("structure.refuse_mssql_default").to_string());
                }
                batch.push(format!(
                    "ALTER TABLE {table} ADD DEFAULT {} FOR {name};",
                    column.default
                ));
            }
        }
        Engine::Snowflake => {
            if column.type_changed() {
                batch.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {name} SET DATA TYPE {data_type};"
                ));
            }
            if column.nullability_changed() {
                let op = match column.nullable {
                    true => "DROP NOT NULL",
                    false => "SET NOT NULL",
                };
                batch.push(format!("ALTER TABLE {table} ALTER COLUMN {name} {op};"));
            }
            if column.default_changed() {
                let op = match column.default.is_empty() {
                    true => "DROP DEFAULT".to_string(),
                    false => format!("SET DEFAULT {}", column.default),
                };
                batch.push(format!("ALTER TABLE {table} ALTER COLUMN {name} {op};"));
            }
        }
        Engine::ClickHouse => {
            if column.nullability_changed() {
                return Err(t!("structure.refuse_clickhouse").to_string());
            }
            if column.type_changed() || column.default_changed() {
                let default = match column.default.is_empty() {
                    true => String::new(),
                    false => format!(" DEFAULT {}", column.default),
                };
                batch.push(format!(
                    "ALTER TABLE {table} MODIFY COLUMN {name} {data_type}{default};"
                ));
            }
        }
        _ => return Err(t!("structure.refuse_columns").to_string()),
    }
    Ok(batch)
}

/// The `WHERE` a partial index carries, or the refusal of the engines that have
/// none.
fn index_predicate(engine: Engine, index: &IndexEdit) -> Result<String, String> {
    let predicate = index.predicate.trim();
    if predicate.is_empty() {
        return Ok(String::new());
    }
    match engine {
        Engine::Postgres | Engine::Sqlite | Engine::SqlServer => Ok(format!(" WHERE {predicate}")),
        Engine::MySql | Engine::MariaDb => Err(t!("structure.refuse_partial_mysql").to_string()),
        _ => Err(t!("structure.refuse_partial").to_string()),
    }
}

fn index_create(
    engine: Engine,
    edit: &TableEdit,
    table: &str,
    index: &IndexEdit,
) -> Result<String, String> {
    let unique = match index.unique {
        true => "UNIQUE ",
        false => "",
    };
    let name = engine.quote_identifier(&index.name);
    let columns = index_columns(index);
    let predicate = index_predicate(engine, index)?;
    Ok(match engine {
        // Its index names live in the schema, unlike Postgres's, so the name is
        // written three parts deep.
        Engine::Sqlite => format!(
            "CREATE {unique}INDEX {} ON {table} ({columns}){predicate};",
            engine.qualified(&edit.schema, &index.name),
        ),
        Engine::Postgres | Engine::DuckDb | Engine::SqlServer => {
            format!("CREATE {unique}INDEX {name} ON {table} ({columns}){predicate};")
        }
        Engine::MySql | Engine::MariaDb => {
            format!("CREATE {unique}INDEX {name} ON {table} ({columns}){predicate};")
        }
        _ => return Err(t!("structure.refuse_index_create").to_string()),
    })
}

fn index_drop(engine: Engine, edit: &TableEdit, index: &IndexEdit) -> Result<String, String> {
    if let Some(refusal) = index.refusal() {
        return Err(refusal);
    }
    let name = engine.quote_identifier(&index.original_name());
    let table = engine.qualified(&edit.schema, &edit.table);
    Ok(match engine {
        Engine::Postgres | Engine::Sqlite | Engine::DuckDb => format!(
            "DROP INDEX {};",
            engine.qualified(&edit.schema, &index.original_name())
        ),
        Engine::MySql | Engine::MariaDb | Engine::SqlServer => {
            format!("DROP INDEX {name} ON {table};")
        }
        _ => return Err(t!("structure.refuse_index_drop").to_string()),
    })
}

fn index_rename(
    engine: Engine,
    edit: &TableEdit,
    table: &str,
    index: &IndexEdit,
) -> Result<Vec<String>, String> {
    if let Some(refusal) = index.refusal() {
        return Err(refusal);
    }
    let old = index.original_name();
    let new = engine.quote_identifier(&index.name);
    Ok(match engine {
        Engine::Postgres => vec![format!(
            "ALTER INDEX {} RENAME TO {new};",
            engine.qualified(&edit.schema, &old)
        )],
        Engine::MySql | Engine::MariaDb => vec![format!(
            "ALTER TABLE {table} RENAME INDEX {} TO {new};",
            engine.quote_identifier(&old)
        )],
        Engine::SqlServer => vec![format!(
            "EXEC sp_rename {}, {}, {};",
            engine.quote_literal(&member_path(table, &old)),
            engine.quote_literal(&index.name),
            engine.quote_literal("INDEX"),
        )],
        // Neither has `ALTER INDEX`: the index goes and is made again, which is
        // also what changing its key costs there.
        Engine::Sqlite | Engine::DuckDb => {
            let mut batch = vec![index_drop(engine, edit, index)?];
            batch.push(index_create(engine, edit, table, index)?);
            batch
        }
        _ => return Err(t!("structure.refuse_index_rename").to_string()),
    })
}

fn index_rebuild(
    engine: Engine,
    edit: &TableEdit,
    table: &str,
    index: &IndexEdit,
) -> Result<Vec<String>, String> {
    if let Some(refusal) = index.refusal() {
        return Err(refusal);
    }
    Ok(match engine {
        // One `ALTER` takes both halves, which is why MySQL does not pay two
        // scans for a changed key.
        Engine::MySql | Engine::MariaDb => {
            let unique = match index.unique {
                true => "UNIQUE ",
                false => "",
            };
            let name = engine.quote_identifier(&index.name);
            let columns = index_columns(index);
            vec![format!(
                "ALTER TABLE {table} DROP INDEX {name}, ADD {unique}INDEX {name} ({columns});"
            )]
        }
        Engine::Postgres | Engine::DuckDb | Engine::Sqlite | Engine::SqlServer => vec![
            index_drop(engine, edit, index)?,
            index_create(engine, edit, table, index)?,
        ],
        _ => return Err(t!("structure.refuse_index_change").to_string()),
    })
}

/// The server's own `CREATE`, rewritten into a statement that replaces the
/// object it names. `None` where this engine has no such spelling, or the
/// definition is one whose keyword dbdelve cannot find.
pub fn view_replacement(engine: Engine, kind: RelationKind, definition: &str) -> Option<String> {
    // A materialized view is refreshed or dropped and made again; no engine
    // here has `CREATE OR REPLACE MATERIALIZED VIEW`.
    if matches!(kind, RelationKind::MaterializedView) {
        return None;
    }
    match engine {
        Engine::Postgres | Engine::DuckDb | Engine::Snowflake => or_replace(definition),
        Engine::MySql | Engine::MariaDb | Engine::SqlServer => respell(definition, "ALTER"),
        _ => None,
    }
}

/// [`view_replacement`] for a routine.
pub fn routine_replacement(engine: Engine, definition: &str) -> Option<String> {
    match engine {
        // `pg_get_functiondef` already writes `CREATE OR REPLACE`.
        Engine::Postgres => Some(definition.to_string()),
        Engine::MariaDb | Engine::Snowflake => or_replace(definition),
        Engine::SqlServer => respell(definition, "ALTER"),
        _ => None,
    }
}

/// The definition with its `CREATE` replaced by `with`, found through the
/// grammar rather than by searching the text: a `CREATE` inside a body or a
/// comment is not the statement's own keyword.
fn respell(definition: &str, with: &str) -> Option<String> {
    let span = crate::sql::create_keyword_span(definition)?;
    Some(format!(
        "{}{with}{}",
        &definition[..span.start],
        &definition[span.end..]
    ))
}

/// The definition with `OR REPLACE` added after its `CREATE`, unless it reads
/// that way already.
fn or_replace(definition: &str) -> Option<String> {
    let span = crate::sql::create_keyword_span(definition)?;
    let rest = definition[span.end..].trim_start();
    if rest
        .get(..10)
        .is_some_and(|head| head.eq_ignore_ascii_case("OR REPLACE"))
    {
        return Some(definition.to_string());
    }
    Some(format!(
        "{}CREATE OR REPLACE{}",
        &definition[..span.start],
        &definition[span.end..]
    ))
}

/// The statement calling `routine` with `arguments`, in the engine's own
/// spelling. An empty value is a `NULL`, and a value typed as `NULL` is left as
/// the keyword, so a parameter of any type can take one.
pub fn call_statement(
    engine: Engine,
    schema: &str,
    routine: &Routine,
    arguments: &[(RoutineParameter, String)],
) -> String {
    let name = engine.qualified(schema, &routine.name);
    let values: Vec<String> = arguments
        .iter()
        .map(|(parameter, value)| literal(engine, &parameter.data_type, value))
        .collect();
    let positional = values.join(", ");
    match routine.kind {
        RoutineKind::Procedure => match engine {
            // A procedure's arguments are named, because the parameters the
            // dialog does not ask for -- an `OUT` -- shift positions.
            Engine::SqlServer => {
                let named = values
                    .iter()
                    .zip(arguments)
                    .map(|(value, (parameter, _))| {
                        format!("{} = {value}", argument_name(parameter))
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                match named.is_empty() {
                    true => format!("EXEC {name};"),
                    false => format!("EXEC {name} {named};"),
                }
            }
            _ => format!("CALL {name}({positional});"),
        },
        RoutineKind::Function => match engine {
            Engine::Postgres => format!("SELECT * FROM {name}({positional});"),
            Engine::SqlServer if routine.result_type.eq_ignore_ascii_case("TABLE") => {
                format!("SELECT * FROM {name}({positional});")
            }
            _ => format!("SELECT {name}({positional});"),
        },
    }
}

/// A parameter's label in a call: SQL Server reads one with a leading `@`,
/// which its catalog already carries.
fn argument_name(parameter: &RoutineParameter) -> String {
    let bare = parameter.name.trim_start_matches('@');
    format!("@{bare}")
}

/// A value as the engine spells it, typed where a literal depends on the type.
fn literal(engine: Engine, data_type: &str, value: &str) -> String {
    if value.is_empty() || value.eq_ignore_ascii_case("NULL") {
        return "NULL".into();
    }
    engine.quote_value(value, Some(data_type))
}

#[cfg(test)]
mod tests {
    use super::super::ParameterMode;
    use super::*;

    /// The editor holds a type as a base name and a parameter list, so the pair
    /// has to come back as the type the server wrote -- every character of it,
    /// including a type nobody touched.
    #[test]
    fn a_type_survives_being_split_and_joined() {
        for data_type in [
            "varchar(80)",
            "numeric(10,2)",
            "Nullable(String)",
            "enum('a','b')",
            "double precision",
            "timestamp with time zone",
            "",
            // Not what any server writes, but the pair has to round-trip
            // whatever the catalog handed back.
            "decimal( 10 , 2 )",
        ] {
            let (base, params) = split_type(data_type);
            assert_eq!(join_type(&base, &params), data_type, "{data_type}");
        }
        assert_eq!(split_type("varchar(80)"), ("varchar".into(), "(80)".into()));
        assert_eq!(split_type("text"), ("text".into(), String::new()));
        // Whitespace around a user's parameter list is not part of the type.
        assert_eq!(join_type("varchar", " (80) "), "varchar(80)");
    }

    fn added(name: &str, data_type: &str, nullable: bool) -> ColumnEdit {
        ColumnEdit {
            original: None,
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            default: String::new(),
            using: String::new(),
            dropped: false,
        }
    }

    fn table(columns: Vec<ColumnEdit>) -> TableEdit {
        TableEdit {
            schema: "public".into(),
            table: "t".into(),
            relation_kind: RelationKind::Table,
            columns,
            indexes: Vec::new(),
        }
    }

    fn routine(kind: RoutineKind, result_type: &str) -> Routine {
        Routine {
            name: "f".into(),
            kind,
            identity_arguments: "integer".into(),
            result_type: result_type.into(),
            language: "sql".into(),
            definition: String::new(),
        }
    }

    #[test]
    fn an_added_column_is_spelled_per_engine() {
        let edit = table(vec![ColumnEdit {
            default: "0".into(),
            ..added("n", "integer", false)
        }]);
        assert_eq!(
            statements(Engine::Postgres, &edit).unwrap(),
            ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"n\" integer DEFAULT 0 NOT NULL;"]
        );
        assert_eq!(
            statements(Engine::MySql, &edit).unwrap(),
            ["ALTER TABLE `public`.`t` ADD COLUMN `n` integer NOT NULL DEFAULT 0;"]
        );
        assert_eq!(
            statements(Engine::SqlServer, &edit).unwrap(),
            ["ALTER TABLE \"public\".\"t\" ADD \"n\" integer NOT NULL DEFAULT 0;"]
        );
        assert_eq!(
            statements(Engine::Snowflake, &edit).unwrap(),
            ["ALTER TABLE \"public\".\"t\" ADD COLUMN \"n\" integer DEFAULT 0 NOT NULL;"]
        );
    }

    #[test]
    fn sqlite_refuses_what_it_cannot_make_and_the_column_it_cannot_complete() {
        let edited = table(vec![ColumnEdit {
            original: Some(ColumnDefinition {
                name: "c".into(),
                data_type: "text".into(),
                nullable: true,
                default: None,
            }),
            name: "c".into(),
            data_type: "integer".into(),
            nullable: true,
            default: String::new(),
            using: String::new(),
            dropped: false,
        }]);
        assert_eq!(
            statements(Engine::Sqlite, &edited).unwrap_err(),
            "SQLite cannot change a column's type, nullability or default in place."
        );
        assert_eq!(
            statements(Engine::Sqlite, &table(vec![added("c", "integer", false)])).unwrap_err(),
            "SQLite cannot add a NOT NULL column without a default."
        );
    }

    #[test]
    fn mysql_writes_every_column_change_as_one_modify() {
        let edit = table(vec![ColumnEdit {
            original: Some(ColumnDefinition {
                name: "c".into(),
                data_type: "int".into(),
                nullable: true,
                default: None,
            }),
            name: "c".into(),
            data_type: "bigint".into(),
            nullable: false,
            default: "7".into(),
            using: String::new(),
            dropped: false,
        }]);
        assert_eq!(
            statements(Engine::MySql, &edit).unwrap(),
            ["ALTER TABLE `public`.`t` MODIFY COLUMN `c` bigint NOT NULL DEFAULT 7;"]
        );
    }

    #[test]
    fn a_removal_runs_last_and_names_the_column_as_it_was() {
        let edit = table(vec![
            ColumnEdit {
                original: Some(ColumnDefinition {
                    name: "old".into(),
                    data_type: "text".into(),
                    nullable: true,
                    default: None,
                }),
                name: "old".into(),
                data_type: "text".into(),
                nullable: true,
                default: String::new(),
                using: String::new(),
                dropped: true,
            },
            added("gone_too", "text", true),
        ]);
        assert_eq!(
            statements(Engine::Postgres, &edit).unwrap(),
            [
                "ALTER TABLE \"public\".\"t\" ADD COLUMN \"gone_too\" text;",
                "ALTER TABLE \"public\".\"t\" DROP COLUMN \"old\";",
            ]
        );
    }

    #[test]
    fn a_call_names_its_arguments_and_passes_an_empty_one_as_null() {
        let parameters = [
            RoutineParameter {
                name: "a".into(),
                data_type: "text".into(),
                mode: ParameterMode::In,
            },
            RoutineParameter {
                name: "@b".into(),
                data_type: "integer".into(),
                mode: ParameterMode::InOut,
            },
        ];
        let arguments = vec![
            (parameters[0].clone(), "it's".into()),
            (parameters[1].clone(), String::new()),
        ];
        let function = routine(RoutineKind::Function, "integer");
        let procedure = routine(RoutineKind::Procedure, "");
        assert_eq!(
            call_statement(Engine::Postgres, "public", &function, &arguments),
            "SELECT * FROM \"public\".\"f\"('it''s', NULL);"
        );
        assert_eq!(
            call_statement(Engine::Postgres, "public", &procedure, &arguments),
            "CALL \"public\".\"f\"('it''s', NULL);"
        );
        assert_eq!(
            call_statement(Engine::MySql, "s", &procedure, &arguments),
            "CALL `s`.`f`('it''s', NULL);"
        );
        assert_eq!(
            call_statement(Engine::SqlServer, "dbo", &procedure, &arguments),
            "EXEC \"dbo\".\"f\" @a = 'it''s', @b = NULL;"
        );
    }

    #[test]
    fn a_definition_is_rewritten_at_its_own_keyword() {
        assert_eq!(
            or_replace("CREATE VIEW v AS SELECT 'CREATE'").as_deref(),
            Some("CREATE OR REPLACE VIEW v AS SELECT 'CREATE'")
        );
        assert_eq!(
            or_replace("CREATE OR REPLACE VIEW v AS SELECT 1").as_deref(),
            Some("CREATE OR REPLACE VIEW v AS SELECT 1")
        );
        assert_eq!(
            respell("CREATE VIEW v AS SELECT 1", "ALTER").as_deref(),
            Some("ALTER VIEW v AS SELECT 1")
        );
        assert_eq!(
            routine_replacement(Engine::Postgres, "CREATE OR REPLACE FUNCTION f()").as_deref(),
            Some("CREATE OR REPLACE FUNCTION f()")
        );
        assert_eq!(
            routine_replacement(Engine::MySql, "CREATE PROCEDURE p()"),
            None
        );
        // A body the grammar has no production for is still a body the keyword
        // ahead of it is found before.
        assert_eq!(
            routine_replacement(
                Engine::MariaDb,
                "CREATE DEFINER=`root`@`localhost` PROCEDURE `p`()\nBEGIN\n  SELECT 1;\nEND"
            )
            .as_deref(),
            Some(
                "CREATE OR REPLACE DEFINER=`root`@`localhost` PROCEDURE `p`()\nBEGIN\n  SELECT 1;\nEND"
            )
        );
        assert_eq!(
            routine_replacement(
                Engine::SqlServer,
                "SET ANSI_NULLS ON\nGO\nCREATE PROCEDURE dbo.p AS BEGIN SELECT 1 END"
            )
            .as_deref(),
            Some("SET ANSI_NULLS ON\nGO\nALTER PROCEDURE dbo.p AS BEGIN SELECT 1 END")
        );
    }
}
