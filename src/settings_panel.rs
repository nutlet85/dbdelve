//! The settings panel: a card docked at the window's right edge, beside the
//! workspace rather than over it, so the change a setting makes can be watched
//! as it is made.
//!
//! Every row is listed once, as data, in [`setting_rows`]: the categories the
//! tabs show and the search box narrows are both read off that one list. A
//! control calls the same `Workspace` method a keystroke or the palette does,
//! which applies the change and writes it to disk at once; there is no Save.

use std::borrow::Cow;
use std::{
    collections::HashMap,
    ops::{Range, RangeInclusive},
};

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px, relative,
};
use gpui_component::{Disableable, Sizable, input::Input, tooltip::Tooltip};

use rust_i18n::t;

use crate::{
    Workspace,
    actions::LimitTarget,
    fuzzy::{Fuzzy, Query},
    icons::icon,
    keybindings::{self, KeybindingSpec},
    palette::Mode as PaletteMode,
    scroller::{SmoothScrollable, smooth},
    sql::{
        COMPACT_CLAUSES_MAX, EXPRESSION_WIDTH_MAX, EXPRESSION_WIDTH_MIN, IndentStyle, KeywordCase,
    },
    theme::{
        Appearance, FontSlot, OPACITY_MAX, OPACITY_MIN, OPACITY_STEP, Theme, fonts, layout,
        popup_theme, theme,
    },
    ui::{
        Control, Tone, button, button_label, chord_hint, highlighted, icon_button, keycap_text,
        keycaps, option_row, picker, raised_card, segmented, segmented_choice, stacked_option_row,
        switch, well,
    },
    views::row_limit_menu,
    workspace::{FONT_SIZE_STEP, Settings, font_size_range, opacity_percent},
};

/// The categories the panel's tabs show, in the order they are numbered.
/// Which one is in front is not remembered across launches: it is where the
/// panel was left a minute ago, not a preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    #[default]
    General,
    Appearance,
    Fonts,
    Keyboard,
}

impl SettingsTab {
    const ALL: [Self; 4] = [Self::General, Self::Appearance, Self::Fonts, Self::Keyboard];

    fn name(self) -> String {
        match self {
            Self::General => t!("settings.tab.general.name").to_string(),
            Self::Appearance => t!("settings.tab.appearance.name").to_string(),
            Self::Fonts => t!("settings.tab.fonts.name").to_string(),
            Self::Keyboard => t!("settings.tab.keyboard.name").to_string(),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::General => icon::SLIDERS,
            Self::Appearance => icon::THEME,
            Self::Fonts => icon::FONT,
            Self::Keyboard => icon::KEYBOARD,
        }
    }

    fn title(self) -> String {
        match self {
            Self::General => t!("settings.tab.general.title").to_string(),
            Self::Appearance => t!("settings.tab.appearance.title").to_string(),
            Self::Fonts => t!("settings.tab.fonts.title").to_string(),
            Self::Keyboard => t!("settings.tab.keyboard.title").to_string(),
        }
    }

    fn description(self) -> String {
        match self {
            Self::General => t!("settings.tab.general.description").to_string(),
            Self::Appearance => t!("settings.tab.appearance.description").to_string(),
            Self::Fonts => t!("settings.tab.fonts.description").to_string(),
            Self::Keyboard => t!("settings.tab.keyboard.description").to_string(),
        }
    }

    /// The eyebrow over the category's heading: `01 · GENERAL`.
    fn eyebrow(self) -> String {
        let number = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0) + 1;
        format!("{number:02} · {}", self.name().to_uppercase())
    }
}

/// One setting, by what it sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingId {
    RowLimit,
    DefaultSorting,
    QualifyCompletions,
    AliasCompletions,
    RunUnreadable,
    FormatKeywordCase,
    FormatIndent,
    FormatExpressionWidth,
    FormatCompactClauses,
    FormatJoins,
    Language,
    CheckForUpdates,
    VerboseLogging,
    MatchSystemAppearance,
    Theme,
    SystemTheme(Appearance),
    Opacity,
    ColorTitlebar,
    Font(FontSlot),
    /// A [`KeybindingSpec::id`].
    Keybinding(&'static str),
}

/// A row the search found, and the characters of its title it matched.
pub(crate) struct SettingMatch {
    id: SettingId,
    title: Vec<Range<usize>>,
}

struct SettingRow {
    id: SettingId,
    tab: SettingsTab,
    group: Cow<'static, str>,
    title: Cow<'static, str>,
    description: Option<Cow<'static, str>>,
    /// Words someone looking for this row might type that its title does not
    /// hold.
    keywords: &'static [&'static str],
}

const FONT_KEYWORDS: &[&str] = &[
    "font", "family", "face", "typeface", "size", "字体", "字号", "字形",
];

