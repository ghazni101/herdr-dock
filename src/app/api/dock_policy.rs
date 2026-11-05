//! Fork-owned dock policy for the pane-facing API surface.
//!
//! The docked sidebar is an ordinary pane whose identity is a mutable label,
//! so every API operation that can name a pane has to decide what it does with
//! that slot. The table here is exhaustive over `pane.*`, `command.invoke`,
//! `plugin.pane_*` and `layout.*`: adding an operation here or upstream fails
//! [`every_pane_operation_has_a_dock_policy`] until someone writes its policy
//! down, instead of the next leak reaching users.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::api::schema::{
    ErrorResponse, Method, PaneListParams, Request, ResponseResult, SuccessResponse,
};
use crate::app::App;
use crate::config::{Config, ShellModeConfig};
use crate::layout::PaneId;
use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockPolicy {
    /// The slot is not a valid target; the request fails without acting.
    Reject,
    /// The operation resolves against the tab's user pane, or it is a no-op
    /// for the slot so focus never rests on it.
    Retarget,
    /// The operation is served for the slot itself: read-only queries, terminal
    /// I/O, or dock management such as closing, resizing, and renaming to the
    /// label.
    Allow,
}

struct Row {
    method: &'static str,
    policy: DockPolicy,
    /// The operation may change the tab's pane set.
    changes_layout: bool,
    /// A dock-targeted rejection should name the sidebar slot.
    names_sidebar: bool,
}

const fn row(
    method: &'static str,
    policy: DockPolicy,
    changes_layout: bool,
    names_sidebar: bool,
) -> Row {
    Row {
        method,
        policy,
        changes_layout,
        names_sidebar,
    }
}

/// Every operation that can name a pane, classified. `pane.wait_for_output` is
/// served by the API server's wait loop rather than the app dispatch, so it is
/// classified here but not dispatched by the table test.
const PANE_POLICY: &[Row] = &[
    row("pane.split", DockPolicy::Reject, false, true),
    row("pane.swap", DockPolicy::Reject, false, true),
    row("pane.move", DockPolicy::Reject, false, true),
    row("pane.zoom", DockPolicy::Reject, false, true),
    row("pane.layout", DockPolicy::Allow, false, false),
    row("pane.process_info", DockPolicy::Allow, false, false),
    row("pane.neighbor", DockPolicy::Allow, false, false),
    row("pane.edges", DockPolicy::Allow, false, false),
    row("pane.focus_direction", DockPolicy::Retarget, false, false),
    row("pane.resize", DockPolicy::Allow, false, false),
    row("pane.scroll", DockPolicy::Allow, false, false),
    row("pane.clear", DockPolicy::Allow, false, false),
    row("pane.edit_scrollback", DockPolicy::Reject, false, true),
    row("pane.selection.read", DockPolicy::Allow, false, false),
    row("pane.copy_motion", DockPolicy::Allow, false, false),
    row("pane.copy_search", DockPolicy::Allow, false, false),
    row("pane.list", DockPolicy::Allow, false, false),
    row("pane.current", DockPolicy::Allow, false, false),
    row("pane.get", DockPolicy::Allow, false, false),
    row("pane.focus", DockPolicy::Reject, false, true),
    row("pane.input.set", DockPolicy::Allow, false, false),
    row("pane.link.activate", DockPolicy::Allow, false, false),
    row("pane.link.resolve", DockPolicy::Allow, false, false),
    // Renaming *to* the label is the plugin's docking API today. The closure
    // of both rename directions ships with the atomic docking API and the
    // plugin switch, not with this table.
    row("pane.rename", DockPolicy::Allow, false, false),
    row("pane.send_text", DockPolicy::Allow, false, false),
    row("pane.send_keys", DockPolicy::Allow, false, false),
    row("pane.send_input", DockPolicy::Allow, false, false),
    row("pane.read", DockPolicy::Allow, false, false),
    row("pane.report_agent", DockPolicy::Allow, false, false),
    row("pane.report_agent_session", DockPolicy::Allow, false, false),
    row("pane.report_metadata", DockPolicy::Allow, false, false),
    row(
        "pane.clear_agent_authority",
        DockPolicy::Allow,
        false,
        false,
    ),
    row("pane.release_agent", DockPolicy::Allow, false, false),
    row("pane.close", DockPolicy::Allow, true, false),
    row("pane.wait_for_output", DockPolicy::Allow, false, false),
    row("command.invoke", DockPolicy::Reject, false, true),
    row("plugin.pane.open", DockPolicy::Reject, false, true),
    // The fixture records the dock as a plugin pane, so these reach the
    // handler body: focus never lands on the slot, and close is the undock path.
    row("plugin.pane.focus", DockPolicy::Retarget, false, false),
    row("plugin.pane.close", DockPolicy::Allow, true, false),
    row("layout.export", DockPolicy::Allow, false, false),
    row("layout.apply", DockPolicy::Allow, true, false),
    row("layout.set_split_ratio", DockPolicy::Allow, false, false),
];

