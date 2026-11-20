//! The app driven the way a person drives it: launched from a saved
//! `profiles.toml`, typed into, run from the keyboard. GPUI's test platform
//! has no screen, so these run headless in CI like any other test.

use std::{borrow::Cow, cell::RefCell, rc::Rc};

use gpui::{
    App, AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext, Window,
};
use gpui_component::{Root, input::InputState};

use super::Workspace;
use crate::{
    actions::{CancelQuery, Disconnect, ImportRowsFromClipboard, RunQuery},
    connection_form::ConnectionForm,
    db::{CancelToken, ConnectionConfig, Engine, ExternalConfig, Fetch, SshTunnel},
    explorer, keybindings,
    palette::{Mode as PaletteMode, Scope},
    session::{CatalogState, ObjectBody, ProfileState, QueryState, QueryTab},
    store,
    theme::{self, Fonts},
};

/// What `main` sets up before the window opens, minus the menu bar and the
/// panic log, which the test platform has no use for.
fn launch(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    launch_with(cx, Vec::new())
}

/// `launch` with these plugins discovered.
fn launch_with<'a>(
    cx: &'a mut TestAppContext,
    plugins: Vec<&'static crate::db::Plugin>,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        cx.text_system()
            .add_fonts(
                guic_gpui_assets::BUNDLED_FONTS
                    .iter()
                    .map(|font| Cow::Borrowed(*font))
                    .collect(),
            )
            .expect("bundled fonts must be loadable");
        gpui_component::init(cx);
        cx.set_global(Fonts::default());
        theme::user::load();
        let theme = theme::starting_theme(theme::Appearance::of_system(cx.window_appearance()));
        theme.apply_to_components(cx);
        cx.set_global(theme);
        let overrides = store::peek_profiles(&plugins)
            .and_then(|list| list.settings)
            .and_then(|settings| settings.custom_keybindings)
            .unwrap_or_default();
        cx.bind_keys(keybindings::build_bindings(&overrides));
    });
    let mut workspace = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let entity = cx.new(|cx| Workspace::new(plugins, window, cx));
        workspace = Some(entity.clone());
        Root::new(entity, window, cx)
    });
    (workspace.expect("the window builds the workspace"), cx)
}

/// Every write `Workspace` makes on the way out -- buffers on release,
/// profiles on quit -- has to land while `with_home` still points the store at
/// the temporary home, not after the test returns and `HOME` is the real one.
fn quit(workspace: Entity<Workspace>, cx: &mut VisualTestContext) {
    drop(workspace);
    // On the app rather than the window: shutting down removes the window.
    cx.cx.update(|cx| cx.shutdown());
    cx.cx.run_until_parked();
}

/// The app launched on one saved SQLite profile, an empty database, with its
/// query tab in front.
fn launch_on_sqlite(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    launch_on_sqlite_holding(cx, "")
}

/// `launch_on_sqlite` over a database `schema` has been run against first.
fn launch_on_sqlite_holding<'a>(
    cx: &'a mut TestAppContext,
    schema: &str,
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    let directory = store::dbdelve_directory().expect("the test home has a data directory");
    std::fs::create_dir_all(&directory).expect("the data directory must be creatable");
    // An empty file is an empty database. dbdelve opens a SQLite file
    // rather than creating one, so it has to be there first.
    let database = directory.join("e2e.db");
    std::fs::write(&database, b"").expect("the database file must be creatable");
    rusqlite::Connection::open(&database)
        .and_then(|connection| connection.execute_batch(schema))
        .expect("the schema must apply");
    std::fs::write(
        directory.join("profiles.toml"),
        format!(
            "active = \"e2e\"\n\
             [settings]\n\
             check_for_updates = false\n\
             [[profiles]]\n\
             id = \"e2e\"\n\
             name = \"E2E\"\n\
             host = \"\"\n\
             database = \"\"\n\
             user = \"\"\n\
             engine = \"sqlite\"\n\
             path = {:?}\n",
            database.display().to_string()
        ),
    )
    .expect("profiles.toml must be writable");

    let (workspace, cx) = launch(cx);
    // CI's live-test job sets `PG*`, which puts an environment profile in
    // front of the saved one.
    workspace.update(cx, |workspace, cx| {
        let index = workspace
            .profiles
            .iter()
            .position(|profile| profile.config.engine() == Engine::Sqlite)
            .expect("the saved SQLite profile is restored");
        workspace.activate(index, cx);
    });
    cx.run_until_parked();
    (workspace, cx)
}

/// The test platform has no frame loop: what the app leaves for the next
/// frame, such as a dialog handing back the keyboard, waits for this.
fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

fn query_tab<T>(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
    read: impl FnOnce(&QueryTab, &Window, &App) -> T,
) -> T {
    cx.update(|window, cx| {
        let tab = workspace
            .read(cx)
            .profile()
            .and_then(|profile| profile.session.active_query_tab())
            .expect("a fresh profile opens on a query tab");
        read(tab, window, cx)
    })
}

fn buffer(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
    query_tab(workspace, cx, |tab, _, cx| {
        tab.editor.read(cx).value().to_string()
    })
}

fn editor_has_keyboard(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    query_tab(workspace, cx, |tab, window, cx| {
        tab.editor.focus_handle(cx).is_focused(window)
    })
}

fn grid_has_keyboard(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    query_tab(workspace, cx, |tab, window, cx| {
        tab.results.focus_handle(cx).is_focused(window)
    })
}

fn sidebar_has_keyboard(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        workspace
            .read(cx)
            .explorer_focus
            .contains_focused(window, cx)
    })
}

fn dialog_has_keyboard(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| workspace.read(cx).dialog_focus.is_focused(window))
}

fn ringed_button(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> usize {
    workspace.read_with(cx, |workspace, _| workspace.dialog_choice.get())
}

/// `selec` is no statement `sql::classify` can read, so running it puts up the
/// run-once prompt instead of reaching the database.
fn raise_run_once_prompt(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("secondary-enter");
    settle(cx);
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.dialog_in_front.get(), Some("pending-run"));
    });
}

#[gpui::test]
fn a_query_tab_names_the_target_it_runs_on(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        settle(cx);
        let bounds = cx
            .debug_bounds("query-execution-target")
            .expect("the query toolbar names the execution target");
        assert!(
            bounds.size.width > gpui::px(20.),
            "the target chip draws its label, not just its box: {bounds:?}"
        );
        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_query_typed_and_run_from_the_keyboard_lands_its_rows(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);

        cx.simulate_input("select 1");
        cx.simulate_keystrokes("secondary-enter");
        cx.run_until_parked();

        query_tab(&workspace, cx, |tab, _, _| match &tab.query {
            QueryState::Complete { rows, .. } => assert_eq!(*rows, 1),
            QueryState::Failed(error) => panic!("the query failed: {error}"),
            _ => panic!("the query never completed"),
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_prompt_takes_the_keyboard_and_hands_it_back_once_answered(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("selec 1");
        raise_run_once_prompt(&workspace, cx);

        assert!(dialog_has_keyboard(&workspace, cx));
        assert_eq!(
            ringed_button(&workspace, cx),
            0,
            "the ring starts on Run once"
        );
        cx.simulate_input("x");
        settle(cx);
        assert_eq!(
            buffer(&workspace, cx),
            "selec 1",
            "the buffer behind took a key"
        );

        for (keystroke, ringed) in [("left", 1), ("right", 0), ("tab", 2), ("shift-tab", 0)] {
            cx.simulate_keystrokes(keystroke);
            settle(cx);
            assert_eq!(ringed_button(&workspace, cx), ringed, "after {keystroke}");
        }

        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(
            buffer(&workspace, cx),
            "selec 1",
            "enter reached the buffer"
        );
        query_tab(&workspace, cx, |tab, _, _| {
            assert!(
                matches!(tab.query, QueryState::Failed(_)),
                "Run once did not run the statement"
            );
        });
        assert!(editor_has_keyboard(&workspace, cx));
        cx.simulate_input("x");
        settle(cx);
        assert_eq!(buffer(&workspace, cx), "selec 1x");

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_prompt_dismissed_from_the_keyboard_hands_it_back_to_the_editor(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("selec 1");

        for (dismiss, typed) in [("tab tab enter", "x"), ("escape", "y")] {
            raise_run_once_prompt(&workspace, cx);
            let before = buffer(&workspace, cx);
            cx.simulate_keystrokes(dismiss);
            settle(cx);
            workspace.read_with(cx, |workspace, _| {
                assert_eq!(
                    workspace.dialog_in_front.get(),
                    None,
                    "{dismiss} left it up"
                );
            });
            query_tab(&workspace, cx, |tab, _, _| {
                assert!(matches!(tab.query, QueryState::Idle), "{dismiss} ran it");
            });
            assert!(editor_has_keyboard(&workspace, cx), "after {dismiss}");
            cx.simulate_input(typed);
            settle(cx);
            assert_eq!(buffer(&workspace, cx), format!("{before}{typed}"));
        }

        quit(workspace, cx);
    });
}

/// A statement that is still running cannot be driven here: its clock ticks
/// on a timer, which the test scheduler fires at once, so nothing parks until
/// it ends. What the chord resolves to, and who handles it, is checked
/// instead, from the editor and from a grid on screen.
#[gpui::test]
fn the_cancel_chord_reaches_the_workspace_from_the_editor_and_the_grid(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("select 1");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        query_tab(&workspace, cx, |tab, _, _| {
            assert!(matches!(tab.query, QueryState::Complete { .. }));
        });

        let resolved: Rc<RefCell<Vec<bool>>> = Rc::default();
        let _observer = cx.update({
            let resolved = resolved.clone();
            |_, cx| {
                cx.observe_keystrokes(move |event, _, _| {
                    resolved.borrow_mut().push(
                        event
                            .action
                            .as_ref()
                            .is_some_and(|action| action.as_any().is::<CancelQuery>()),
                    );
                })
            }
        });
        let (editor, results) = query_tab(&workspace, cx, |tab, _, _| {
            (tab.editor.clone(), tab.results.clone())
        });
        for in_grid in [false, true] {
            cx.update(|window, cx| match in_grid {
                true => results.focus_handle(cx).focus(window, cx),
                false => editor.focus_handle(cx).focus(window, cx),
            });
            settle(cx);
            cx.update(|window, cx| {
                let focused = match in_grid {
                    true => results.focus_handle(cx),
                    false => editor.focus_handle(cx),
                };
                assert!(focused.is_focused(window), "in grid: {in_grid}");
                assert!(
                    window.is_action_available(&CancelQuery, cx),
                    "in grid: {in_grid}"
                );
            });
            resolved.borrow_mut().clear();
            cx.simulate_keystrokes("secondary-.");
            assert_eq!(*resolved.borrow(), [true], "in grid: {in_grid}");
        }

        quit(workspace, cx);
    });
}

#[cfg(debug_assertions)]
#[gpui::test]
fn a_running_query_keeps_its_two_cancel_buttons_distinct_in_the_a11y_tree(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.update(|window, _| window.set_a11y_active_for_test(true));
        // Long enough to still be running when the tree below is built; the
        // Cancel chord stops it once the tree has been read.
        cx.simulate_input(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 20000000) SELECT count(*) FROM c;",
        );
        cx.update(|window, cx| window.dispatch_action(Box::new(RunQuery), cx));
        query_tab(&workspace, cx, |tab, _, _| {
            assert!(
                matches!(tab.query, QueryState::Running { .. }),
                "the query should still be running"
            );
        });
        let tree = cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.debug_a11y_tree_json()
        });
        let tree = tree.expect("the accessibility tree should be captured");
        for id in ["cancel-query-pane", "cancel-query-status"] {
            assert!(tree.contains(id), "the tree should contain {id}");
        }
        // The ids are distinct on purpose: the a11y builder panics on a
        // duplicate node, and the check above catches either id changing.
        cx.simulate_keystrokes("secondary-.");
        settle(cx);
        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_note_about_one_tab_shows_only_while_that_tab_is_in_front(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        let shown = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                let session = &workspace.profile().expect("a profile is open").session;
                session.shown_notice().map(str::to_string)
            })
        };
        let first = workspace.update(cx, |workspace, _| {
            let session = &mut workspace.profile_mut().unwrap().session;
            session.tab_notice = Some((session.active, "cancelled here".into()));
            session.active
        });
        assert_eq!(shown(cx).as_deref(), Some("cancelled here"));

        cx.simulate_keystrokes("secondary-t");
        settle(cx);
        assert_eq!(shown(cx), None);

        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.activate_tab(first, window, cx)
            })
        });
        settle(cx);
        assert_eq!(shown(cx).as_deref(), Some("cancelled here"));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_move_tab_chords_reorder_the_strip(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        let strip = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                let session = &workspace.profile().expect("a profile is open").session;
                (session.strip_tabs(), session.active)
            })
        };
        cx.simulate_keystrokes("secondary-t");
        settle(cx);
        let (before, active) = strip(cx);
        assert_eq!(before.last(), Some(&active), "a new tab opens at the end");

        cx.simulate_keystrokes("ctrl-shift-pageup");
        settle(cx);
        let (after, _) = strip(cx);
        assert_eq!(after[before.len() - 2], active);

        cx.simulate_keystrokes("ctrl-shift-pagedown");
        settle(cx);
        assert_eq!(strip(cx).0, before);

        // The right end is a wall.
        cx.simulate_keystrokes("ctrl-shift-pagedown");
        settle(cx);
        assert_eq!(strip(cx).0, before);

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_command_palette_opens_on_the_welcome_surface(cx: &mut TestAppContext) {
    // A `PG*` connection from the environment opens in place of the welcome
    // surface, as CI's live-test job sets.
    if ["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    {
        return;
    }
    store::tests::with_home(|| {
        let (workspace, cx) = launch(cx);
        settle(cx);
        workspace.read_with(cx, |workspace, _| assert!(workspace.profile().is_none()));

        cx.simulate_keystrokes("secondary-shift-p");
        settle(cx);
        workspace.read_with(cx, |workspace, cx| {
            let palette = workspace.palette.as_ref().expect("the palette is up");
            assert!(palette.read(cx).mode() == PaletteMode::Search(Scope::Commands));
        });
        cx.simulate_keystrokes("escape");
        settle(cx);
        workspace.read_with(cx, |workspace, _| assert!(workspace.palette.is_none()));

        // The titlebar's magnifier is the way in without the chord.
        let icon = cx
            .debug_bounds("command-palette")
            .expect("the titlebar opens the palette")
            .center();
        cx.simulate_click(icon, gpui::Modifiers::default());
        settle(cx);
        workspace.read_with(cx, |workspace, cx| {
            let palette = workspace.palette.as_ref().expect("the palette is up");
            assert!(palette.read(cx).mode() == PaletteMode::Search(Scope::Commands));
        });
        cx.simulate_keystrokes("escape");
        settle(cx);

        quit(workspace, cx);
    });
}