/// Every row the panel can show, in the order it shows them: by category,
/// then by group, then as listed.
fn setting_rows() -> Vec<SettingRow> {
    let row = |id, tab, group, title, description, keywords| SettingRow {
        id,
        tab,
        group,
        title,
        description: Some(description),
        keywords,
    };
    let mut rows = vec![
        row(
            SettingId::RowLimit,
            SettingsTab::General,
            t!("settings.group.results"),
            t!("settings.row.row_limit.title"),
            t!("settings.row.row_limit.description"),
            &["rows", "limit", "default", "page", "行数", "上限", "默认"],
        ),
        row(
            SettingId::DefaultSorting,
            SettingsTab::General,
            t!("settings.group.results"),
            t!("settings.row.default_sorting.title"),
            t!("settings.row.default_sorting.description"),
            &[
                "sort",
                "order",
                "server",
                "client",
                "排序",
                "服务器",
                "客户端",
            ],
        ),
        row(
            SettingId::Language,
            SettingsTab::General,
            t!("settings.group.language"),
            t!("settings.row.language.title"),
            t!("settings.row.language.description"),
            &["language", "中文", "chinese", "locale"],
        ),
        row(
            SettingId::QualifyCompletions,
            SettingsTab::General,
            t!("settings.group.editor"),
            t!("settings.row.qualify_completions.title"),
            t!("settings.row.qualify_completions.description"),
            &[
                "autocomplete",
                "completion",
                "schema",
                "prefix",
                "qualified",
                "自动补全",
                "补全",
                "模式",
                "前缀",
            ],
        ),
        row(
            SettingId::AliasCompletions,
            SettingsTab::General,
            t!("settings.group.editor"),
            t!("settings.row.alias_completions.title"),
            t!("settings.row.alias_completions.description"),
            &["autocomplete", "completion", "alias", "join", "foreign key"],
        ),
        row(
            SettingId::FormatKeywordCase,
            SettingsTab::General,
            t!("settings.group.format"),
            t!("settings.row.format_keyword_case.title"),
            t!("settings.row.format_keyword_case.description"),
            &[
                "format",
                "formatter",
                "keyword",
                "case",
                "upper",
                "lower",
                "格式化",
                "关键字",
                "大写",
                "小写",
            ],
        ),
        row(
            SettingId::FormatIndent,
            SettingsTab::General,
            t!("settings.group.format"),
            t!("settings.row.format_indent.title"),
            t!("settings.row.format_indent.description"),
            &[
                "format",
                "indent",
                "spaces",
                "tab",
                "缩进",
                "空格",
                "制表符",
            ],
        ),
        row(
            SettingId::FormatExpressionWidth,
            SettingsTab::General,
            t!("settings.group.format"),
            t!("settings.row.format_expression_width.title"),
            t!("settings.row.format_expression_width.description"),
            &[
                "format",
                "expression",
                "width",
                "wrap",
                "parentheses",
                "表达式",
                "宽度",
                "换行",
                "括号",
            ],
        ),
        row(
            SettingId::FormatCompactClauses,
            SettingsTab::General,
            t!("settings.group.format"),
            t!("settings.row.format_compact_clauses.title"),
            t!("settings.row.format_compact_clauses.description"),
            &[
                "format", "compact", "clause", "list", "width", "紧凑", "子句", "列表", "宽度",
            ],
        ),
        row(
            SettingId::FormatJoins,
            SettingsTab::General,
            t!("settings.group.format"),
            t!("settings.row.format_joins.title"),
            t!("settings.row.format_joins.description"),
            &[
                "format",
                "join",
                "line",
                "from",
                "格式化",
                "换行",
                "独立成行",
            ],
        ),
        row(
            SettingId::RunUnreadable,
            SettingsTab::General,
            t!("settings.group.queries"),
            t!("settings.row.run_unreadable.title"),
            t!("settings.row.run_unreadable.description"),
            &[
                "parse",
                "unparseable",
                "unreadable",
                "confirm",
                "ask",
                "prompt",
                "run",
            ],
        ),
        row(
            SettingId::CheckForUpdates,
            SettingsTab::General,
            t!("settings.group.updates"),
            t!("settings.row.check_for_updates.title"),
            t!("settings.row.check_for_updates.description"),
            &["update", "release", "version", "github", "更新", "版本"],
        ),
        row(
            SettingId::VerboseLogging,
            SettingsTab::General,
            t!("settings.group.diagnostics"),
            t!("settings.row.verbose_logging.title"),
            t!("settings.row.verbose_logging.description"),
            &[
                "log",
                "logs",
                "debug",
                "diagnostics",
                "bug",
                "report",
                "verbose",
                "日志",
                "调试",
                "诊断",
                "错误报告",
            ],
        ),
        row(
            SettingId::MatchSystemAppearance,
            SettingsTab::Appearance,
            t!("settings.group.theme"),
            t!("settings.row.match_system_appearance.title"),
            t!("settings.row.match_system_appearance.description"),
            &[
                "auto",
                "automatic",
                "follow",
                "mode",
                "dark",
                "light",
                "os",
                "自动",
                "跟随",
                "深色",
                "浅色",
                "系统",
            ],
        ),
        row(
            SettingId::Theme,
            SettingsTab::Appearance,
            t!("settings.group.theme"),
            t!("settings.row.theme.title"),
            t!("settings.row.theme.description"),
            &["color", "colour", "dark", "light", "颜色", "深色", "浅色"],
        ),
        row(
            SettingId::SystemTheme(Appearance::Light),
            SettingsTab::Appearance,
            t!("settings.group.theme"),
            t!("settings.row.light_theme.title"),
            t!("settings.row.light_theme.description"),
            &["color", "colour", "system", "appearance", "系统", "外观"],
        ),
        row(
            SettingId::SystemTheme(Appearance::Dark),
            SettingsTab::Appearance,
            t!("settings.group.theme"),
            t!("settings.row.dark_theme.title"),
            t!("settings.row.dark_theme.description"),
            &["color", "colour", "system", "appearance", "系统", "外观"],
        ),
        row(
            SettingId::Opacity,
            SettingsTab::Appearance,
            t!("settings.group.theme"),
            t!("settings.row.opacity.title"),
            t!("settings.row.opacity.description"),
            &[
                "transparency",
                "translucent",
                "glass",
                "frost",
                "blur",
                "透明",
                "毛玻璃",
                "模糊",
            ],
        ),
        row(
            SettingId::ColorTitlebar,
            SettingsTab::Appearance,
            t!("settings.group.window"),
            t!("settings.row.color_titlebar.title"),
            t!("settings.row.color_titlebar.description"),
            &[
                "colour",
                "connection",
                "band",
                "tint",
                "连接",
                "色带",
                "着色",
                "标题栏",
            ],
        ),
        row(
            SettingId::Font(FontSlot::Chrome),
            SettingsTab::Fonts,
            t!("settings.group.faces"),
            t!("settings.row.fonts.chrome.title"),
            t!("settings.row.fonts.chrome.description"),
            FONT_KEYWORDS,
        ),
        row(
            SettingId::Font(FontSlot::Editor),
            SettingsTab::Fonts,
            t!("settings.group.faces"),
            t!("settings.row.fonts.editor.title"),
            t!("settings.row.fonts.editor.description"),
            FONT_KEYWORDS,
        ),
        row(
            SettingId::Font(FontSlot::Grid),
            SettingsTab::Fonts,
            t!("settings.group.faces"),
            t!("settings.row.fonts.grid.title"),
            t!("settings.row.fonts.grid.description"),
            FONT_KEYWORDS,
        ),
    ];

    // Grouped by where a binding fires, each group where its first binding
    // stands in the registry, and the registry's order inside it.
    let mut groups: Vec<&'static str> = Vec::new();
    for spec in keybindings::available_registry() {
        let group = keyboard_group(spec.context);
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    for group in groups {
        rows.extend(
            keybindings::available_registry()
                .filter(|spec| keyboard_group(spec.context) == group)
                .map(|spec| SettingRow {
                    id: SettingId::Keybinding(spec.id),
                    tab: SettingsTab::Keyboard,
                    group: keyboard_group_label(group),
                    title: keybindings::label_of(spec).into(),
                    description: None,
                    keywords: &[],
                }),
        );
    }
    rows
}

/// A binding's context as the place it works, in words.
fn keyboard_group(context: Option<&'static str>) -> &'static str {
    match context {
        None => "Everywhere",
        Some("Palette > Input") => "Palette",
        Some("DataTable") => "Results grid",
        Some("Editor > Input" | "Input && mode == editor") => "Editor",
        Some("ConnectionForm") => "Connection form",
        Some("Dialog") => "Dialogs",
        Some(other) => other,
    }
}

/// A group's id in words. Ids come from `keyboard_group` and stay stable;
/// only the label is translated.
fn keyboard_group_label(group: &'static str) -> Cow<'static, str> {
    match group {
        "Everywhere" => t!("settings.group.everywhere"),
        "Palette" => t!("settings.group.palette"),
        "Results grid" => t!("settings.group.results_grid"),
        "Editor" => t!("settings.group.editor"),
        "Connection form" => t!("settings.group.connection_form"),
        "Dialogs" => t!("settings.group.dialogs"),
        "Explorer" => t!("settings.group.explorer"),
        other => Cow::Borrowed(other),
    }
}

/// How a chord's `secondary-` is spelled for someone typing it: the glyphs
/// `keycap_text` draws cannot be typed, so `cmd-shift-p` and `escape` have to
/// find their rows too.
const SECONDARY_KEY: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "ctrl"
};

/// The rows `query` finds, in the panel's order, across every category.
///
/// A row is matched on its title, its group, its category and its keywords,
/// and a binding on its chord too, so `⌘P` or `cmd-p` finds whatever holds
/// it. Never on
/// the description: a paragraph holds nearly any run of letters in order.
///
/// Each word typed has to match within one of those, not across them: run
/// together, "sort" is found in "Close Tab Everywhere Keyboard" (an s, then o,
/// r and t further on), and nearly every row matches any short word.
pub(crate) fn filter_settings(
    query: &str,
    overrides: &HashMap<String, String>,
) -> Vec<SettingMatch> {
    let words: Vec<(Query, bool)> = query
        .split_whitespace()
        .map(|word| (Query::new(word), word.starts_with('!')))
        .collect();
    let mut fuzzy = Fuzzy::new();
    let mut found = Vec::new();
    for row in setting_rows() {
        let mut fields: Vec<String> =
            [row.title.to_string(), row.group.to_string(), row.tab.name()]
                .into_iter()
                .chain(row.keywords.iter().copied().map(str::to_string))
                .collect();
        if let SettingId::Keybinding(id) = row.id
            && let Some(spec) = keybinding(id)
        {
            for chord in keybindings::chords_for(spec, overrides) {
                fields.push(keycap_text(chord));
                fields.push(chord.replace("secondary", SECONDARY_KEY));
            }
        }
        // fzf's `!word` excludes: it holds only when no field has the word.
        let matches = words.iter().all(|(word, excludes)| {
            let mut matched = fields.iter().map(|field| fuzzy.matches(word, field));
            if *excludes {
                matched.all(|kept| kept)
            } else {
                matched.any(|found| found)
            }
        });
        if !matches {
            continue;
        }
        let title = words
            .iter()
            .filter(|(_, excludes)| !excludes)
            .flat_map(|(word, _)| fuzzy.highlights(word, &row.title))
            .collect();
        found.push(SettingMatch {
            id: row.id,
            title: merged(title),
        });
    }
    found
}

/// Ranges sorted and joined where they overlap or touch, which is how
/// `ui::highlighted` takes them: each word typed highlights on its own, and
/// two can land on the same letters.
fn merged(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

fn keybinding(id: &str) -> Option<&'static KeybindingSpec> {
    keybindings::available_registry().find(|spec| spec.id == id)
}

/// Whether a row is on offer as things are set: the one theme while the
/// theme does not follow the system, and its two sides while it does.
fn applies(id: SettingId, settings: &Settings) -> bool {
    match id {
        SettingId::Theme => !settings.match_system_appearance,
        SettingId::SystemTheme(_) => settings.match_system_appearance,
        _ => true,
    }
}

/// A row on screen, with the characters of its title the search matched.
struct Shown<'a> {
    row: &'a SettingRow,
    matched: &'a [Range<usize>],
}

pub(crate) fn render_settings_panel(
    workspace: &Workspace,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = popup_theme(cx);
    let searching = workspace.settings_matches.is_some();
    let rows: Vec<SettingRow> = setting_rows()
        .into_iter()
        .filter(|row| applies(row.id, &workspace.settings))
        .collect();
    let shown: Vec<Shown> = match &workspace.settings_matches {
        Some(found) => rows
            .iter()
            .filter_map(|row| {
                let found = found.iter().find(|found| found.id == row.id)?;
                Some(Shown {
                    row,
                    matched: &found.title,
                })
            })
            .collect(),
        None => rows
            .iter()
            .filter(|row| row.tab == workspace.settings_tab)
            .map(|row| Shown { row, matched: &[] })
            .collect(),
    };

    let body = if shown.is_empty() {
        render_no_match(t).into_any_element()
    } else {
        let sections: Vec<AnyElement> = shown
            .chunk_by(|a, b| a.row.tab == b.row.tab)
            .map(|section| render_section(workspace, section, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(layout::SPACE_LG * 2.))
            .children(sections)
            .into_any_element()
    };

    // Inset from the window's edges rather than flush with them, so it reads
    // as a sheet set down beside the workspace rather than a column of it.
    div()
        .flex_shrink_0()
        .w(px(layout::SETTINGS_PANEL_WIDTH + layout::SPACE_SM))
        .max_w(relative(layout::SETTINGS_PANEL_MAX_SHARE))
        .h_full()
        .pt(px(layout::SPACE_SM))
        .pr(px(layout::SPACE_SM))
        .pb(px(layout::SPACE_SM))
        .child(
            div()
                .id("settings-panel")
                .track_focus(&workspace.settings_focus)
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .text_size(px(layout::chrome(layout::TEXT_MD)))
                .text_color(t.text)
                .border_1()
                .border_color(t.border_strong)
                .rounded(px(layout::RADIUS_CARD))
                .map(|card| raised_card(card, t))
                .child(render_header(workspace, t, cx))
                .child(
                    div()
                        .flex_shrink_0()
                        .px(px(layout::SPACE_LG))
                        .pb(px(layout::SPACE_MD))
                        .child(
                            Input::new(&workspace.settings_search).prefix(
                                icon(icon::SEARCH)
                                    .size(px(layout::chrome(layout::ICON_SIZE)))
                                    .text_color(t.text_muted),
                            ),
                        ),
                )
                .child(render_tabs(workspace, searching, t, cx))
                .child(
                    div()
                        .id("settings-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .smooth_scroll(&smooth("settings-body", cx))
                        .px(px(layout::SPACE_LG))
                        // On the one child rather than here, so scrolling it
                        // to the top (`scroll_to`) lands at offset zero.
                        .child(
                            div()
                                .pt(px(layout::SPACE_LG))
                                .pb(px(layout::SPACE_LG * 2.))
                                .child(body),
                        ),
                ),
        )
        .into_any_element()
}

fn render_header(workspace: &Workspace, t: Theme, cx: &mut Context<Workspace>) -> impl IntoElement {
    let tile = layout::chrome(layout::PALETTE_TILE_SIZE);
    let close = match chord_hint("show_editor", &workspace.settings.custom_keybindings) {
        hint if hint.is_empty() => t!("settings.close").to_string(),
        hint => t!("settings.close_hint", hint = hint).to_string(),
    };
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(layout::SPACE_MD))
        .min_h(px(layout::chrome(layout::SETTINGS_HEADER_HEIGHT)))
        .px(px(layout::SPACE_LG))
        .child(
            div()
                .flex_shrink_0()
                .size(px(tile))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(layout::RADIUS_CONTROL))
                .bg(t.accent)
                .child(
                    icon(icon::SLIDERS)
                        .size(px(layout::chrome(layout::ICON_SIZE)))
                        .text_color(t.on_accent),
                ),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(layout::chrome(layout::TEXT_LG)))
                .font_weight(FontWeight::MEDIUM)
                .child(t!("settings.title").to_string()),
        )
        .child(
            icon_button(
                "settings-close",
                icon::CLOSE,
                Tone::Quiet,
                Control::Compact,
                t,
            )
            .tooltip(close)
            .on_click(cx.listener(|workspace, _: &ClickEvent, window, cx| {
                workspace.close_settings(window, cx);
            })),
        )
}

