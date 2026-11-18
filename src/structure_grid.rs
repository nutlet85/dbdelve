use std::{borrow::Cow, collections::HashMap, ops::Range, sync::Arc};

use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, Pixels, SharedString, Stateful,
    StatefulInteractiveElement, Styled, StyledText, WeakEntity, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::{
    ActiveTheme,
    highlighter::HighlightTheme,
    menu::{ContextMenuExt, PopupMenuItem},
    table::{Column, TableDelegate, TableEvent, TableState},
};
use rust_i18n::t;

use crate::{
    Workspace,
    db::{IndexKind, Structure, Syntax},
    icons::icon,
    theme::{ConnectionColor, layout, theme},
    ui::{Code, code_highlights},
};

/// One part of a relation's definition, shown alone in the Structure grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Section {
    #[default]
    Columns,
    Indexes,
    Constraints,
    ForeignKeys,
    ReferencedBy,
}

impl Section {
    pub(crate) const ALL: [Section; 5] = [
        Section::Columns,
        Section::Indexes,
        Section::Constraints,
        Section::ForeignKeys,
        Section::ReferencedBy,
    ];

    /// The section's name as a stable id, whatever the interface language: an
    /// element it names must not move under a language switch.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Section::Columns => "Columns",
            Section::Indexes => "Indexes",
            Section::Constraints => "Constraints",
            Section::ForeignKeys => "Foreign keys",
            Section::ReferencedBy => "Referenced by",
        }
    }

    pub(crate) fn label(self) -> String {
        match self {
            Section::Columns => t!("structure.section.columns"),
            Section::Indexes => t!("structure.section.indexes"),
            Section::Constraints => t!("structure.section.constraints"),
            Section::ForeignKeys => t!("structure.section.foreign_keys"),
            Section::ReferencedBy => t!("structure.section.referenced_by"),
        }
        .to_string()
    }

    pub(crate) fn count(self, structure: &Structure) -> usize {
        match self {
            Section::Columns => structure.columns.len(),
            Section::Indexes => structure.indexes.len(),
            Section::Constraints => structure.constraints.len(),
            Section::ForeignKeys => structure.foreign_keys.len(),
            Section::ReferencedBy => structure.referenced_by.len(),
        }
    }

    /// Columns is always offered, so a table with no columns still says so;
    /// any other section only when it has something in it.
    pub(crate) fn offered(self, structure: &Structure) -> bool {
        self == Section::Columns || self.count(structure) > 0
    }

    /// Indexes leave out `where` when none is partial, which on most engines
    /// none can be.
    fn headers(self, structure: &Structure) -> Vec<(&'static str, f32)> {
        match self {
            Section::Columns => vec![
                ("name", 220.),
                ("type", 200.),
                ("nullable", 100.),
                ("default", 320.),
            ],
            Section::Indexes => {
                let mut headers = vec![("name", 240.), ("kind", 90.), ("columns", 240.)];
                if partial(structure) {
                    headers.push(("where", 240.));
                }
                headers.push(("definition", 560.));
                headers
            }
            Section::Constraints => vec![("name", 260.), ("definition", 640.)],
            Section::ForeignKeys => vec![("column", 220.), ("references", 420.)],
            Section::ReferencedBy => vec![
                ("table", 320.),
                ("column", 220.),
                ("referenced column", 220.),
            ],
        }
    }

    /// How a column reads as code, when it does.
    fn code(self, header: &str) -> Option<Code> {
        match (self, header) {
            (Section::Indexes, "columns" | "where") => Some(Code::Expression),
            (Section::Indexes, "definition") => Some(Code::Statement),
            _ => None,
        }
    }

    fn rows(self, structure: &Structure) -> Vec<Vec<SharedString>> {
        match self {
            Section::Columns => structure
                .columns
                .iter()
                .map(|column| {
                    vec![
                        column.name.clone().into(),
                        column.data_type.clone().into(),
                        if column.nullable {
                            "nullable"
                        } else {
                            "not null"
                        }
                        .into(),
                        column.default.clone().unwrap_or_default().into(),
                    ]
                })
                .collect(),
            Section::Indexes => {
                let partial = partial(structure);
                structure
                    .indexes
                    .iter()
                    .map(|index| {
                        let mut row = vec![
                            index.name.clone().into(),
                            match index.kind {
                                IndexKind::Primary => PRIMARY,
                                IndexKind::Unique => UNIQUE,
                                IndexKind::Plain => "",
                            }
                            .into(),
                            index.columns.join(", ").into(),
                        ];
                        if partial {
                            row.push(index.predicate.clone().unwrap_or_default().into());
                        }
                        row.push(index.definition.clone().into());
                        row
                    })
                    .collect()
            }
            Section::Constraints => named(&structure.constraints),
            Section::ForeignKeys => structure
                .foreign_keys
                .iter()
                .map(|key| {
                    vec![
                        key.column.clone().into(),
                        format!(
                            "{}.{}.{}",
                            key.referenced_schema, key.referenced_table, key.referenced_column
                        )
                        .into(),
                    ]
                })
                .collect(),
            Section::ReferencedBy => structure
                .referenced_by
                .iter()
                .map(|reference| {
                    vec![
                        format!("{}.{}", reference.schema, reference.table).into(),
                        reference.column.clone().into(),
                        reference.referenced_column.clone().into(),
                    ]
                })
                .collect(),
        }
    }
}