#[gpui::test]
fn ctrl_space_asks_for_completions_in_the_query_buffer(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        let menu_open = |cx: &mut VisualTestContext| {
            query_tab(&workspace, cx, |tab, _, cx| {
                tab.editor.read(cx).completion_menu_state().open
            })
        };
        cx.simulate_input("sel");
        settle(cx);
        cx.simulate_keystrokes("escape");
        settle(cx);
        assert!(!menu_open(cx), "escape left the menu up");

        cx.simulate_keystrokes("ctrl-space");
        settle(cx);
        assert!(menu_open(cx));
        assert_eq!(buffer(&workspace, cx), "sel");

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_keyboard_moves_between_the_buffer_and_its_rows(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("select 1");
        cx.simulate_keystrokes("secondary-k r");
        settle(cx);
        assert!(
            editor_has_keyboard(&workspace, cx),
            "with no rows yet, the buffer keeps it"
        );

        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        cx.simulate_keystrokes("secondary-k r");
        settle(cx);
        assert!(grid_has_keyboard(&workspace, cx));
        query_tab(&workspace, cx, |tab, _, cx| {
            assert_eq!(tab.results.read(cx).delegate().active(), Some((0, 0)));
        });

        cx.simulate_keystrokes("escape");
        settle(cx);
        assert!(editor_has_keyboard(&workspace, cx));
        cx.simulate_input("x");
        settle(cx);
        assert_eq!(buffer(&workspace, cx), "select 1x");

        cx.simulate_keystrokes("secondary-k r");
        settle(cx);
        assert!(grid_has_keyboard(&workspace, cx));
        cx.simulate_keystrokes("secondary-k r");
        settle(cx);
        assert!(editor_has_keyboard(&workspace, cx));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_sidebar_takes_the_keyboard_and_escape_hands_it_back(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        workspace.update(cx, |workspace, _| workspace.sidebar_hidden = true);

        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        workspace.read_with(cx, |workspace, _| assert!(!workspace.sidebar_hidden));
        assert!(sidebar_has_keyboard(&workspace, cx));

        // On the schema's row, which folds away its Tables and the table.
        cx.simulate_keystrokes("enter");
        settle(cx);
        cx.update(|_, cx| {
            let tree = &workspace.read(cx).profile().unwrap().session.explorer_tree;
            assert!(
                tree.read(cx).entry(1).is_none(),
                "enter left the schema open"
            );
        });
        assert!(sidebar_has_keyboard(&workspace, cx));

        cx.simulate_keystrokes("escape");
        settle(cx);
        assert!(editor_has_keyboard(&workspace, cx));
        cx.simulate_input("x");
        settle(cx);
        assert_eq!(buffer(&workspace, cx), "x");

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_schema_folded_without_a_filter_is_saved_and_collapse_all_closes_it(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        let filter_to = |text: &'static str, cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let profile = workspace.profile().unwrap();
                    let (id, filter) =
                        (profile.id.clone(), profile.session.explorer_filter.clone());
                    filter.update(cx, |input, cx| input.set_value(text, window, cx));
                    workspace.refresh_explorer(&id, cx);
                });
            });
        };
        let fold_schema = |cx: &mut VisualTestContext| {
            cx.simulate_keystrokes("secondary-k e");
            settle(cx);
            cx.simulate_keystrokes("enter");
            settle(cx);
        };
        let schema_open = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                let tree = &workspace.read(cx).profile().unwrap().session.explorer_tree;
                tree.read(cx).entry(0).unwrap().is_expanded()
            })
        };
        let saved = || {
            store::peek_profiles(&[]).unwrap().profiles[0]
                .folds
                .get("schema-main")
                .copied()
        };

        filter_to("birds", cx);
        fold_schema(cx);
        assert!(!schema_open(cx));
        assert_eq!(saved(), None);
        filter_to("", cx);
        assert!(schema_open(cx), "the only schema, opened by default");

        fold_schema(cx);
        assert!(!schema_open(cx));
        assert_eq!(saved(), Some(false));

        // Picked as the active schema while folded under a filter, it opens
        // there and then, not once the filter is cleared.
        filter_to("birds", cx);
        fold_schema(cx);
        assert!(!schema_open(cx));
        workspace.update(cx, |workspace, cx| workspace.set_schema("main".into(), cx));
        settle(cx);
        assert!(schema_open(cx));
        assert_eq!(saved(), Some(true));

        cx.dispatch_action(crate::actions::CollapseAllSchemas);
        settle(cx);
        let filter = cx.update(|_, cx| {
            let session = &workspace.read(cx).profile().unwrap().session;
            session.explorer_filter.read(cx).value().to_string()
        });
        assert_eq!(filter, "", "a filter would have held the schema open");
        assert!(!schema_open(cx));
        assert_eq!(saved(), Some(false));

        cx.dispatch_action(crate::actions::ExpandAllSchemas);
        settle(cx);
        assert!(schema_open(cx));
        assert_eq!(saved(), Some(true));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn enter_opens_a_sidebar_row_and_its_structure_takes_the_keyboard(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (name text); insert into birds values ('wren');",
        );

        // The schema, its Tables, then the table.
        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        cx.simulate_keystrokes("down down enter");
        settle(cx);
        // Whether the tab in front is the structure, and whether its grid has
        // the keyboard.
        let front_grid_has_keyboard = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let tab = workspace
                    .read(cx)
                    .profile()
                    .and_then(|profile| profile.session.active_object())
                    .expect("an object tab is in front");
                assert_eq!(tab.name, "birds");
                match &tab.body {
                    ObjectBody::Relation { results, .. } => {
                        (false, results.focus_handle(cx).is_focused(window))
                    }
                    ObjectBody::Structure { grid, .. } => {
                        (true, grid.focus_handle(cx).is_focused(window))
                    }
                    ObjectBody::Routine(_) => panic!("birds is a table"),
                }
            })
        };
        assert_eq!(
            front_grid_has_keyboard(cx),
            (false, true),
            "enter opened the rows"
        );

        cx.simulate_keystrokes("secondary-shift-d");
        settle(cx);
        assert_eq!(front_grid_has_keyboard(cx), (true, true));
        cx.simulate_keystrokes("down");
        settle(cx);
        cx.update(|_, cx| {
            let workspace = workspace.read(cx);
            let Some(ObjectBody::Structure { grid, .. }) = workspace
                .profile()
                .and_then(|profile| profile.session.active_object())
                .map(|tab| &tab.body)
            else {
                panic!("the structure tab is in front");
            };
            assert!(
                grid.read(cx).delegate().selection_text().is_some(),
                "the arrow reached the structure grid"
            );
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_sidebar_selects_the_row_of_the_tab_in_front(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (name text); create table flocks (id integer);",
        );
        let selected = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                let session = &workspace.read(cx).profile().unwrap().session;
                session
                    .explorer_tree
                    .read(cx)
                    .selected_item()
                    .map(|item| item.id.to_string())
            })
        };
        let active = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                workspace.profile().unwrap().session.active
            })
        };
        let open = |name: &str, cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                let leaf = workspace.profile().unwrap().session.explorer_leaves
                    [&explorer::relation_row("main", name)];
                workspace.open_explorer_target(leaf.target, window, cx);
            });
            settle(cx);
        };
        let birds_row = explorer::relation_row("main", "birds");
        let flocks_row = explorer::relation_row("main", "flocks");
        assert_eq!(selected(cx), None, "a scratch query has no row");

        open("birds", cx);
        assert_eq!(selected(cx), Some(birds_row.clone()));
        let birds = active(cx);
        open("flocks", cx);
        assert_eq!(selected(cx), Some(flocks_row.clone()));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_tab(birds, window, cx);
        });
        settle(cx);
        assert_eq!(
            selected(cx),
            Some(birds_row.clone()),
            "a tab switch follows"
        );

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.new_query(&crate::actions::NewQuery, window, cx);
        });
        settle(cx);
        assert_eq!(selected(cx), None, "a new scratch query leaves no row lit");

        workspace.update_in(cx, |workspace, window, cx| {
            workspace.ask_for_name(String::new(), window, cx);
            let name = workspace.profile().unwrap().session.save_name.clone();
            name.update(cx, |input, cx| input.set_value("daily", window, cx));
            workspace.confirm_save(window, cx);
        });
        settle(cx);
        assert_eq!(
            selected(cx),
            Some(explorer::query_row("daily")),
            "saved, the query has a row of its own"
        );

        let daily = active(cx);
        workspace.update_in(cx, |workspace, window, cx| {
            let crate::session::Tab::Query(id) = daily else {
                panic!("the saved query is in front");
            };
            workspace.close_buffer(id, window, cx);
        });
        settle(cx);
        let fallback =
            workspace.read_with(cx, |workspace, _| workspace.profile().unwrap().front_row());
        assert!(fallback.is_some(), "a table tab came to the front");
        assert_eq!(selected(cx), fallback, "a close follows to the tab behind");

        // A pin repeats the table's row; selected, it keeps the selection.
        workspace.update(cx, |workspace, cx| {
            let profile = workspace.profile_mut().unwrap();
            profile
                .shortcuts
                .toggle_pin(explorer::Pin::Member(explorer::Member::Object(
                    explorer::ObjectRef {
                        database: None,
                        schema: "main".into(),
                        name: "birds".into(),
                        kind: explorer::ObjectKind::Relation(crate::db::RelationKind::Table),
                    },
                )));
            let id = profile.id.clone();
            workspace.refresh_explorer(&id, cx);
        });
        settle(cx);
        let pinned = workspace.update(cx, |workspace, cx| {
            let tree = workspace.profile().unwrap().session.explorer_tree.clone();
            tree.update(cx, |tree, cx| {
                let pinned = tree.entry(1).expect("the pin is listed").item().id.clone();
                assert_ne!(pinned.as_ref(), birds_row);
                tree.set_selected_index(Some(1), cx);
                pinned.to_string()
            })
        });
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_tab(birds, window, cx);
        });
        settle(cx);
        assert_eq!(selected(cx), Some(pinned));

        // Arrowed off the tab's row, the selection outlives a rebuild that
        // lands in the background, as table sizes and folder changes do.
        workspace.update(cx, |workspace, cx| {
            let profile = workspace.profile().unwrap();
            let tree = profile.session.explorer_tree.clone();
            let id = profile.id.clone();
            tree.update(cx, |tree, cx| {
                let flocks = tree.index_of(&flocks_row.clone().into());
                assert!(flocks.is_some(), "flocks is listed");
                tree.set_selected_index(flocks, cx);
            });
            workspace.refresh_query_library(&id, cx);
        });
        settle(cx);
        assert_eq!(
            selected(cx),
            Some(flocks_row.clone()),
            "a background rebuild keeps the row the user moved to"
        );

        quit(workspace, cx);
    });
}

/// A key pressed and let go: a focused button clicks on the release, which
/// `simulate_keystrokes` never sends.
fn press(cx: &mut VisualTestContext, key: &str) {
    let keystroke = gpui::Keystroke::parse(key).expect("a key the test names");
    cx.simulate_event(gpui::KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui::KeyUpEvent { keystroke });
    settle(cx);
}

fn in_dialog(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| workspace.read(cx).dialog_focus.contains_focused(window, cx))
}

fn front_dialog(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<&'static str> {
    workspace.read_with(cx, |workspace, _| workspace.dialog_in_front.get())
}

fn palette_mode(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<PaletteMode> {
    workspace.read_with(cx, |workspace, cx| {
        workspace
            .palette
            .as_ref()
            .map(|palette| palette.read(cx).mode())
    })
}

#[gpui::test]
fn tab_walks_the_import_dialog_and_its_buttons_answer_the_keyboard(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        let file = store::dbdelve_directory().unwrap().join("birds.csv");
        std::fs::write(&file, "name\nwren\n").unwrap();
        workspace.update_in(cx, |workspace, window, cx| {
            let profile = workspace.profile().unwrap().id.clone();
            workspace.open_import_file(profile, "main".into(), "birds".into(), file, window, cx);
        });
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("import-rows"));
        assert!(dialog_has_keyboard(&workspace, cx));
        let format = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
                dialog.map(|dialog| dialog.options.format)
            })
        };

        // Change file…, then the format's CSV and TSV.
        cx.simulate_keystrokes("tab tab tab");
        settle(cx);
        assert!(in_dialog(&workspace, cx));
        assert!(!dialog_has_keyboard(&workspace, cx));
        press(cx, "space");
        assert_eq!(format(cx), Some(crate::import_rows::Format::Tsv));

        // JSON, the header box, then the delimiter's list, which `escape` closes
        // without taking the dialog down with it.
        let in_menu = |cx: &mut VisualTestContext| {
            cx.update(|window, _| {
                window
                    .context_stack()
                    .iter()
                    .any(|context| context.contains("PopupMenu"))
            })
        };
        cx.simulate_keystrokes("tab tab tab");
        settle(cx);
        press(cx, "space");
        assert!(in_menu(cx), "space opened no list");
        cx.simulate_keystrokes("escape");
        settle(cx);
        assert!(!in_menu(cx));
        assert_eq!(front_dialog(&workspace, cx), Some("import-rows"));

        // Round and round, never out of the dialog nor into its preview.
        let preview = workspace.read_with(cx, |workspace, _| {
            let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
            dialog.unwrap().preview.clone()
        });
        for _ in 0..40 {
            cx.simulate_keystrokes("tab");
            settle(cx);
            assert!(in_dialog(&workspace, cx));
            cx.update(|window, cx| assert!(!preview.focus_handle(cx).is_focused(window)));
        }

        // The file is checked a moment after the last change to how it is read.
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        settle(cx);
        let skip_lines = workspace.read_with(cx, |workspace, _| {
            let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
            assert_eq!(super::import_dialog::import_blocked(dialog.unwrap()), None);
            dialog.unwrap().inputs.skip_lines.clone()
        });
        cx.update(|window, cx| skip_lines.focus_handle(cx).focus(window, cx));
        press(cx, "enter");
        workspace.read_with(cx, |workspace, _| {
            let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
            let editing = matches!(dialog.unwrap().phase, crate::session::ImportPhase::Editing);
            assert!(editing, "enter in a field started the import");
        });

        // From the dialog's own focus, back to Import and then Cancel.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.dialog_focus.focus(window, cx)
        });
        cx.simulate_keystrokes("shift-tab shift-tab");
        press(cx, "enter");
        assert_eq!(front_dialog(&workspace, cx), None);
        assert!(editor_has_keyboard(&workspace, cx));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn copy_and_paste_in_a_table_carry_rows_into_the_import_dialog(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, name text, note text);
             insert into birds values (1, 'wren', null), (2, 'robin', '');",
        );
        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        cx.simulate_keystrokes("down down enter");
        settle(cx);
        let clipboard = |cx: &mut VisualTestContext| {
            let item = cx.read_from_clipboard().expect("something was copied");
            let metadata = item.entries().iter().find_map(|entry| match entry {
                gpui::ClipboardEntry::String(string) => string.metadata.clone(),
                _ => None,
            });
            (item.text().unwrap_or_default(), metadata)
        };
        let gutter = |row: usize, cx: &mut VisualTestContext| {
            let selector: &'static str = format!("row-number-{row}").leak();
            cx.debug_bounds(selector).expect("the row is drawn")
        };

        let first = gutter(0, cx).center();
        cx.simulate_click(first, gpui::Modifiers::default());
        settle(cx);
        cx.simulate_keystrokes("secondary-c");
        let (text, metadata) = clipboard(cx);
        assert_eq!(text, "id\tname\tnote\n1\twren\t\n");
        assert!(metadata.is_some(), "a row copy carries its rows");

        let second = gutter(1, cx).center();
        cx.simulate_click(second, gpui::Modifiers::shift());
        settle(cx);
        cx.simulate_keystrokes("secondary-c");
        let (text, metadata) = clipboard(cx);
        assert_eq!(text, "id\tname\tnote\n1\twren\t\n2\trobin\t\"\"\n");
        let pasted = crate::import_rows::pasted(&text, metadata.as_deref(), false);
        assert_eq!(
            pasted.map(|(format, bytes, _)| (format, String::from_utf8(bytes).unwrap())),
            Some((
                crate::import_rows::Format::Json,
                "{\"id\":\"1\",\"name\":\"wren\",\"note\":null}\n\
                 {\"id\":\"2\",\"name\":\"robin\",\"note\":\"\"}"
                    .into()
            ))
        );

        // A cell of the first row, clicked: that cell alone, as Copy Cell has it.
        let cell = gutter(0, cx).right_center() + gpui::point(gpui::px(12.), gpui::px(0.));
        cx.simulate_click(cell, gpui::Modifiers::default());
        settle(cx);
        cx.simulate_keystrokes("right secondary-c");
        settle(cx);
        assert_eq!(clipboard(cx), ("wren".into(), None));

        // An open input keeps both keystrokes for its own text.
        let resolved: Rc<RefCell<Vec<Option<&'static str>>>> = Rc::default();
        let _observer = cx.update({
            let resolved = resolved.clone();
            |_, cx| {
                cx.observe_keystrokes(move |event, _, _| {
                    let action = event.action.as_ref().map(|action| action.name());
                    resolved.borrow_mut().push(action);
                })
            }
        });
        cx.simulate_keystrokes("enter");
        settle(cx);
        resolved.borrow_mut().clear();
        cx.simulate_keystrokes("secondary-c secondary-v");
        settle(cx);
        assert_eq!(
            *resolved.borrow(),
            [Some("input::Copy"), Some("input::Paste")]
        );
        assert_eq!(front_dialog(&workspace, cx), None);
        cx.simulate_keystrokes("escape");
        settle(cx);

        cx.write_to_clipboard(gpui::ClipboardItem::new_string("name\nfinch\n".into()));
        cx.simulate_keystrokes("secondary-v");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("import-rows"));
        workspace.read_with(cx, |workspace, _| {
            let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
            let dialog = dialog.expect("the dialog is open");
            assert_eq!(dialog.source.name(), "Clipboard");
            assert_eq!(&*dialog.bytes, b"name\nfinch\n");
        });

        // A value a paste could not type holds the import up where it is.
        let profile =
            workspace.read_with(cx, |workspace, _| workspace.profile().unwrap().id.clone());
        workspace.update(cx, |workspace, cx| {
            let session = &mut workspace.profile_mut().unwrap().session;
            let dialog = session.import_rows.as_mut().unwrap();
            dialog.options.format = crate::import_rows::Format::Json;
            dialog.bytes = b"{\"name\":\"finch\"}".as_slice().into();
            dialog.source =
                crate::session::ImportSource::Clipboard(vec![crate::import_rows::Problem {
                    row: Some(0),
                    line: Some(1),
                    column: Some(0),
                    message: "unreadable".into(),
                }]);
            workspace.recheck_import(&profile, cx);
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
            let Some(Ok(prepared)) = &dialog.unwrap().prepared else {
                panic!("the paste was not checked");
            };
            let problems = &prepared.checked.problems;
            assert!(
                problems
                    .iter()
                    .any(|problem| problem.message == "unreadable"),
                "{problems:?}"
            );
        });

        // A paste read after another import was asked for, or after a dialog
        // opened, opens nothing.
        let take = |cx: &mut VisualTestContext| {
            workspace.update(cx, |workspace, _| {
                workspace.profile_mut().unwrap().session.import_rows.take()
            })
        };
        let shown = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
                dialog.map(|dialog| dialog.bytes.to_vec())
            })
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("late\n1\n".into()));
        let opened = take(cx);
        let opened_bytes = opened.as_ref().map(|dialog| dialog.bytes.to_vec());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.import_rows_from_clipboard(&ImportRowsFromClipboard, window, cx);
            workspace.profile_mut().unwrap().session.import_rows = opened;
        });
        settle(cx);
        assert_eq!(shown(cx), opened_bytes);

        take(cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.import_rows_from_clipboard(&ImportRowsFromClipboard, window, cx);
            workspace.import_rows_into("main".into(), "birds".into(), window, cx);
        });
        settle(cx);
        assert_eq!(shown(cx), None);

        // Nor one read while the profile moved to another database, whose
        // table of the same name is not the one asked for.
        let moved = |workspace: &mut Workspace| workspace.profile_mut().unwrap().generation += 1;
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.import_rows_from_clipboard(&ImportRowsFromClipboard, window, cx);
            moved(workspace);
        });
        settle(cx);
        assert_eq!(shown(cx), None);

        // A file read is held to both the same way.
        let file = store::dbdelve_directory().unwrap().join("late.csv");
        std::fs::write(&file, "id\n7\n").unwrap();
        let read_file =
            |workspace: &mut Workspace, window: &mut Window, cx: &mut gpui::Context<Workspace>| {
                let profile = workspace.profile().unwrap().id.clone();
                workspace.open_import_file(
                    profile,
                    "main".into(),
                    "birds".into(),
                    file.clone(),
                    window,
                    cx,
                );
            };
        workspace.update_in(cx, |workspace, window, cx| {
            read_file(workspace, window, cx);
            workspace.import_rows_from_clipboard(&ImportRowsFromClipboard, window, cx);
        });
        settle(cx);
        assert_eq!(shown(cx), Some(b"late\n1\n".to_vec()));
        take(cx);
        workspace.update_in(cx, |workspace, window, cx| {
            read_file(workspace, window, cx);
            moved(workspace);
        });
        settle(cx);
        assert_eq!(shown(cx), None);

        quit(workspace, cx);
    });
}