/// The categories as icons, the one in front underlined in the accent. None
/// is while searching: the results come from all of them.
fn render_tabs(
    workspace: &Workspace,
    searching: bool,
    t: Theme,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let size = layout::chrome(layout::SETTINGS_TAB_SIZE);
    let underline = layout::SETTINGS_TAB_UNDERLINE_WIDTH;
    let tabs = SettingsTab::ALL.into_iter().map(|tab| {
        let active = !searching && tab == workspace.settings_tab;
        div()
            .id(("settings-tab", tab as usize))
            .relative()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(layout::RADIUS_CONTROL))
            .cursor_pointer()
            .when(!active, |tab| tab.hover(|style| style.bg(t.element_hover)))
            .child(
                icon(tab.icon())
                    .size(px(layout::chrome(layout::ICON_SIZE)))
                    .text_color(if active { t.accent } else { t.text_muted }),
            )
            .when(active, |tab| {
                tab.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left(px((size - underline) / 2.))
                        .w(px(underline))
                        .h(px(layout::SETTINGS_TAB_UNDERLINE_HEIGHT))
                        .rounded_full()
                        .bg(t.accent),
                )
            })
            .tooltip(move |window, cx| Tooltip::new(tab.name()).build(window, cx))
            .on_click(cx.listener(move |workspace, _: &ClickEvent, window, cx| {
                workspace.set_settings_tab(tab, window, cx);
            }))
    });
    div()
        .flex_shrink_0()
        .flex()
        .flex_wrap()
        .gap(px(layout::SPACE_XS))
        .px(px(layout::SPACE_LG))
        .border_b_1()
        .border_color(t.border)
        .children(tabs)
}