const PRIMARY: &str = "primary";
const UNIQUE: &str = "unique";

/// A column header's words, from the key [`Section::headers`] names it by. Read
/// every frame, so a language switch re-letters the header in place.
fn header_label(key: &str) -> String {
    match key {
        "name" => t!("structure.header.name").to_string(),
        "type" => t!("structure.header.type").to_string(),
        "nullable" => t!("structure.header.nullable").to_string(),
        "default" => t!("structure.header.default").to_string(),
        "kind" => t!("structure.header.kind").to_string(),
        "columns" => t!("structure.header.columns").to_string(),
        "where" => t!("structure.header.where").to_string(),
        "definition" => t!("structure.header.definition").to_string(),
        "column" => t!("structure.header.column").to_string(),
        "references" => t!("structure.header.references").to_string(),
        "table" => t!("structure.header.table").to_string(),
        "referenced column" => t!("structure.header.referenced_column").to_string(),
        _ => key.to_string(),
    }
}

/// The locale key a cell's value is said by, where the value is schema
/// vocabulary rather than data. The row keeps the raw word, so a copy or an
/// export stays what a statement can paste.
fn value_key(section: Section, header: &str, value: &str) -> Option<&'static str> {
    match (section, header, value) {
        (Section::Columns, "nullable", "nullable") => Some("structure.value.nullable"),
        (Section::Columns, "nullable", "not null") => Some("structure.value.not_null"),
        (Section::Indexes, "kind", PRIMARY) => Some("structure.value.primary"),
        (Section::Indexes, "kind", UNIQUE) => Some("structure.value.unique"),
        _ => None,
    }
}

fn partial(structure: &Structure) -> bool {
    structure
        .indexes
        .iter()
        .any(|index| index.predicate.is_some())
}

fn named(definitions: &[crate::db::NamedDefinition]) -> Vec<Vec<SharedString>> {
    definitions
        .iter()
        .map(|definition| {
            vec![
                definition.name.clone().into(),
                definition.definition.clone().into(),
            ]
        })
        .collect()
}

type Highlights = Vec<(Range<usize>, HighlightStyle)>;

/// Whether the cell at `col` names a column of the primary key. Only the
/// Columns section draws the marker, and only its first column holds names.
fn primary_column(section: Section, col: usize, name: &str, primary_key: &[String]) -> bool {
    section == Section::Columns && col == 0 && primary_key.iter().any(|key| key.as_str() == name)
}