#[gpui::test]
fn rows_copied_from_one_table_paste_into_another_exactly(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, note text, big integer, ratio real, body text, pic blob);
             insert into birds values
               (1, null, 9223372036854775807, 0.1, 'a	b \"q\" ''s''
ünï ✓', x'00FF10'),
               (2, '', -9007199254740993, 1.5e300, 'plain', null),
               (3, 'NULL', 0, -2.5, '', x'');
             create table nests (pic blob, body text, ratio real, big integer, note text, id integer primary key);",
        );
        cx.executor().allow_parking();
        let read = |table: &str| -> Vec<Vec<String>> {
            // Not `pic`: Import Rows writes the grid's `x'00FF10'` for a blob
            // as text, as it does reading the same value from a file.
            let columns = ["id", "note", "big", "ratio", "body"]
                .map(|c| format!("typeof({c}) || ' ' || quote({c})"))
                .join(", ");
            let db = rusqlite::Connection::open(store::dbdelve_directory().unwrap().join("e2e.db"))
                .unwrap();
            let mut statement = db
                .prepare(&format!("select {columns} from {table} order by id"))
                .unwrap();
            statement
                .query_map([], |row| (0..5).map(|i| row.get::<_, String>(i)).collect())
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let open = |name: &str, cx: &mut VisualTestContext| {
            cx.simulate_keystrokes("secondary-p");
            settle(cx);
            cx.simulate_input(name);
            cx.simulate_keystrokes("enter");
            settle(cx);
            let front = workspace.read_with(cx, |workspace, _| {
                let session = &workspace.profile().unwrap().session;
                session.active_object().map(|tab| tab.name.clone())
            });
            assert_eq!(front.as_deref(), Some(name));
        };
        let paste_and_import = |cx: &mut VisualTestContext| {
            cx.simulate_keystrokes("secondary-v");
            settle(cx);
            assert_eq!(front_dialog(&workspace, cx), Some("import-rows"));
            cx.executor()
                .advance_clock(std::time::Duration::from_secs(1));
            settle(cx);
            workspace.read_with(cx, |workspace, _| {
                let dialog = workspace.profile().unwrap().session.import_rows.as_ref();
                assert_eq!(super::import_dialog::import_blocked(dialog.unwrap()), None);
            });
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.dialog_focus.focus(window, cx)
            });
            cx.simulate_keystrokes("shift-tab");
            press(cx, "enter");
            for _ in 0..100 {
                if front_dialog(&workspace, cx).is_none() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
                settle(cx);
            }
            assert_eq!(
                front_dialog(&workspace, cx),
                None,
                "the import never finished"
            );
        };

        open("birds", cx);
        let gutter = |row: usize, cx: &mut VisualTestContext| {
            let selector: &'static str = format!("row-number-{row}").leak();
            cx.debug_bounds(selector).expect("the row is drawn")
        };
        let first = gutter(0, cx).center();
        cx.simulate_click(first, gpui::Modifiers::default());
        let last = gutter(2, cx).center();
        cx.simulate_click(last, gpui::Modifiers::shift());
        settle(cx);
        cx.simulate_keystrokes("secondary-c");

        // The way a mouse opens it: an empty table, its keyboard on nothing
        // the grid draws.
        let nests = explorer_row(&workspace, "relation-main\0nests", cx).center();
        cx.simulate_click(nests, gpui::Modifiers::default());
        settle(cx);
        paste_and_import(cx);
        assert_eq!(read("nests"), read("birds"));

        // A spreadsheet's cells, which carry no types and no NULL of their own.
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            "id\tbody\tnote\tbig\n10\tfrom a sheet\t\t42\n".into(),
        ));
        paste_and_import(cx);
        assert_eq!(
            read("nests").last().unwrap(),
            &[
                "integer 10",
                "null NULL",
                "integer 42",
                "null NULL",
                "text 'from a sheet'",
            ]
        );

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_import_question_picks_its_project_from_the_keyboard(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.pending_import = Some(super::PendingImport {
                source: crate::import::Source::TablePlus,
                report: crate::import::Report::default(),
                fresh: 1,
                project: Some("Imported from TablePlus".into()),
            });
            cx.notify();
        });
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("import-choice"));
        assert_eq!(ringed_button(&workspace, cx), 0);

        cx.simulate_keystrokes("left left");
        settle(cx);
        assert_eq!(
            ringed_button(&workspace, cx),
            2,
            "the ring reaches the project"
        );
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert!(palette_mode(&workspace, cx) == Some(PaletteMode::ImportProject));

        cx.simulate_input("No project");
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert!(palette_mode(&workspace, cx).is_none());
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(workspace.pending_import.as_ref().unwrap().project, None);
        });
        assert_eq!(front_dialog(&workspace, cx), Some("import-choice"));
        assert!(dialog_has_keyboard(&workspace, cx));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_ring_reaches_dont_ask_again_and_enter_or_space_ticks_it(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        workspace.update(cx, |workspace, cx| {
            workspace.set_mode(crate::sql::Mode::Full, cx)
        });
        cx.simulate_input("delete from birds");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("pending-run"));
        assert_eq!(ringed_button(&workspace, cx), 0, "the ring starts on Run");
        let dont_ask = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                let session = &workspace.profile().unwrap().session;
                session.pending_run.as_ref().map(|pending| pending.dont_ask)
            })
        };

        for (keystroke, ringed, ticked) in [
            // A space typed on as the prompt goes up presses nothing.
            ("space", 0, false),
            ("left left", 2, false),
            ("enter", 2, true),
            ("space", 2, false),
            ("enter", 2, true),
            ("tab", 1, true),
            ("tab", 0, true),
            ("tab", 2, true),
        ] {
            cx.simulate_keystrokes(keystroke);
            settle(cx);
            assert_eq!(ringed_button(&workspace, cx), ringed, "after {keystroke}");
            assert_eq!(dont_ask(cx), Some(ticked), "after {keystroke}");
            assert_eq!(front_dialog(&workspace, cx), Some("pending-run"));
        }

        cx.simulate_keystrokes("right right enter");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), None);
        workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().unwrap();
            assert!(
                profile
                    .confirmed
                    .contains(&crate::sql::Destructive::UnfilteredDelete),
                "Run ran with the box ticked"
            );
        });

        quit(workspace, cx);
    });
}

/// The opt-in coming into force under a prompt already up -- switched on, or
/// a Read-only hold landing -- must not take the prompt down with its
/// statement neither run nor cancelled.
#[gpui::test]
fn a_run_once_prompt_already_up_stays_up_when_the_opt_in_applies(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("selec 1");
        raise_run_once_prompt(&workspace, cx);

        workspace.update(cx, |workspace, cx| workspace.set_run_unreadable(true, cx));
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("pending-run"));

        press(cx, "enter");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), None);
        query_tab(&workspace, cx, |tab, _, _| {
            assert!(matches!(tab.query, QueryState::Failed(_)), "Run once ran");
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn ticking_run_once_s_box_runs_the_next_unreadable_statement_unasked(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table t (a integer);");
        cx.executor().allow_parking();
        cx.simulate_input("selec 1");
        raise_run_once_prompt(&workspace, cx);

        cx.simulate_keystrokes("left left");
        settle(cx);
        assert_eq!(ringed_button(&workspace, cx), 2, "the ring reaches the box");
        cx.simulate_keystrokes("enter");
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            let session = &workspace.profile().unwrap().session;
            assert!(session.pending_run.as_ref().unwrap().dont_ask);
            assert!(!workspace.settings.run_unreadable, "ticking alone set it");
        });

        cx.simulate_keystrokes("right right enter");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), None);
        query_tab(&workspace, cx, |tab, _, _| {
            assert!(matches!(tab.query, QueryState::Failed(_)), "Run once ran");
        });
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.settings.run_unreadable)
        });
        let saved =
            std::fs::read_to_string(store::dbdelve_directory().unwrap().join("profiles.toml"))
                .unwrap();
        assert!(saved.contains("run_unreadable = true"), "{saved}");

        // Unreadable to sqlparser, and a read SQLite answers with rows.
        cx.simulate_keystrokes("secondary-a");
        cx.simulate_input("PRAGMA table_info(t)");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), None, "it asked again");
        query_tab(&workspace, cx, |tab, _, _| match &tab.query {
            QueryState::Complete { rows, .. } => assert_eq!(*rows, 1),
            QueryState::Failed(error) => panic!("the pragma failed: {error}"),
            _ => panic!("the pragma never ran"),
        });

        // What an edit's refresh reruns, unasked: that still asks, and offers
        // no box, because nobody pressed Run.
        workspace.update(cx, |workspace, cx| {
            let tab = workspace.profile().unwrap().session.active;
            workspace.execute_sql("PRAGMA table_info(t)".into(), tab, cx);
        });
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("pending-run"));
        workspace.read_with(cx, |workspace, _| assert!(!workspace.dialog_checkbox()));

        quit(workspace, cx);
    });
}

/// Birds and the sightings that reference them, with the keyboard on the
/// first bird's key.
fn launch_on_a_referenced_key(
    cx: &mut TestAppContext,
) -> (Entity<Workspace>, &mut VisualTestContext) {
    let (workspace, cx) = launch_on_sqlite_holding(
        cx,
        "create table birds (id integer primary key, name text);
         create table sightings (id integer primary key, bird integer references birds(id));
         insert into birds values (1, 'wren');
         insert into sightings values (1, 1);",
    );
    cx.simulate_keystrokes("secondary-k e");
    settle(cx);
    cx.simulate_keystrokes("down down enter");
    settle(cx);
    assert_eq!(front_object(&workspace, cx).as_deref(), Some("birds"));
    cx.simulate_keystrokes("down");
    settle(cx);
    (workspace, cx)
}

fn front_object(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<String> {
    workspace.read_with(cx, |workspace, _| {
        let session = &workspace.profile().unwrap().session;
        session.active_object().map(|tab| tab.name.clone())
    })
}

fn ask_for_references(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("secondary-shift-p");
    settle(cx);
    cx.simulate_input("Show references");
    cx.simulate_keystrokes("enter");
    settle(cx);
}

#[gpui::test]
fn show_references_lists_the_referencing_tables_in_the_palette(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_a_referenced_key(cx);
        ask_for_references(cx);
        assert!(palette_mode(&workspace, cx) == Some(PaletteMode::References));

        cx.simulate_keystrokes("enter");
        settle(cx);
        assert!(palette_mode(&workspace, cx).is_none());
        assert_eq!(front_object(&workspace, cx).as_deref(), Some("sightings"));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn references_listed_for_one_key_never_open_from_another(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, name text);
             create table flocks (id integer primary key);
             create table members (id integer primary key, flock integer references flocks(id));
             create table sightings (id integer primary key, bird integer references birds(id));
             insert into birds values (1, 'wren');
             insert into flocks values (1);
             insert into members values (1, 1);
             insert into sightings values (1, 1);",
        );
        let active = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                workspace.profile().unwrap().session.active
            })
        };
        // Flocks' key under the keyboard, then birds'.
        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        cx.simulate_keystrokes("down down down enter");
        settle(cx);
        assert_eq!(front_object(&workspace, cx).as_deref(), Some("flocks"));
        cx.simulate_keystrokes("down");
        settle(cx);
        let flocks = active(cx);
        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        cx.simulate_keystrokes("up enter");
        settle(cx);
        assert_eq!(front_object(&workspace, cx).as_deref(), Some("birds"));
        cx.simulate_keystrokes("down");
        settle(cx);
        let birds = active(cx);

        // Picked once flocks is in front again: its own reference sits at the
        // same index, and opening it would read members with flocks' key.
        ask_for_references(cx);
        assert!(palette_mode(&workspace, cx) == Some(PaletteMode::References));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_tab(flocks, window, cx);
        });
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(active(cx), flocks);
        assert_eq!(front_object(&workspace, cx).as_deref(), Some("flocks"));

        // Answered once flocks is in front again.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.activate_tab(birds, window, cx);
        });
        settle(cx);
        let checks_before = workspace.read_with(cx, |workspace, _| workspace.reference_checks);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.show_references(&crate::actions::ShowReferences, window, cx);
            workspace.activate_tab(flocks, window, cx);
        });
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(
                workspace.reference_checks,
                checks_before + 1,
                "no lookup started, so nothing stale was tested"
            );
        });
        assert!(
            palette_mode(&workspace, cx).is_none(),
            "a stale answer was listed"
        );
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.reference_popup.is_none())
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn escape_closes_the_reference_arrows_list(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        workspace.update(cx, |workspace, cx| {
            workspace.reference_popup = Some(super::ReferencePopup {
                at: Some(gpui::point(gpui::px(10.), gpui::px(10.))),
                source: super::ReferenceSource {
                    profile: String::new(),
                    tab: 0,
                    column: String::new(),
                    value: String::new(),
                    referenced_by: Vec::new(),
                },
                choices: Some(Vec::new()),
                check: 0,
            });
            cx.notify();
        });
        settle(cx);
        cx.simulate_keystrokes("escape");
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.reference_popup.is_none())
        });
        assert!(editor_has_keyboard(&workspace, cx));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn tab_reaches_the_default_sorting_choice_in_settings(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_keystrokes("secondary-,");
        settle(cx);
        cx.simulate_input("Default sorting");
        settle(cx);
        let client_sort =
            |cx: &mut VisualTestContext| workspace.read_with(cx, |w, _| w.settings.client_sort);
        assert!(!client_sort(cx));

        // Server, then Client.
        cx.simulate_keystrokes("tab tab");
        settle(cx);
        press(cx, "space");
        assert!(client_sort(cx));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_interval_row_off_choice_stops_the_checks(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        // The fixture launches with the checks off, which keeps every test off
        // the network. Seed the state the row would show for someone checking
        // daily; the fields are set directly rather than through
        // `set_update_interval`, which would start a check of its own.
        workspace.update(cx, |workspace, _| {
            workspace.settings.check_for_updates = true;
            workspace.settings.update_interval_hours = 24;
        });
        cx.simulate_keystrokes("secondary-,");
        settle(cx);
        cx.simulate_input("Check interval");
        settle(cx);
        // Off, 6 hours, Daily, Weekly: the row opens on the daily one, so one
        // Tab reaches Off.
        cx.simulate_keystrokes("tab");
        settle(cx);
        press(cx, "space");
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            assert!(!workspace.settings.check_for_updates);
            // Off keeps the cadence, so turning it back on is one click.
            assert_eq!(workspace.settings.update_interval_hours, 24);
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_format_setting_changes_the_next_format_query(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("SELECT a FROM t");
        settle(cx);
        cx.simulate_keystrokes("secondary-,");
        settle(cx);
        cx.simulate_input("Keyword case");
        settle(cx);
        // Preserve, Upper, Lower; the row starts on Upper.
        cx.simulate_keystrokes("tab tab tab");
        settle(cx);
        press(cx, "space");
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(
                workspace.settings.format.keyword_case,
                crate::sql::CaseStyle::Lower
            );
        });

        cx.simulate_keystrokes("secondary-shift-f");
        settle(cx);
        let text = buffer(&workspace, cx);
        assert!(text.starts_with("select"), "{text}");
        assert!(!text.contains("SELECT"), "{text}");
        quit(workspace, cx);
    });
}

#[cfg(target_os = "macos")]
#[gpui::test]
fn an_existing_cmd_m_binding_can_be_recaptured_in_settings(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("profiles.toml"),
            "[settings]\ncheck_for_updates = false\n\
             [settings.custom_keybindings]\nshow_messages = \"secondary-m\"\n",
        )
        .unwrap();
        let (workspace, cx) = launch(cx);
        cx.simulate_keystrokes("secondary-,");
        settle(cx);
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.start_rebind("show_messages", window, cx);
            });
        });
        settle(cx);
        cx.simulate_keystrokes("cmd-m");
        settle(cx);
        let saved = store::peek_profiles(&[])
            .unwrap()
            .settings
            .unwrap()
            .custom_keybindings
            .unwrap();
        assert_eq!(
            saved.get("show_messages").map(String::as_str),
            Some("cmd-m")
        );
        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_selection_failing_part_way_says_what_it_already_committed(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "CREATE TABLE t (a INTEGER);");
        cx.executor().allow_parking();
        cx.simulate_input(
            "insert into t values (1); insert into t values (2); \
             select * from nope; insert into t values (3)",
        );
        cx.simulate_keystrokes("secondary-a secondary-enter");
        settle(cx);
        let failed = workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().expect("a profile is active");
            assert!(profile.session.queue_failure.is_some());
            let tab = profile.session.active_query_tab().expect("a query tab");
            let last = tab.queue.as_ref().and_then(|queue| queue.done.last());
            match &last.expect("a queue with results").state {
                QueryState::Failed(error) => error.message.clone(),
                _ => panic!("the third statement did not fail"),
            }
        });
        assert!(
            failed.ends_with("\n\n2 statements before it were already committed."),
            "{failed}"
        );

        quit(workspace, cx);
    });
}

#[gpui::test]
fn stepping_to_the_next_result_keeps_the_keyboard_in_the_grid(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.executor().allow_parking();
        cx.simulate_input("select 1; select 2");
        cx.simulate_keystrokes("secondary-a secondary-enter");
        settle(cx);
        cx.simulate_keystrokes("secondary-k r");
        settle(cx);
        let shown = |cx: &mut VisualTestContext| {
            query_tab(&workspace, cx, |tab, window, cx| {
                (
                    tab.switcher_selected(),
                    tab.shown().1.focus_handle(cx).is_focused(window),
                )
            })
        };
        assert_eq!(shown(cx), (Some(0), true));

        cx.dispatch_action(crate::actions::NextResult);
        settle(cx);
        assert_eq!(shown(cx), (Some(1), true));
        assert!(!editor_has_keyboard(&workspace, cx));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_palette_switches_who_sorts_the_view(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("select 1 as a");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        let client = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                crate::views::view_sorts_on_client(workspace.profile().unwrap(), cx)
            })
        };
        assert_eq!(client(cx), Some(false));

        for (row, after) in [("Sort on the client", true), ("Sort on the server", false)] {
            cx.simulate_keystrokes("secondary-shift-p");
            settle(cx);
            cx.simulate_input(row);
            cx.simulate_keystrokes("enter");
            settle(cx);
            assert_eq!(client(cx), Some(after), "after {row}");
        }

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_palette_copies_what_the_explorer_menu_copies(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        cx.simulate_keystrokes("down down enter");
        settle(cx);

        for (row, copied) in [
            ("Copy SELECT (Top 100)", "SELECT"),
            ("Copy DROP", "DROP TABLE"),
        ] {
            cx.simulate_keystrokes("secondary-shift-p");
            settle(cx);
            cx.simulate_input(row);
            cx.simulate_keystrokes("enter");
            settle(cx);
            let text = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default();
            assert!(
                text.contains(copied) && text.contains("birds"),
                "{row}: {text}"
            );
        }
        // Copied, never run.
        let tables = rusqlite::Connection::open(store::dbdelve_directory().unwrap().join("e2e.db"))
            .and_then(|db| {
                db.query_row("select count(*) from sqlite_master", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .unwrap();
        assert_eq!(tables, 1);

        quit(workspace, cx);
    });
}

#[gpui::test]
fn an_empty_table_opened_from_the_sidebar_keeps_the_keyboard(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        cx.simulate_keystrokes("down down enter");
        settle(cx);
        let rows = workspace.read_with(cx, |workspace, cx| {
            let session = &workspace.profile().unwrap().session;
            session
                .active_results()
                .map(|grid| grid.read(cx).delegate().total_rows())
        });
        assert_eq!(rows, Some(0));
        cx.update(|window, _| assert!(!window.context_stack().is_empty(), "no dispatch path"));

        cx.simulate_keystrokes("secondary-shift-p");
        settle(cx);
        assert!(palette_mode(&workspace, cx).is_some());

        quit(workspace, cx);
    });
}