fn render_no_match(t: Theme) -> impl IntoElement {
    div()
        .py(px(layout::SPACE_LG * 2.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(layout::SPACE_XS))
        .child(t!("settings.no_match").to_string())
        .child(
            div()
                .text_size(px(layout::chrome(layout::TEXT_SM)))
                .text_color(t.text_muted)
                .child(t!("settings.try_another").to_string()),
        )
}

/// One category's heading and its groups of rows, every row of one category.
fn render_section(
    workspace: &Workspace,
    rows: &[Shown],
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = popup_theme(cx);
    let tab = rows[0].row.tab;
    let mono = fonts(cx).family(FontSlot::Editor).clone();
    let groups: Vec<AnyElement> = rows
        .chunk_by(|a, b| a.row.group == b.row.group)
        .map(|group| render_group(workspace, group, cx))
        .collect();
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .font_family(mono)
                .text_size(px(layout::chrome(layout::TEXT_XS)))
                .text_color(t.accent)
                .child(tab.eyebrow()),
        )
        .child(
            div()
                .mt(px(layout::SPACE_XS))
                .text_size(px(layout::chrome(layout::TEXT_LG)))
                .font_weight(FontWeight::MEDIUM)
                .child(tab.title()),
        )
        .child(
            div()
                .mt(px(layout::SPACE_XS))
                .text_size(px(layout::chrome(layout::TEXT_SM)))
                .text_color(t.text_muted)
                .child(tab.description()),
        )
        .child(
            div()
                .mt(px(layout::SPACE_LG))
                .flex()
                .flex_col()
                .gap(px(layout::SPACE_LG))
                .children(groups),
        )
        .into_any_element()
}

