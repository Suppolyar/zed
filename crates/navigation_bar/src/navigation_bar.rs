#[cfg(test)]
mod navigation_bar_tests;

use std::{cmp::Ordering, path::Path, sync::Arc};

use file_icons::FileIcons;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, ListSizingBehavior,
    ScrollStrategy, Subscription, Task, UniformListScrollHandle, WeakEntity, Window, actions,
    uniform_list,
};
use project::{EntryKind, Project, ProjectEntryId, ProjectPath, WorktreeId};
use project_panel::project_panel_settings::ProjectPanelSettings;
use settings::Settings;
use ui::{ListItem, ListItemSpacing, prelude::*};
use util::{ResultExt, paths::compare_rel_paths_by, rel_path::RelPath};
use workspace::{ModalView, Workspace};

actions!(
    navigation_bar,
    [
        /// Opens the navigation bar at the active file. Use left and right to move
        /// between nesting levels and up and down to move between siblings.
        Toggle
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(NavigationBar::register).detach();
}

#[derive(Clone)]
struct NavigationEntry {
    worktree_id: WorktreeId,
    entry_id: ProjectEntryId,
    path: Arc<RelPath>,
    name: SharedString,
    kind: EntryKind,
    is_ignored: bool,
}

impl NavigationEntry {
    fn is_dir(&self) -> bool {
        !matches!(self.kind, EntryKind::File)
    }
}

pub struct NavigationBar {
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
    /// The directory whose children are listed, or `None` for the list of worktree roots.
    location: Option<(WorktreeId, Arc<RelPath>)>,
    entries: Vec<NavigationEntry>,
    selected_index: usize,
    expand_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for NavigationBar {}
impl ModalView for NavigationBar {}

impl Focusable for NavigationBar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl NavigationBar {
    fn register(
        workspace: &mut Workspace,
        _window: Option<&mut Window>,
        _: &mut Context<Workspace>,
    ) {
        workspace.register_action(|workspace, _: &Toggle, window, cx| {
            let project = workspace.project().clone();
            let active_path = workspace
                .active_item(cx)
                .and_then(|item| item.project_path(cx));
            let workspace_handle = cx.entity().downgrade();
            workspace.toggle_modal(window, cx, move |window, cx| {
                Self::new(workspace_handle, project, active_path, window, cx)
            });
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        active_path: Option<ProjectPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&project, |this, _, event, cx| {
            if let project::Event::WorktreeUpdatedEntries(worktree_id, _)
            | project::Event::WorktreeRemoved(worktree_id) = event
                && this.is_showing_worktree(*worktree_id)
            {
                this.reload_preserving_selection(cx);
            }
        });

        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let mut this = Self {
            workspace,
            project,
            focus_handle,
            scroll_handle: UniformListScrollHandle::new(),
            location: None,
            entries: Vec::new(),
            selected_index: 0,
            expand_task: None,
            _subscriptions: vec![subscription],
        };

        if let Some(active_path) = active_path {
            this.reveal(active_path, cx);
        } else {
            let single_worktree_id = {
                let mut visible_worktrees = this.project.read(cx).visible_worktrees(cx);
                match (visible_worktrees.next(), visible_worktrees.next()) {
                    (Some(worktree), None) => Some(worktree.read(cx).id()),
                    _ => None,
                }
            };
            let location =
                single_worktree_id.map(|worktree_id| (worktree_id, RelPath::empty_arc()));
            this.show_location(location, None, cx);
        }
        this
    }

    fn is_showing_worktree(&self, worktree_id: WorktreeId) -> bool {
        match &self.location {
            Some((location_worktree_id, _)) => *location_worktree_id == worktree_id,
            None => true,
        }
    }

    /// Lists the parent directory of `project_path` with `project_path` selected.
    fn reveal(&mut self, project_path: ProjectPath, cx: &mut Context<Self>) {
        let location = project_path
            .path
            .parent()
            .map(|parent| (project_path.worktree_id, parent.into_arc()));
        self.show_location(location, Some(project_path), cx);
    }

    fn show_location(
        &mut self,
        location: Option<(WorktreeId, Arc<RelPath>)>,
        selection: Option<ProjectPath>,
        cx: &mut Context<Self>,
    ) {
        self.location = location;
        self.entries = self.load_entries(cx);
        self.selected_index = selection
            .and_then(|selection| {
                self.entries.iter().position(|entry| {
                    entry.worktree_id == selection.worktree_id && entry.path == selection.path
                })
            })
            .unwrap_or(0);
        self.scroll_to_selection();
        cx.notify();
    }

    fn reload_preserving_selection(&mut self, cx: &mut Context<Self>) {
        let selection = self.selected_entry().map(|entry| ProjectPath {
            worktree_id: entry.worktree_id,
            path: entry.path.clone(),
        });
        let location = self.location.clone();
        self.show_location(location, selection, cx);
    }

    fn load_entries(&self, cx: &App) -> Vec<NavigationEntry> {
        let project = self.project.read(cx);
        let Some((worktree_id, directory)) = &self.location else {
            return project
                .visible_worktrees(cx)
                .filter_map(|worktree| {
                    let worktree = worktree.read(cx);
                    let root_entry = worktree.root_entry()?;
                    Some(NavigationEntry {
                        worktree_id: worktree.id(),
                        entry_id: root_entry.id,
                        path: root_entry.path.clone(),
                        name: SharedString::from(worktree.root_name_str().to_string()),
                        kind: root_entry.kind,
                        is_ignored: false,
                    })
                })
                .collect();
        };

        let Some(worktree) = project.worktree_for_id(*worktree_id, cx) else {
            return Vec::new();
        };
        let worktree = worktree.read(cx);
        let settings = ProjectPanelSettings::get_global(cx);
        let mut children = worktree
            .child_entries(directory)
            .filter(|entry| !(settings.hide_hidden && entry.is_hidden))
            .filter(|entry| !(settings.hide_gitignore && entry.is_ignored))
            .filter_map(|entry| {
                Some(NavigationEntry {
                    worktree_id: *worktree_id,
                    entry_id: entry.id,
                    path: entry.path.clone(),
                    name: SharedString::from(entry.path.file_name()?.to_string()),
                    kind: entry.kind,
                    is_ignored: entry.is_ignored,
                })
            })
            .collect::<Vec<_>>();
        children.sort_by(|left, right| -> Ordering {
            compare_rel_paths_by(
                (&left.path, !left.is_dir()),
                (&right.path, !right.is_dir()),
                settings.sort_mode.into(),
                settings.sort_order.into(),
            )
        });
        children
    }

    fn selected_entry(&self) -> Option<&NavigationEntry> {
        self.entries.get(self.selected_index)
    }

    fn scroll_to_selection(&self) {
        self.scroll_handle
            .scroll_to_item(self.selected_index, ScrollStrategy::Nearest);
    }

    fn select_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.entries.len() {
            self.selected_index = index;
            self.scroll_to_selection();
            cx.notify();
        }
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if !self.entries.is_empty() {
            self.select_index((self.selected_index + 1) % self.entries.len(), cx);
        }
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.entries.is_empty() {
            let index = self
                .selected_index
                .checked_sub(1)
                .unwrap_or(self.entries.len() - 1);
            self.select_index(index, cx);
        }
    }

    fn select_first(&mut self, _: &menu::SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.select_index(0, cx);
    }

    fn select_last(&mut self, _: &menu::SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.select_index(self.entries.len().saturating_sub(1), cx);
    }

    fn select_parent(&mut self, _: &menu::SelectParent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((worktree_id, directory)) = self.location.clone() else {
            return;
        };
        self.reveal(
            ProjectPath {
                worktree_id,
                path: directory,
            },
            cx,
        );
    }

    fn select_child(&mut self, _: &menu::SelectChild, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry().cloned()
            && entry.is_dir()
        {
            self.enter_directory(entry, cx);
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry().cloned() {
            self.activate_entry(entry, window, cx);
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn activate_entry(
        &mut self,
        entry: NavigationEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entry.is_dir() {
            self.enter_directory(entry, cx);
        } else {
            self.open_file(entry, window, cx);
        }
    }

    fn enter_directory(&mut self, entry: NavigationEntry, cx: &mut Context<Self>) {
        // Ignored directories are not scanned until something asks for their contents.
        if entry.kind == EntryKind::UnloadedDir {
            let expand_task = self.project.update(cx, |project, cx| {
                project.expand_entry(entry.worktree_id, entry.entry_id, cx)
            });
            if let Some(expand_task) = expand_task {
                self.expand_task = Some(cx.spawn(async move |this, cx| {
                    if expand_task.await.log_err().is_some() {
                        this.update(cx, |this, cx| this.reload_preserving_selection(cx))
                            .log_err();
                    }
                }));
            }
        }
        self.show_location(Some((entry.worktree_id, entry.path)), None, cx);
    }

    fn open_file(&mut self, entry: NavigationEntry, window: &mut Window, cx: &mut Context<Self>) {
        let project_path = ProjectPath {
            worktree_id: entry.worktree_id,
            path: entry.path,
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        cx.emit(DismissEvent);
        workspace.update(cx, |workspace, cx| {
            workspace
                .open_path(project_path, None, true, window, cx)
                .detach_and_log_err(cx);
        });
    }

    /// The path segments shown above the list: the worktree root, each directory
    /// leading to the selection, and the selection itself.
    fn breadcrumb_segments(&self, cx: &App) -> Vec<(ProjectPath, SharedString)> {
        let focused_path = match (self.selected_entry(), &self.location) {
            (Some(entry), _) => ProjectPath {
                worktree_id: entry.worktree_id,
                path: entry.path.clone(),
            },
            (None, Some((worktree_id, directory))) => ProjectPath {
                worktree_id: *worktree_id,
                path: directory.clone(),
            },
            (None, None) => return Vec::new(),
        };
        let Some(worktree) = self
            .project
            .read(cx)
            .worktree_for_id(focused_path.worktree_id, cx)
        else {
            return Vec::new();
        };
        let root_name = SharedString::from(worktree.read(cx).root_name_str().to_string());

        let mut segments = Vec::new();
        let mut ancestor = Some(focused_path.path.as_ref());
        while let Some(path) = ancestor {
            let name = path
                .file_name()
                .map(|name| SharedString::from(name.to_string()))
                .unwrap_or_else(|| root_name.clone());
            segments.push((
                ProjectPath {
                    worktree_id: focused_path.worktree_id,
                    path: path.into_arc(),
                },
                name,
            ));
            ancestor = path.parent();
        }
        segments.reverse();
        segments
    }

    fn render_breadcrumbs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let segments = self.breadcrumb_segments(cx);
        let last_index = segments.len().saturating_sub(1);
        h_flex()
            .id("navigation-bar-breadcrumbs")
            .w_full()
            .px_2()
            .py_1p5()
            .gap_0p5()
            .overflow_x_scroll()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .children(
                segments
                    .into_iter()
                    .enumerate()
                    .map(|(index, (project_path, name))| {
                        let is_last = index == last_index;
                        h_flex()
                            .gap_0p5()
                            .flex_none()
                            .when(index > 0, |this| {
                                this.child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                            })
                            .child(
                                div()
                                    .id(("navigation-bar-segment", index))
                                    .px_1()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .hover(|style| {
                                        style.bg(cx.theme().colors().element_hover)
                                    })
                                    .when(is_last, |this| {
                                        this.bg(cx.theme().colors().element_selected)
                                    })
                                    .child(Label::new(name).size(LabelSize::Small).color(
                                        if is_last {
                                            Color::Default
                                        } else {
                                            Color::Muted
                                        },
                                    ))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.reveal(project_path.clone(), cx);
                                    })),
                            )
                    }),
            )
    }

    fn render_entry(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.entries.get(index)?;
        let name_path = Path::new(entry.name.as_ref());
        let icon = if entry.is_dir() {
            FileIcons::get_folder_icon(false, name_path, cx)
        } else {
            FileIcons::get_icon(name_path, cx)
        };
        let entry_to_activate = entry.clone();
        Some(
            ListItem::new(index)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(index == self.selected_index)
                .start_slot::<Icon>(
                    icon.map(|icon| Icon::from_path(icon).color(Color::Muted)),
                )
                .child(Label::new(entry.name.clone()).color(if entry.is_ignored {
                    Color::Muted
                } else {
                    Color::Default
                }))
                .end_slot::<Icon>(entry.is_dir().then(|| {
                    Icon::new(IconName::ChevronRight)
                        .size(IconSize::Small)
                        .color(Color::Muted)
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected_index = index;
                    this.activate_entry(entry_to_activate.clone(), window, cx);
                }))
                .into_any_element(),
        )
    }
}

impl Render for NavigationBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("NavigationBar menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::select_parent))
            .on_action(cx.listener(Self::select_child))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .elevation_3(cx)
            .w(rems(34.))
            .overflow_hidden()
            .child(self.render_breadcrumbs(cx))
            .child(if self.entries.is_empty() {
                div()
                    .p_2()
                    .child(
                        Label::new("Empty directory")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .max_h(rems(24.))
                    .child(
                        uniform_list(
                            "navigation-bar-entries",
                            self.entries.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .filter_map(|index| this.render_entry(index, cx))
                                    .collect()
                            }),
                        )
                        .with_sizing_behavior(ListSizingBehavior::Infer)
                        .flex_grow_1()
                        .py_1()
                        .track_scroll(&self.scroll_handle),
                    )
                    .into_any_element()
            })
    }
}