#[gpui::test]
fn tab_onto_a_welcome_project_shows_its_rename_and_delete(cx: &mut TestAppContext) {
    if ["PGHOST", "PGPORT", "PGDATABASE", "PGUSER"]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    {
        return;
    }
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("profiles.toml"),
            "[settings]\ncheck_for_updates = false\n[[projects]]\nname = \"Birds\"\n",
        )
        .unwrap();
        let (workspace, cx) = launch(cx);
        settle(cx);
        let buttons_have_keyboard = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let buttons = workspace.read(cx).welcome_project_focus.borrow()[0].clone();
                buttons.contains_focused(window, cx)
            })
        };
        assert!(!buttons_have_keyboard(cx));

        let mut reached = false;
        for _ in 0..30 {
            cx.simulate_keystrokes("tab");
            settle(cx);
            if buttons_have_keyboard(cx) {
                reached = true;
                break;
            }
        }
        assert!(reached, "Tab never reached the project's buttons");
        cx.update(|window, cx| {
            let buttons = workspace.read(cx).welcome_project_focus.borrow()[0].clone();
            assert!(
                !buttons.is_focused(window),
                "Rename or Delete has it, not their holder"
            );
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_clicked_box_leaves_the_ring_in_charge_of_the_keyboard(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        workspace.update(cx, |workspace, cx| {
            workspace.set_mode(crate::sql::Mode::Full, cx)
        });
        cx.simulate_input("delete from birds");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("pending-run"));

        let box_at = cx
            .debug_bounds("pending-run-2")
            .expect("the box is drawn in the ring")
            .center();
        cx.simulate_click(box_at, gpui::Modifiers::default());
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            let session = &workspace.profile().unwrap().session;
            assert!(session.pending_run.as_ref().unwrap().dont_ask);
        });

        // Round to Run, which `enter` presses rather than the box it would tick.
        cx.simulate_keystrokes("tab tab tab");
        settle(cx);
        assert_eq!(ringed_button(&workspace, cx), 0);
        assert!(dialog_has_keyboard(&workspace, cx));
        press(cx, "enter");
        assert_eq!(front_dialog(&workspace, cx), None, "enter pressed nothing");
        workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().unwrap();
            assert!(
                profile
                    .confirmed
                    .contains(&crate::sql::Destructive::UnfilteredDelete)
            );
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn format_query_reaches_the_definition_editor(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().expect("the test home has a data directory");
        std::fs::create_dir_all(&directory).expect("the data directory must be creatable");
        let database = directory.join("definition.duckdb");
        ::duckdb::Connection::open(&database)
            .and_then(|connection| {
                connection.execute_batch(
                    "CREATE TABLE items (id INTEGER PRIMARY KEY); \
                     INSERT INTO items (id) VALUES (1); \
                     create view item_view as select id from items;",
                )
            })
            .expect("the schema must apply");
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\n\
                 check_for_updates = false\n\
                 [[profiles]]\n\
                 id = \"e2e\"\n\
                 name = \"E2E\"\n\
                 host = \"\"\n\
                 database = \"\"\n\
                 user = \"\"\n\
                 engine = \"duckdb\"\n\
                 path = {:?}\n",
                database.display().to_string()
            ),
        )
        .expect("profiles.toml must be writable");
        let (workspace, cx) = launch(cx);
        cx.executor().allow_parking();
        workspace.update(cx, |workspace, cx| {
            let index = workspace
                .profiles
                .iter()
                .position(|profile| profile.config.engine() == Engine::DuckDb)
                .expect("the saved DuckDB profile is restored");
            workspace.activate(index, cx);
        });
        cx.run_until_parked();
        workspace.update(cx, |workspace, cx| {
            workspace.edit_definition(
                super::definition_editor::DefinitionTarget::Relation {
                    schema: "main".into(),
                    name: "item_view".into(),
                    kind: crate::db::RelationKind::View,
                },
                cx,
            );
        });
        settle(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let editor = workspace.read_with(cx, |workspace, _| {
            workspace
                .profile()
                .expect("the DuckDB profile is open")
                .session
                .definition_editor
                .as_ref()
                .expect("the definition editor is open")
                .editor
                .clone()
                .expect("the definition editor has a buffer")
        });
        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.focus_handle(cx).focus(window, cx);
            });
        });
        cx.simulate_keystrokes("secondary-shift-f");
        settle(cx);
        let formatted = cx.update(|_, cx| editor.read(cx).value().to_string());
        assert!(formatted.contains("CREATE"), "{formatted}");
        assert!(formatted.contains("SELECT"), "{formatted}");
        assert!(formatted.contains("FROM"), "{formatted}");

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_session_changed_behind_the_save_panel_is_not_asked_about_the_table(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().expect("the test home has a data directory");
        std::fs::create_dir_all(&directory).expect("the data directory must be creatable");
        let database = directory.join("e2e.duckdb");
        ::duckdb::Connection::open(&database)
            .and_then(|connection| {
                connection.execute_batch(
                    "CREATE TABLE t (id INTEGER PRIMARY KEY, \
                     twice INTEGER GENERATED ALWAYS AS (id * 2) VIRTUAL); \
                     INSERT INTO t (id) VALUES (1);",
                )
            })
            .expect("the schema must apply");
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\n\
                 check_for_updates = false\n\
                 [[profiles]]\n\
                 id = \"e2e\"\n\
                 name = \"E2E\"\n\
                 host = \"\"\n\
                 database = \"\"\n\
                 user = \"\"\n\
                 engine = \"duckdb\"\n\
                 path = {:?}\n",
                database.display().to_string()
            ),
        )
        .expect("profiles.toml must be writable");
        let (workspace, cx) = launch(cx);
        cx.executor().allow_parking();
        workspace.update(cx, |workspace, cx| {
            let index = workspace
                .profiles
                .iter()
                .position(|profile| profile.config.engine() == Engine::DuckDb)
                .expect("the saved DuckDB profile is restored");
            workspace.activate(index, cx);
        });
        cx.run_until_parked();
        cx.simulate_input("SELECT * FROM t");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);

        let mut export = |changed: bool, name: &str| {
            let path = directory.join(name);
            workspace.update(cx, |workspace, cx| {
                workspace.export_results(crate::export::FileAs::Inserts, cx);
            });
            settle(cx);
            if changed {
                workspace.update(cx, |workspace, _| {
                    workspace.profile_mut().unwrap().generation += 1;
                });
            }
            let chosen = path.clone();
            cx.simulate_new_path_selection(|_| Some(chosen));
            for _ in 0..500 {
                settle(cx);
                if path.exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::fs::read_to_string(&path).expect("the file is written")
        };

        assert!(!export(false, "same.sql").contains("twice"));
        assert!(export(true, "changed.sql").contains("twice"));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_export_question_takes_the_keyboard_and_hands_it_back_when_dismissed(
    cx: &mut TestAppContext,
) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        // Past the default row limit, so the tab holds the first rows only.
        cx.simulate_input(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 5000) \
             SELECT x FROM n",
        );
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        query_tab(&workspace, cx, |tab, _, _| {
            assert!(
                matches!(tab.query, QueryState::Complete { .. }),
                "the query never completed"
            );
        });

        for (dismiss, typed) in [("escape", " "), ("tab tab enter", "x")] {
            workspace.update(cx, |workspace, cx| {
                workspace
                    .export_results(crate::export::FileAs::Rows(crate::export::Format::Csv), cx);
            });
            settle(cx);
            workspace.read_with(cx, |workspace, _| {
                assert_eq!(workspace.dialog_in_front.get(), Some("pending-export"));
            });
            assert!(dialog_has_keyboard(&workspace, cx));
            assert_eq!(
                ringed_button(&workspace, cx),
                0,
                "the ring starts on Export All Rows"
            );
            for (keystroke, ringed) in [
                ("left", 1),
                ("left", 2),
                ("left", 0),
                ("right", 2),
                ("right", 1),
                ("right", 0),
            ] {
                cx.simulate_keystrokes(keystroke);
                settle(cx);
                assert_eq!(ringed_button(&workspace, cx), ringed, "after {keystroke}");
            }

            let before = buffer(&workspace, cx);
            cx.simulate_keystrokes(dismiss);
            settle(cx);
            workspace.read_with(cx, |workspace, _| {
                assert_eq!(
                    workspace.dialog_in_front.get(),
                    None,
                    "{dismiss} left it up"
                );
                let session = &workspace.profile().unwrap().session;
                assert!(session.exporting.is_none(), "{dismiss} started an export");
            });
            assert!(editor_has_keyboard(&workspace, cx), "after {dismiss}");
            cx.simulate_input(typed);
            settle(cx);
            assert_eq!(buffer(&workspace, cx), format!("{before}{typed}"));
        }

        quit(workspace, cx);
    });
}

fn relation_names(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(cx, |workspace, _| {
        let Some(CatalogState::Loaded(catalog, _)) =
            workspace.profile().map(|profile| &profile.catalog)
        else {
            panic!("the catalog should have loaded");
        };
        catalog
            .schemas
            .iter()
            .flat_map(|schema| &schema.relations)
            .map(|relation| relation.name.clone())
            .collect()
    })
}

#[gpui::test]
fn the_refresh_shortcut_finds_a_table_made_elsewhere_over_the_same_session(
    cx: &mut TestAppContext,
) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        settle(cx);
        assert!(relation_names(&workspace, cx).is_empty());
        let generation = workspace.read_with(cx, |workspace, _| {
            workspace.profile().map(|profile| profile.generation)
        });

        let database = store::dbdelve_directory()
            .expect("the test home has a data directory")
            .join("e2e.db");
        rusqlite::Connection::open(database)
            .expect("the database should open")
            .execute_batch("CREATE TABLE made_elsewhere (id INTEGER)")
            .expect("the table should be created");

        cx.simulate_keystrokes("secondary-shift-r");
        settle(cx);
        assert_eq!(relation_names(&workspace, cx), ["made_elsewhere"]);

        // Sent straight to the session, because its table has to stay
        // uncommitted: only a read beside the session cannot see it, and a
        // refresh that dropped the session would leave nothing to `COMMIT`.
        let session = workspace
            .read_with(cx, |workspace, _| {
                workspace.profile().and_then(|profile| profile.connection())
            })
            .expect("the profile is connected");
        let run = |sql: &str| {
            session
                .query(sql, &CancelToken::default(), Fetch::default())
                .expect("the statement should run");
        };
        run("BEGIN");
        run("CREATE TABLE not_committed (id INTEGER)");
        workspace.update(cx, |workspace, _| {
            if let Some(profile) = workspace.profile_mut() {
                profile.in_transaction = true;
            }
        });

        cx.simulate_keystrokes("secondary-shift-r");
        settle(cx);
        assert_eq!(relation_names(&workspace, cx), ["made_elsewhere"]);
        workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().expect("the profile is still open");
            assert_eq!(Some(profile.generation), generation, "it reconnected");
            assert!(!profile.refreshing_schema());
            assert_eq!(
                profile.session.notice.as_deref(),
                Some("Schema refreshed."),
                "the read beside the session failed"
            );
        });

        run("COMMIT");
        cx.simulate_keystrokes("secondary-shift-r");
        settle(cx);
        let mut names = relation_names(&workspace, cx);
        names.sort();
        assert_eq!(names, ["made_elsewhere", "not_committed"]);

        quit(workspace, cx);
    });
}

fn history_view(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> crate::palette::HistoryView {
    workspace.read_with(cx, |workspace, cx| {
        workspace
            .palette
            .as_ref()
            .expect("the palette is up")
            .read(cx)
            .history()
    })
}

#[gpui::test]
fn the_history_scope_filters_and_sorts_its_runs_from_the_keyboard(cx: &mut TestAppContext) {
    use crate::palette::{HistoryOrder, HistoryStatus, HistoryView};
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("select 1");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            let last = &workspace.profile().unwrap().session.history[0];
            assert_eq!((last.sql.as_str(), last.rows), ("select 1", Some(1)));
            assert!(last.at.is_some() && last.ms.is_some() && last.error.is_none());
        });

        // Outside History the chords do nothing.
        cx.simulate_keystrokes("secondary-p");
        settle(cx);
        cx.simulate_keystrokes("secondary-k f secondary-k o");
        settle(cx);
        assert_eq!(history_view(&workspace, cx), HistoryView::default());

        cx.simulate_keystrokes("tab tab tab");
        settle(cx);
        assert!(palette_mode(&workspace, cx) == Some(PaletteMode::Search(Scope::History)));
        cx.simulate_keystrokes("secondary-k f secondary-k o secondary-k o");
        settle(cx);
        assert_eq!(
            history_view(&workspace, cx),
            HistoryView {
                status: HistoryStatus::Succeeded,
                order: HistoryOrder::MostRows,
            }
        );

        // A palette opened again starts unfiltered.
        cx.simulate_keystrokes("escape");
        settle(cx);
        cx.simulate_keystrokes("secondary-p");
        settle(cx);
        assert_eq!(history_view(&workspace, cx), HistoryView::default());

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_definitions_scope_lands_empty_on_a_catalog_that_carries_no_view_text(
    cx: &mut TestAppContext,
) {
    use crate::session::DefinitionIndex;
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_keystrokes("secondary-p");
        settle(cx);
        // Objects, then Definitions.
        cx.simulate_keystrokes("tab");
        settle(cx);
        assert!(
            palette_mode(&workspace, cx) == Some(PaletteMode::Search(Scope::Definitions)),
            "tab did not reach the Definitions scope"
        );
        // SQLite's catalog carries no view text and has no routines to hold
        // any of their own, so the index is answered with nothing rather than
        // left loading: the scope is empty and the palette still takes keys.
        workspace.read_with(cx, |workspace, _| {
            let definitions = &workspace.profile().unwrap().session.definitions;
            assert!(
                matches!(definitions, DefinitionIndex::Loaded(read) if read.is_empty()),
                "the definitions were left waiting"
            );
        });
        cx.simulate_input("view");
        settle(cx);
        // Typing narrows the scope it is on rather than closing it, and the
        // empty state it draws is the one for a search with no results.
        assert!(
            palette_mode(&workspace, cx) == Some(PaletteMode::Search(Scope::Definitions)),
            "typing left the Definitions scope"
        );
        cx.simulate_keystrokes("escape");
        settle(cx);
        assert!(palette_mode(&workspace, cx).is_none());

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_count_whose_tab_closed_under_it_still_leaves_its_outcome_in_the_history(
    cx: &mut TestAppContext,
) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        cx.executor().allow_parking();
        cx.simulate_keystrokes("secondary-k e");
        settle(cx);
        cx.simulate_keystrokes("down down enter");
        settle(cx);
        let object = workspace.read_with(cx, |workspace, _| {
            match workspace.profile().unwrap().session.active {
                crate::session::Tab::Object(id) => id,
                crate::session::Tab::Query(_) => panic!("the table did not open"),
            }
        });

        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.count_rows(object, cx);
                workspace.close_object(object, window, cx);
            })
        });
        settle(cx);

        workspace.read_with(cx, |workspace, _| {
            let last = &workspace.profile().unwrap().session.history[0];
            assert!(last.sql.to_lowercase().contains("count"), "{}", last.sql);
            assert!(last.ms.is_some(), "the count's outcome was dropped");
        });
        let profile =
            workspace.read_with(cx, |workspace, _| workspace.profile().unwrap().id.clone());
        assert!(store::history(&profile)[0].ms.is_some());

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_new_folder_takes_its_name_from_the_keyboard_and_hands_it_back(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                let leaf =
                    workspace.profile().unwrap().session.explorer_leaves["relation-main\0birds"];
                let member = workspace.member_at(leaf.target);
                assert!(member.is_some(), "the table is nameable");
                workspace.new_folder(member, window, cx);
            });
        });
        settle(cx);
        let naming = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let session = &workspace.read(cx).profile().unwrap().session;
                session
                    .query_rename
                    .as_ref()
                    .map(|rename| rename.input.focus_handle(cx).is_focused(window))
            })
        };
        assert_eq!(
            naming(cx),
            Some(true),
            "the folder's field has the keyboard"
        );

        cx.simulate_input("Birds");
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(naming(cx), None);
        assert!(editor_has_keyboard(&workspace, cx));
        cx.update(|_, cx| {
            let profile = workspace.read(cx).profile().unwrap();
            let folders = &profile.shortcuts.folders;
            assert_eq!(folders.len(), 1);
            assert_eq!(folders[0].name, "Birds");
            assert_eq!(folders[0].members.len(), 1);
            let tree = profile.session.explorer_tree.read(cx);
            let row = tree.entry(0).expect("the folder leads the sidebar");
            assert_eq!(row.item().label.as_ref(), "Birds");
            assert_eq!(row.item().children[0].label.as_ref(), "birds");
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_folder_keeps_the_sidebar_drawn_while_the_catalog_is_away(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        cx.update(|_, cx| {
            workspace.update(cx, |workspace, _| {
                let profile = workspace.profile_mut().unwrap();
                profile.catalog = CatalogState::Failed("gone".into());
                assert!(profile.session.saved_queries.is_empty());
                assert!(!super::forms::explorer_drawn(profile));

                profile.shortcuts.add_folder("Hot").unwrap();
                assert!(
                    super::forms::explorer_drawn(profile),
                    "its Remove and Unpin are reachable"
                );
            });
        });

        quit(workspace, cx);
    });
}