/// A group's label over a well of its rows.
fn render_group(workspace: &Workspace, rows: &[Shown], cx: &mut Context<Workspace>) -> AnyElement {
    let t = popup_theme(cx);
    let label = rows[0].row.group.clone();
    let rows: Vec<AnyElement> = rows
        .iter()
        .map(|row| render_row(workspace, row.row, row.matched, cx))
        .collect();
    div()
        .flex()
        .flex_col()
        .gap(px(layout::SPACE_SM))
        .child(
            div()
                .text_size(px(layout::chrome(layout::TEXT_XS)))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.text_muted)
                .child(label.to_string()),
        )
        .child(well(t, rows))
        .into_any_element()
}

fn render_row(
    workspace: &Workspace,
    row: &SettingRow,
    matched: &[Range<usize>],
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let t = popup_theme(cx);
    let glass_only = row.id == SettingId::Opacity && !t.is_glass;
    let description = row.description.as_ref().map(|description| {
        if glass_only {
            SharedString::from(format!("{description}{}", t!("settings.glass_only")))
        } else {
            SharedString::from(description.to_string())
        }
    });
    let title = highlighted(row.title.to_string(), matched, t);
    let control = render_control(workspace, row.id, t, cx);
    if matches!(row.id, SettingId::Font(_)) {
        stacked_option_row(title, description, control, t).into_any_element()
    } else {
        option_row(title, description, control, t).into_any_element()
    }
}

/// A number between two buttons: `−` steps down, `+` up, and the readout
/// between them says what the number means.
fn width_stepper(
    id: &'static str,
    value: usize,
    range: RangeInclusive<usize>,
    step: usize,
    label: String,
    set: fn(&mut Workspace, usize, &mut Context<Workspace>),
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let (min, max) = (*range.start(), *range.end());
    let t = *theme(cx);
    let down = value.saturating_sub(step).max(min);
    let up = value.saturating_add(step).min(max);
    div()
        .flex()
        .items_center()
        .gap(px(layout::SPACE_SM))
        .child(
            button((id, 0usize), "−", Tone::Quiet, Control::Compact, t)
                .disabled(value <= min)
                .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                    set(workspace, down, cx);
                })),
        )
        .child(
            div()
                .min_w(px(layout::chrome(64.)))
                .text_center()
                .text_size(px(layout::chrome(layout::TEXT_SM)))
                .child(label),
        )
        .child(
            button((id, 1usize), "+", Tone::Quiet, Control::Compact, t)
                .disabled(value >= max)
                .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                    set(workspace, up, cx);
                })),
        )
        .into_any_element()
}

