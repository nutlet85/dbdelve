//! The table editor: a relation's columns and indexes as fields, the SQL an
//! Apply would run written underneath them, and the three ways out.
//!
//! It is a dialog rather than an editor tab because what it produces is
//! dbdelve's own statement, not the user's: nothing here is spliced into a
//! buffer (hard rule 1), and the batch it writes is what
//! `sql::is_generated_structure` admits.
//!
//! The rows are built the first time the dialog is drawn after the structure
//! lands, because creating an input needs a window and the structure arrives on
//! a task that has none. The statement underneath is recomputed from the fields
//! every frame and never stored: one source for what is on screen and what
//! runs.

use gpui::SharedString;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::InputState;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::{IndexPath, Sizable};

use super::*;
use crate::db::{ColumnDefinition, Index, IndexKind, RelationKind, alter};
use crate::session::{ObjectBody, StructureState};
use crate::sql::StructureScope;

pub(crate) struct StructureEditor {
    /// The structure tab it was opened from: the definition it waits on, and
    /// the one an apply refreshes. The dialog keeps no copy of the definition
    /// itself -- it reads the tab's, which is the one the Structure view is
    /// showing and the one a refresh replaces.
    tab: u64,
    schema: String,
    relation: String,
    kind: RelationKind,
    /// Empty until the structure lands and the frame after builds them.
    rows: Vec<ColumnRow>,
    indexes: Vec<IndexRow>,
    /// Why the last Apply could not be written or run, in the engine's words or
    /// dbdelve's.
    pub(crate) error: Option<String>,
    pub(crate) applying: bool,
}

struct ColumnRow {
    original: Option<ColumnDefinition>,
    name: Entity<InputState>,
    /// The base type, picked from the engine's list: what is left of the
    /// column's type once its parameter list is taken off.
    data_type: Entity<SelectState<SearchableVec<SharedString>>>,
    /// The parameter list, `(80)`, `(10,2)` or `(String)`, typed rather than
    /// picked: it is the part no list can enumerate.
    type_params: Entity<InputState>,
    default: Entity<InputState>,
    using: Entity<InputState>,
    nullable: bool,
    /// A removed row stays where it was, greyed, so the removal can be taken
    /// back without reloading the table. Nothing reaches the server until
    /// Apply.
    dropped: bool,
}

struct IndexRow {
    original: Option<Index>,
    name: Entity<InputState>,
    /// The key's entries, comma separated, as the server renders them.
    columns: Entity<InputState>,
    predicate: Entity<InputState>,
    unique: bool,
    dropped: bool,
}

impl StructureEditor {
    fn loading(tab: u64, schema: String, relation: String, kind: RelationKind) -> Self {
        Self {
            tab,
            schema,
            relation,
            kind,
            rows: Vec::new(),
            indexes: Vec::new(),
            error: None,
            applying: false,
        }
    }

    /// The edit the fields now describe.
    pub(crate) fn edit(&self, cx: &App) -> alter::TableEdit {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        alter::TableEdit {
            schema: self.schema.clone(),
            table: self.relation.clone(),
            relation_kind: self.kind,
            columns: self
                .rows
                .iter()
                .map(|row| alter::ColumnEdit {
                    original: row.original.clone(),
                    name: value(&row.name).trim().to_string(),
                    data_type: alter::join_type(
                        row.data_type
                            .read(cx)
                            .selected_value()
                            .map(|base| base.as_str())
                            .unwrap_or_default(),
                        &value(&row.type_params),
                    ),
                    nullable: row.nullable,
                    default: value(&row.default).trim().to_string(),
                    using: value(&row.using).trim().to_string(),
                    dropped: row.dropped,
                })
                .collect(),
            indexes: self
                .indexes
                .iter()
                .map(|row| alter::IndexEdit {
                    original: row.original.clone(),
                    name: value(&row.name).trim().to_string(),
                    unique: row.unique,
                    columns: value(&row.columns)
                        .split(',')
                        .map(str::trim)
                        .filter(|column| !column.is_empty())
                        .map(str::to_string)
                        .collect(),
                    predicate: value(&row.predicate).trim().to_string(),
                    dropped: row.dropped,
                })
                .collect(),
        }
    }

