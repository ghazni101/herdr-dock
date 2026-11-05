//! Fork default layout for the docked sidebar.
//!
//! The docked sidebar is a pane labelled `herdr:sidebar` that the client paints
//! its sidebar over. Once any tab in the session carries that slot, spaces and
//! tabs created through the normal workspace, tab, and pane-move paths get one
//! too, so the sidebar does not vanish when the user creates a workspace or tab.
//! Creation paths that supply their own layout, such as `layout apply` and
//! plugin-opened tabs, keep exactly the panes they build. A session that never
//! docked (including the very first space, which the user's layout plugin
//! builds) is left untouched.

use ratatui::layout::Direction;

use super::App;
use crate::api::schema::{DOCKED_MAIN_PANE_LABEL, DOCKED_SIDEBAR_PANE_LABEL};
use crate::layout::{Node, PaneId};

/// Dock width used when no existing dock shows the width to inherit.
const DEFAULT_DOCK_COLUMNS: f32 = 26.0;

impl App {
    pub(super) fn pane_is_dock_slot(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.state.pane_is_dock_slot(ws_idx, pane_id)
    }

    /// Resolve a pane for an operation that did not name a target. A restored
    /// layout can leave server focus on the dock slot, so an implicit target
    /// falls back to the tab's primary user pane instead of the chrome slot.
    /// Explicit dock targets are rejected by their callers, never retargeted.
    pub(super) fn implicit_operation_pane(&self, ws_idx: usize, pane_id: PaneId) -> Option<PaneId> {
        if !self.pane_is_dock_slot(ws_idx, pane_id) {
            return Some(pane_id);
        }
        let tab_idx = self
            .state
            .workspaces
            .get(ws_idx)?
            .find_tab_index_for_pane(pane_id)?;
        self.primary_pane_in_tab(ws_idx, tab_idx)
    }

    /// The user's primary pane in a tab: the labelled `main` pane when a
    /// plugin supplied one, then the largest pane, then the first pane that is
    /// not a dock slot. The label is a hint, not a requirement.
    pub(super) fn primary_pane_in_tab(&self, ws_idx: usize, tab_idx: usize) -> Option<PaneId> {
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        let candidates = tab
            .layout
            .pane_ids()
            .into_iter()
            .filter(|pane_id| !self.pane_is_dock_slot(ws_idx, *pane_id))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        let labelled = candidates.iter().copied().find(|pane_id| {
            tab.terminal_id(*pane_id)
                .and_then(|terminal_id| self.state.terminals.get(terminal_id))
                .is_some_and(|terminal| {
                    terminal.manual_label.as_deref() == Some(DOCKED_MAIN_PANE_LABEL)
                })
        });
        if labelled.is_some() {
            return labelled;
        }
        tab.layout
            .panes(self.state.new_pane_area())
            .into_iter()
            .filter(|pane| candidates.contains(&pane.id))
            .max_by_key(|pane| u32::from(pane.rect.width) * u32::from(pane.rect.height))
            .map(|pane| pane.id)
            .or_else(|| candidates.first().copied())
    }

    /// Panes of the tab that are docked-sidebar slots.
    pub(super) fn tab_dock_slot_panes(&self, ws_idx: usize, tab_idx: usize) -> Vec<PaneId> {
        let Some(tab) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        else {
            return Vec::new();
        };
        tab.layout
            .pane_ids()
            .into_iter()
            .filter(|pane_id| self.pane_is_dock_slot(ws_idx, *pane_id))
            .collect()
    }