fn render_control(
    workspace: &Workspace,
    id: SettingId,
    t: Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let settings = &workspace.settings;
    match id {
        SettingId::RowLimit => row_limit_menu(
            LimitTarget::Default,
            settings.row_limit,
            workspace
                .custom_limit
                .as_ref()
                .filter(|(target, _)| *target == LimitTarget::Default)
                .map(|(_, input)| input),
            t,
        ),
        SettingId::DefaultSorting => {
            let client_sort = settings.client_sort;
            segmented(t)
                .children(
                    [
                        (false, t!("settings.sorting.server")),
                        (true, t!("settings.sorting.client")),
                    ]
                    .map(|(client, label)| {
                        segmented_choice(
                            ("default-sorting", client as usize),
                            label.to_string(),
                            client == client_sort,
                            t,
                        )
                        .on_click(cx.listener(
                            move |workspace, _: &ClickEvent, _, cx| {
                                workspace.set_client_sort(client, cx);
                            },
                        ))
                    }),
                )
                .into_any_element()
        }
        SettingId::Language => {
            let current = settings.language.as_deref();
            // "System" is the locale's word, the language names are already
            // written in their own language, and the rest comes from
            // `locale::SUPPORTED`, so the choices here cannot drift from what
            // the app resolves.
            let choices = std::iter::once((0, None, t!("settings.language.system").to_string()))
                .chain(
                    crate::locale::SUPPORTED
                        .iter()
                        .enumerate()
                        .map(|(index, (code, name))| (index + 1, Some(*code), name.to_string())),
                );
            segmented(t)
                .children(choices.map(|(index, code, label)| {
                    segmented_choice(("language", index), label, code == current, t).on_click(
                        cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                            workspace.set_language(code.map(str::to_string), cx);
                        }),
                    )
                }))
                .into_any_element()
        }
        SettingId::CheckForUpdates => switch("check-for-updates", settings.check_for_updates, t)
            .on_click(cx.listener(|workspace, _: &ClickEvent, _, cx| {
                let check = !workspace.settings.check_for_updates;
                workspace.set_check_for_updates(check, cx);
            }))
            .into_any_element(),
        SettingId::VerboseLogging => switch("verbose-logging", settings.verbose_logging, t)
            .on_click(cx.listener(|workspace, _: &ClickEvent, _, cx| {
                let verbose = !workspace.settings.verbose_logging;
                workspace.set_verbose_logging(verbose, cx);
            }))
            .into_any_element(),
        SettingId::QualifyCompletions => {
            switch("qualify-completions", settings.qualify_completions, t)
                .on_click(cx.listener(|workspace, _: &ClickEvent, _, cx| {
                    let qualify = !workspace.settings.qualify_completions;
                    workspace.set_qualify_completions(qualify, cx);
                }))
                .into_any_element()
        }
        SettingId::AliasCompletions => switch("alias-completions", settings.alias_completions, t)
            .on_click(cx.listener(|workspace, _: &ClickEvent, _, cx| {
                let alias = !workspace.settings.alias_completions;
                workspace.set_alias_completions(alias, cx);
            }))
            .into_any_element(),
        SettingId::FormatKeywordCase => {
            let current = settings.format.keyword_case;
            let choices = [
                (
                    KeywordCase::Preserve,
                    t!("settings.format.keyword_case.preserve"),
                ),
                (KeywordCase::Upper, t!("settings.format.keyword_case.upper")),
                (KeywordCase::Lower, t!("settings.format.keyword_case.lower")),
            ];
            segmented(t)
                .children(choices.map(|(case, label)| {
                    segmented_choice(
                        ("format-keyword-case", case as usize),
                        label.to_string(),
                        case == current,
                        t,
                    )
                    .on_click(cx.listener(
                        move |workspace, _: &ClickEvent, _, cx| {
                            workspace.set_keyword_case(case, cx);
                        },
                    ))
                }))
                .into_any_element()
        }
        SettingId::FormatIndent => {
            let current = settings.format.indent;
            let choices = [
                (
                    IndentStyle::TwoSpaces,
                    t!("settings.format.indent.two_spaces"),
                ),
                (
                    IndentStyle::FourSpaces,
                    t!("settings.format.indent.four_spaces"),
                ),
                (IndentStyle::Tab, t!("settings.format.indent.tab")),
            ];
            segmented(t)
                .children(choices.map(|(indent, label)| {
                    segmented_choice(
                        ("format-indent", indent as usize),
                        label.to_string(),
                        indent == current,
                        t,
                    )
                    .on_click(cx.listener(
                        move |workspace, _: &ClickEvent, _, cx| {
                            workspace.set_indent(indent, cx);
                        },
                    ))
                }))
                .into_any_element()
        }
        SettingId::FormatExpressionWidth => width_stepper(
            "format-expression-width",
            settings.format.expression_width,
            EXPRESSION_WIDTH_MIN..=EXPRESSION_WIDTH_MAX,
            10,
            t!(
                "settings.format.width_value",
                width = settings.format.expression_width
            )
            .to_string(),
            |workspace, width, cx| workspace.set_expression_width(width, cx),
            cx,
        ),
        SettingId::FormatCompactClauses => {
            let width = settings.format.compact_clauses.unwrap_or(0);
            let label = if width == 0 {
                t!("settings.format.compact_clauses.off").to_string()
            } else {
                t!("settings.format.width_value", width = width).to_string()
            };
            width_stepper(
                "format-compact-clauses",
                width,
                0..=COMPACT_CLAUSES_MAX,
                20,
                label,
                |workspace, width, cx| workspace.set_compact_clauses(width, cx),
                cx,
            )
        }
        SettingId::FormatJoins => switch(
            "format-joins-on-their-own-line",
            settings.format.joins_on_their_own_line,
            t,
        )
        .on_click(cx.listener(|workspace, _: &ClickEvent, _, cx| {
            let on = !workspace.settings.format.joins_on_their_own_line;
            workspace.set_joins_on_their_own_line(on, cx);
        }))
        .into_any_element(),
        SettingId::RunUnreadable => switch("run-unreadable", settings.run_unreadable, t)
            .on_click(cx.listener(|workspace, _: &ClickEvent, _, cx| {
                let run = !workspace.settings.run_unreadable;
                workspace.set_run_unreadable(run, cx);
            }))
            .into_any_element(),
        SettingId::ColorTitlebar => switch("color-titlebar", settings.color_titlebar, t)
            .on_click(cx.listener(|workspace, _: &ClickEvent, _, cx| {
                let color = !workspace.settings.color_titlebar;
                workspace.set_color_titlebar(color, cx);
            }))
            .into_any_element(),
        SettingId::MatchSystemAppearance => switch(
            "match-system-appearance",
            settings.match_system_appearance,
            t,
        )
        .on_click(cx.listener(|workspace, _: &ClickEvent, window, cx| {
            let on = !workspace.settings.match_system_appearance;
            workspace.set_match_system_appearance(on, window, cx);
        }))
        .into_any_element(),
        // The palette lists the themes, and moving through it previews each
        // one on the window this panel sits in.
        SettingId::Theme => picker("theme", t.name, t)
            .on_click(cx.listener(|workspace, _: &ClickEvent, window, cx| {
                workspace.open_palette(PaletteMode::Theme(None), window, cx);
            }))
            .into_any_element(),
        // A side's list previews too, whichever side the system is on.
        SettingId::SystemTheme(side) => picker(
            ("system-theme", side as usize),
            settings.theme_for(side).to_string(),
            t,
        )
        .on_click(cx.listener(move |workspace, _: &ClickEvent, window, cx| {
            workspace.open_palette(PaletteMode::Theme(Some(side)), window, cx);
        }))
        .into_any_element(),
        SettingId::Opacity => render_opacity(workspace, t, cx),
        SettingId::Font(slot) => render_font(workspace, slot, t, cx),
        SettingId::Keybinding(id) => match keybinding(id) {
            Some(spec) => render_keybinding(workspace, spec, t, cx),
            None => div().into_any_element(),
        },
    }
}