fn classified(method: &str) -> bool {
    method.starts_with("pane.")
        || method == "command.invoke"
        || method.starts_with("plugin.pane.")
        || method.starts_with("layout.")
}

#[test]
fn every_pane_operation_has_a_dock_policy() {
    let table: BTreeSet<_> = PANE_POLICY.iter().map(|row| row.method).collect();
    assert_eq!(
        table.len(),
        PANE_POLICY.len(),
        "a dock policy row is duplicated"
    );
    let operations: BTreeSet<_> = Method::OPERATIONS
        .iter()
        .map(|operation| operation.name)
        .filter(|name| classified(name))
        .collect();
    let missing = operations.difference(&table).copied().collect::<Vec<_>>();
    let stale = table.difference(&operations).copied().collect::<Vec<_>>();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "dock policy table is out of date: missing {missing:?}, unknown {stale:?}"
    );
}

#[test]
fn every_dock_policy_is_represented() {
    for policy in [DockPolicy::Reject, DockPolicy::Retarget, DockPolicy::Allow] {
        assert!(
            PANE_POLICY.iter().any(|row| row.policy == policy),
            "no operation uses the {policy:?} policy"
        );
    }
}

struct Fixture {
    app: App,
    ids: Ids,
    dock: PaneId,
    _receivers: Vec<tokio::sync::mpsc::Receiver<bytes::Bytes>>,
}

struct Ids {
    tab: String,
    dock: String,
    main: String,
    command: String,
}