/// A read-only grid over one section of a relation's structure.
pub(crate) struct StructureGrid {
    /// For the jumps to a column's data, which a delegate handed only its
    /// table's context has no other way to reach.
    workspace: WeakEntity<Workspace>,
    /// What the code cells are highlighted as. One grid only ever shows one
    /// connection's relations, though an edit can move that connection to an
    /// engine written in another language.
    syntax: Syntax,
    section: Section,
    headers: Vec<(&'static str, f32)>,
    rows: Vec<Vec<SharedString>>,
    /// The primary key's columns, as the structure read them, so the Columns
    /// section can mark one without the row itself carrying anything.
    primary_key: Vec<String>,
    widths: Vec<Pixels>,
    /// The library's own selection, kept here because it does not say whether
    /// its cell or its row is the current one: `(row, Some(col))` for a cell,
    /// `(row, None)` for a whole row.
    selected: Option<(usize, Option<usize>)>,
    /// Each code cell's highlights by `(row, col)`, parsed the first time the
    /// cell is drawn rather than on every frame, and kept with the theme they
    /// were resolved against, so a theme switch parses them again rather than
    /// leaving the old colours up.
    highlights: HashMap<(usize, usize), Highlights>,
    highlighted_in: Option<Arc<HighlightTheme>>,
}

impl StructureGrid {
    pub(crate) fn section(&self) -> Section {
        self.section
    }

    /// The selected cell, or the selected row as one line of TSV.
    pub(crate) fn selection_text(&self) -> Option<String> {
        let (row, col) = self.selected?;
        self.text(row, col)
    }

    fn text(&self, row: usize, col: Option<usize>) -> Option<String> {
        copy_text(self.rows.get(row)?, col)
    }

    pub(crate) fn set_syntax(&mut self, syntax: Syntax) {
        self.syntax = syntax;
        self.highlights.clear();
    }
}

/// A cell's text, or with no column its row's as one line of TSV.
fn copy_text(cells: &[SharedString], col: Option<usize>) -> Option<String> {
    match col {
        Some(col) => cells.get(col).map(ToString::to_string),
        None => Some(
            cells
                .iter()
                .map(|cell| crate::export::csv_field('\t', cell))
                .collect::<Vec<_>>()
                .join("\t"),
        ),
    }
}

/// Put `section` of `structure` in the grid, or Columns if that section has
/// nothing in it any more.
pub(crate) fn show(
    grid: &Entity<TableState<StructureGrid>>,
    structure: &Structure,
    section: Section,
    cx: &mut App,
) {
    let section = match section.offered(structure) {
        true => section,
        false => Section::Columns,
    };
    grid.update(cx, |table, cx| {
        let grid = table.delegate_mut();
        grid.section = section;
        grid.headers = section.headers(structure);
        grid.rows = section.rows(structure);
        grid.primary_key = structure.primary_key();
        grid.widths = grid.headers.iter().map(|&(_, w)| px(w)).collect();
        grid.selected = None;
        grid.highlights.clear();
        table.clear_selection(cx);
        table.refresh(cx);
    });
}

pub(crate) fn new_grid(
    syntax: Syntax,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<TableState<StructureGrid>> {
    let grid = StructureGrid {
        workspace: cx.entity().downgrade(),
        syntax,
        section: Section::Columns,
        headers: Vec::new(),
        rows: Vec::new(),
        primary_key: Vec::new(),
        widths: Vec::new(),
        selected: None,
        highlights: HashMap::new(),
        highlighted_in: None,
    };
    let grid = cx.new(|cx| {
        TableState::new(grid, window, cx)
            .col_movable(false)
            .col_resizable(true)
            .col_selectable(false)
            .row_selectable(true)
            .cell_selectable(true)
            // A second click on the selected cell selects its row instead.
            .row_header(false)
    });
    cx.subscribe(&grid, |_, table, event: &TableEvent, cx| {
        let selected = match *event {
            TableEvent::SelectCell(row, col) => Some((row, Some(col))),
            TableEvent::SelectRow(row) => Some((row, None)),
            TableEvent::ClearSelection => None,
            TableEvent::ColumnWidthsChanged(ref widths) => {
                let widths = widths.clone();
                table.update(cx, |table, _| table.delegate_mut().widths = widths);
                return;
            }
            _ => return,
        };
        table.update(cx, |table, _| table.delegate_mut().selected = selected);
    })
    .detach();
    grid
}

impl TableDelegate for StructureGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.headers.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (name, width) = self.headers[col_ix];
        Column::new(name, header_label(name))
            .width(px(width))
            .min_width(px(80.))
            .max_width(px(f32::MAX))
            .resizable(true)
            .movable(false)
            .p_0()
    }