    /// Every name the edit reads or writes, which is what the gate is given: a
    /// statement naming anything else did not come from here.
    fn scope(edit: &alter::TableEdit) -> StructureScope {
        let mut names = Vec::new();
        for column in &edit.columns {
            if let Some(original) = &column.original {
                names.push(original.name.clone());
            }
            if !column.name.is_empty() {
                names.push(column.name.clone());
            }
        }
        for index in &edit.indexes {
            if let Some(original) = &index.original {
                names.push(original.name.clone());
            }
            if !index.name.is_empty() {
                names.push(index.name.clone());
            }
        }
        names.sort();
        names.dedup();
        StructureScope {
            schema: edit.schema.clone(),
            relation: edit.table.clone(),
            names,
        }
    }
}

/// A cell of the editor: a field with a box of its own, because a dialog of
/// eleven flat rows gives no sign of where text may be typed.
fn field(input: &Entity<InputState>) -> Input {
    Input::new(input).small()
}

// The column widths, written once: the header and the rows under it have to
// agree to the pixel, and a second copy of a number this layout depends on is
// a column that drifts the first time one of them is changed.
const NAME_COLUMN: f32 = 150.0;
const TYPE_COLUMN: f32 = 170.0;
/// The parameter list's own field, `(80)` or `(10,2)`.
const PARAMS_COLUMN: f32 = 60.0;
/// A checkbox and no label: the width is the word above it and nothing more.
/// The box is centred in what is left, so the space either side of it reads as
/// the column's, not as a gap opening onto the default field.
const NULLABLE_COLUMN: f32 = 48.0;
const USING_COLUMN: f32 = 120.0;

const INDEX_NAME_COLUMN: f32 = 160.0;
const INDEX_COLUMNS_COLUMN: f32 = 200.0;
/// The Unique checkbox's column, for the reason `NULLABLE_COLUMN` is its own:
/// the box carries its label here, so the two do not share a width.
const INDEX_UNIQUE_COLUMN: f32 = 96.0;

/// One of those widths as drawn: through the chrome scale, because everything
/// inside them is rem-sized by gpui-component and grows with the chrome font
/// size. A fixed width would leave a header word wider than its own column
/// past the default size.
fn column(width: f32) -> gpui::Pixels {
    px(layout::chrome(width))
}

impl Workspace {
    /// Open `schema.relation`'s structure tab and put the editor over it: the
    /// tab holds the definition an apply refreshes, so the editor is opened
    /// from one rather than beside one.
    pub(crate) fn edit_structure(
        &mut self,
        schema: String,
        relation: String,
        kind: RelationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_notice();
        let (name, table) = (schema.clone(), relation.clone());
        let Some(tab) = self.open_relation_view(schema, relation, kind, true, window, cx) else {
            return;
        };
        let Some(profile) = self.profile_mut() else {
            return;
        };
        // After the tab was activated, which clears whatever prompts the profile
        // in front was holding.
        profile.session.structure_editor = Some(StructureEditor::loading(tab, name, table, kind));
        cx.notify();
    }