fn docked_fixture() -> Fixture {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.default_shell = crate::app::api::test_support::exiting_test_command().into();
    app.state.shell_mode = ShellModeConfig::NonLogin;
    app.state.workspaces = vec![Workspace::test_new("dock-policy")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.ensure_test_terminals();
    let main = app.state.workspaces[0].tabs[0].root_pane;
    let dock = crate::app::api::test_support::dock_tab(&mut app, 0, 0);

    // A screen fixture keeps the read/copy/link operations meaningful, and the
    // live channel keeps send operations from failing on a closed pipe.
    let mut receivers = Vec::new();
    for pane_id in [main, dock] {
        let (runtime, receiver) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"dock policy\r\n",
                8,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        receivers.push(receiver);
    }

    // `command.invoke` must resolve a real command so the dock-target guard is
    // reached instead of failing at command lookup.
    app.endpoint_commands = crate::app::custom_commands::EndpointCommandRegistry::new(&[
        crate::config::CustomCommandKeybind {
            bindings: crate::config::ActionKeybinds::prefix("z"),
            label: "prefix+z".into(),
            command: crate::app::api::test_support::exiting_test_command().into(),
            action: crate::config::CustomCommandAction::Shell,
            description: None,
            width: None,
            height: None,
        },
    ]);
    let command = app.client_shell_command_manifest()[0].command_id.clone();
    // A registered plugin lets `plugin.pane.open` reach the explicit dock
    // split-target guard instead of stopping at plugin lookup.
    app.state.installed_plugins.insert(
        "dock-policy".into(),
        crate::api::schema::InstalledPluginInfo {
            plugin_id: "dock-policy".into(),
            name: "Dock policy".into(),
            version: "0.0.0".into(),
            min_herdr_version: String::new(),
            description: None,
            manifest_path: "/tmp/dock-policy/plugin.toml".into(),
            plugin_root: "/tmp/dock-policy".into(),
            enabled: true,
            platforms: None,
            build: Vec::new(),
            startup: Vec::new(),
            actions: Vec::new(),
            events: Vec::new(),
            panes: vec![crate::api::schema::PluginManifestPane {
                id: "dock".into(),
                title: "Dock".into(),
                description: None,
                platforms: None,
                placement: crate::api::schema::PluginPanePlacement::Split,
                width: None,
                height: None,
                command: vec![crate::app::api::test_support::exiting_test_command().into()],
            }],
            link_handlers: Vec::new(),
            source: Default::default(),
            warnings: Vec::new(),
        },
    );
    // Recording the dock as a plugin pane lets `plugin.pane.focus` and
    // `plugin.pane.close` reach their handler bodies instead of the
    // plugin-pane identity guard.
    app.state.plugin_panes.insert(
        dock,
        crate::app::state::PluginPaneRecord {
            plugin_id: "dock-policy".into(),
            entrypoint: "dock".into(),
        },
    );

    let ids = Ids {
        tab: app.public_tab_id(0, 0).unwrap(),
        dock: app.public_pane_id(0, dock).unwrap(),
        main: app.public_pane_id(0, main).unwrap(),
        command,
    };
    Fixture {
        app,
        ids,
        dock,
        _receivers: receivers,
    }
}

fn params_for(method: &str, app: &App, ids: &Ids, dock: PaneId) -> Option<Value> {
    let params = match method {
        "pane.split" => json!({"target_pane_id": ids.dock, "direction": "right"}),
        "pane.swap" => json!({"source_pane_id": ids.dock, "target_pane_id": ids.main}),
        "pane.move" => json!({"pane_id": ids.dock, "destination": {"type": "new_tab"}}),
        "pane.zoom" => json!({"pane_id": ids.dock}),
        "pane.layout" => json!({"pane_id": ids.dock}),
        "pane.process_info" => json!({"pane_id": ids.dock}),
        "pane.neighbor" => json!({"pane_id": ids.dock, "direction": "right"}),
        "pane.edges" => json!({"pane_id": ids.dock}),
        "pane.focus_direction" => json!({"pane_id": ids.dock, "direction": "right"}),
        "pane.resize" => json!({"pane_id": ids.dock, "direction": "left", "amount": 0.05}),
        "pane.scroll" => json!({"pane_id": ids.dock, "offset_from_bottom": 0}),
        "pane.clear" => json!({"pane_id": ids.dock}),
        "pane.edit_scrollback" => json!({"pane_id": ids.dock}),
        "pane.selection.read" => json!({
            "pane_id": ids.dock,
            "anchor": {"row": 0, "col": 0},
            "cursor": {"row": 0, "col": 4}
        }),
        "pane.copy_motion" => json!({
            "pane_id": ids.dock,
            "cursor": {"row": 0, "col": 0},
            "motion": "line_end"
        }),
        "pane.copy_search" => json!({
            "pane_id": ids.dock,
            "query": "dock",
            "direction": "forward",
            "cursor": {"row": 0, "col": 0},
            "content_revision": app
                .state
                .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, dock)
                .map(|runtime| runtime.content_seq())
                .unwrap_or(0)
        }),
        "pane.list" => serde_json::to_value(PaneListParams::default()).unwrap(),
        "pane.current" => json!({"caller_pane_id": ids.dock}),
        "pane.get" => json!({"pane_id": ids.dock}),
        "pane.focus" => json!({"pane_id": ids.dock}),
        "pane.input.set" => json!({"pane_id": ids.dock, "right_click": "pane"}),
        "pane.link.activate" => json!({"pane_id": ids.dock, "viewport_row": 0, "col": 0}),
        "pane.link.resolve" => json!({"pane_id": ids.dock, "viewport_row": 0, "col": 0}),
        "pane.rename" => json!({
            "pane_id": ids.dock,
            "label": crate::api::schema::DOCKED_SIDEBAR_PANE_LABEL
        }),
        "pane.send_text" => json!({"pane_id": ids.dock, "text": "x"}),
        "pane.send_keys" => json!({"pane_id": ids.dock, "keys": ["Enter"]}),
        "pane.send_input" => json!({"pane_id": ids.dock, "text": "", "keys": []}),
        "pane.read" => json!({"pane_id": ids.dock, "source": "recent"}),
        "pane.report_agent" => json!({
            "pane_id": ids.dock,
            "source": "dock-policy",
            "agent": "claude",
            "state": "idle"
        }),
        "pane.report_agent_session" => json!({
            "pane_id": ids.dock,
            "source": "dock-policy",
            "agent": "claude"
        }),
        "pane.report_metadata" => json!({
            "pane_id": ids.dock,
            "source": "dock-policy",
            "title": "dock"
        }),
        "pane.clear_agent_authority" => json!({"pane_id": ids.dock}),
        "pane.release_agent" => json!({
            "pane_id": ids.dock,
            "source": "dock-policy",
            "agent": "claude"
        }),
        "pane.close" => json!({"pane_id": ids.dock}),
        // Handled by the API server's wait loop, not the app dispatch.
        "pane.wait_for_output" => return None,
        "command.invoke" => json!({"command_id": ids.command, "pane_id": ids.dock}),
        "plugin.pane.open" => json!({
            "plugin_id": "dock-policy",
            "entrypoint": "dock",
            "target_pane_id": ids.dock,
            "placement": "split"
        }),
        "plugin.pane.focus" => json!({"pane_id": ids.dock}),
        "plugin.pane.close" => json!({"pane_id": ids.dock}),
        "layout.export" => json!({"pane_id": ids.dock}),
        "layout.apply" => json!({
            "tab_id": ids.tab,
            "focus": true,
            "root": {"type": "pane"}
        }),
        "layout.set_split_ratio" => json!({
            "pane_id": ids.dock,
            "path": [],
            "ratio": 0.25
        }),
        other => panic!("dock policy row {other} has no request shape"),
    };
    Some(params)
}