fn render_opacity(workspace: &Workspace, t: Theme, cx: &mut Context<Workspace>) -> AnyElement {
    // Whole percents, as the field reads: a step can land on 0.77000004, and
    // comparing that f32 to a bound would leave a button live that does nothing.
    let opacity = opacity_percent(t.opacity);
    let default_opacity = t.default_opacity();
    // Disabled wholesale on an opaque theme rather than hidden: the setting is
    // still remembered, it is just that a theme which paints its own chrome
    // has no desktop behind it for this to let through.
    div()
        .flex()
        .items_center()
        .gap(px(layout::SPACE_SM))
        .child(
            button("opacity-down", "−", Tone::Quiet, Control::Compact, t)
                .disabled(!t.is_glass || opacity <= opacity_percent(OPACITY_MIN))
                .on_click(cx.listener(|workspace, _: &ClickEvent, window, cx| {
                    workspace.step_opacity(-OPACITY_STEP, window, cx);
                })),
        )
        .child(
            // The sign sits beside the field rather than in it. `suffix` lays
            // out inside the width declared here and pads again on its own, so
            // a small input carrying one leaves under twenty points for the
            // text: at 56 the committed 95 rendered as a clipped 9 and a 5.
            // This is the readout's own width instead -- three typed digits
            // and the caret, since 100 is reachable on the way to a value that
            // clamps, inside the small size's 8-point padding.
            Input::new(&workspace.opacity_input)
                .small()
                .w(px(52.))
                .disabled(!t.is_glass),
        )
        .child(
            div()
                .text_size(px(layout::chrome(layout::TEXT_SM)))
                .text_color(t.text_faint)
                .child("%"),
        )
        .child(
            button("opacity-up", "+", Tone::Quiet, Control::Compact, t)
                .disabled(!t.is_glass || opacity >= opacity_percent(OPACITY_MAX))
                .on_click(cx.listener(|workspace, _: &ClickEvent, window, cx| {
                    workspace.step_opacity(OPACITY_STEP, window, cx);
                })),
        )
        .child(
            button(
                "opacity-reset",
                t!("settings.reset").to_string(),
                Tone::Quiet,
                Control::Compact,
                t,
            )
            .disabled(!t.is_glass)
            .on_click(cx.listener(move |workspace, _: &ClickEvent, window, cx| {
                workspace.set_opacity(default_opacity, window, cx);
            })),
        )
        .into_any_element()
}

/// The family, picked from the palette, which lists every family the text
/// system resolved and marks the one in use; then the size.
fn render_font(
    workspace: &Workspace,
    slot: FontSlot,
    t: Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let index = slot as usize;
    let family = fonts(cx).family(slot).clone();
    let size = workspace.settings.font_size(slot);
    let (min, default, max) = font_size_range(slot);
    // Disabled at the ends rather than clamped again here: `step_font_size`
    // already refuses to go past them, and a button that looks live and does
    // nothing is worse than one that says it cannot.
    let stepper = div()
        .flex()
        .items_center()
        .child(
            button(
                ("font-smaller", index),
                "−",
                Tone::Quiet,
                Control::Compact,
                t,
            )
            .disabled(size <= min)
            .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                workspace.step_font_size(slot, -FONT_SIZE_STEP, cx);
            })),
        )
        .child(
            div()
                .min_w(px(layout::chrome(layout::CONTROL_HEIGHT)))
                .flex()
                .justify_center()
                .child(button_label(
                    format!("{size}"),
                    Tone::Quiet,
                    Control::Compact,
                    t,
                )),
        )
        .child(
            button(
                ("font-larger", index),
                "+",
                Tone::Quiet,
                Control::Compact,
                t,
            )
            .disabled(size >= max)
            .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                workspace.step_font_size(slot, FONT_SIZE_STEP, cx);
            })),
        );
    let reset = icon_button(
        ("font-size-reset", index),
        icon::RESET,
        Tone::Quiet,
        Control::Compact,
        t,
    )
    .disabled(size == default)
    .tooltip(t!("settings.reset_to", default = default).to_string())
    .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
        workspace.set_font_size(slot, default, cx);
    }));
    // The picker takes what the family's name needs and the rest of the row
    // is air, so the steppers line up down the three rows.
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(layout::SPACE_SM))
        .child(picker(("font", index), family, t).on_click(cx.listener(
            move |workspace, _: &ClickEvent, window, cx| {
                workspace.open_palette(PaletteMode::Font(slot), window, cx);
            },
        )))
        .child(div().flex_1())
        .child(stepper.child(reset))
        .into_any_element()
}