/// Where the sidebar draws `row`, by its id.
fn explorer_row(
    workspace: &Entity<Workspace>,
    row: &str,
    cx: &mut VisualTestContext,
) -> gpui::Bounds<gpui::Pixels> {
    let index = cx
        .update(|_, cx| {
            let tree = workspace
                .read(cx)
                .profile()
                .unwrap()
                .session
                .explorer_tree
                .clone();
            tree.read(cx).index_of(&row.to_string().into())
        })
        .expect("the row is listed");
    // `debug_bounds` takes a `'static` selector; a test's few rows can leak.
    let selector: &'static str = format!("explorer-row-{index}").leak();
    cx.debug_bounds(selector).expect("the row is drawn")
}

/// The id of the row listing `member` in `folder`, or among the pins.
fn shortcut_row(
    workspace: &Entity<Workspace>,
    member: &explorer::Member,
    folder: Option<&str>,
    cx: &mut VisualTestContext,
) -> String {
    cx.update(|_, cx| {
        let session = &workspace.read(cx).profile().unwrap().session;
        let listed = explorer::Shortcut::Member {
            member: member.clone(),
            folder: folder.map(str::to_string),
            missing: false,
        };
        session
            .explorer_shortcuts
            .iter()
            .find(|(_, shortcut)| **shortcut == listed)
            .map(|(row, _)| row.clone())
            .expect("the shortcut is listed")
    })
}

/// A left-button drag from `from` to `to`, frame by frame, as the pointer
/// would make it.
fn drag(
    cx: &mut VisualTestContext,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    let none = gpui::Modifiers::default();
    cx.simulate_mouse_down(from, gpui::MouseButton::Left, none);
    for at in [from + gpui::point(gpui::px(0.), gpui::px(8.)), to] {
        cx.simulate_mouse_move(at, gpui::MouseButton::Left, none);
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    cx.simulate_mouse_up(to, gpui::MouseButton::Left, none);
    settle(cx);
}

#[gpui::test]
fn a_shortcut_dragged_between_folders_moves_and_one_from_the_schemas_is_added(
    cx: &mut TestAppContext,
) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        let table = "relation-main\0birds";
        let birds = cx.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                let leaf = workspace.profile().unwrap().session.explorer_leaves[table];
                let birds = workspace.member_at(leaf.target).unwrap();
                let shortcuts = &mut workspace.profile_mut().unwrap().shortcuts;
                shortcuts.add_folder("Hot").unwrap();
                shortcuts.add_folder("Cold").unwrap();
                workspace.toggle_in_folder("Hot".into(), birds.clone(), cx);
                birds
            })
        });
        settle(cx);
        let held = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                let shortcuts = &workspace.read(cx).profile().unwrap().shortcuts;
                ["Hot", "Cold"].map(|name| shortcuts.holds(name, &birds))
            })
        };
        let in_hot = shortcut_row(&workspace, &birds, Some("Hot"), cx);

        let from = explorer_row(&workspace, &in_hot, cx).center();
        let to = explorer_row(&workspace, &explorer::folder_row("Cold"), cx).center();
        drag(cx, from, to);
        assert_eq!(
            held(cx),
            [false, true],
            "out of one folder into another moves"
        );

        let from = explorer_row(&workspace, table, cx).center();
        let to = explorer_row(&workspace, &explorer::folder_row("Hot"), cx).center();
        drag(cx, from, to);
        assert_eq!(held(cx), [true, true], "from the schemas it is added");
        assert!(
            sidebar_has_keyboard(&workspace, cx),
            "the sidebar keeps the keyboard"
        );
        assert!(
            cx.update(|_, cx| workspace
                .read(cx)
                .profile()
                .unwrap()
                .session
                .objects
                .is_empty()),
            "a drag opens nothing"
        );

        // Dropped where nothing takes it, a drag changes nothing.
        let from = explorer_row(&workspace, table, cx).center();
        drag(cx, from, from + gpui::point(gpui::px(0.), gpui::px(2_000.)));
        assert_eq!(held(cx), [true, true]);

        let at = explorer_row(&workspace, table, cx).center();
        cx.simulate_click(at, gpui::Modifiers::default());
        settle(cx);
        assert_eq!(
            cx.update(|_, cx| workspace.read(cx).profile().unwrap().session.objects.len()),
            1,
            "a click still opens the row"
        );

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_pin_dropped_on_the_top_half_of_another_goes_before_it(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (name text); create table fish (name text);",
        );
        let [birds, fish] = cx.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                ["birds", "fish"].map(|name| {
                    let leaf = workspace.profile().unwrap().session.explorer_leaves
                        [&explorer::relation_row("main", name)];
                    let member = workspace.member_at(leaf.target).unwrap();
                    workspace.toggle_pin(explorer::Pin::Member(member.clone()), cx);
                    member
                })
            })
        });
        settle(cx);

        let from = explorer_row(&workspace, &shortcut_row(&workspace, &fish, None, cx), cx);
        let onto = explorer_row(&workspace, &shortcut_row(&workspace, &birds, None, cx), cx);
        drag(
            cx,
            from.center(),
            onto.origin + gpui::point(gpui::px(20.), gpui::px(2.)),
        );
        let pins = cx.update(|_, cx| workspace.read(cx).profile().unwrap().shortcuts.pins.clone());
        assert_eq!(
            pins,
            [explorer::Pin::Member(fish), explorer::Pin::Member(birds)]
        );

        quit(workspace, cx);
    });
}

/// Through the real restore and the real save: what `Profile::stored` writes
/// for a plugin profile is the user's string, never its password, and it
/// restores to the same connection.
#[gpui::test]
fn an_odbc_profile_saves_and_restores_as_it_was(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let odbc = store::tests::odbc_plugin();
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        // A SQLite profile in front, since launching connects the active one
        // and a plugin profile would reach for the keychain.
        let database = directory.join("e2e.db");
        std::fs::write(&database, b"").unwrap();
        let strings = [
            "Db2Dev",
            "DRIVER={PostgreSQL Unicode}; Server = pg.internal ;Port=5432;Opts={a;b}}c}",
        ];
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\ncheck_for_updates = false\n\
                 [[profiles]]\nid = \"e2e\"\nname = \"E2E\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"sqlite\"\npath = {:?}\n\
                 [[profiles]]\nid = \"dsn\"\nname = \"DSN\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"a@b.example\"\nengine = \"plugin:odbc\"\nspeaks = \"generic\"\n\
                 connection = {:?}\nstatement_timeout = 30\n\
                 [[profiles]]\nid = \"tunnel\"\nname = \"Tunnel\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"u\"\nengine = \"plugin:odbc\"\nspeaks = \"postgres\"\n\
                 connection = {:?}\n\
                 [profiles.ssh]\nhost = \"bastion.example\"\nport = 2222\nuser = \"me\"\n",
                database.display().to_string(),
                strings[0],
                strings[1],
            ),
        )
        .unwrap();

        let (workspace, cx) = launch_with(cx, vec![odbc]);
        let external = |workspace: &Workspace| {
            workspace
                .profiles
                .iter()
                .filter(|profile| matches!(profile.config, ConnectionConfig::External(_)))
                .map(|profile| profile.config.clone())
                .collect::<Vec<_>>()
        };
        let restored = cx.update(|_, cx| external(workspace.read(cx)));
        let expected = |connection: &str, speaks, user: &str, timeout, ssh| {
            ConnectionConfig::External(ExternalConfig {
                plugin: odbc,
                speaks,
                connection: connection.into(),
                user: user.into(),
                password: String::new(),
                statement_timeout: timeout,
                ssh,
            })
        };
        assert_eq!(
            restored,
            [
                expected(strings[0], Engine::Generic, "a@b.example", 30, None),
                expected(
                    strings[1],
                    Engine::Postgres,
                    "u",
                    0,
                    Some(SshTunnel {
                        host: "bastion.example".into(),
                        port: Some(2222),
                        user: "me".into(),
                        identity_file: None,
                        run_as: None,
                        passphrase: String::new(),
                    })
                ),
            ]
        );

        workspace.update(cx, |workspace, cx| {
            for profile in &mut workspace.profiles {
                if let ConnectionConfig::External(config) = &mut profile.config {
                    config.password = "hunter2".into();
                }
            }
            workspace.remember_profiles(cx);
        });
        let saved = std::fs::read_to_string(directory.join("profiles.toml")).unwrap();
        assert!(!saved.contains("hunter2"), "{saved}");
        let list = store::decode_profiles(&saved, &[odbc]).unwrap();
        let written = list
            .profiles
            .iter()
            .filter_map(|stored| {
                Some((
                    stored.engine.as_deref()?,
                    stored.speaks.as_deref()?,
                    stored.connection.as_deref()?,
                ))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            written,
            [
                ("plugin:odbc", "generic", strings[0]),
                ("plugin:odbc", "postgres", strings[1]),
            ]
        );

        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.profiles.clear();
                for stored in list.profiles {
                    workspace.restore_profile(stored, window, cx);
                }
            });
        });
        assert_eq!(cx.update(|_, cx| external(workspace.read(cx))), restored);

        quit(workspace, cx);
    });
}

/// Through the real restore, the edit form and the real save: a password
/// command comes back as written, disables the Password field while it is
/// there, and is dropped from an engine whose login never runs one.
#[gpui::test]
fn a_password_command_restores_into_the_edit_form_and_saves_back(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        // A SQLite profile in front, since launching connects the active one
        // and a server profile would reach for its login.
        let database = directory.join("e2e.db");
        std::fs::write(&database, b"").unwrap();
        let command = "aws rds generate-db-auth-token --hostname %HOSTNAME% --username %USERNAME%";
        let server = |id: &str, engine: &str| {
            format!(
                "[[profiles]]\nid = \"{id}\"\nname = \"{id}\"\nhost = \"db.internal\"\n\
                 database = \"d\"\nuser = \"u\"\nengine = \"{engine}\"\n\
                 password_command = {command:?}\npassword_expires = 900\n"
            )
        };
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\ncheck_for_updates = false\n\
                 [[profiles]]\nid = \"e2e\"\nname = \"E2E\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"sqlite\"\npath = {:?}\n{}{}",
                database.display().to_string(),
                server("rds", "postgres"),
                server("mongo", "mongodb"),
            ),
        )
        .unwrap();

        let (workspace, cx) = launch(cx);
        let rds = cx.update(|_, cx| {
            workspace
                .read(cx)
                .profiles
                .iter()
                .position(|profile| profile.id == "rds")
                .unwrap()
        });
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| workspace.edit_profile(rds, window, cx));
        });
        settle(cx);
        cx.update(|_, cx| {
            let form = workspace.read(cx).form.as_ref().expect("the form is open");
            assert_eq!(form.password_command.read(cx).value(), command);
            assert_eq!(form.password_expires.read(cx).value(), "900");
            let password = form.password.read(cx).presentation();
            assert!(password.is_disabled());
            assert_eq!(password.placeholder(), "From the command");
            let (_, config) = form.config(cx).unwrap();
            assert_eq!(
                config.password_command(),
                Some(&crate::db::PasswordCommand {
                    command: command.into(),
                    expires: 900,
                })
            );
            assert_eq!(config.password(), Some(""));
        });

        // Cleared, the Password field is the profile's own again.
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                let form = workspace.form.as_ref().unwrap();
                form.password_command
                    .update(cx, |input, cx| input.set_value("", window, cx));
                cx.notify();
            });
        });
        settle(cx);
        cx.update(|_, cx| {
            let form = workspace.read(cx).form.as_ref().unwrap();
            let password = form.password.read(cx).presentation();
            assert!(!password.is_disabled());
            assert_eq!(password.placeholder(), "Unchanged");
            assert_eq!(form.config(cx).unwrap().1.password_command(), None);
        });

        cx.update(|_, cx| workspace.update(cx, |workspace, cx| workspace.remember_profiles(cx)));
        let saved = std::fs::read_to_string(directory.join("profiles.toml")).unwrap();
        let list = store::decode_profiles(&saved, &[]).unwrap();
        let written = |id: &str| {
            let stored = list.profiles.iter().find(|stored| stored.id == id).unwrap();
            (stored.password_command.as_deref(), stored.password_expires)
        };
        assert_eq!(written("rds"), (Some(command), Some(900)));
        assert_eq!(written("mongo"), (None, None));

        quit(workspace, cx);
    });
}

/// A duplicate has no Keychain entry of its own, so its Password field carries
/// the original's, for an ODBC profile as for a server.
#[gpui::test]
fn duplicating_an_odbc_profile_keeps_its_password(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let database = directory.join("e2e.db");
        std::fs::write(&database, b"").unwrap();
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\ncheck_for_updates = false\n\
                 [[profiles]]\nid = \"e2e\"\nname = \"E2E\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"sqlite\"\npath = {:?}\n\
                 [[profiles]]\nid = \"dsn\"\nname = \"DSN\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"a@b.example\"\nengine = \"plugin:odbc\"\nspeaks = \"generic\"\n\
                 connection = \"Db2Dev\"\n",
                database.display().to_string(),
            ),
        )
        .unwrap();
        let (workspace, cx) = launch_with(cx, vec![store::tests::odbc_plugin()]);
        let password = cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                let profile = workspace
                    .profiles
                    .iter()
                    .find(|profile| profile.id == "dsn")
                    .unwrap();
                let form = ConnectionForm::duplicating(
                    profile,
                    "DSN copy".into(),
                    [Some("hunter2".into()), None],
                    window,
                    cx,
                );
                form.password.read(cx).unmask_value().to_string()
            })
        });
        assert_eq!(password, "hunter2");
        quit(workspace, cx);
    });
}

/// What Save and Test read the form as refuses Run as beside a host that is
/// no socket directory, as a connect would, and leaves a blank one beside a
/// service to the connect, since the service may name a socket directory.
#[gpui::test]
fn the_forms_save_and_test_refuse_run_as_without_a_socket_directory(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let database = directory.join("e2e.db");
        std::fs::write(&database, b"").unwrap();
        let postgres = |id: &str, host: &str, service: &str| {
            format!(
                "[[profiles]]\nid = \"{id}\"\nname = \"{id}\"\nhost = \"{host}\"\n\
                 database = \"d\"\nuser = \"u\"\nengine = \"postgres\"\n{service}\
                 [profiles.ssh]\nhost = \"bastion\"\nrun_as = \"postgres\"\n"
            )
        };
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\ncheck_for_updates = false\n\
                 [[profiles]]\nid = \"e2e\"\nname = \"E2E\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"sqlite\"\npath = {:?}\n{}{}{}",
                database.display().to_string(),
                postgres("tcp", "db.internal", ""),
                postgres("socket", "/var/run/postgresql", ""),
                postgres("service", "", "service = \"socketed\"\n"),
            ),
        )
        .unwrap();
        let (workspace, cx) = launch(cx);
        let read_as = |cx: &mut VisualTestContext, id: &str| {
            cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    let profile = workspace
                        .profiles
                        .iter()
                        .find(|profile| profile.id == id)
                        .unwrap();
                    let form = ConnectionForm::duplicating(
                        profile,
                        "Copy".into(),
                        [None, None],
                        window,
                        cx,
                    );
                    form.config(cx).map(|_| ())
                })
            })
        };
        let refused = read_as(cx, "tcp").unwrap_err();
        assert!(refused.starts_with("Run as needs Host"), "{refused}");
        assert_eq!(read_as(cx, "socket"), Ok(()));
        assert_eq!(read_as(cx, "service"), Ok(()));
        quit(workspace, cx);
    });
}

/// Turning the tunnel off with the keyboard in Key passphrase takes that row
/// away, so the keyboard goes to Password rather than nowhere, and typing
/// lands there.
#[gpui::test]
fn turning_the_tunnel_off_from_key_passphrase_hands_the_keyboard_to_password(
    cx: &mut TestAppContext,
) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch(cx);
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.new_connection_in(None, window, cx);
            });
        });
        settle(cx);
        let toggle_tunnel = |cx: &mut VisualTestContext| {
            let switch = cx
                .debug_bounds("ssh-tunnel")
                .expect("the switch is drawn")
                .center();
            cx.simulate_click(switch, gpui::Modifiers::default());
            settle(cx);
        };
        let field = |cx: &mut VisualTestContext,
                     field: fn(&ConnectionForm) -> &Entity<InputState>| {
            cx.update(|_, cx| field(workspace.read(cx).form.as_ref().unwrap()).clone())
        };

        toggle_tunnel(cx);
        let passphrase = field(cx, |form| &form.ssh_passphrase);
        cx.update(|window, cx| passphrase.focus_handle(cx).focus(window, cx));
        settle(cx);
        cx.simulate_input("correct horse");
        assert_eq!(
            cx.update(|_, cx| passphrase.read(cx).unmask_value().to_string()),
            "correct horse"
        );

        toggle_tunnel(cx);
        let password = field(cx, |form| &form.password);
        assert!(!workspace.read_with(cx, |workspace, _| workspace.form.as_ref().unwrap().ssh));
        assert!(cx.update(|window, cx| password.focus_handle(cx).is_focused(window)));
        cx.simulate_input("hunter2");
        assert_eq!(
            cx.update(|_, cx| password.read(cx).unmask_value().to_string()),
            "hunter2"
        );

        quit(workspace, cx);
    });
}