    /// Build the rows from the definition the structure tab holds.
    fn seed_structure_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let indexes_edit = self.target().edits_indexes();
        // Asked before the structure is borrowed out of the profile.
        let engine_types = self.target().data_types();
        let Some(tab_id) = self
            .profile()
            .and_then(|profile| profile.session.structure_editor.as_ref())
            .map(|editor| editor.tab)
        else {
            return;
        };
        let loaded = self
            .profile()
            .and_then(|profile| profile.session.objects.iter().find(|tab| tab.id == tab_id))
            .and_then(|tab| match &tab.body {
                ObjectBody::Structure { structure, .. } => Some(structure),
                _ => None,
            });
        let Some(StructureState::Loaded(structure)) = loaded else {
            return;
        };
        if self
            .profile()
            .and_then(|profile| profile.session.structure_editor.as_ref())
            .is_some_and(|editor| !editor.rows.is_empty() || !editor.indexes.is_empty())
        {
            return;
        }
        let structure = structure.clone();
        let Some(editor) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
        else {
            return;
        };
        for column in structure.columns {
            let nullable = column.nullable;
            let name = text_input(&column.name, window, cx);
            let (base, params) = alter::split_type(&column.data_type);
            let data_type = type_picker(&base, engine_types, window, cx);
            watch_type(&data_type, cx);
            let type_params = text_input(params.as_str(), window, cx);
            let default = text_input(column.default.as_deref().unwrap_or(""), window, cx);
            let using = text_input("", window, cx);
            for input in [&type_params, &default, &using] {
                watch(input, cx);
            }
            editor.rows.push(ColumnRow {
                original: Some(column),
                name,
                data_type,
                type_params,
                default,
                using,
                nullable,
                dropped: false,
            });
        }
        if indexes_edit {
            for index in structure.indexes {
                // The primary key is the key itself, which this editor does not
                // change: its index is not a row.
                if index.kind == IndexKind::Primary {
                    continue;
                }
                let unique = index.kind == IndexKind::Unique;
                let name = text_input(&index.name, window, cx);
                let columns = text_input(&index.columns.join(", "), window, cx);
                let predicate = text_input(index.predicate.as_deref().unwrap_or(""), window, cx);
                for input in [&name, &columns, &predicate] {
                    watch(input, cx);
                }
                editor.indexes.push(IndexRow {
                    original: Some(index),
                    name,
                    columns,
                    predicate,
                    unique,
                    dropped: false,
                });
            }
        }
        cx.notify();
    }

    pub(crate) fn add_structure_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Asked before the profile is borrowed, where the engine is not.
        let engine_types = self.target().data_types();
        let name = text_input("", window, cx);
        // Seeded empty: a column being added has no type until one is picked.
        let data_type = type_picker("", engine_types, window, cx);
        watch_type(&data_type, cx);
        let type_params = text_input("", window, cx);
        let default = text_input("", window, cx);
        let using = text_input("", window, cx);
        for input in [&name, &type_params, &default, &using] {
            watch(input, cx);
        }
        if let Some(editor) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
        {
            editor.rows.push(ColumnRow {
                original: None,
                name,
                data_type,
                type_params,
                default,
                using,
                nullable: true,
                dropped: false,
            });
        }
        cx.notify();
    }

    pub(crate) fn add_structure_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = hinted_input("", "name", window, cx);
        let columns = hinted_input("", "column, column", window, cx);
        let predicate = hinted_input("", "WHERE", window, cx);
        for input in [&name, &columns, &predicate] {
            watch(input, cx);
        }
        if let Some(editor) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
        {
            editor.indexes.push(IndexRow {
                original: None,
                name,
                columns,
                predicate,
                unique: false,
                dropped: false,
            });
        }
        cx.notify();
    }

    pub(crate) fn fold_structure_column(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(column) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
            .and_then(|editor| editor.rows.get_mut(row))
        {
            column.dropped = !column.dropped;
            cx.notify();
        }
    }

    pub(crate) fn fold_structure_index(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(index) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
            .and_then(|editor| editor.indexes.get_mut(row))
        {
            index.dropped = !index.dropped;
            cx.notify();
        }
    }

    pub(crate) fn set_structure_nullable(
        &mut self,
        row: usize,
        nullable: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(column) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
            .and_then(|editor| editor.rows.get_mut(row))
        {
            column.nullable = nullable;
            cx.notify();
        }
    }

    pub(crate) fn set_structure_unique(
        &mut self,
        row: usize,
        unique: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
            .and_then(|editor| editor.indexes.get_mut(row))
        {
            index.unique = unique;
            cx.notify();
        }
    }

    pub(crate) fn cancel_structure_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.profile_mut() {
            profile.session.structure_editor = None;
        }
        cx.notify();
    }

    /// The batch the fields describe, as one submission, and what it is allowed
    /// to touch. `None` when there is nothing to apply, `Err` the sentence the
    /// dialog shows instead.
    fn structure_batch(&self, cx: &App) -> Result<Option<(String, StructureScope)>, String> {
        let Some(editor) = self
            .profile()
            .and_then(|profile| profile.session.structure_editor.as_ref())
        else {
            return Ok(None);
        };
        let edit = editor.edit(cx);
        let statements = alter::statements(self.engine(), &edit)?;
        if statements.is_empty() {
            return Ok(None);
        }
        let scope = StructureEditor::scope(&edit);
        Ok(Some((statements.join("\n"), scope)))
    }

    /// Apply: write the batch, gate it as dbdelve's own, and run it where
    /// nothing has a tab to land in.
    pub(crate) fn apply_structure_edit(&mut self, cx: &mut Context<Self>) {
        let engine = self.engine();
        let (sql, scope) = match self.structure_batch(cx) {
            Ok(Some(batch)) => batch,
            Ok(None) => return,
            Err(reason) => return self.structure_editor_error(Some(reason), cx),
        };
        if !sql::is_generated_structure(engine, &sql, &scope) {
            // dbdelve wrote this statement; that its own gate refuses it is a
            // bug here, not something the user can fix in the dialog.
            return self
                .structure_editor_error(Some(t!("structure_editor.refused").to_string()), cx);
        }
        let (tab, schema, relation) = {
            let Some(editor) = self
                .profile_mut()
                .and_then(|profile| profile.session.structure_editor.as_mut())
            else {
                return;
            };
            editor.error = None;
            editor.applying = true;
            (editor.tab, editor.schema.clone(), editor.relation.clone())
        };
        cx.notify();
        self.run_ddl(
            sql,
            crate::session::AfterDdl::Structure {
                tab,
                schema,
                relation,
            },
            cx,
        );
    }

    /// The batch in a query tab of its own: for a statement the user wants to
    /// read, change or run by hand.
    pub(crate) fn structure_edit_in_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = match self.structure_batch(cx) {
            Ok(Some((sql, _))) => sql,
            Ok(None) => return,
            Err(reason) => return self.structure_editor_error(Some(reason), cx),
        };
        self.cancel_structure_editor(cx);
        self.open_query_with(sql, false, window, cx);
    }

    fn structure_editor_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(editor) = self
            .profile_mut()
            .and_then(|profile| profile.session.structure_editor.as_mut())
        {
            editor.error = error;
            editor.applying = false;
        }
        cx.notify();
    }

    /// What a run of this dialog's DDL leaves behind: the dialog closing over a
    /// table that is reloading, or the error the server gave.
    pub(crate) fn structure_ddl_landed(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        match error {
            Some(error) => {
                if let Some(editor) = self
                    .profile_mut()
                    .and_then(|profile| profile.session.structure_editor.as_mut())
                {
                    editor.applying = false;
                    editor.error = Some(error);
                }
            }
            None => {
                if let Some(profile) = self.profile_mut() {
                    profile.session.structure_editor = None;
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn render_structure_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.seed_structure_editor(window, cx);
        let t = popup_theme(cx);
        let code = fonts(cx).editor.clone();
        let engine = self.engine();
        let target = self.target();
        let using_column = target.alters_type_using();
        let indexes_edit = target.edits_indexes();
        let profile = self.profile()?;
        profile.session.structure_editor.as_ref()?;
        let mode = profile.mode;
        let profile_name = profile.name.clone();
        let ready = self
            .profile()
            .and_then(|profile| profile.session.structure_editor.as_ref())?;
        let title = format!(
            "{} — {}.{}",
            t!("structure_editor.title"),
            ready.schema,
            ready.relation
        );
        let tab_id = ready.tab;
        let loading = match self
            .profile()
            .and_then(|profile| profile.session.objects.iter().find(|tab| tab.id == tab_id))
            .map(|tab| &tab.body)
        {
            Some(ObjectBody::Structure {
                structure: StructureState::Loading,
                ..
            }) => Some(None),
            Some(ObjectBody::Structure {
                structure: StructureState::Failed(error),
                ..
            }) => Some(Some(error.clone())),
            Some(ObjectBody::Structure { .. }) => None,
            // A tab that closed under the dialog took the definition with it.
            _ => Some(Some(t!("structure_editor.tab_closed").to_string())),
        };
        let error = ready.error.clone();
        let applying = ready.applying;
        let rows: Vec<_> = ready
            .rows
            .iter()
            .map(|row| {
                (
                    row.name.clone(),
                    row.data_type.clone(),
                    row.type_params.clone(),
                    row.default.clone(),
                    row.using.clone(),
                    row.nullable,
                    row.dropped,
                )
            })
            .collect();
        let indexes: Vec<_> = ready
            .indexes
            .iter()
            .map(|row| {
                (
                    row.name.clone(),
                    row.columns.clone(),
                    row.predicate.clone(),
                    row.unique,
                    row.dropped,
                )
            })
            .collect();

        let batch = self.structure_batch(cx);
        let (statements, refusal) = match batch {
            Ok(batch) => (
                batch
                    .map(|(sql, _)| sql)
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                None,
            ),
            Err(reason) => (Vec::new(), Some(reason)),
        };
        let empty = statements.is_empty() && refusal.is_none();
        let note = alter::execution_note(engine);
        let labels = (
            t!("structure_editor.add_column"),
            t!("structure_editor.add_index"),
        );

        let add_column = cx.entity().downgrade();
        let add_index = cx.entity().downgrade();
        let cancel = cx.entity().downgrade();
        let open = cx.entity().downgrade();
        let apply = cx.entity().downgrade();

        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    dialog(t)
                        .w(px(880.))
                        .child(section_label(t, &title))
                        .child(
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(t.text_muted)
                                .child(t!(
                                    "structure_editor.mode_note",
                                    profile = profile_name.as_str(),
                                    mode = mode.label().as_str()
                                )),
                        )
                        .children(loading.map(|loading| {
                            let (text, colour) = match loading {
                                None => (t!("structure_editor.reading").to_string(), t.text_muted),
                                Some(error) => (error, t.danger),
                            };
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(colour)
                                .child(text)
                                .into_any_element()
                        }))
                        .child(section_label(t, &t!("structure_editor.columns")))
                        .children((!rows.is_empty()).then(|| {
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(layout::SPACE_XS))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(layout::SPACE_XS))
                                        .debug_selector(|| "structure-columns-header".into())
                                        .text_size(px(layout::chrome(layout::TEXT_XS)))
                                        .text_color(t.text_faint)
                                        .child(
                                            div()
                                                .w(column(NAME_COLUMN))
                                                .child(t!("structure_editor.name")),
                                        )
                                        .child(
                                            div()
                                                .w(column(TYPE_COLUMN))
                                                .debug_selector(|| "structure-header-type".into())
                                                .child(t!("structure_editor.type")),
                                        )
                                        .child(
                                            div()
                                                .w(column(PARAMS_COLUMN))
                                                .child(t!("structure_editor.params")),
                                        )
                                        .child(
                                            // Centred over the box the row
                                            // draws, which this column is only
                                            // just wide enough for.
                                            div()
                                                .w(column(NULLABLE_COLUMN))
                                                .flex()
                                                .justify_center()
                                                .child(t!("structure_editor.nullable")),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(t!("structure_editor.default")),
                                        )
                                        .children(using_column.then(|| {
                                            div()
                                                .w(column(USING_COLUMN))
                                                .child(t!("structure_editor.using"))
                                        }))
                                        // The width of the fold button every
                                        // row ends with, so the columns above
                                        // line up with the fields below.
                                        .child(
                                            div().w(px(Control::Inline.height())).flex_shrink_0(),
                                        ),
                                )
                                .children(rows.into_iter().enumerate().map(
                                    |(
                                        row,
                                        (
                                            name,
                                            data_type,
                                            type_params,
                                            default,
                                            using,
                                            nullable,
                                            struck,
                                        ),
                                    )| {
                                        let fold = cx.entity().downgrade();
                                        let toggle = cx.entity().downgrade();
                                        div()
                                            .id(("structure-column", row))
                                            .flex()
                                            .items_center()
                                            .gap(px(layout::SPACE_XS))
                                            .when(struck, |row| row.opacity(0.5))
                                            .child(div().w(column(NAME_COLUMN)).child(field(&name)))
                                            .child(
                                                div()
                                                    .w(column(TYPE_COLUMN))
                                                    .debug_selector(move || {
                                                        format!("structure-type-{row}")
                                                    })
                                                    .child(
                                                        Select::new(&data_type)
                                                            .small()
                                                            .w_full()
                                                            // A column being added
                                                            // has no type yet.
                                                            .placeholder(t!(
                                                                "structure_editor.type"
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .w(column(PARAMS_COLUMN))
                                                    .debug_selector(move || {
                                                        format!("structure-params-{row}")
                                                    })
                                                    .child(field(&type_params)),
                                            )
                                            .child(
                                                div()
                                                    .w(column(NULLABLE_COLUMN))
                                                    .debug_selector(move || {
                                                        format!("structure-nullable-cell-{row}")
                                                    })
                                                    .flex()
                                                    .justify_center()
                                                    .child(
                                                        Checkbox::new(("structure-nullable", row))
                                                            .text_size(px(layout::chrome(
                                                                layout::TEXT_SM,
                                                            )))
                                                            // The header names the
                                                            // column; the box still
                                                            // needs a name of its
                                                            // own.
                                                            .accessibility_label(t!(
                                                                "structure_editor.nullable"
                                                            ))
                                                            .checked(nullable)
                                                            .on_click(move |checked, _, cx| {
                                                                _ = toggle.update(
                                                                    cx,
                                                                    |workspace, cx| {
                                                                        workspace
                                                                            .set_structure_nullable(
                                                                                row, *checked, cx,
                                                                            );
                                                                    },
                                                                );
                                                            }),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .debug_selector(move || {
                                                        format!("structure-default-{row}")
                                                    })
                                                    .child(field(&default)),
                                            )
                                            .children(using_column.then(|| {
                                                div().w(column(USING_COLUMN)).child(field(&using))
                                            }))
                                            .child(
                                                icon_button(
                                                    ("structure-fold-column", row),
                                                    match struck {
                                                        true => icon::CHECK,
                                                        false => icon::CLOSE,
                                                    },
                                                    Tone::Quiet,
                                                    Control::Inline,
                                                    t,
                                                )
                                                .tooltip(match struck {
                                                    true => t!("structure_editor.restore"),
                                                    false => t!("structure_editor.fold"),
                                                })
                                                .on_click(move |_, _, cx| {
                                                    _ = fold.update(cx, |workspace, cx| {
                                                        workspace.fold_structure_column(row, cx);
                                                    });
                                                }),
                                            )
                                    },
                                ))
                                .child(
                                    button(
                                        "structure-add-column",
                                        labels.0.clone(),
                                        Tone::Quiet,
                                        Control::Compact,
                                        t,
                                    )
                                    .on_click(
                                        move |_, window, cx| {
                                            _ = add_column.update(cx, |workspace, cx| {
                                                workspace.add_structure_column(window, cx);
                                            });
                                        },
                                    ),
                                )
                                .into_any_element()
                        }))
                        .children(indexes_edit.then(|| {
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(layout::SPACE_XS))
                                .child(section_label(t, &t!("structure_editor.indexes")))
                                .children(indexes.into_iter().enumerate().map(
                                    |(row, (name, columns, predicate, unique, struck))| {
                                        let fold = cx.entity().downgrade();
                                        let toggle = cx.entity().downgrade();
                                        div()
                                            .id(("structure-index", row))
                                            .flex()
                                            .items_center()
                                            .gap(px(layout::SPACE_XS))
                                            .when(struck, |row| row.opacity(0.5))
                                            .child(
                                                div()
                                                    .w(column(INDEX_NAME_COLUMN))
                                                    .child(field(&name)),
                                            )
                                            .child(
                                                div()
                                                    .w(column(INDEX_COLUMNS_COLUMN))
                                                    .child(field(&columns)),
                                            )
                                            .child(
                                                div().flex_1().min_w_0().child(field(&predicate)),
                                            )
                                            .child(
                                                div().w(column(INDEX_UNIQUE_COLUMN)).child(
                                                    Checkbox::new(("structure-unique", row))
                                                        .text_size(px(layout::chrome(
                                                            layout::TEXT_SM,
                                                        )))
                                                        .label(
                                                            t!("structure_editor.unique")
                                                                .to_string(),
                                                        )
                                                        .checked(unique)
                                                        .on_click(move |checked, _, cx| {
                                                            _ = toggle.update(
                                                                cx,
                                                                |workspace, cx| {
                                                                    workspace.set_structure_unique(
                                                                        row, *checked, cx,
                                                                    );
                                                                },
                                                            );
                                                        }),
                                                ),
                                            )
                                            .child(
                                                icon_button(
                                                    ("structure-fold-index", row),
                                                    match struck {
                                                        true => icon::CHECK,
                                                        false => icon::CLOSE,
                                                    },
                                                    Tone::Quiet,
                                                    Control::Inline,
                                                    t,
                                                )
                                                .tooltip(match struck {
                                                    true => t!("structure_editor.restore"),
                                                    false => t!("structure_editor.fold"),
                                                })
                                                .on_click(move |_, _, cx| {
                                                    _ = fold.update(cx, |workspace, cx| {
                                                        workspace.fold_structure_index(row, cx);
                                                    });
                                                }),
                                            )
                                    },
                                ))
                                .child(
                                    button(
                                        "structure-add-index",
                                        labels.1.clone(),
                                        Tone::Quiet,
                                        Control::Compact,
                                        t,
                                    )
                                    .on_click(
                                        move |_, window, cx| {
                                            _ = add_index.update(cx, |workspace, cx| {
                                                workspace.add_structure_index(window, cx);
                                            });
                                        },
                                    ),
                                )
                                .into_any_element()
                        }))
                        .child(
                            div()
                                .id("structure-editor-sql")
                                .max_h(px(layout::DIALOG_TEXT_MAX_HEIGHT))
                                .overflow_y_scroll()
                                .p(px(layout::SPACE_SM))
                                .rounded(px(layout::RADIUS_CONTROL))
                                .bg(t.surface)
                                .font_family(code)
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(t.text)
                                .children(statements.into_iter().map(|statement| {
                                    let danger = statement.contains("DROP");
                                    div()
                                        .when(danger, |line| line.text_color(t.danger))
                                        .child(statement)
                                })),
                        )
                        .children(note.map(|note| {
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(t.text_muted)
                                .child(note)
                        }))
                        .children(refusal.or(error).map(|reason| {
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(t.danger)
                                .child(reason)
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(layout::SPACE_SM))
                                .child(
                                    self.choice(
                                        "structure-editor",
                                        2,
                                        button(
                                            "structure-open-query",
                                            t!("structure_editor.open_query"),
                                            Tone::Quiet,
                                            Control::Standard,
                                            t,
                                        )
                                        .on_click(
                                            move |_, window, cx| {
                                                _ = open.update(cx, |workspace, cx| {
                                                    workspace.structure_edit_in_query(window, cx);
                                                });
                                            },
                                        ),
                                        t,
                                    ),
                                )
                                .child(
                                    self.choice(
                                        "structure-editor",
                                        1,
                                        button(
                                            "structure-cancel",
                                            t!("button.cancel"),
                                            Tone::Quiet,
                                            Control::Standard,
                                            t,
                                        )
                                        .on_click(
                                            move |_, _, cx| {
                                                _ = cancel.update(cx, |workspace, cx| {
                                                    workspace.cancel_structure_editor(cx);
                                                });
                                            },
                                        ),
                                        t,
                                    ),
                                )
                                .child(
                                    self.choice(
                                        "structure-editor",
                                        0,
                                        button(
                                            "structure-apply",
                                            t!("structure_editor.apply"),
                                            Tone::Primary,
                                            Control::Standard,
                                            t,
                                        )
                                        .disabled(applying || empty)
                                        .on_click(
                                            move |_, _, cx| {
                                                _ = apply.update(cx, |workspace, cx| {
                                                    workspace.apply_structure_edit(cx);
                                                });
                                            },
                                        ),
                                        t,
                                    ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// The base-name picker: the engine's types, plus the type the column already
/// has when the list does not name it, so opening the editor never rewrites a
/// type nobody touched.
///
/// Searchable, because the lists are long enough that scrolling them is worse
/// than typing three letters of the name.
fn type_picker(
    base: &str,
    types: &'static [&'static str],
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<SelectState<SearchableVec<SharedString>>> {
    let mut items: Vec<SharedString> = types.iter().map(|name| SharedString::from(*name)).collect();
    if !base.is_empty() && !items.iter().any(|name| name == base) {
        items.push(SharedString::from(base.to_string()));
    }
    let index = items
        .iter()
        .position(|name| name == base)
        .map(IndexPath::new);
    cx.new(|cx| SelectState::new(SearchableVec::new(items), index, window, cx).searchable(true))
}

/// A field seeded with `text`, which starts empty for a column being added.
fn text_input(text: &str, window: &mut Window, cx: &mut Context<Workspace>) -> Entity<InputState> {
    let text = text.to_string();
    cx.new(|cx| InputState::new(window, cx).default_value(text))
}

/// [`text_input`] with a hint of what belongs in it: the index rows keep one,
/// since the columns have a header and say what each cell is, and the indexes
/// do not.
fn hinted_input(
    text: &str,
    hint: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<InputState> {
    let text = text.to_string();
    cx.new(|cx| {
        InputState::new(window, cx)
            .default_value(text)
            .placeholder(hint)
    })
}

/// [`watch`] for the type picker, which reports a pick rather than a keystroke.
fn watch_type(
    picker: &Entity<SelectState<SearchableVec<SharedString>>>,
    cx: &mut Context<Workspace>,
) {
    cx.subscribe(
        picker,
        |workspace, _, event: &SelectEvent<SearchableVec<SharedString>>, cx| {
            // Only a confirmed pick changes the statement; opening and closing
            // the menu must not clear the last Apply's reason.
            if matches!(event, SelectEvent::Confirm(_)) {
                clear_structure_error(workspace);
                cx.notify();
            }
        },
    )
    .detach();
}

/// Redraw the dialog as a field changes, since the statement underneath it is
/// recomputed from the fields on every frame, and let go of the last Apply's
/// reason: it was about the statement that has just been edited.
fn watch(input: &Entity<InputState>, cx: &mut Context<Workspace>) {
    cx.subscribe(input, |workspace, _, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            clear_structure_error(workspace);
            cx.notify();
        }
    })
    .detach();
}

/// The last Apply's reason was about the statement that has just been edited.
fn clear_structure_error(workspace: &mut Workspace) {
    if let Some(editor) = workspace
        .profile_mut()
        .and_then(|profile| profile.session.structure_editor.as_mut())
    {
        editor.error = None;
    }
}
