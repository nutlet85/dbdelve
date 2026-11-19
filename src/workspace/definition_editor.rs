//! The definition editor: a view's or a routine's own `CREATE`, loaded from the
//! server, rewritten into a replacement, and run from a dialog.
//!
//! What it shows is the statement that runs, whole and editable -- the server's
//! own rendering with its first keyword changed, never a regeneration, so a
//! body, a comment and a clause dbdelve has no model of all survive. A
//! definition that cannot be rewritten that way is refused before the dialog
//! opens: dbdelve writes no `DROP`, so a view is never replaced by dropping it
//! (hard rule 1).

use gpui_component::input::{Editor, EditorState};

use super::forms::confirm_button;
use super::*;
use crate::db::{RelationKind, Routine, RoutineKind, alter};
use crate::session::AfterDdl;

/// What a dialog is being asked to replace.
#[derive(Clone)]
pub(crate) enum DefinitionTarget {
    Relation {
        schema: String,
        name: String,
        kind: RelationKind,
    },
    Routine {
        schema: String,
        routine: Routine,
    },
}

enum Load {
    Loading,
    Loaded,
    Failed(String),
}

pub(crate) struct DefinitionEditor {
    target: DefinitionTarget,
    /// The statement the server's definition was rewritten into, which the
    /// editor is built over at the first frame after it lands.
    text: String,
    pub(crate) editor: Option<Entity<EditorState>>,
    load: Load,
    pub(crate) error: Option<String>,
    pub(crate) applying: bool,
}

impl DefinitionTarget {
    fn title(&self) -> String {
        match self {
            Self::Relation { schema, name, .. } => {
                format!("{} — {schema}.{name}", t!("definition_editor.title_view"))
            }
            Self::Routine { schema, routine } => {
                let label = match routine.kind {
                    RoutineKind::Function => t!("definition_editor.title_function"),
                    RoutineKind::Procedure => t!("definition_editor.title_procedure"),
                };
                format!(
                    "{label} — {schema}.{}({})",
                    routine.name, routine.identity_arguments
                )
            }
        }
    }
}

impl Workspace {
    /// Load `target`'s definition and open the editor over it.
    ///
    /// Only called where the engine has a replacement to derive, which is the
    /// menu's own question ([`crate::db::Target::view_definition`]).
    pub(crate) fn edit_definition(&mut self, target: DefinitionTarget, cx: &mut Context<Self>) {
        self.clear_notice();
        let Some(profile) = self.profile() else {
            return;
        };
        let Some(connection) = profile.connection() else {
            return;
        };
        let profile_id = profile.id.clone();
        let generation = profile.generation;
        let engine = self.engine();
        let asking = target.clone();
        let read = spawn_blocking(cx.background_executor(), move || match &asking {
            DefinitionTarget::Relation {
                schema, name, kind, ..
            } => connection
                .ddl(schema, name, *kind)
                .map(|definition| alter::view_replacement(engine, *kind, &definition)),
            DefinitionTarget::Routine { schema, routine } => connection
                .routine_ddl(schema, routine)
                .map(|definition| alter::routine_replacement(engine, &definition)),
        });
        let Some(profile) = self.profile_mut() else {
            return;
        };
        profile.session.definition_editor = Some(DefinitionEditor {
            target,
            text: String::new(),
            editor: None,
            load: Load::Loading,
            error: None,
            applying: false,
        });
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let result = read.await;
            workspace
                .update(cx, |workspace, cx| {
                    if workspace.issued_to(&profile_id, generation).is_none() {
                        return;
                    }
                    let Some(editor) = workspace
                        .profile_mut()
                        .and_then(|profile| profile.session.definition_editor.as_mut())
                    else {
                        return;
                    };
                    editor.load = match result {
                        Err(error) => Load::Failed(super::server_said(&error.message)),
                        // The server's text could not be found, or this engine
                        // has no spelling for it: refusing beats running
                        // something that is not the object.
                        Ok(None) => Load::Failed(t!("definition_editor.unsafe").to_string()),
                        Ok(Some(statement)) => {
                            editor.text = statement;
                            Load::Loaded
                        }
                    };
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    /// The object's own `CREATE`, as the server wrote it, in a query tab: for
    /// an engine with no replacement statement, where changing a definition
    /// means running the `DROP` and the `CREATE` the user decides on.
    ///
    /// Nothing is rewritten and nothing is dropped here. What lands in the
    /// buffer is the definition exactly as read, which is the user's statement
    /// from that point on.
    pub(crate) fn define_in_query(
        &mut self,
        target: DefinitionTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_notice();
        let Some(profile) = self.profile() else {
            return;
        };
        let Some(connection) = profile.connection() else {
            return;
        };
        let profile_id = profile.id.clone();
        let generation = profile.generation;
        let read = spawn_blocking(cx.background_executor(), move || match &target {
            DefinitionTarget::Relation {
                schema, name, kind, ..
            } => connection.ddl(schema, name, *kind),
            DefinitionTarget::Routine { schema, routine } => {
                connection.routine_ddl(schema, routine)
            }
        });
        cx.spawn_in(window, async move |workspace, cx| {
            let result = read.await;
            _ = workspace.update_in(cx, |workspace, window, cx| {
                if workspace.issued_to(&profile_id, generation).is_none() {
                    return;
                }
                match result {
                    Ok(definition) => {
                        workspace.open_query_with(definition, false, window, cx);
                    }
                    Err(error) => workspace.note(error.message, cx),
                }
            });
        })
        .detach();
    }

    pub(crate) fn cancel_definition_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.profile_mut() {
            profile.session.definition_editor = None;
        }
        cx.notify();
    }

    /// Apply: the buffer is the statement, so what runs is what is on screen.
    pub(crate) fn apply_definition(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self
            .profile()
            .and_then(|profile| profile.session.definition_editor.as_ref())
        else {
            return;
        };
        let Some(state) = &editor.editor else {
            return;
        };
        let sql = state.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return self.definition_error(Some(t!("definition_editor.nothing").to_string()), cx);
        }
        if let Some(editor) = self
            .profile_mut()
            .and_then(|profile| profile.session.definition_editor.as_mut())
        {
            editor.error = None;
            editor.applying = true;
        }
        cx.notify();
        self.run_ddl(sql, AfterDdl::Catalog, cx);
    }

    fn definition_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(editor) = self
            .profile_mut()
            .and_then(|profile| profile.session.definition_editor.as_mut())
        {
            editor.error = error;
            editor.applying = false;
        }
        cx.notify();
    }

