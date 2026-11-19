//! The Run dialog of a routine: one field per argument it takes, and the call
//! those arguments make, in a query tab of its own.
//!
//! The call lands in a buffer rather than running from here, deliberately. What
//! it produces is a statement the user may read, change and run again, and a
//! routine's call is exactly the statement a mode gate and a Cancel already
//! know how to handle -- so it goes through the same path a typed one does.

use gpui_component::Sizable;
use gpui_component::input::InputState;

use super::forms::confirm_button;
use super::*;
use crate::db::{Routine, RoutineParameter, alter};

enum Load {
    Loading,
    Loaded(Vec<RoutineParameter>),
    Failed(String),
}

pub(crate) struct RoutineCall {
    schema: String,
    routine: Routine,
    load: Load,
    /// One per parameter, in the same order. Empty until the first frame after
    /// the parameters land, since creating a field needs a window.
    values: Vec<Entity<InputState>>,
    /// Whether that frame has come, which an empty parameter list would
    /// otherwise look like it never had.
    seeded: bool,
    error: Option<String>,
}

impl Workspace {
    /// Ask the server what `routine` takes, and open the call dialog over it.
    pub(crate) fn call_routine(
        &mut self,
        schema: String,
        routine: Routine,
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
        let (asked, asking) = (schema.clone(), routine.clone());
        let parameters = spawn_blocking(cx.background_executor(), move || {
            connection.routine_parameters(&asked, &asking)
        });
        let Some(profile) = self.profile_mut() else {
            return;
        };
        profile.session.routine_call = Some(RoutineCall {
            schema,
            routine,
            load: Load::Loading,
            values: Vec::new(),
            seeded: false,
            error: None,
        });
        cx.notify();

        cx.spawn(async move |workspace, cx| {
            let result = parameters.await;
            workspace
                .update(cx, |workspace, cx| {
                    if workspace.issued_to(&profile_id, generation).is_none() {
                        return;
                    }
                    let Some(call) = workspace
                        .profile_mut()
                        .and_then(|profile| profile.session.routine_call.as_mut())
                    else {
                        return;
                    };
                    call.load = match result {
                        Ok(parameters) => Load::Loaded(parameters),
                        Err(error) => Load::Failed(super::server_said(&error.message)),
                    };
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    pub(crate) fn cancel_routine_call(&mut self, cx: &mut Context<Self>) {
        if let Some(profile) = self.profile_mut() {
            profile.session.routine_call = None;
        }
        cx.notify();
    }

    /// Run: write the call into a new query tab and send it.
    pub(crate) fn run_routine_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.profile() else {
            return;
        };
        let Some(call) = profile.session.routine_call.as_ref() else {
            return;
        };
        let Load::Loaded(parameters) = &call.load else {
            return;
        };
        let arguments: Vec<(RoutineParameter, String)> = parameters
            .iter()
            .zip(&call.values)
            .map(|(parameter, value)| (parameter.clone(), value.read(cx).value().to_string()))
            .collect();
        let sql = alter::call_statement(self.engine(), &call.schema, &call.routine, &arguments);
        if let Some(profile) = self.profile_mut() {
            profile.session.routine_call = None;
        }
        self.open_query_with(sql, true, window, cx);
    }

    pub(crate) fn render_routine_call(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // The fields are made at the first frame after the parameters land:
        // creating one needs a window, and the parameters arrive on a task.
        let pending = self
            .profile()
            .and_then(|profile| profile.session.routine_call.as_ref())
            .filter(|call| !call.seeded && matches!(call.load, Load::Loaded(_)))
            .and_then(|call| match &call.load {
                Load::Loaded(parameters) => Some(parameters.clone()),
                _ => None,
            });
        if let Some(parameters) = pending {
            let values: Vec<Entity<InputState>> = parameters
                .iter()
                .map(|_| cx.new(|cx| InputState::new(window, cx).placeholder("NULL")))
                .collect();
            if let Some(call) = self
                .profile_mut()
                .and_then(|profile| profile.session.routine_call.as_mut())
            {
                call.values = values;
                call.seeded = true;
            }
        }

        let t = popup_theme(cx);
        let profile = self.profile()?;
        let call = profile.session.routine_call.as_ref()?;
        let title = format!(
            "{} — {}.{}",
            t!("routine_call.title"),
            call.schema,
            call.routine.name
        );
        let error = call.error.clone();
        let loading = match &call.load {
            Load::Loading => Some(None),
            Load::Loaded(_) => None,
            Load::Failed(error) => Some(Some(error.clone())),
        };
        let rows: Vec<(String, String, Entity<InputState>)> = match &call.load {
            Load::Loaded(parameters) => parameters
                .iter()
                .zip(&call.values)
                .map(|(parameter, value)| {
                    (
                        parameter.name.clone(),
                        parameter.data_type.clone(),
                        value.clone(),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        let ready = matches!(call.load, Load::Loaded(_));

        let cancel = cx.entity().downgrade();
        let run = cx.entity().downgrade();

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
                        .w(px(520.))
                        .child(section_label(t, &title))
                        .children(loading.map(|loading| {
                            let (text, colour) = match loading {
                                None => (t!("routine_call.reading").to_string(), t.text_muted),
                                Some(error) => (error, t.danger),
                            };
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(colour)
                                .child(text)
                                .into_any_element()
                        }))
                        .children((!rows.is_empty()).then(|| {
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(layout::SPACE_XS))
                                .children(rows.into_iter().map(|(name, data_type, value)| {
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(layout::SPACE_XS))
                                        .child(
                                            div()
                                                .w(px(150.))
                                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                                .child(name),
                                        )
                                        .child(
                                            div()
                                                .w(px(130.))
                                                .text_color(t.text_muted)
                                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                                .child(data_type),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(Input::new(&value).small()),
                                        )
                                }))
                                .into_any_element()
                        }))
                        .child(
                            div()
                                .text_size(px(layout::chrome(layout::TEXT_SM)))
                                .text_color(t.text_muted)
                                .child(t!("routine_call.note")),
                        )
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
                                    self.choice(
                                        "routine-call",
                                        1,
                                        button(
                                            "routine-cancel",
                                            t!("button.cancel"),
                                            Tone::Quiet,
                                            Control::Standard,
                                            t,
                                        )
                                        .on_click(
                                            move |_, _, cx| {
                                                _ = cancel.update(cx, |workspace, cx| {
                                                    workspace.cancel_routine_call(cx);
                                                });
                                            },
                                        ),
                                        t,
                                    ),
                                )
                                .child(
                                    self.choice(
                                        "routine-call",
                                        0,
                                        confirm_button(
                                            "routine-run",
                                            t!("routine_call.run"),
                                            Tone::Primary,
                                            t,
                                            cx,
                                        )
                                        .disabled(!ready)
                                        .on_click(
                                            move |_, window, cx| {
                                                _ = run.update(cx, |workspace, cx| {
                                                    workspace.run_routine_call(window, cx);
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