fn dispatch(app: &mut App, method: &str, params: Value) -> String {
    let request: Request = serde_json::from_value(json!({
        "id": "policy",
        "method": method,
        "params": params,
    }))
    .unwrap_or_else(|error| panic!("sample for {method}: {error}"));
    app.handle_api_request(request)
}

fn tab_has_user_pane(app: &App, ws_idx: usize, tab_idx: usize) -> bool {
    app.state.workspaces[ws_idx].tabs[tab_idx]
        .layout
        .pane_ids()
        .into_iter()
        .any(|pane_id| !app.pane_is_dock_slot(ws_idx, pane_id))
}

fn assert_focus_off_the_slot(app: &App) {
    let ws_idx = 0;
    let Some(workspace) = app.state.workspaces.get(ws_idx) else {
        return;
    };
    let tab_idx = workspace.active_tab;
    let Some(tab) = workspace.tabs.get(tab_idx) else {
        return;
    };
    if !tab_has_user_pane(app, ws_idx, tab_idx) {
        return;
    }
    let focused = tab.layout.focused();
    assert!(
        !app.pane_is_dock_slot(ws_idx, focused),
        "focus stayed on the dock slot after the operation"
    );
}

#[tokio::test]
async fn docked_tab_enforces_each_classified_operation() {
    for row in PANE_POLICY {
        let mut fixture = docked_fixture();
        let Some(params) = params_for(row.method, &fixture.app, &fixture.ids, fixture.dock) else {
            continue;
        };
        let response = dispatch(&mut fixture.app, row.method, params);

        match row.policy {
            DockPolicy::Reject => {
                let error: ErrorResponse = serde_json::from_str(&response).unwrap_or_else(|_| {
                    panic!("{} should reject the dock slot: {response}", row.method)
                });
                assert_eq!(error.id, "policy");
                if row.names_sidebar {
                    assert!(
                        error.error.message.contains("sidebar"),
                        "{} rejection should name the sidebar slot, got {:?}",
                        row.method,
                        error.error.message
                    );
                }
            }
            DockPolicy::Retarget | DockPolicy::Allow => {
                let success: SuccessResponse =
                    serde_json::from_str(&response).unwrap_or_else(|_| {
                        panic!("{} should serve the dock slot: {response}", row.method)
                    });
                assert!(matches!(success.result, _), "{}", row.method);
            }
        }

        assert_focus_off_the_slot(&fixture.app);

        let ws_idx = 0;
        let tab_idx = fixture.app.state.workspaces[ws_idx].active_tab;
        let has_dock = fixture.app.state.workspaces[ws_idx].tabs[tab_idx]
            .layout
            .pane_ids()
            .contains(&fixture.dock);
        if row.changes_layout {
            assert!(!has_dock, "{} should remove the docked slot", row.method);
        } else {
            assert!(
                has_dock,
                "{} unexpectedly changed the docked layout",
                row.method
            );
        }
        crate::app::api::test_support::shutdown_test_runtimes(&mut fixture.app);
    }
}