/// The ODBC tile is picked from the engine picker by keyboard, and the
/// keyboard goes on working through both field-set changes: the Connection
/// field takes it, and Host takes it back once Postgres unmounts that field.
#[gpui::test]
fn picking_the_odbc_tile_puts_the_keyboard_in_its_connection_string(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_with(cx, vec![store::tests::odbc_plugin()]);
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.new_connection_in(None, window, cx);
            });
        });
        settle(cx);
        let has_keyboard =
            |cx: &mut VisualTestContext, field: fn(&ConnectionForm) -> &Entity<InputState>| {
                cx.update(|window, cx| {
                    let workspace = workspace.read(cx);
                    let form = workspace.form.as_ref().expect("the form is open");
                    field(form).focus_handle(cx).is_focused(window)
                })
            };
        let pick = |cx: &mut VisualTestContext, typed: &str| {
            cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_palette(PaletteMode::Engine, window, cx);
                });
            });
            settle(cx);
            cx.simulate_input(typed);
            cx.simulate_keystrokes("enter");
            settle(cx);
        };

        pick(cx, "odbc");
        assert!(palette_mode(&workspace, cx).is_none());
        workspace.read_with(cx, |workspace, _| {
            let form = workspace.form.as_ref().unwrap();
            assert_eq!(form.target().fields(), crate::db::Fields::ConnectionString);
            assert_eq!(form.engine, Engine::Generic);
            assert_eq!(form.mode, crate::sql::Mode::ReadOnly);
        });
        assert!(has_keyboard(cx, |form| &form.connection));
        cx.simulate_input("Db2Dev");
        let typed = cx.update(|_, cx| {
            let workspace = workspace.read(cx);
            let form = workspace.form.as_ref().unwrap();
            form.connection.read(cx).value().to_string()
        });
        assert_eq!(typed, "Db2Dev");

        pick(cx, "postgres");
        workspace.read_with(cx, |workspace, _| {
            let form = workspace.form.as_ref().unwrap();
            assert_eq!(form.target(), crate::db::Target::built_in(Engine::Postgres));
        });
        assert!(has_keyboard(cx, |form| &form.host));

        quit(workspace, cx);
    });
}

/// Read-only picked while a connect is under way finds no connection to hold
/// (`set_mode`), so the session that lands is opened again under it rather
/// than left as the Read-write it set out as, with Read-only on screen.
#[gpui::test]
fn a_mode_picked_while_connecting_is_the_one_the_session_holds(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let echo = crate::db::echo::plugin();
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let database = directory.join("e2e.db");
        std::fs::write(&database, b"").unwrap();
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\ncheck_for_updates = false\n\
                 [[profiles]]\nid = \"e2e\"\nname = \"E2E\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"sqlite\"\npath = {:?}\n\
                 [[profiles]]\nid = \"echo\"\nname = \"Echo\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"plugin:echo\"\nspeaks = \"postgres\"\n\
                 connection = \"DSN=echo\"\n",
                database.display().to_string(),
            ),
        )
        .unwrap();

        let (workspace, cx) = launch_with(cx, vec![echo]);
        workspace.update(cx, |workspace, cx| {
            let index = workspace
                .profiles
                .iter()
                .position(|profile| matches!(profile.config, ConnectionConfig::External(_)))
                .expect("the echo profile is restored");
            assert_eq!(workspace.profiles[index].mode, crate::sql::Mode::ReadWrite);
            // Set so the connect never reaches for the keychain.
            if let ConnectionConfig::External(config) = &mut workspace.profiles[index].config {
                config.password = "x".into();
            }
            workspace.activate(index, cx);
            workspace.set_mode(crate::sql::Mode::ReadOnly, cx);
        });
        cx.run_until_parked();

        let connection = cx
            .update(|_, cx| {
                workspace
                    .read(cx)
                    .profile()
                    .and_then(|profile| profile.connection())
            })
            .expect("the echo profile connects");
        let last = connection
            .query("last", &CancelToken::default(), Fetch::default())
            .unwrap();
        assert_eq!(
            last.rows[0].get(0),
            Some("SET default_transaction_read_only = on")
        );

        quit(workspace, cx);
    });
}

/// A bar edit Generic refuses is put back, so the bars the tab saves stay the
/// ones its filter ran from.
#[gpui::test]
fn a_refused_bar_edit_leaves_the_bars_as_they_ran(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let database = directory.join("e2e.db");
        std::fs::write(&database, b"").unwrap();
        std::fs::write(
            directory.join("profiles.toml"),
            format!(
                "active = \"e2e\"\n\
                 [settings]\ncheck_for_updates = false\n\
                 [[profiles]]\nid = \"e2e\"\nname = \"E2E\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"sqlite\"\npath = {:?}\n\
                 [[profiles]]\nid = \"echo\"\nname = \"Echo\"\nhost = \"\"\ndatabase = \"\"\n\
                 user = \"\"\nengine = \"plugin:echo\"\nspeaks = \"generic\"\n\
                 connection = \"DSN=echo\"\n",
                database.display().to_string(),
            ),
        )
        .unwrap();

        let (workspace, cx) = launch_with(cx, vec![crate::db::echo::plugin()]);
        // The connect and the tab's query run on threads the scheduler does
        // not own; echo answers both at once, so each wait below ends.
        cx.executor().allow_parking();
        workspace.update(cx, |workspace, cx| {
            let index = workspace
                .profiles
                .iter()
                .position(|profile| matches!(profile.config, ConnectionConfig::External(_)))
                .unwrap();
            if let ConnectionConfig::External(config) = &mut workspace.profiles[index].config {
                config.password = "x".into();
            }
            workspace.activate(index, cx);
        });
        cx.run_until_parked();

        let bar = |column: &str, operator, value: &str| crate::filter::FilterBar {
            column: Some(column.into()),
            operator,
            value: value.into(),
            ..Default::default()
        };
        let ran = vec![
            bar("name", crate::filter::Operator::Contains, "ab"),
            bar("id", crate::filter::Operator::Equals, "50%"),
        ];
        let id = cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                let opened = crate::session::OpenedObject::Relation {
                    schema: "public".into(),
                    name: "accounts".into(),
                    kind: Default::default(),
                    filter: crate::filter::derived_filter(Engine::Generic, &ran, &[]),
                    filters: ran.clone(),
                };
                let id = workspace.open_object(opened, window, cx).unwrap();
                workspace.activate_tab(crate::session::Tab::Object(id), window, cx);
                id
            })
        });
        cx.run_until_parked();

        let saved = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                let workspace = workspace.read(cx);
                let profile = workspace.profile().unwrap();
                let tab = profile
                    .session
                    .objects
                    .iter()
                    .find(|tab| tab.id == id)
                    .unwrap();
                let notice = profile.session.shown_notice().map(str::to_string);
                (tab.filters(Engine::Generic), notice)
            })
        };
        let as_ran: Vec<_> = ran.iter().map(crate::filter::stored_filter).collect();

        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                let input = workspace.filter_rows_mut(id).unwrap()[0].value.clone();
                input.update(cx, |input, cx| input.set_value("a%b", window, cx));
                workspace.submit_filter(id, &input, cx);
            })
        });
        let (bars, notice) = saved(cx);
        assert_eq!(bars, as_ran);
        assert!(notice.is_some_and(|notice| notice.contains("LIKE")));

        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                let action = crate::actions::SetFilterOperator {
                    row: 1,
                    operator: crate::filter::Operator::Contains,
                };
                workspace.set_filter_operator(&action, window, cx);
            })
        });
        assert_eq!(saved(cx).0, as_ran);

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_headers_filter_entry_aims_a_bar_at_its_column(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, name text);
             insert into birds values (1, 'a');",
        );
        cx.executor().allow_parking();
        let row = explorer_row(&workspace, "relation-main\0birds", cx).center();
        cx.simulate_click(row, gpui::Modifiers::default());
        settle(cx);

        let (id, filters_on_server) = workspace.read_with(cx, |workspace, cx| {
            let session = &workspace.profile().unwrap().session;
            let id = session.active_object().unwrap().id;
            let filters = session
                .active_results()
                .unwrap()
                .read(cx)
                .delegate()
                .filters_on_server();
            (id, filters)
        });
        assert!(
            filters_on_server,
            "a relation's headers narrow it on the server"
        );

        // The header's own menu, opened the way a right click opens it, and
        // its first entry: Filter on name.
        let header = cx
            .debug_bounds("column-header-1")
            .expect("the name header is drawn")
            .center();
        cx.simulate_mouse_down(header, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.simulate_mouse_up(header, gpui::MouseButton::Right, gpui::Modifiers::default());
        settle(cx);
        cx.update(|window, _| {
            assert!(
                window
                    .context_stack()
                    .iter()
                    .any(|context| context.contains("PopupMenu")),
                "the header menu opened"
            )
        });
        cx.simulate_keystrokes("down");
        settle(cx);
        press(cx, "enter");
        settle(cx);

        let last = workspace.read_with(cx, |workspace, _| {
            let tab = workspace
                .profile()
                .unwrap()
                .session
                .objects
                .iter()
                .find(|tab| tab.id == id)
                .unwrap();
            let ObjectBody::Relation { filters, .. } = &tab.body else {
                panic!("a relation tab holds bars");
            };
            filters
                .last()
                .map(|filter| (filter.column.clone(), filter.raw, filter.applied.clone()))
        });
        assert_eq!(
            last,
            Some((Some("name".to_string()), false, String::new())),
            "a bar aimed at the column, with nothing applied yet"
        );
        // A query's result has no filter model, so its headers never offer
        // this.
        assert!(!crate::result_grid::ResultGrid::empty().filters_on_server());

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_editor_follows_its_buffer_until_the_split_is_dragged(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.executor().allow_parking();
        let panel = |cx: &mut VisualTestContext| {
            query_tab(&workspace, cx, |tab, _, cx| {
                tab.editor_split.read(cx).sizes().first().copied()
            })
        };
        let viewport = cx.update(|window, _| window.viewport_size().height.as_f32());

        // Empty or one line, the editor takes its floor rather than a fixed
        // pane; the results get everything else.
        cx.simulate_input("select 1");
        settle(cx);
        let short = panel(cx).expect("the split has been laid out");
        assert!(
            f32::from(short) >= crate::theme::layout::EDITOR_MIN_HEIGHT,
            "the editor keeps its floor"
        );

        // A long buffer stops at the window's own 40%.
        let tall = (0..40)
            .map(|line| format!("select {line};\n"))
            .collect::<String>();
        cx.simulate_keystrokes("secondary-a");
        cx.simulate_input(&tall);
        settle(cx);
        let long = panel(cx).expect("the split has been laid out");
        assert!(
            f32::from(long) <= viewport * 0.4 + 1.,
            "a long buffer took {} of {viewport}",
            f32::from(long)
        );

        // A drag moves the divider. Nothing about the content changed, so the
        // next frame must leave it where the drag put it -- a render that
        // re-applies the content height every frame makes the results pane
        // impossible to grow.
        let split = query_tab(&workspace, cx, |tab, _, _| tab.editor_split.clone());
        cx.update(|window, cx| {
            split.update(cx, |state, cx| {
                state.resize_panel(0, gpui::px(400.), window, cx)
            })
        });
        settle(cx);
        assert_eq!(
            panel(cx),
            Some(gpui::px(400.)),
            "the split follows the drag"
        );
        settle(cx);
        assert_eq!(
            panel(cx),
            Some(gpui::px(400.)),
            "and stays there while nothing else changes"
        );

        // Once dragged, the split is the user's: shrinking the buffer no
        // longer pulls it back to the content.
        let id = workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().unwrap();
            (
                profile.id.clone(),
                profile.session.active_query_tab().unwrap().id,
            )
        });
        workspace.update(cx, |workspace, cx| {
            workspace.mark_editor_resized(&id.0, id.1, cx);
        });
        cx.simulate_keystrokes("secondary-a");
        cx.simulate_input("select 1");
        settle(cx);
        assert_eq!(
            panel(cx),
            Some(gpui::px(400.)),
            "a dragged split does not follow the buffer"
        );

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_sorting_toggle_sits_with_the_rows_and_gives_the_keyboard_back(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.executor().allow_parking();
        cx.simulate_input("select 1 as n");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);

        // It is a control over the rows, drawn in their toolbar rather than in
        // the status bar below them.
        let client = cx
            .debug_bounds("view-sorting-client")
            .expect("the client chip is drawn")
            .center();
        assert!(cx.debug_bounds("view-sorting-server").is_some());
        let viewport = cx.update(|window, _| window.viewport_size().height.as_f32());
        assert!(
            f32::from(client.y)
                < viewport - crate::theme::layout::chrome(crate::theme::layout::STATUS_HEIGHT),
            "the toggle belongs to the rows, not the bar"
        );

        // A click switches who sorts and puts the keyboard back on the grid.
        cx.simulate_click(client, gpui::Modifiers::default());
        settle(cx);
        assert!(workspace.read_with(cx, |workspace, cx| {
            crate::views::view_sorts_on_client(workspace.profile().unwrap(), cx) == Some(true)
        }));
        assert!(
            grid_has_keyboard(&workspace, cx),
            "the grid takes the keyboard back"
        );

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_cell_value_opens_whole_and_hands_the_keyboard_back(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table docs (id integer primary key, body text);
             insert into docs values (1, '{\"a\":1}'), (2, 'plain');",
        );
        cx.executor().allow_parking();
        cx.simulate_input("select * from docs");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);

        let grid = workspace.read_with(cx, |workspace, _| {
            workspace
                .profile()
                .unwrap()
                .session
                .active_results()
                .unwrap()
                .clone()
        });
        let focused = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| grid.focus_handle(cx).is_focused(window))
        };
        let view_body = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                grid.focus_handle(cx).focus(window, cx);
                window.dispatch_action(Box::new(crate::actions::ViewCell { row: 0, col: 1 }), cx);
            });
            settle(cx);
        };

        view_body(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("cell-detail"));
        // The whole value, indented, not the one line the column had room for.
        let copy = cx
            .debug_bounds("cell-detail-0")
            .expect("the dialog's Copy is drawn")
            .center();
        cx.simulate_click(copy, gpui::Modifiers::default());
        settle(cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("{\n  \"a\": 1\n}".to_string())
        );
        // Copy says so, and the dialog takes itself down a moment later: no
        // second keystroke to dismiss a dialog that has done its job.
        assert!(
            cx.debug_bounds("cell-detail-copied").is_some(),
            "the receipt"
        );
        assert!(
            cx.debug_bounds("cell-detail-0").is_none(),
            "the button gave way to the receipt"
        );
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), None);
        assert!(focused(cx), "the grid takes the keyboard back");

        // The ring moves with the next arrow key, so the keyboard is really
        // the grid's.
        let active = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .profile()
                    .unwrap()
                    .session
                    .active_results()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .active()
            })
        };
        cx.simulate_keystrokes("down");
        settle(cx);
        let first = active(cx);
        cx.simulate_keystrokes("down");
        settle(cx);
        let second = active(cx);
        assert_eq!((first, second), (Some((0, 0)), Some((1, 0))));

        // Enter closes it without copying, and Escape does the same from the
        // same place.
        view_body(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("cell-detail"));
        press(cx, "enter");
        assert_eq!(front_dialog(&workspace, cx), None);
        assert!(focused(cx));

        view_body(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("cell-detail"));
        press(cx, "escape");
        assert_eq!(front_dialog(&workspace, cx), None);
        assert!(focused(cx));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn find_in_the_grid_walks_the_matches_and_hands_the_keyboard_back(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table notes (id integer primary key, body text);
             insert into notes values (1, 'alpha'), (2, 'beta'), (3, 'ALPHA');",
        );
        cx.executor().allow_parking();
        cx.simulate_input("select * from notes order by id");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);

        let grid = workspace.read_with(cx, |workspace, _| {
            workspace
                .profile()
                .unwrap()
                .session
                .active_results()
                .unwrap()
                .clone()
        });
        let state = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                let find = workspace.find.as_ref().map(|find| (find.at, find.total));
                let active = workspace
                    .profile()
                    .unwrap()
                    .session
                    .active_results()
                    .unwrap()
                    .read(cx)
                    .delegate()
                    .active();
                (find, active)
            })
        };

        // The grid has the keyboard, the way a click gives it.
        cx.update(|window, cx| grid.focus_handle(cx).focus(window, cx));
        cx.simulate_keystrokes("secondary-f");
        settle(cx);
        assert!(cx.debug_bounds("find-count").is_some(), "the bar is drawn");

        cx.simulate_input("alpha");
        settle(cx);
        // Two rows, in row order, and the ring on the first -- in the body
        // column, not the id.
        assert_eq!(state(cx), (Some((0, 2)), Some((0, 1))));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            None,
            "finding copies nothing"
        );

        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(state(cx), (Some((1, 2)), Some((2, 1))));
        cx.simulate_keystrokes("shift-enter");
        settle(cx);
        assert_eq!(state(cx), (Some((0, 2)), Some((0, 1))));

        // Escape puts the bar away, unmarks the cells, and leaves the ring's
        // keyboard on the grid.
        cx.simulate_keystrokes("escape");
        settle(cx);
        assert_eq!(state(cx), (None, Some((0, 1))));
        assert!(cx.debug_bounds("find-count").is_none(), "the bar is gone");
        assert!(
            cx.update(|window, cx| grid.focus_handle(cx).is_focused(window)),
            "the grid takes the keyboard back"
        );
        cx.simulate_keystrokes("down");
        settle(cx);
        assert_eq!(state(cx), (None, Some((1, 1))));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_summary_view_replaces_the_rows_and_the_data_chip_comes_back(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "");
        cx.executor().allow_parking();
        // A tab that has not run has nothing to summarise, so the chip is not
        // there yet.
        assert!(cx.debug_bounds("view-Summary").is_none());

        cx.simulate_input("select 1 as one");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        // Data is what a finished run leaves up, with the trio above it.
        assert!(cx.debug_bounds("summary").is_none());
        assert!(cx.debug_bounds("export-results").is_some());

        let chip = |selector: &'static str, cx: &mut VisualTestContext| {
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is drawn"))
                .center();
            cx.simulate_click(bounds, gpui::Modifiers::default());
            settle(cx);
        };
        chip("view-Summary", cx);
        assert!(cx.debug_bounds("summary").is_some(), "the summary is up");
        assert!(
            cx.debug_bounds("export-results").is_none(),
            "the rows and their toolbar gave way"
        );
        assert!(workspace.read_with(cx, |workspace, _| {
            workspace
                .profile()
                .unwrap()
                .session
                .active_query_tab()
                .unwrap()
                .summary_shown()
        }));

        chip("view-Data", cx);
        assert!(cx.debug_bounds("summary").is_none());
        assert!(cx.debug_bounds("export-results").is_some());
        assert!(!workspace.read_with(cx, |workspace, _| {
            workspace
                .profile()
                .unwrap()
                .session
                .active_query_tab()
                .unwrap()
                .summary_shown()
        }));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_relations_copies_ride_above_its_rows_and_its_structure(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, name text);",
        );
        cx.executor().allow_parking();
        let row = explorer_row(&workspace, "relation-main\0birds", cx).center();
        cx.simulate_click(row, gpui::Modifiers::default());
        settle(cx);
        // The DDL and the count are offered on a live connection; the connect
        // lands on the executor a frame or two after the tab opens.
        for _ in 0..10 {
            let connected = workspace.read_with(cx, |workspace, _| {
                matches!(
                    workspace.profile().unwrap().state,
                    ProfileState::Connected(_)
                )
            });
            if connected {
                break;
            }
            settle(cx);
        }
        settle(cx);

        // The data page: its rows' actions over the grid, its definition's
        // beside them, and nothing of either in the status bar below.
        assert!(cx.debug_bounds("import-rows").is_some(), "import");
        assert!(cx.debug_bounds("count-rows").is_some(), "count");
        assert!(cx.debug_bounds("copy-create").is_some(), "ddl");

        cx.simulate_keystrokes("secondary-shift-d");
        settle(cx);
        let structure = workspace.read_with(cx, |workspace, _| {
            matches!(
                workspace
                    .profile()
                    .unwrap()
                    .session
                    .active_object()
                    .unwrap()
                    .body,
                ObjectBody::Structure { .. }
            )
        });
        assert!(structure, "the structure page is in front");
        assert!(
            cx.debug_bounds("copy-create").is_some(),
            "the structure page keeps them"
        );

        quit(workspace, cx);
    });
}