    /// Capped at the columns' own width, for the reason `ResultGrid::render_tr`
    /// gives: the library sizes every row to the whole table.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> Stateful<gpui::Div> {
        let width: Pixels = self.widths.iter().copied().sum();
        div().id(("structure-row", row_ix)).max_w(width)
    }

    fn render_last_empty_col(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
    }

    /// Dressed as `ResultGrid::data_th`, so the two views of one relation read
    /// as one grid.
    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .h_full()
            .flex_1()
            .min_w_0()
            .pl(px(layout::SPACE_SM + 1.))
            .pr(px(layout::SPACE_SM))
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(px(layout::grid(layout::TEXT_SM)))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme(cx).text)
            .child(header_label(self.headers[col_ix].0))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let t = theme(cx);
        let header = self.headers[col_ix].0;
        let value = self.rows[row_ix].get(col_ix).cloned();
        let color = match (self.section, header) {
            _ if col_ix == 0 => t.text,
            // The same colour the editor gives a type name, so structure and
            // SQL read as one vocabulary.
            (Section::Columns, "type") => t.syntax_type,
            // Two hues apart, so a key stands out of a long list at a glance;
            // the primary one in the warm hue the data grid's key icon wears.
            (Section::Indexes, "kind") if value.as_deref() == Some(PRIMARY) => t.syntax_number,
            (Section::Indexes, "kind") => t.syntax_function,
            _ => t.text_muted,
        };
        let highlights = self
            .section
            .code(header)
            .zip(value.as_ref().filter(|value| !value.is_empty()))
            .map(|(code, value)| {
                let syntax = &cx.theme().highlight_theme;
                if !self
                    .highlighted_in
                    .as_ref()
                    .is_some_and(|theme| Arc::ptr_eq(theme, syntax))
                {
                    self.highlights.clear();
                    self.highlighted_in = Some(syntax.clone());
                }
                self.highlights
                    .entry((row_ix, col_ix))
                    .or_insert_with(|| code_highlights(value, self.syntax, code, syntax))
                    .clone()
            });
        let (divider, faint, text) = (t.border, t.text_faint, t.text);
        let column_name = (self.section == Section::Columns && col_ix == 0)
            .then(|| value.clone())
            .flatten();
        let primary = value
            .as_deref()
            .is_some_and(|name| primary_column(self.section, col_ix, name, &self.primary_key));
        // Said at draw time rather than stored in the row, so a language switch
        // re-letters the cells already on screen.
        let shown = match value
            .as_deref()
            .and_then(|word| value_key(self.section, header, word))
        {
            Some(key) => Some(SharedString::from(t!(key))),
            None => value.clone(),
        };
        let grid = cx.entity().downgrade();
        div()
            .id(("structure-cell", row_ix * self.headers.len() + col_ix))
            .relative()
            .size_full()
            .px(px(layout::SPACE_SM))
            // Transparent, so text sits where `ResultGrid`'s ringed cells put it.
            .border_1()
            .flex()
            .items_center()
            .child(
                div()
                    .absolute()
                    .top(px(-1.))
                    .bottom(px(-1.))
                    .right(px(-1.))
                    .w(px(1.))
                    .bg(divider),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(layout::SPACE_XS))
                    .children(primary.then(|| {
                        icon(icon::PRIMARY_KEY)
                            .size(px(layout::grid(12.)))
                            .flex_shrink_0()
                            .text_color(ConnectionColor::Yellow.swatch())
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(color)
                            .when(header == "kind", |cell| {
                                cell.font_weight(FontWeight::MEDIUM)
                            })
                            .when_some(shown, |cell, word| match highlights {
                                Some(highlights) => {
                                    cell.child(StyledText::new(word).with_highlights(highlights))
                                }
                                None => cell.child(word),
                            }),
                    ),
            )
            // A column listed here is a way to its data: back to the rows,
            // scrolled to it. Dressed as `ResultGrid`'s follow-key square.
            .children(column_name.map(|name| {
                let reveal = self.workspace.clone();
                div()
                    .id(("reveal-column", row_ix))
                    .flex_shrink_0()
                    .h_full()
                    .aspect_square()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(faint)
                    .hover(|button| button.text_color(text))
                    .child(icon(icon::FOLLOW_KEY).size(px(layout::grid(14.))))
                    .on_click(move |_, window, cx| {
                        // Or the cell underneath takes the click as a selection.
                        cx.stop_propagation();
                        _ = reveal.update(cx, |workspace, cx| {
                            workspace.reveal_in_data(name.to_string(), window, cx);
                        });
                    })
            }))
            // Not the `CopyCell` action the data grid's menu sends: that copies
            // the selection, and a right click here selects nothing.
            .context_menu(move |menu, _, _| {
                let copy = |label: Cow<'static, str>, col: Option<usize>| {
                    let grid = grid.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let text = grid
                            .upgrade()
                            .and_then(|grid| grid.read(cx).delegate().text(row_ix, col));
                        if let Some(text) = text {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    })
                };
                menu.item(copy(t!("grid.copy_cell"), Some(col_ix)))
                    .item(copy(t!("grid.copy_row"), None))
            })
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.rows
            .get(row_ix)
            .and_then(|row| row.get(col_ix))
            .map(ToString::to_string)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Index;

    fn index(name: &str, kind: IndexKind, columns: &[&str], predicate: Option<&str>) -> Index {
        Index {
            name: name.into(),
            kind,
            columns: columns.iter().map(ToString::to_string).collect(),
            predicate: predicate.map(Into::into),
            definition: format!("CREATE INDEX {name}"),
        }
    }

    #[test]
    fn an_index_row_reads_as_its_kind_key_and_condition_and_copies_as_written() {
        let mut structure = Structure {
            indexes: vec![
                index("pkey", IndexKind::Primary, &["id"], None),
                index("email", IndexKind::Unique, &["lower(email)"], None),
                index(
                    "active",
                    IndexKind::Plain,
                    &["org_id", "coalesce(a, b)"],
                    Some("deleted_at IS NULL"),
                ),
            ],
            ..Structure::default()
        };
        let names = |structure: &Structure| -> Vec<&str> {
            Section::Indexes
                .headers(structure)
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        };
        assert_eq!(
            names(&structure),
            ["name", "kind", "columns", "where", "definition"]
        );
        let rows = Section::Indexes.rows(&structure);
        assert_eq!(rows[0][1], "primary");
        assert_eq!(rows[1][1], "unique");
        assert_eq!(rows[2][1], "");
        assert_eq!(rows[0][3], "");
        assert_eq!(
            copy_text(&rows[2], None).as_deref(),
            Some("active\t\torg_id, coalesce(a, b)\tdeleted_at IS NULL\tCREATE INDEX active")
        );
        assert_eq!(
            copy_text(&rows[2], Some(2)).as_deref(),
            Some("org_id, coalesce(a, b)")
        );

        structure.indexes.pop();
        assert_eq!(names(&structure), ["name", "kind", "columns", "definition"]);
        assert!(
            Section::Indexes
                .rows(&structure)
                .iter()
                .all(|row| row.len() == 4)
        );
    }

    #[test]
    fn the_structure_page_is_translated() {
        assert_eq!(t!("structure.loading", locale = "zh-CN"), "正在读取结构…");
        assert_eq!(t!("structure.section.columns", locale = "zh-CN"), "列");
        assert_eq!(t!("structure.section.indexes", locale = "zh-CN"), "索引");
        assert_eq!(
            t!("structure.section.constraints", locale = "zh-CN"),
            "约束"
        );
        assert_eq!(
            t!("structure.section.foreign_keys", locale = "zh-CN"),
            "外键"
        );
        assert_eq!(
            t!("structure.section.referenced_by", locale = "zh-CN"),
            "被引用"
        );
        assert_eq!(t!("structure.header.name", locale = "zh-CN"), "名称");
        assert_eq!(t!("structure.header.type", locale = "zh-CN"), "类型");
        assert_eq!(t!("structure.header.nullable", locale = "zh-CN"), "可空");
        assert_eq!(t!("structure.header.default", locale = "zh-CN"), "默认值");
        assert_eq!(t!("structure.header.kind", locale = "zh-CN"), "类型");
        assert_eq!(t!("structure.header.columns", locale = "zh-CN"), "列");
        assert_eq!(t!("structure.header.where", locale = "zh-CN"), "条件");
        assert_eq!(t!("structure.header.definition", locale = "zh-CN"), "定义");
        assert_eq!(t!("structure.header.column", locale = "zh-CN"), "列");
        assert_eq!(t!("structure.header.references", locale = "zh-CN"), "引用");
        assert_eq!(t!("structure.header.table", locale = "zh-CN"), "表");
        assert_eq!(
            t!("structure.header.referenced_column", locale = "zh-CN"),
            "被引用列"
        );
        assert_eq!(t!("structure.value.nullable", locale = "zh-CN"), "可空");
        assert_eq!(t!("structure.value.not_null", locale = "zh-CN"), "非空");
        assert_eq!(t!("structure.value.primary", locale = "zh-CN"), "主键");
        assert_eq!(t!("structure.value.unique", locale = "zh-CN"), "唯一");
    }

    #[test]
    fn a_schema_word_is_said_by_its_locale_key() {
        assert_eq!(
            value_key(Section::Columns, "nullable", "nullable"),
            Some("structure.value.nullable")
        );
        assert_eq!(
            value_key(Section::Columns, "nullable", "not null"),
            Some("structure.value.not_null")
        );
        assert_eq!(
            value_key(Section::Indexes, "kind", PRIMARY),
            Some("structure.value.primary")
        );
        assert_eq!(
            value_key(Section::Indexes, "kind", UNIQUE),
            Some("structure.value.unique")
        );
        // Only where the word is the whole cell, and only in the column it
        // belongs to: a column named `primary` is a name, and an index of no
        // kind says nothing.
        assert_eq!(value_key(Section::Indexes, "kind", ""), None);
        assert_eq!(value_key(Section::Columns, "name", "not null"), None);
        assert_eq!(value_key(Section::Indexes, "columns", "primary"), None);
    }

    #[test]
    fn the_primary_key_columns_are_marked() {
        let key = ["id".to_string()];
        assert!(primary_column(Section::Columns, 0, "id", &key));
        assert!(!primary_column(Section::Columns, 0, "email", &key));
        // Only the Columns section lists names, and only in its first column.
        assert!(!primary_column(Section::Indexes, 0, "id", &key));
        assert!(!primary_column(Section::Columns, 1, "id", &key));
        assert!(!primary_column(Section::Columns, 0, "id", &[]));
    }
}
