//! Fork default layout for the docked sidebar.
//!
//! The docked sidebar is a pane labelled `herdr:sidebar` that the client paints
//! its sidebar over. Once any tab in the session carries that slot, every new
//! space and tab gets one too, so the sidebar does not vanish when the user
//! creates a workspace or tab. A session that never docked (including the very
//! first space, which the user's layout plugin builds) is left untouched.

use ratatui::layout::Direction;

use super::App;
use crate::api::schema::DOCKED_SIDEBAR_PANE_LABEL;
use crate::layout::{Node, PaneId};

/// Dock width used when no existing dock shows the width to inherit.
const DEFAULT_DOCK_COLUMNS: f32 = 26.0;

impl App {
    fn pane_is_dock_slot(&self, ws_idx: usize, pane_id: PaneId) -> bool {
        self.state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
            .is_some_and(|terminal| {
                terminal.manual_label.as_deref() == Some(DOCKED_SIDEBAR_PANE_LABEL)
            })
    }

    /// Split ratio for a new dock, or None when the session has no dock yet.
    /// Inherits the width share of an existing left-hand dock, preferring the
    /// active space, so new tabs match the dock the user already sized.
    fn session_dock_ratio(&self) -> Option<f32> {
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
        let main = {
            let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
            let panes = tab.layout.pane_ids();
            if panes.len() != 1 {
                return None;
            }
            panes[0]
        };
        if self.pane_is_dock_slot(ws_idx, main) {
            return None;
        }
        let ratio = self.session_dock_ratio()?;
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