    /// Keep server focus on a user pane in every tab that still has one.
    ///
    /// The docked sidebar is client chrome, so focus must never rest on its
    /// slot: a third-party API client would read `focused: true` for chrome the
    /// user cannot type into, and the client fallback would have to mask it.
    /// Tabs that contain only slots keep their focus, so a tab never loses
    /// focus entirely. Focus moves without recording history, so closing the
    /// pane that had focus cannot return to the slot.
    pub(super) fn normalize_slot_focus(&mut self) -> bool {
        let mut changed = false;
        for ws_idx in 0..self.state.workspaces.len() {
            let tab_count = self.state.workspaces[ws_idx].tabs.len();
            for tab_idx in 0..tab_count {
                let focused = self.state.workspaces[ws_idx].tabs[tab_idx].layout.focused();
                if !self.state.pane_is_dock_slot(ws_idx, focused) {
                    continue;
                }
                let Some(target) = self.primary_pane_in_tab(ws_idx, tab_idx) else {
                    continue;
                };
                if let Some(tab) = self
                    .state
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|ws| ws.tabs.get_mut(tab_idx))
                {
                    tab.layout.focus_pane_without_history(target);
                    changed = true;
                }
            }
        }
        if changed {
            self.state.mark_session_dirty();
        }
        changed
    }

    /// Split ratio for a new dock, or None when the session has no dock yet.
    /// Inherits the width share of an existing left-hand dock, preferring the
    /// active space, so new tabs match the dock the user already sized.
    pub(super) fn session_dock_ratio(&self) -> Option<f32> {
        let mut found = false;
        let active = self.state.active;
        let order = active
            .into_iter()
            .chain((0..self.state.workspaces.len()).filter(|ws_idx| Some(*ws_idx) != active));
        for ws_idx in order {
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                continue;
            };
            for tab in &ws.tabs {
                for pane_id in tab.layout.pane_ids() {
                    if !self.pane_is_dock_slot(ws_idx, pane_id) {
                        continue;
                    }
                    found = true;
                    if let Some(ratio) = left_dock_ratio(tab.layout.root(), pane_id) {
                        return Some(ratio);
                    }
                }
            }
        }
        found.then(|| self.default_dock_ratio())
    }

    fn default_dock_ratio(&self) -> f32 {
        let width = f32::from(self.state.new_pane_area().width.max(1));
        (DEFAULT_DOCK_COLUMNS / width).clamp(0.12, 0.5)
    }

    /// Give a freshly created single-pane tab the docked-sidebar slot to the
    /// left of its pane when the session already uses the docked layout. The
    /// original pane keeps focus and stays the tab's root pane. Returns the
    /// dock pane when one was added.
    pub(super) fn apply_default_dock_slot(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<PaneId> {
        let ratio = self.session_dock_ratio()?;
        self.apply_default_dock_slot_with_ratio(ws_idx, tab_idx, ratio)
    }

    pub(super) fn apply_default_dock_slot_with_ratio(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
        ratio: f32,
    ) -> Option<PaneId> {
        let main = {
            let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
            let panes = tab.layout.pane_ids();
            if panes.len() != 1 {
                return None;
            }
            panes[0]
        };
        // The dock ends up first with `ratio`; it is created as the second
        // child of `main` and swapped, so size it as that slot's share.
        let (rows, cols) = self
            .state
            .new_pane_size(crate::ui::NewPanePlacement::Split {
                ws_idx,
                target: main,
                direction: Direction::Horizontal,
                ratio: 1.0 - ratio,
            });
        let cwd = self.launch_cwd_for_pane_in_workspace(ws_idx, main);
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.pane_host_terminal_appearance();
        let placeholder_argv = dock_slot_argv();
        let split = self
            .state
            .workspaces
            .get_mut(ws_idx)?
            .split_pane_argv_command_with_ratio(
                main,
                Direction::Horizontal,
                ratio,
                rows,
                cols,
                cwd,
                &placeholder_argv,
                Vec::new(),
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                false,
            );
        let new_pane = match split? {
            Ok((_, new_pane)) => new_pane,
            Err(err) => {
                tracing::warn!(err = %err, "failed to create the docked sidebar slot");
                return None;
            }
        };
        let dock = new_pane.pane_id;
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state.remove_alias_shadowed_by_new_pane(dock);
        let mut terminal = new_pane.terminal;
        terminal.set_manual_label(DOCKED_SIDEBAR_PANE_LABEL.to_string());
        self.state.terminals.insert(terminal.id.clone(), terminal);
        if let Some(tab) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
        {
            tab.layout.swap_panes(main, dock);
        }
        Some(dock)
    }
}

/// The dock slot is client chrome, not a terminal the user can reach: the
/// client only paints the sidebar over it. Spawn an inert placeholder so every
/// tab does not pay for a second interactive shell that can source rc files or
/// be detected as an agent.
pub(crate) fn dock_slot_argv() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "cmd.exe".to_string(),
            "/D".to_string(),
            "/C".to_string(),
            "pause >NUL".to_string(),
        ]
    }
    #[cfg(not(windows))]
    {
        vec!["sleep".to_string(), "2147483647".to_string()]
    }
}