fn dock_labelled_pane(node: &crate::api::schema::LayoutNode) -> Option<String> {
    match node {
        crate::api::schema::LayoutNode::Pane { pane } => (pane.label.as_deref()
            == Some(crate::api::schema::DOCKED_SIDEBAR_PANE_LABEL))
        .then(|| pane.pane_id.clone())
        .flatten(),
        crate::api::schema::LayoutNode::Split { first, second, .. } => {
            dock_labelled_pane(first).or_else(|| dock_labelled_pane(second))
        }
    }
}

#[tokio::test]
async fn layout_apply_reports_focus_on_the_user_pane() {
    let mut fixture = docked_fixture();
    let response = dispatch(
        &mut fixture.app,
        "layout.apply",
        json!({
            "tab_id": fixture.ids.tab,
            "focus": true,
            "root": {
                "type": "split",
                "direction": "right",
                "ratio": 0.2,
                "first": {
                    "type": "pane",
                    "label": crate::api::schema::DOCKED_SIDEBAR_PANE_LABEL
                },
                "second": {"type": "pane"}
            }
        }),
    );

    let success: SuccessResponse = serde_json::from_str(&response).unwrap();
    let ResponseResult::LayoutApply { layout } = success.result else {
        panic!("expected a layout apply result");
    };
    let slot = dock_labelled_pane(&layout.root).expect("the supplied slug label is kept");
    assert_ne!(
        layout.focused_pane_id, slot,
        "the response must not report focus on the dock slot"
    );
    assert_focus_off_the_slot(&fixture.app);
    crate::app::api::test_support::shutdown_test_runtimes(&mut fixture.app);
}

#[tokio::test]
async fn retargeted_navigation_steps_over_the_slot() {
    let mut fixture = docked_fixture();
    let main = fixture.ids.main.clone();
    let response = dispatch(
        &mut fixture.app,
        "pane.focus_direction",
        json!({"pane_id": fixture.ids.dock, "direction": "right"}),
    );

    let success: SuccessResponse = serde_json::from_str(&response).unwrap();
    let ResponseResult::PaneFocusDirection { focus } = success.result else {
        panic!("expected a focus-direction result");
    };
    assert!(!focus.changed, "navigation from the slot is a no-op");
    assert_eq!(focus.focused_pane_id.as_deref(), Some(main.as_str()));
    crate::app::api::test_support::shutdown_test_runtimes(&mut fixture.app);
}