#[gpui::test]
fn pinning_leaves_every_row_its_height(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        let table = explorer::relation_row("main", "birds");
        let unpinned = explorer_row(&workspace, &table, cx).size.height;

        cx.update(|_, cx| {
            workspace.update(cx, |workspace, cx| {
                let leaf = workspace.profile().unwrap().session.explorer_leaves[&table];
                let birds = workspace.member_at(leaf.target).unwrap();
                workspace.toggle_pin(explorer::Pin::Member(birds), cx);
            })
        });
        settle(cx);

        assert_eq!(explorer_row(&workspace, &table, cx).size.height, unpinned);
        assert_eq!(
            explorer_row(&workspace, explorer::PINNED_GROUP, cx)
                .size
                .height,
            unpinned,
            "the Pinned group, listed first, is the row every row is sized by"
        );

        quit(workspace, cx);
    });
}

/// The arguments a routine row opens into: one line each, drawn under it and
/// opening nothing themselves.
#[gpui::test]
fn an_argument_row_is_drawn_under_its_routine_and_opens_nothing(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        settle(cx);
        // Set in directly: the shape the builder makes when a routine's
        // arguments have been read. SQLite has no routines to read them from.
        let routine = "routine-Function-main\0account_name(account_id bigint)";
        let argument = explorer::argument_row(routine, 0);
        let items = vec![
            gpui_component::tree::TreeItem::new(routine, "account_name")
                .expanded(true)
                .child(
                    gpui_component::tree::TreeItem::new(argument.clone(), "account_id bigint")
                        .disabled(true),
                ),
        ];
        cx.update(|_, cx| {
            let tree = workspace
                .read(cx)
                .profile()
                .unwrap()
                .session
                .explorer_tree
                .clone();
            tree.update(cx, |tree, cx| tree.set_items(items, cx));
        });
        settle(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let drawn = explorer_row(&workspace, &argument, cx);
        assert!(
            drawn.origin.y > explorer_row(&workspace, routine, cx).origin.y,
            "the argument is listed under the routine it belongs to"
        );
        // Nothing to open or select: the line is text, not a row.
        cx.simulate_click(drawn.center(), gpui::Modifiers::default());
        settle(cx);
        let selected = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                let tree = &workspace.read(cx).profile().unwrap().session.explorer_tree;
                tree.read(cx).selected_index()
            })
        };
        assert_eq!(selected(cx), None);
        assert_eq!(
            cx.update(|_, cx| workspace.read(cx).profile().unwrap().session.objects.len()),
            0
        );
        // The routine's own row, which a click does select.
        let routine_row = explorer_row(&workspace, routine, cx).center();
        cx.simulate_click(routine_row, gpui::Modifiers::default());
        settle(cx);
        assert!(selected(cx).is_some());

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_service_launch_does_not_reuse_a_profiles_explicit_port(cx: &mut TestAppContext) {
    // PGSERVICE must be set before the process starts, not raced against
    // every other test that reads the environment.
    const CHILD: &str = "DBDELVE_SERVICE_PORT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "workspace::e2e::a_service_launch_does_not_reuse_a_profiles_explicit_port",
                "--exact",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PGSERVICE", "startup-port")
            .env_remove("PGSERVICEFILE")
            .env_remove("PGSYSCONFDIR")
            .status()
            .expect("the child test should run");
        assert!(status.success());
        return;
    }

    store::tests::with_home(|| {
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("profiles.toml"),
            "active = \"explicit\"\n\
             [settings]\n\
             check_for_updates = false\n\
             [[profiles]]\n\
             id = \"explicit\"\n\
             name = \"Explicit port\"\n\
             host = \"\"\n\
             port = 5432\n\
             database = \"\"\n\
             user = \"\"\n\
             service = \"startup-port\"\n\
             [[profiles]]\n\
             id = \"inherited\"\n\
             name = \"Service port\"\n\
             host = \"\"\n\
             database = \"\"\n\
             user = \"\"\n\
             service = \"startup-port\"\n",
        )
        .unwrap();
        let (workspace, cx) = launch(cx);
        let active =
            workspace.read_with(cx, |workspace, _| workspace.profile().unwrap().id.clone());
        quit(workspace, cx);
        assert_eq!(active, "inherited");
    });
}

#[gpui::test]
fn a_click_on_an_empty_table_takes_the_keyboard_for_a_paste(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) =
            launch_on_sqlite_holding(cx, "create table nests (id integer primary key);");
        cx.executor().allow_parking();
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("id\n5\n".into()));
        let nests = explorer_row(&workspace, "relation-main\0nests", cx).center();
        cx.simulate_click(nests, gpui::Modifiers::default());
        settle(cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus.focus(window, cx)
        });
        settle(cx);

        let empty = cx
            .debug_bounds("empty-results")
            .expect("the message is drawn");
        cx.simulate_click(empty.center(), gpui::Modifiers::default());
        settle(cx);
        cx.simulate_keystrokes("secondary-v");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("import-rows"));

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_ways_to_import_show_only_where_rows_can_be_imported(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key); insert into birds values (1);
             create table nests (id integer primary key);",
        );
        cx.executor().allow_parking();
        let drawn = |selector: &'static str, cx: &mut VisualTestContext| {
            cx.debug_bounds(selector).is_some()
        };
        let open = |name: &str, cx: &mut VisualTestContext| {
            let row: &'static str = format!("relation-main\0{name}").leak();
            let row = explorer_row(&workspace, row, cx).center();
            cx.simulate_click(row, gpui::Modifiers::default());
            settle(cx);
        };
        assert!(!drawn("import-rows", cx), "a query tab imports nothing");

        open("birds", cx);
        assert!(drawn("import-rows", cx));
        assert!(
            !drawn("paste-rows", cx),
            "a table with rows needs no invitation"
        );
        let menu_offers_paste = cx.update(|_, cx| {
            let session = &workspace.read(cx).profile().unwrap().session;
            session
                .active_results()
                .unwrap()
                .read(cx)
                .delegate()
                .takes_imports()
        });
        assert!(menu_offers_paste);

        open("nests", cx);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("id\n5\n".into()));
        let paste = cx
            .debug_bounds("paste-rows")
            .expect("an empty table invites rows");
        cx.simulate_click(paste.center(), gpui::Modifiers::default());
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("import-rows"));
        cx.simulate_keystrokes("escape");
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), None);

        workspace.update(cx, |workspace, cx| {
            workspace.set_mode(crate::sql::Mode::ReadOnly, cx)
        });
        settle(cx);
        assert!(!drawn("paste-rows", cx), "read-only has nothing to invite");

        quit(workspace, cx);
    });
}

#[gpui::test]
fn file_exports_from_the_menu_ask_for_the_right_file(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) =
            launch_on_sqlite_holding(cx, "create table birds (id integer primary key);");
        cx.executor().allow_parking();
        cx.simulate_input("select * from birds");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);

        for (downs, name) in [(3, "results.csv"), (4, "results.json"), (5, "results.sql")] {
            super::exports::SUGGESTED_NAMES.with_borrow_mut(Vec::clear);
            let bounds = cx
                .debug_bounds("export-results")
                .expect("Export button is drawn");
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
            settle(cx);
            for _ in 0..downs {
                cx.simulate_keystrokes("down");
            }
            press(cx, "enter");
            cx.simulate_new_path_selection(|_| None);
            settle(cx);
            let asked = super::exports::SUGGESTED_NAMES.with_borrow(Clone::clone);
            assert_eq!(asked, [name], "{downs} downs");
        }

        quit(workspace, cx);
    });
}

#[gpui::test]
fn copy_results_formats_work_from_menu_and_palette_and_restore_focus(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, note text);
             insert into birds values (1, null), (2, ''), (3, 'NULL');",
        );
        cx.executor().allow_parking();
        let sql = "select * from birds order by id";
        cx.simulate_input(sql);
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);

        let read_clipboard = |cx: &mut VisualTestContext| {
            let item = cx.read_from_clipboard().expect("copy writes the clipboard");
            let metadata = item.entries().iter().find_map(|entry| match entry {
                gpui::ClipboardEntry::String(string) => string.metadata.clone(),
                _ => None,
            });
            (item.text().unwrap(), metadata)
        };
        let expected = |format: crate::export::ClipboardFormat, cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                let grid = workspace
                    .profile()
                    .unwrap()
                    .session
                    .active_results()
                    .unwrap()
                    .read(cx)
                    .delegate();
                format.render(Engine::Sqlite, grid.result()).unwrap()
            })
        };
        let open_menu = |cx: &mut VisualTestContext| {
            let bounds = cx
                .debug_bounds("export-results")
                .expect("Export button is drawn");
            cx.simulate_click(bounds.center(), gpui::Modifiers::default());
            settle(cx);
            cx.update(|window, _| {
                assert!(
                    window
                        .context_stack()
                        .iter()
                        .any(|context| context.contains("PopupMenu"))
                )
            });
        };

        for object_tab in [false, true] {
            if object_tab {
                let row = explorer_row(&workspace, "relation-main\0birds", cx).center();
                cx.simulate_click(row, gpui::Modifiers::default());
                settle(cx);
            }
            open_menu(cx);
            cx.simulate_keystrokes("down");
            press(cx, "enter");
            assert_eq!(
                read_clipboard(cx).0,
                expected(crate::export::ClipboardFormat::Tsv, cx)
            );

            for (ix, format) in crate::export::ClipboardFormat::ALL.into_iter().enumerate() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string("before menu".into()));
                open_menu(cx);
                cx.simulate_keystrokes("down down");
                settle(cx);
                // The menu hangs off the toolbar's left end, so its submenu
                // opens rightwards.
                cx.simulate_keystrokes("right");
                settle(cx);
                for _ in 0..ix {
                    cx.simulate_keystrokes("down");
                }
                press(cx, "enter");
                let (text, metadata) = read_clipboard(cx);
                assert_eq!(text, expected(format, cx), "menu {format:?}");
                let metadata = metadata.expect("all successful table copies carry original rows");
                let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
                assert_eq!(metadata["rows"][0]["note"], serde_json::Value::Null);
                assert_eq!(metadata["rows"][1]["note"], "");
                assert_eq!(metadata["rows"][2]["note"], "NULL");
                assert!(metadata["columns"][0]["type"].is_string());
                let (_, bytes, _) = crate::import_rows::pasted(
                    &text,
                    Some(&serde_json::to_string(&metadata).unwrap()),
                    false,
                )
                .unwrap();
                let pasted: Vec<serde_json::Value> = String::from_utf8(bytes)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                assert_eq!(pasted, metadata["rows"].as_array().unwrap().clone());
                settle(cx);
                cx.update(|window, _| {
                    assert!(
                        !window
                            .context_stack()
                            .iter()
                            .any(|context| context.contains("PopupMenu"))
                    )
                });

                cx.write_to_clipboard(gpui::ClipboardItem::new_string("before palette".into()));
                cx.simulate_keystrokes("secondary-shift-p");
                settle(cx);
                cx.simulate_input(&format!("Copy results as {}", format.label()));
                press(cx, "enter");
                assert_eq!(read_clipboard(cx).0, text, "palette {format:?}");
            }
            open_menu(cx);
            press(cx, "escape");
            cx.update(|window, cx| {
                assert!(
                    !window
                        .context_stack()
                        .iter()
                        .any(|context| context.contains("PopupMenu"))
                );
                assert!(
                    window.focused(cx).is_some(),
                    "Escape returns a live dispatch path"
                );
            });
            // The existing copy action still defaults to TSV after a table format was used.
            cx.simulate_keystrokes("secondary-k r");
            settle(cx);
            cx.update(|window, cx| {
                window.dispatch_action(Box::new(crate::actions::CopyResults), cx);
            });
            assert_eq!(
                read_clipboard(cx).0,
                expected(crate::export::ClipboardFormat::Tsv, cx)
            );
            if !object_tab {
                assert_eq!(buffer(&workspace, cx), sql);
            } else {
                let grid = workspace.read_with(cx, |workspace, _| {
                    workspace
                        .profile()
                        .unwrap()
                        .session
                        .active_results()
                        .unwrap()
                        .clone()
                });
                grid.update(cx, |grid, cx| {
                    assert!(grid.delegate_mut().set_pending(
                        0,
                        1,
                        crate::result_grid::NewValue::Value("pending".into())
                    ));
                    cx.notify();
                });
                workspace.update(cx, |_, cx| cx.notify());
                settle(cx);
                assert!(cx.debug_bounds("export-results").is_none());
                for format in crate::export::ClipboardFormat::ALL {
                    workspace.update(cx, |workspace, cx| workspace.copy_results_as(format, cx));
                    assert_eq!(read_clipboard(cx).0, expected(format, cx));
                    assert!(!read_clipboard(cx).0.contains("pending"));
                }
                grid.update(cx, |grid, _| grid.delegate_mut().discard_pending());
            }
        }
        quit(workspace, cx);
    });
}

#[gpui::test]
fn refused_table_copies_preserve_the_clipboard(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        let grid = workspace.read_with(cx, |workspace, _| {
            workspace
                .profile()
                .unwrap()
                .session
                .active_results()
                .unwrap()
                .clone()
        });
        let column = crate::db::Column {
            name: "v".into(),
            data_type: Some("text".into()),
        };
        let result = crate::db::QueryResult {
            columns: vec![column.clone()],
            rows: crate::db::rows(vec![vec![Some(
                "x".repeat(crate::result_grid::COPIED_ROWS_TEXT_LIMIT),
            )]]),
            ..Default::default()
        };
        grid.update(cx, |grid, _| {
            *grid.delegate_mut() =
                crate::result_grid::ResultGrid::new(result, crate::sql::Mode::ReadOnly);
        });
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("previous clipboard".into()));
        for format in crate::export::ClipboardFormat::ALL.into_iter().skip(2) {
            workspace.update(cx, |workspace, cx| workspace.copy_results_as(format, cx));
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("previous clipboard")
            );
            workspace.read_with(cx, |workspace, _| {
                assert_eq!(
                    workspace.profile().unwrap().session.notice.as_deref(),
                    Some("Table output exceeds 16 MiB. Copy as TSV/CSV or export to a file.")
                );
            });
        }
        let width = (crate::result_grid::COPIED_ROWS_TEXT_LIMIT - 24) / 5;
        let value = format!("{}\u{301}", "x".repeat(width));
        grid.update(cx, |grid, _| {
            *grid.delegate_mut() = crate::result_grid::ResultGrid::new(
                crate::db::QueryResult {
                    columns: vec![column.clone()],
                    rows: crate::db::rows(vec![vec![Some(value.clone())]]),
                    ..Default::default()
                },
                crate::sql::Mode::ReadOnly,
            );
        });
        workspace.update(cx, |workspace, cx| {
            workspace.copy_results_as(crate::export::ClipboardFormat::AsciiTable, cx);
        });
        let item = cx.read_from_clipboard().unwrap();
        assert_eq!(
            item.text().unwrap().len(),
            crate::result_grid::COPIED_ROWS_TEXT_LIMIT
        );
        let metadata = item
            .entries()
            .iter()
            .find_map(|entry| match entry {
                gpui::ClipboardEntry::String(string) => string.metadata.as_deref(),
                _ => None,
            })
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(metadata).unwrap();
        assert_eq!(metadata["rows"][0]["v"], value);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("previous clipboard".into()));
        for result in [
            crate::result_grid::ResultGrid::new(
                crate::db::QueryResult::default(),
                crate::sql::Mode::ReadOnly,
            ),
            crate::result_grid::ResultGrid::new(
                crate::db::QueryResult {
                    columns: vec![column.clone()],
                    capped_from: Some(10),
                    ..Default::default()
                },
                crate::sql::Mode::ReadOnly,
            ),
            crate::result_grid::ResultGrid::new(
                crate::db::QueryResult {
                    columns: vec![column],
                    ..Default::default()
                },
                crate::sql::Mode::ReadOnly,
            )
            .streaming(0),
        ] {
            grid.update(cx, |grid, _| *grid.delegate_mut() = result);
            for format in crate::export::ClipboardFormat::ALL {
                workspace.update(cx, |workspace, cx| workspace.copy_results_as(format, cx));
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().as_deref(),
                    Some("previous clipboard")
                );
            }
        }
        quit(workspace, cx);
    });
}

fn disconnect(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.dispatch_action(Box::new(Disconnect), cx));
    settle(cx);
}

/// What the palette up lists, whatever scope each row is in.
fn palette_labels(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(cx, |workspace, cx| {
        let palette = workspace.palette.as_ref().expect("the palette is up");
        palette
            .read(cx)
            .labels()
            .into_iter()
            .map(str::to_string)
            .collect()
    })
}

fn buffers_of(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(cx, |workspace, cx| {
        workspace.profiles[0]
            .session
            .queries
            .iter()
            .map(|tab| tab.editor.read(cx).value().to_string())
            .collect()
    })
}

