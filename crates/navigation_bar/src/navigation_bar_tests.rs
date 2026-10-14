use std::sync::Arc;

use super::*;
use editor::Editor;
use gpui::{TestAppContext, VisualTestContext};
use menu::{Cancel, Confirm, SelectChild, SelectNext, SelectParent, SelectPrevious};
use pretty_assertions::assert_eq;
use project::Project;
use serde_json::json;
use util::{path, rel_path::rel_path};
use workspace::{AppState, Item, MultiWorkspace};

fn init_test(cx: &mut TestAppContext) -> Arc<AppState> {
    cx.update(|cx| {
        let state = AppState::test(cx);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        super::init(cx);
        editor::init(cx);
        state
    })
}

async fn build_workspace(
    cx: &mut TestAppContext,
) -> (Entity<Workspace>, WorktreeId, &mut VisualTestContext) {
    let app_state = init_test(cx);
    app_state
        .fs
        .as_fake()
        .insert_tree(
            path!("/root"),
            json!({
                "src": {
                    "components": {
                        "button.ts": "",
                        "input.ts": "",
                    },
                    "main.ts": "",
                },
                "a.json": "",
                "b.md": "",
            }),
        )
        .await;
    let project = Project::test(app_state.fs.clone(), [path!("/root").as_ref()], cx).await;
    let worktree_id = project.read_with(cx, |project, cx| {
        project
            .worktrees(cx)
            .next()
            .expect("project has a worktree")
            .read(cx)
            .id()
    });
    let (multi_workspace, cx) =
        cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
    let workspace =
        multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
    (workspace, worktree_id, cx)
}

async fn open_file(
    workspace: &Entity<Workspace>,
    worktree_id: WorktreeId,
    path: &str,
    cx: &mut VisualTestContext,
) {
    workspace
        .update_in(cx, |workspace, window, cx| {
            workspace.open_path(
                ProjectPath {
                    worktree_id,
                    path: rel_path(path).into_arc(),
                },
                None,
                true,
                window,
                cx,
            )
        })
        .await
        .expect("file opens");
}

fn active_navigation_bar(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
) -> Option<Entity<NavigationBar>> {
    workspace.update(cx, |workspace, cx| {
        workspace.active_modal::<NavigationBar>(cx)
    })
}

#[track_caller]
fn assert_state(
    workspace: &Entity<Workspace>,
    cx: &mut VisualTestContext,
    expected_entries: &[&str],
    expected_selection: &str,
    expected_breadcrumbs: &[&str],
) {
    let navigation_bar =
        active_navigation_bar(workspace, cx).expect("navigation bar should be open");
    navigation_bar.read_with(cx, |navigation_bar, cx| {
        let entries = navigation_bar
            .entries
            .iter()
            .map(|entry| entry.name.to_string())
            .collect::<Vec<_>>();
        let selection = navigation_bar
            .selected_entry()
            .map(|entry| entry.name.to_string());
        let breadcrumbs = navigation_bar
            .breadcrumb_segments(cx)
            .into_iter()
            .map(|(_, name)| name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(entries, expected_entries);
        assert_eq!(selection.as_deref(), Some(expected_selection));
        assert_eq!(breadcrumbs, expected_breadcrumbs);
    });
}

#[gpui::test]
async fn test_opens_at_active_file(cx: &mut TestAppContext) {
    let (workspace, worktree_id, cx) = build_workspace(cx).await;
    open_file(&workspace, worktree_id, "src/components/input.ts", cx).await;

    cx.dispatch_action(Toggle);
    assert_state(
        &workspace,
        cx,
        &["button.ts", "input.ts"],
        "input.ts",
        &["root", "src", "components", "input.ts"],
    );

    cx.dispatch_action(Cancel);
    assert!(active_navigation_bar(&workspace, cx).is_none());
}

#[gpui::test]
async fn test_opens_at_root_without_active_file(cx: &mut TestAppContext) {
    let (workspace, _, cx) = build_workspace(cx).await;

    cx.dispatch_action(Toggle);
    assert_state(
        &workspace,
        cx,
        &["src", "a.json", "b.md"],
        "src",
        &["root", "src"],
    );
}

#[gpui::test]
async fn test_moves_between_nesting_levels(cx: &mut TestAppContext) {
    let (workspace, worktree_id, cx) = build_workspace(cx).await;
    open_file(&workspace, worktree_id, "src/components/input.ts", cx).await;
    cx.dispatch_action(Toggle);

    cx.dispatch_action(SelectParent);
    assert_state(
        &workspace,
        cx,
        &["components", "main.ts"],
        "components",
        &["root", "src", "components"],
    );

    cx.dispatch_action(SelectParent);
    assert_state(
        &workspace,
        cx,
        &["src", "a.json", "b.md"],
        "src",
        &["root", "src"],
    );

    cx.dispatch_action(SelectParent);
    assert_state(&workspace, cx, &["root"], "root", &["root"]);

    cx.dispatch_action(SelectParent);
    assert_state(&workspace, cx, &["root"], "root", &["root"]);

    cx.dispatch_action(SelectChild);
    assert_state(
        &workspace,
        cx,
        &["src", "a.json", "b.md"],
        "src",
        &["root", "src"],
    );

    cx.dispatch_action(SelectChild);
    assert_state(
        &workspace,
        cx,
        &["components", "main.ts"],
        "components",
        &["root", "src", "components"],
    );
}

#[gpui::test]
async fn test_moves_between_siblings_and_opens_file(cx: &mut TestAppContext) {
    let (workspace, worktree_id, cx) = build_workspace(cx).await;
    open_file(&workspace, worktree_id, "src/components/input.ts", cx).await;
    cx.dispatch_action(Toggle);
    cx.dispatch_action(SelectParent);

    cx.dispatch_action(SelectNext);
    assert_state(
        &workspace,
        cx,
        &["components", "main.ts"],
        "main.ts",
        &["root", "src", "main.ts"],
    );

    cx.dispatch_action(SelectNext);
    assert_state(
        &workspace,
        cx,
        &["components", "main.ts"],
        "components",
        &["root", "src", "components"],
    );

    cx.dispatch_action(SelectPrevious);
    assert_state(
        &workspace,
        cx,
        &["components", "main.ts"],
        "main.ts",
        &["root", "src", "main.ts"],
    );

    cx.dispatch_action(SelectChild);
    assert_state(
        &workspace,
        cx,
        &["components", "main.ts"],
        "main.ts",
        &["root", "src", "main.ts"],
    );

    cx.dispatch_action(Confirm);
    cx.run_until_parked();
    assert!(active_navigation_bar(&workspace, cx).is_none());
    let active_title = workspace.read_with(cx, |workspace, cx| {
        workspace
            .active_item_as::<Editor>(cx)
            .map(|editor| editor.read(cx).tab_content_text(0, cx))
    });
    assert_eq!(active_title.as_deref(), Some("main.ts"));
}

#[gpui::test]
async fn test_confirm_on_directory_enters_it(cx: &mut TestAppContext) {
    let (workspace, worktree_id, cx) = build_workspace(cx).await;
    open_file(&workspace, worktree_id, "a.json", cx).await;
    cx.dispatch_action(Toggle);
    cx.dispatch_action(SelectPrevious);
    assert_state(
        &workspace,
        cx,
        &["src", "a.json", "b.md"],
        "src",
        &["root", "src"],
    );

    cx.dispatch_action(Confirm);
    assert_state(
        &workspace,
        cx,
        &["components", "main.ts"],
        "components",
        &["root", "src", "components"],
    );
}