    /// What a run of this dialog's statement leaves behind: the dialog closing
    /// over a catalog that is reloading, or the server's own words.
    pub(crate) fn definition_ddl_landed(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        match error {
            Some(error) => self.definition_error(Some(error), cx),
            None => {
                if let Some(profile) = self.profile_mut() {
                    profile.session.definition_editor = None;
                }
                cx.notify();
            }
        }
    }

    pub(crate) fn render_definition_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Built here rather than where the text landed: an editor entity needs
        // a window, and the definition arrives on a task that has none.
        let pending = self
            .profile()
            .and_then(|profile| profile.session.definition_editor.as_ref())
            .filter(|editor| editor.editor.is_none() && matches!(editor.load, Load::Loaded))
            .map(|editor| editor.text.clone());
        if let Some(text) = pending {
            let syntax = self.engine().syntax();
            let state = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(syntax.highlighter())
                    .soft_wrap(false)
                    .default_value(text.clone())
            });
            if let Some(editor) = self
                .profile_mut()
                .and_then(|profile| profile.session.definition_editor.as_mut())
            {
                editor.editor = Some(state);
            }
        }

        let t = popup_theme(cx);
        let font = fonts(cx).editor.clone();
        let size = self.settings.editor_font_size;
        let profile = self.profile()?;
        let editor = profile.session.definition_editor.as_ref()?;
        let title = editor.target.title();
        let error = editor.error.clone();
        let applying = editor.applying;
        let loading = match &editor.load {
            Load::Loading => Some(None),
            Load::Loaded => None,
            Load::Failed(error) => Some(Some(error.clone())),
        };
        let state = editor.editor.clone();

        let cancel = cx.entity().downgrade();
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
                        .w(px(760.))
                        .child(section_label(t, &title))
                        .child(
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(t.text_muted)
                                .child(t!("definition_editor.note")),
                        )
                        .children(loading.map(|loading| {
                            let (text, colour) = match loading {
                                None => (t!("definition_editor.reading").to_string(), t.text_muted),
                                Some(error) => (error, t.danger),
                            };
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(colour)
                                .child(text)
                                .into_any_element()
                        }))
                        .children(state.map(|state| {
                            div()
                                .h(px(layout::DIALOG_TEXT_MAX_HEIGHT * 1.6))
                                .p(px(layout::SPACE_SM))
                                .rounded(px(layout::RADIUS_CONTROL))
                                .border_1()
                                .border_color(t.border)
                                .font_family(font)
                                .child(
                                    Editor::new(&state)
                                        .h_full()
                                        .appearance(false)
                                        .bordered(false)
                                        .text_size(px(size)),
                                )
                        }))
                        .children(error.map(|reason| {
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
                                    button(
                                        "definition-format",
                                        t!("editor.format_query"),
                                        Tone::Quiet,
                                        Control::Standard,
                                        t,
                                    )
                                    .on_click(cx.listener(
                                        |workspace, _: &ClickEvent, window, cx| {
                                            workspace.format_query(&FormatQuery, window, cx);
                                        },
                                    )),
                                )
                                .child(
                                    self.choice(
                                        "definition-editor",
                                        1,
                                        button(
                                            "definition-cancel",
                                            t!("button.cancel"),
                                            Tone::Quiet,
                                            Control::Standard,
                                            t,
                                        )
                                        .on_click(
                                            move |_, _, cx| {
                                                _ = cancel.update(cx, |workspace, cx| {
                                                    workspace.cancel_definition_editor(cx);
                                                });
                                            },
                                        ),
                                        t,
                                    ),
                                )
                                .child(
                                    self.choice(
                                        "definition-editor",
                                        0,
                                        confirm_button(
                                            "definition-apply",
                                            t!("button.apply"),
                                            Tone::Primary,
                                            t,
                                            cx,
                                        )
                                        .disabled(applying)
                                        .on_click(
                                            move |_, _, cx| {
                                                _ = apply.update(cx, |workspace, cx| {
                                                    workspace.apply_definition(cx);
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