#[gpui::test]
fn disconnect_leads_home_and_a_relaunch_stays_there(cx: &mut TestAppContext) {
    // CI's live job sets `PG*`, whose profile would be put in front on launch.
    crate::IGNORE_ENVIRONMENT.set(true);
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        cx.simulate_input("select 1");
        cx.simulate_keystrokes("secondary-shift-p");
        settle(cx);
        let offered = palette_labels(&workspace, cx);
        assert!(offered.iter().any(|label| label == "Disconnect"));
        assert!(offered.iter().any(|label| label == "Disconnect from E2E"));
        cx.simulate_keystrokes("escape");
        settle(cx);

        disconnect(cx);
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.profile().is_none());
            assert!(matches!(workspace.profiles[0].state, ProfileState::Idle));
        });
        assert_eq!(buffers_of(&workspace, cx), ["select 1"]);
        assert_eq!(store::peek_profiles(&[]).unwrap().active, None);
        cx.update(|window, cx| {
            assert!(
                window.focused(cx).is_some(),
                "the welcome surface takes the keyboard"
            )
        });

        // Home lists the connections and nothing that needs one in front.
        cx.update(|window, cx| {
            window.dispatch_action(Box::new(crate::actions::SwitchConnection), cx)
        });
        settle(cx);
        assert!(palette_mode(&workspace, cx) == Some(PaletteMode::Search(Scope::Connections)));
        let offered = palette_labels(&workspace, cx);
        assert!(offered.iter().any(|label| label == "E2E"));
        assert!(!offered.iter().any(|label| label.starts_with("Disconnect")));
        cx.simulate_keystrokes("escape");
        settle(cx);

        // A second launch over the same home, as `launch` builds it.
        drop(workspace);
        let mut relaunched = None;
        let (_, cx) = cx.cx.add_window_view(|window, cx| {
            let entity = cx.new(|cx| Workspace::new(Vec::new(), window, cx));
            relaunched = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let workspace = relaunched.expect("the window builds the workspace");
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.profile().is_none());
            assert!(matches!(workspace.profiles[0].state, ProfileState::Idle));
        });
        assert_eq!(buffers_of(&workspace, cx), ["select 1"]);

        quit(workspace, cx);
    });
}

#[gpui::test]
fn a_connection_picked_on_the_welcome_surface_comes_in_front_connected(cx: &mut TestAppContext) {
    crate::IGNORE_ENVIRONMENT.set(true);
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite(cx);
        disconnect(cx);
        let row = cx
            .debug_bounds("welcome-connection-0")
            .expect("the connection is listed");
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().expect("the picked one is in front");
            assert!(matches!(profile.state, ProfileState::Connected(_)));
        });
        assert_eq!(
            store::peek_profiles(&[]).unwrap().active.as_deref(),
            Some("e2e")
        );

        quit(workspace, cx);
    });
}

#[gpui::test]
fn disconnect_is_refused_while_a_grid_holds_unapplied_edits(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, name text); \
             insert into birds values (1, 'wren');",
        );
        cx.simulate_input("select * from birds");
        cx.simulate_keystrokes("secondary-enter");
        settle(cx);
        let grid = query_tab(&workspace, cx, |tab, _, _| tab.results.clone());
        grid.update(cx, |grid, _| {
            assert!(grid.delegate_mut().set_pending(
                0,
                1,
                crate::result_grid::NewValue::Value("robin".into())
            ));
        });

        disconnect(cx);
        workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().expect("the connection stays in front");
            assert!(matches!(profile.state, ProfileState::Connected(_)));
            let notice = profile.session.notice.as_deref().unwrap_or_default();
            assert!(
                notice.ends_with("so the connection was not closed."),
                "{notice}"
            );
        });

        grid.update(cx, |grid, _| grid.delegate_mut().discard_pending());
        quit(workspace, cx);
    });
}

#[gpui::test]
fn disconnect_is_refused_while_an_export_runs(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        workspace.update(cx, |workspace, _| {
            let profile = workspace.profile_mut().expect("a connection in front");
            profile.session.exporting = Some(crate::session::Exporting {
                file: "birds.csv".into(),
                started: std::time::Instant::now(),
                progress: Default::default(),
            });
        });

        disconnect(cx);
        workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().expect("the connection stays in front");
            assert!(matches!(profile.state, ProfileState::Connected(_)));
            let notice = profile.session.notice.as_deref().unwrap_or_default();
            assert_eq!(
                notice,
                "An export to birds.csv is running, so the connection was not closed."
            );
        });

        workspace.update(cx, |workspace, _| {
            workspace.profile_mut().expect("in front").session.exporting = None;
        });
        quit(workspace, cx);
    });
}

#[gpui::test]
fn disconnect_stops_what_a_relation_tab_holds_the_connection_for(cx: &mut TestAppContext) {
    crate::IGNORE_ENVIRONMENT.set(true);
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        let row = explorer_row(&workspace, "relation-main\0birds", cx).center();
        cx.simulate_click(row, gpui::Modifiers::default());
        settle(cx);
        // A count still out, which a run left going is the same as: a task
        // holding a clone of the connection until the server answers.
        workspace.update(cx, |workspace, _| {
            let profile = workspace.profile_mut().expect("a profile is open");
            let tab = profile
                .session
                .objects
                .first_mut()
                .expect("the table opened");
            let ObjectBody::Relation { count, .. } = &mut tab.body else {
                panic!("a relation tab");
            };
            *count = crate::session::RowCount::Counting {
                filter: String::new(),
                started: std::time::Instant::now(),
                cancel: CancelToken::default(),
                cancelling: false,
            };
        });

        disconnect(cx);
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.profile().is_none());
            let tab = &workspace.profiles[0].session.objects[0];
            assert!(matches!(
                &tab.body,
                ObjectBody::Relation {
                    count: crate::session::RowCount::Counting {
                        cancelling: true,
                        ..
                    },
                    ..
                }
            ));
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn disconnect_closes_an_import_dialog_that_is_not_running(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(cx, "create table birds (name text);");
        let file = store::dbdelve_directory().unwrap().join("birds.csv");
        std::fs::write(&file, "name\nwren\n").unwrap();
        let profile = workspace.update_in(cx, |workspace, window, cx| {
            let profile = workspace.profile().unwrap().id.clone();
            workspace.open_import_file(
                profile.clone(),
                "main".into(),
                "birds".into(),
                file,
                window,
                cx,
            );
            profile
        });
        settle(cx);
        assert_eq!(front_dialog(&workspace, cx), Some("import-rows"));

        disconnect(cx);
        workspace.read_with(cx, |workspace, _| {
            let found = workspace.profiles.iter().find(|found| found.id == profile);
            assert!(found.unwrap().session.import_rows.is_none());
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn files_opened_while_connecting_are_not_run_by_a_later_connect(cx: &mut TestAppContext) {
    crate::IGNORE_ENVIRONMENT.set(true);
    store::tests::with_home(|| {
        let (workspace, cx) = launch(cx);
        settle(cx);
        cx.executor().allow_parking();
        let directory = store::dbdelve_directory().unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("birds.csv");
        std::fs::write(&file, "name\nwren\n").unwrap();
        // In one update, so the connect the file starts cannot land between.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_data_files(vec![file], window, cx);
            assert!(!workspace.runs_on_connect.is_empty());
            workspace.disconnect(&Disconnect, window, cx);
            assert!(workspace.runs_on_connect.is_empty());
        });
        settle(cx);

        workspace.update(cx, |workspace, cx| workspace.activate(0, cx));
        settle(cx);
        workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().expect("the files profile is in front");
            assert!(matches!(profile.state, ProfileState::Connected(_)));
        });
        query_tab(&workspace, cx, |tab, _, cx| {
            assert!(matches!(tab.query, QueryState::Idle));
            assert!(
                !tab.editor.read(cx).value().is_empty(),
                "the statement stays"
            );
        });

        quit(workspace, cx);
    });
}

#[gpui::test]
fn the_default_chip_puts_a_typed_then_emptied_insert_field_back_out_of_the_statement(
    cx: &mut TestAppContext,
) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "create table birds (id integer primary key, name text, wing text);",
        );
        workspace.update_in(cx, |workspace, window, cx| {
            let leaf = workspace.profile().unwrap().session.explorer_leaves
                [&explorer::relation_row("main", "birds")];
            workspace.open_explorer_target(leaf.target, window, cx);
        });
        settle(cx);
        let open_form = |cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.new_row(&crate::actions::NewRow, window, cx);
            });
            settle(cx);
        };
        let input = |index: usize, cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, _| {
                let form = workspace.profile().unwrap().session.insert_form.as_ref();
                form.unwrap().fields[index].input.clone()
            })
        };
        let focus = |index: usize, cx: &mut VisualTestContext| {
            let input = input(index, cx);
            cx.update(|window, cx| input.focus_handle(cx).focus(window, cx));
            settle(cx);
        };
        let typing_in = |index: usize, cx: &mut VisualTestContext| {
            let input = input(index, cx);
            cx.update(|window, cx| input.focus_handle(cx).is_focused(window))
        };
        let state = |index: usize, cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                let form = workspace.profile().unwrap().session.insert_form.as_ref();
                let field = &form.unwrap().fields[index];
                let input = field.input.read(cx);
                (
                    field.nulled,
                    field.touched,
                    input.value().to_string(),
                    input.presentation().placeholder().to_string(),
                )
            })
        };
        let statement = |cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.confirm_new_row(window, cx);
            });
            settle(cx);
            workspace.read_with(cx, |workspace, _| {
                let review = workspace.profile().unwrap().session.apply_review.as_ref();
                review.unwrap().sql.clone()
            })
        };
        let left_out = || (false, false, String::new(), "DEFAULT".to_string());
        let nulled = || (true, false, String::new(), "NULL".to_string());
        let typed = |text: &str| (false, true, text.to_string(), "empty string".to_string());

        open_form(cx);
        focus(0, cx);
        cx.simulate_input("7");
        settle(cx);
        assert_eq!(state(0, cx), typed("7"));
        cx.simulate_keystrokes("backspace");
        settle(cx);
        assert_eq!(state(0, cx), typed(""), "emptying keeps the field a value");

        // Each field is its DEFAULT, its NULL, then its box.
        cx.simulate_keystrokes("shift-tab shift-tab");
        settle(cx);
        assert!(!typing_in(0, cx));
        press(cx, "enter");
        assert_eq!(state(0, cx), left_out(), "DEFAULT puts the field back out");
        cx.simulate_keystrokes("tab");
        settle(cx);
        press(cx, "space");
        assert_eq!(state(0, cx), nulled());
        cx.simulate_keystrokes("tab");
        settle(cx);
        assert!(typing_in(0, cx));
        cx.simulate_input("9");
        settle(cx);
        assert_eq!(state(0, cx), typed("9"), "typing turns the NULL off");
        cx.simulate_keystrokes("shift-tab");
        settle(cx);
        press(cx, "space");
        assert_eq!(state(0, cx), nulled(), "NULL clears the box");
        press(cx, "space");
        assert_eq!(state(0, cx), left_out());

        cx.simulate_keystrokes("tab tab tab tab");
        settle(cx);
        assert!(typing_in(1, cx));
        cx.simulate_input("robin");
        cx.simulate_keystrokes("tab tab tab");
        settle(cx);
        assert!(typing_in(2, cx));
        cx.simulate_input("x");
        cx.simulate_keystrokes("backspace");
        settle(cx);
        assert_eq!(state(1, cx), typed("robin"));
        assert_eq!(state(2, cx), typed(""));
        let sql = statement(cx);
        assert!(!sql.contains("\"id\""), "{sql}");
        assert!(sql.contains("\"name\"") && sql.contains("'robin'"), "{sql}");
        assert!(sql.contains("\"wing\"") && sql.contains("''"), "{sql}");

        workspace.update(cx, |workspace, cx| workspace.close_apply_review(cx));
        open_form(cx);
        focus(1, cx);
        cx.simulate_input("robin");
        cx.simulate_keystrokes("shift-tab");
        settle(cx);
        press(cx, "space");
        assert_eq!(state(1, cx), nulled());
        let sql = statement(cx);
        assert!(sql.contains("NULL") && !sql.contains("robin"), "{sql}");

        quit(workspace, cx);
    });
}

/// The table editor's whole path: a column removed in the dialog, the batch
/// gated as a write on a Read-only connection, answered by the mode prompt, and
/// the structure tab reloaded from what the server now holds.
#[gpui::test]
fn a_column_removed_in_the_table_editor_runs_through_the_mode_prompt(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "CREATE TABLE items (id integer NOT NULL PRIMARY KEY, note text);",
        );
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.edit_structure(
                    "main".into(),
                    "items".into(),
                    crate::db::RelationKind::Table,
                    window,
                    cx,
                );
            });
        });
        // The definition the tab asked for lands, and the frame after seeds the
        // dialog's rows from it.
        settle(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            front_dialog(&workspace, cx),
            Some("structure-editor"),
            "the editor is the dialog in front"
        );

        // The second row is `note`, by the order the server listed them.
        workspace.update(cx, |workspace, cx| workspace.fold_structure_column(1, cx));
        workspace.update(cx, |workspace, cx| workspace.apply_structure_edit(cx));
        settle(cx);
        assert_eq!(
            front_dialog(&workspace, cx),
            Some("pending-run"),
            "a DROP COLUMN on a Read-only connection asks first"
        );
        cx.simulate_keystrokes("escape");
        workspace.update(cx, |workspace, cx| workspace.cancel_pending_run(cx));
        settle(cx);
        assert_eq!(
            front_dialog(&workspace, cx),
            Some("structure-editor"),
            "declining the prompt leaves the editor where it was"
        );

        workspace.update(cx, |workspace, cx| {
            workspace.set_mode(crate::sql::Mode::Full, cx)
        });
        workspace.update(cx, |workspace, cx| workspace.apply_structure_edit(cx));
        settle(cx);
        let columns: Vec<String> = workspace.read_with(cx, |workspace, _| {
            let profile = workspace.profile().expect("the profile is in front");
            let tab = profile
                .session
                .objects
                .iter()
                .find(|tab| matches!(tab.body, ObjectBody::Structure { .. }))
                .expect("the structure tab is open");
            let ObjectBody::Structure { structure, .. } = &tab.body else {
                unreachable!()
            };
            match structure {
                crate::session::StructureState::Loaded(structure) => structure
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                crate::session::StructureState::Failed(error) => {
                    panic!("the structure never loaded: {error}")
                }
                crate::session::StructureState::Loading => {
                    panic!("the structure never loaded")
                }
            }
        });
        assert_eq!(columns, ["id"], "the column the editor removed is gone");
        assert_eq!(
            front_dialog(&workspace, cx),
            None,
            "a successful run closes the dialog"
        );

        quit(workspace, cx);
    });
}

/// The type cell of the table editor: a searchable picker over the engine's own
/// type names, with the parameter list in its own field, so `varchar(80)` is
/// picked and typed rather than spelled out from memory.
#[gpui::test]
fn a_column_type_is_picked_from_the_list_in_the_table_editor(cx: &mut TestAppContext) {
    store::tests::with_home(|| {
        let (workspace, cx) = launch_on_sqlite_holding(
            cx,
            "CREATE TABLE items (id integer NOT NULL PRIMARY KEY, note text);",
        );
        cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.edit_structure(
                    "main".into(),
                    "items".into(),
                    crate::db::RelationKind::Table,
                    window,
                    cx,
                );
            });
        });
        settle(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let header = cx
            .debug_bounds("structure-columns-header")
            .expect("the columns carry a header");
        // Each header cell sits over its column: the type's over the picker,
        // and the whole row above the fields rather than beside them.
        let header_type = cx
            .debug_bounds("structure-header-type")
            .expect("the type column is named");
        let picker = cx
            .debug_bounds("structure-type-0")
            .expect("the first column's picker is drawn");
        assert_eq!(header_type.origin.x, picker.origin.x);
        assert!(header.origin.y < picker.origin.y);
        // The nullable cell is the width of the box it holds, so the gap to
        // the default field is the row's own spacing and not an empty column.
        let nullable = cx
            .debug_bounds("structure-nullable-cell-0")
            .expect("the nullable cell is drawn");
        let default = cx
            .debug_bounds("structure-default-0")
            .expect("the default field is drawn");
        // At the default chrome size, which this test leaves as it is.
        assert_eq!(nullable.size.width, gpui::px(48.));
        assert_eq!(
            default.origin.x - (nullable.origin.x + nullable.size.width),
            gpui::px(crate::theme::layout::SPACE_XS),
            "nothing but the row's gap sits between the box and the default field"
        );

        // The picker of the second column, `note`, by the order the server
        // listed them.
        let picker = cx
            .debug_bounds("structure-type-1")
            .expect("the picker is drawn")
            .center();
        let types = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .profile()
                    .and_then(|profile| profile.session.structure_editor.as_ref())
                    .expect("the editor is open")
                    .edit(cx)
                    .columns
                    .iter()
                    .map(|column| column.data_type.clone())
                    .collect::<Vec<_>>()
            })
        };
        // As the server lists them; SQLite's catalog answers in upper case,
        // and a type the list does not name is kept as it is.
        assert_eq!(types(cx), ["INTEGER", "TEXT"], "the types as they load");

        cx.simulate_click(picker, gpui::Modifiers::default());
        settle(cx);
        // The menu is a deferred layer, drawn after the frame that opened it,
        // and its search field is what the keystrokes reach.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_input("int");
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(
            types(cx),
            ["INTEGER", "integer"],
            "the picked base name is the column's type, and the column before it is untouched"
        );

        // Again, with a type the parameter field stays empty for.
        let picker = cx
            .debug_bounds("structure-type-1")
            .expect("the picker is drawn")
            .center();
        cx.simulate_click(picker, gpui::Modifiers::default());
        settle(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_input("float");
        cx.simulate_keystrokes("enter");
        settle(cx);
        assert_eq!(types(cx), ["INTEGER", "float"]);

        // The type is the picked name plus the parameter list beside it: a
        // `varchar` and a `(80)` are the `varchar(80)` the server reads.
        let picker = cx
            .debug_bounds("structure-type-1")
            .expect("the picker is drawn")
            .center();
        cx.simulate_click(picker, gpui::Modifiers::default());
        settle(cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_input("varchar");
        cx.simulate_keystrokes("enter");
        settle(cx);
        let params = cx
            .debug_bounds("structure-params-1")
            .expect("the parameter field is drawn")
            .center();
        cx.simulate_click(params, gpui::Modifiers::default());
        settle(cx);
        cx.simulate_input("(80)");
        settle(cx);
        assert_eq!(types(cx), ["INTEGER", "varchar(80)"]);

        quit(workspace, cx);
    });
}