/// The ratio of the horizontal split whose first child is `dock`, i.e. the
/// width share of a dock on the left edge of its split.
fn left_dock_ratio(node: &Node, dock: PaneId) -> Option<f32> {
    match node {
        Node::Pane(_) => None,
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            if *direction == Direction::Horizontal
                && matches!(**first, Node::Pane(id) if id == dock)
            {
                return Some(*ratio);
            }
            left_dock_ratio(first, dock).or_else(|| left_dock_ratio(second, dock))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::api::test_support::{exiting_test_command, shutdown_test_runtimes};
    use super::*;
    use crate::api::schema::{
        Method, Request, ResponseResult, SuccessResponse, TabCreateParams, WorkspaceCreateParams,
    };
    use crate::config::{Config, ShellModeConfig};
    use crate::workspace::Workspace;

    fn app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        app.state.workspaces = vec![Workspace::test_new("dock")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();
        app
    }

    /// Dock the first space the way the layout plugin does: split the main
    /// pane, swap the new pane to the left, and label it as the slot.
    fn dock_first_space(app: &mut App, ratio: f32) -> PaneId {
        let main = app.state.workspaces[0].tabs[0].root_pane;
        let dock = app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.ensure_test_terminals();
        let tab = &mut app.state.workspaces[0].tabs[0];
        tab.layout.set_ratio_at(&[], ratio);
        tab.layout.swap_panes(main, dock);
        tab.layout.focus_pane(main);
        let terminal_id = tab.terminal_id(dock).cloned().unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label(DOCKED_SIDEBAR_PANE_LABEL.into());
        dock
    }

    /// The tab's panes as (is dock, is focused), left to right, plus the
    /// dock's split ratio.
    fn tab_shape(app: &App, ws_idx: usize, tab_idx: usize) -> (Vec<(bool, bool)>, Option<f32>) {
        let tab = &app.state.workspaces[ws_idx].tabs[tab_idx];
        let focused = tab.layout.focused();
        let panes = tab
            .layout
            .pane_ids()
            .into_iter()
            .map(|pane_id| (app.pane_is_dock_slot(ws_idx, pane_id), pane_id == focused))
            .collect::<Vec<_>>();
        let ratio = tab
            .layout
            .pane_ids()
            .into_iter()
            .find(|pane_id| app.pane_is_dock_slot(ws_idx, *pane_id))
            .and_then(|dock| left_dock_ratio(tab.layout.root(), dock));
        (panes, ratio)
    }

    #[cfg(unix)]
    fn dock_launch_argv(app: &App, ws_idx: usize, tab_idx: usize) -> Option<Vec<String>> {
        let tab = &app.state.workspaces[ws_idx].tabs[tab_idx];
        let dock = tab
            .layout
            .pane_ids()
            .into_iter()
            .find(|pane_id| app.pane_is_dock_slot(ws_idx, *pane_id))?;
        let terminal_id = tab.terminal_id(dock)?;
        app.state.terminals.get(terminal_id)?.launch_argv.clone()
    }

    fn create_tab(app: &mut App) -> usize {
        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: Method::TabCreate(TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: true,
                label: None,
                env: Default::default(),
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::TabCreated { root_pane, .. } = success.result else {
            panic!("expected a created tab");
        };
        let tab_idx = app.state.workspaces[0].tabs.len() - 1;
        let tab = &app.state.workspaces[0].tabs[tab_idx];
        // The reported root pane stays the working pane, not the dock.
        assert_eq!(
            app.public_pane_id(0, tab.root_pane),
            Some(root_pane.pane_id)
        );
        tab_idx
    }

    fn create_workspace(app: &mut App) -> usize {
        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: Method::WorkspaceCreate(WorkspaceCreateParams {
                cwd: Some(std::env::temp_dir().display().to_string()),
                focus: true,
                label: None,
                source_workspace_id: None,
                env: Default::default(),
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceCreated { root_pane, .. } = success.result else {
            panic!("expected a created workspace");
        };
        let ws_idx = app.state.workspaces.len() - 1;
        let tab = &app.state.workspaces[ws_idx].tabs[0];
        assert_eq!(
            app.public_pane_id(ws_idx, tab.root_pane),
            Some(root_pane.pane_id)
        );
        ws_idx
    }

    /// A read-only request: it must not touch focus or dirty the session.
    fn read_only_request(app: &mut App) -> String {
        app.handle_api_request(Request {
            id: "read".into(),
            method: Method::PaneList(crate::api::schema::PaneListParams::default()),
        })
    }

    #[test]
    fn explicit_focus_refuses_the_dock_slot() {
        let mut app = app();
        let main = app.state.workspaces[0].tabs[0].root_pane;
        let dock = dock_first_space(&mut app, 0.2);

        assert!(!app.state.focus_pane_in_workspace(0, dock));

        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), main);
    }

    #[test]
    fn api_dispatch_normalizes_focus_left_on_a_slot() {
        let mut app = app();
        let main = app.state.workspaces[0].tabs[0].root_pane;
        let dock = dock_first_space(&mut app, 0.2);
        app.state.workspaces[0].tabs[0].layout.focus_pane(dock);
        app.state.session_dirty = false;

        let response = read_only_request(&mut app);

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::PaneList { .. }));
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), main);
        assert!(app.state.session_dirty, "a focus correction must persist");
    }

    #[test]
    fn a_slot_only_tab_keeps_its_focus() {
        let mut app = app();
        let only = app.state.workspaces[0].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        let terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(only)
            .cloned()
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label(crate::api::schema::DOCKED_SIDEBAR_PANE_LABEL.into());
        app.state.session_dirty = false;

        read_only_request(&mut app);

        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), only);
        assert!(!app.state.session_dirty);
    }

    #[test]
    fn api_dispatch_does_not_dirty_a_docked_session_without_a_focus_change() {
        let mut app = app();
        dock_first_space(&mut app, 0.2);
        app.state.session_dirty = false;

        read_only_request(&mut app);

        assert!(!app.state.session_dirty);
    }

    #[tokio::test]
    async fn new_tab_inherits_the_docked_sidebar_slot() {
        let mut app = app();
        dock_first_space(&mut app, 0.2);

        let tab_idx = create_tab(&mut app);

        let (panes, ratio) = tab_shape(&app, 0, tab_idx);
        assert_eq!(panes, vec![(true, false), (false, true)]);
        assert_eq!(ratio, Some(0.2));
        shutdown_test_runtimes(&mut app);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_tab_dock_slot_runs_a_placeholder_not_a_shell() {
        let mut app = app();
        dock_first_space(&mut app, 0.2);

        let tab_idx = create_tab(&mut app);
        let workspace_idx = create_workspace(&mut app);

        let tab_argv = dock_launch_argv(&app, 0, tab_idx).expect("dock slot should run a command");
        assert_eq!(tab_argv.first().map(String::as_str), Some("sleep"));
        let workspace_argv =
            dock_launch_argv(&app, workspace_idx, 0).expect("dock slot should run a command");
        assert_eq!(workspace_argv.first().map(String::as_str), Some("sleep"));
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn new_workspace_inherits_the_docked_sidebar_slot() {
        let mut app = app();
        dock_first_space(&mut app, 0.25);

        let ws_idx = create_workspace(&mut app);

        let (panes, ratio) = tab_shape(&app, ws_idx, 0);
        assert_eq!(panes, vec![(true, false), (false, true)]);
        assert_eq!(ratio, Some(0.25));
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn sessions_without_a_dock_keep_single_pane_tabs_and_spaces() {
        let mut app = app();

        let tab_idx = create_tab(&mut app);
        let ws_idx = create_workspace(&mut app);

        assert_eq!(tab_shape(&app, 0, tab_idx).0, vec![(false, true)]);
        assert_eq!(tab_shape(&app, ws_idx, 0).0, vec![(false, true)]);
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn first_default_space_is_left_for_the_layout_plugin() {
        let mut app = app();
        app.state.workspaces.clear();
        app.state.active = None;

        assert!(app.ensure_default_workspace());

        assert_eq!(tab_shape(&app, 0, 0).0, vec![(false, true)]);
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn new_tab_announces_the_dock_pane() {
        let mut app = app();
        dock_first_space(&mut app, 0.2);
        let sequence = app.event_hub.current_sequence();

        let tab_idx = create_tab(&mut app);

        let dock = app.state.workspaces[0].tabs[tab_idx]
            .layout
            .pane_ids()
            .into_iter()
            .find(|pane_id| app.pane_is_dock_slot(0, *pane_id))
            .and_then(|pane_id| app.public_pane_id(0, pane_id))
            .unwrap();
        let announced = app
            .event_hub
            .events_after(sequence)
            .into_iter()
            .filter_map(|(_, event)| match event.data {
                crate::api::schema::EventData::PaneCreated { pane } => Some(pane.pane_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            announced.contains(&dock),
            "{announced:?} should include {dock}"
        );
        shutdown_test_runtimes(&mut app);
    }
}