/// What is bound to the action today and the buttons that change it, or --
/// while it is the row [`Workspace::rebinding`] names -- the prompt for the
/// next keystroke. The keystroke itself is taken by the interceptor
/// `Workspace::new` installs; nothing here listens for keys.
fn render_keybinding(
    workspace: &Workspace,
    spec: &'static KeybindingSpec,
    t: Theme,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let id = spec.id;
    if workspace.rebinding == Some(id) {
        return div()
            .text_size(px(layout::chrome(layout::TEXT_SM)))
            .text_color(t.accent)
            .child(t!("settings.press_any_key").to_string())
            .into_any_element();
    }
    let overrides = &workspace.settings.custom_keybindings;
    let chords = keybindings::chords_for(spec, overrides);
    let bound = if chords.is_empty() {
        div()
            .text_size(px(layout::chrome(layout::TEXT_SM)))
            .text_color(t.text_faint)
            .child(t!("settings.unbound").to_string())
    } else {
        div()
            .flex()
            .items_center()
            .gap(px(layout::SPACE_SM))
            .children(chords.into_iter().map(|chord| keycaps(chord, t)))
    };
    div()
        .flex()
        .items_center()
        .gap(px(layout::SPACE_SM))
        .child(bound)
        .child(
            button(
                SharedString::from(format!("keybind-edit-{id}")),
                t!("settings.edit").to_string(),
                Tone::Quiet,
                Control::Compact,
                t,
            )
            .on_click(cx.listener(move |workspace, _: &ClickEvent, window, cx| {
                workspace.start_rebind(id, window, cx);
            })),
        )
        .children(overrides.contains_key(id).then(|| {
            button(
                SharedString::from(format!("keybind-reset-{id}")),
                t!("settings.reset").to_string(),
                Tone::Quiet,
                Control::Compact,
                t,
            )
            .on_click(cx.listener(move |workspace, _: &ClickEvent, _, cx| {
                workspace.reset_keybinding(id, cx);
            }))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(found: Vec<SettingMatch>) -> Vec<SettingId> {
        found.into_iter().map(|found| found.id).collect()
    }

    fn found(query: &str) -> Vec<SettingId> {
        ids(filter_settings(query, &HashMap::new()))
    }

    #[test]
    fn font_finds_the_three_font_rows() {
        let found = found("font");
        for slot in [FontSlot::Chrome, FontSlot::Editor, FontSlot::Grid] {
            assert!(found.contains(&SettingId::Font(slot)), "{found:?}");
        }
    }

    #[test]
    fn a_setting_is_found_by_the_word_the_user_would_type() {
        assert!(found("sort").contains(&SettingId::DefaultSorting));
        assert!(found("titlebar").contains(&SettingId::ColorTitlebar));
        assert!(found("autocomplete").contains(&SettingId::QualifyCompletions));
        assert!(found("alias").contains(&SettingId::AliasCompletions));
        assert!(found("unparseable").contains(&SettingId::RunUnreadable));
        assert!(found("transparency").contains(&SettingId::Opacity));
        assert!(found("updates").contains(&SettingId::CheckForUpdates));
        assert!(found("auto").contains(&SettingId::MatchSystemAppearance));
    }

    #[test]
    fn a_binding_is_found_by_its_label_and_by_its_chord() {
        assert!(found("palette next row").contains(&SettingId::Keybinding("palette_next")));
        let chord = keycap_text("secondary-shift-p");
        assert!(found(&chord).contains(&SettingId::Keybinding("command_palette")));
    }

    #[test]
    fn a_binding_is_found_by_its_keys_spelled_out() {
        assert!(found("escape").contains(&SettingId::Keybinding("show_editor")));
        let spelled = format!("{SECONDARY_KEY}-shift-p");
        assert!(found(&spelled).contains(&SettingId::Keybinding("command_palette")));
    }

    #[test]
    fn a_rebound_chord_is_searched_as_rebound() {
        let overrides = HashMap::from([("run_query".to_string(), "secondary-shift-j".to_string())]);
        let chord = keycap_text("secondary-shift-j");
        assert!(
            ids(filter_settings(&chord, &overrides)).contains(&SettingId::Keybinding("run_query"))
        );
        assert!(!found(&chord).contains(&SettingId::Keybinding("run_query")));
    }

    #[test]
    fn a_rebound_action_gives_up_its_old_chord() {
        let shipped = keycap_text(keybinding("run_query").unwrap().defaults[0]);
        let run_query = SettingId::Keybinding("run_query");
        assert!(found(&shipped).contains(&run_query));
        let overrides = HashMap::from([("run_query".to_string(), "secondary-shift-j".to_string())]);
        assert!(!ids(filter_settings(&shipped, &overrides)).contains(&run_query));
    }

    #[test]
    fn a_word_matches_within_one_field_not_across_them() {
        assert_eq!(
            found("sort"),
            [
                SettingId::DefaultSorting,
                SettingId::Keybinding("reset_confirmations"),
                SettingId::Keybinding("palette_history_filter"),
                SettingId::Keybinding("palette_history_sort"),
                SettingId::Keybinding("sort_active_column"),
            ]
        );
        assert_eq!(found("!keyboard").len(), 22);
    }

    #[test]
    fn the_title_highlights_every_word_that_landed_in_it() {
        let found = filter_settings("color bar", &HashMap::new());
        let titlebar = found
            .iter()
            .find(|found| found.id == SettingId::ColorTitlebar)
            .unwrap();
        assert_eq!(titlebar.title, [0..5, 15..18]);
        assert_eq!(merged(vec![4..6, 0..2, 1..3, 6..7]), [0..3, 4..7]);
    }

    #[test]
    fn results_span_categories() {
        let found = found("editor");
        assert!(found.contains(&SettingId::Font(FontSlot::Editor)));
        assert!(found.contains(&SettingId::Keybinding("toggle_comment")));
    }

    #[test]
    fn nothing_typed_lists_every_row_in_category_order() {
        let found = found("  ");
        let rows = setting_rows();
        assert_eq!(found, rows.iter().map(|row| row.id).collect::<Vec<_>>());
        let tabs: Vec<SettingsTab> = rows.iter().map(|row| row.tab).collect();
        assert!(tabs.is_sorted_by_key(|tab| *tab as usize));
    }

    #[test]
    fn nonsense_finds_nothing() {
        assert!(found("qzxv").is_empty());
    }

    #[test]
    fn every_row_carries_real_text_not_its_key() {
        // `t!` falls back to the key itself when an entry is missing, in either
        // locale; a row built on one would show the key on screen and search
        // on it too.
        for row in setting_rows() {
            for field in [&row.title, &row.group] {
                assert!(!field.starts_with("settings."), "{:?}: {}", row.id, field);
                assert!(!field.starts_with("key."), "{:?}: {}", row.id, field);
            }
            if let Some(description) = &row.description {
                assert!(
                    !description.starts_with("settings."),
                    "{:?}: {}",
                    row.id,
                    description
                );
            }
        }
    }

    #[test]
    fn matching_the_system_trades_the_theme_row_for_its_two_sides() {
        let mut settings = Settings::default();
        let offered = |settings: &Settings| -> Vec<SettingId> {
            setting_rows()
                .into_iter()
                .filter(|row| row.group == "Theme" && applies(row.id, settings))
                .map(|row| row.id)
                .collect()
        };
        assert_eq!(
            offered(&settings),
            [
                SettingId::MatchSystemAppearance,
                SettingId::Theme,
                SettingId::Opacity
            ]
        );
        settings.match_system_appearance = true;
        assert_eq!(
            offered(&settings),
            [
                SettingId::MatchSystemAppearance,
                SettingId::SystemTheme(Appearance::Light),
                SettingId::SystemTheme(Appearance::Dark),
                SettingId::Opacity
            ]
        );
    }

    #[test]
    fn the_eyebrow_numbers_the_category() {
        assert_eq!(SettingsTab::General.eyebrow(), "01 · GENERAL");
        assert_eq!(SettingsTab::Keyboard.eyebrow(), "04 · KEYBOARD");
    }
}
