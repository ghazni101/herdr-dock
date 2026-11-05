use super::*;
use crate::protocol::{PaneSurfaceFrame, PaneSurfacePane, SurfaceRect};

fn dock_pane(focused: bool) -> ClientShellPane {
    ClientShellPane {
        pane_id: "pane_dock".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: Some(DOCKED_SIDEBAR_LABEL.into()),
        cwd: Some("/tmp".into()),
        foreground_cwd: Some("/tmp".into()),
        focused,
        right_click_passthrough: false,
    }
}

fn dock_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    snapshot.focused_pane_id = Some("pane_dock".into());
    snapshot.panes[0].focused = false;
    snapshot.panes.push(dock_pane(true));
    snapshot
}

fn surface_pane(pane_id: &str, x: u16, width: u16, height: u16, focused: bool) -> PaneSurfacePane {
    PaneSurfacePane {
        pane_id: pane_id.into(),
        content_revision: 0,
        rect: SurfaceRect {
            x,
            y: 0,
            width,
            height,
        },
        inner_rect: SurfaceRect {
            x,
            y: 0,
            width,
            height,
        },
        scrollbar_rect: None,
        scroll: None,
        focused,
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        alternate_screen_active: false,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn surface_with(panes: Vec<PaneSurfacePane>) -> PaneSurfaceFrame {
    surface_sized(80, 10, panes)
}

fn surface_sized(cols: u16, rows: u16, panes: Vec<PaneSurfacePane>) -> PaneSurfaceFrame {
    let buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
    PaneSurfaceFrame {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        surface_revision: 1,
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]),
        panes,
        splits: Vec::new(),
        popup: None,
        graphics: Default::default(),
    }
}

fn docked_state(snapshot: ClientShellSnapshot, surface: PaneSurfaceFrame) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot));
    state.set_pane_surface(surface);
    state
}

fn first_endpoint_method(input: &ClientShellInput) -> &crate::api::schema::Method {
    let Some(ClientShellAction::Endpoint { request, .. }) = input.actions.first() else {
        panic!(
            "expected an endpoint request, got {:?}",
            input.actions.len()
        );
    };
    &request.method
}

#[test]
fn dock_focus_falls_back_without_main_label() {
    let state = docked_state(
        dock_snapshot(),
        surface_with(vec![
            surface_pane("pane_dock", 0, 20, 10, false),
            surface_pane("pane_1", 20, 60, 10, false),
        ]),
    );
    assert_eq!(state.focused_pane_id().as_deref(), Some("pane_1"));
}

#[test]
fn dock_focus_prefers_labelled_main_then_largest_pane() {
    let mut snapshot = dock_snapshot();
    snapshot.panes[0].label = Some(DOCKED_MAIN_LABEL.into());
    snapshot.panes.push(ClientShellPane {
        pane_id: "pane_2".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        label: None,
        cwd: Some("/tmp".into()),
        foreground_cwd: Some("/tmp".into()),
        focused: false,
        right_click_passthrough: false,
    });
    let state = docked_state(
        snapshot,
        surface_with(vec![
            surface_pane("pane_dock", 0, 20, 10, false),
            surface_pane("pane_2", 20, 55, 10, false),
            surface_pane("pane_1", 75, 5, 10, false),
        ]),
    );
    // The label wins even though the labelled pane is the smallest.
    assert_eq!(state.focused_pane_id().as_deref(), Some("pane_1"));

    let mut unlabelled = dock_snapshot();
    unlabelled.panes[0].label = None;
    unlabelled.panes.push(ClientShellPane {
        pane_id: "pane_2".into(),
        ..unlabelled.panes[0].clone()
    });
    // Snapshot order starts with the small pane; the geometric fallback still
    // picks the large one.
    unlabelled.panes.swap(0, 2);
    let state = docked_state(
        unlabelled,
        surface_with(vec![
            surface_pane("pane_dock", 0, 20, 10, false),
            surface_pane("pane_2", 20, 10, 10, false),
            surface_pane("pane_1", 30, 50, 10, false),
        ]),
    );
    assert_eq!(state.focused_pane_id().as_deref(), Some("pane_1"));
}

#[test]
fn pane_cycle_skips_the_dock_slot() {
    let mut snapshot = dock_snapshot();
    snapshot.focused_pane_id = Some("pane_1".into());
    snapshot.panes[0].focused = true;
    snapshot.panes[1].focused = false;
    snapshot.panes.push(ClientShellPane {
        pane_id: "pane_2".into(),
        ..snapshot.panes[0].clone()
    });
    snapshot.panes[2].pane_id = "pane_2".into();
    snapshot.panes[2].focused = false;
    let mut state = docked_state(
        snapshot,
        surface_with(vec![
            surface_pane("pane_1", 20, 30, 10, true),
            surface_pane("pane_2", 50, 30, 10, false),
            surface_pane("pane_dock", 0, 20, 10, false),
        ]),
    );

    let mut next = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CyclePaneNext),
        &mut next,
    );
    assert!(matches!(
        first_endpoint_method(&next),
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));

    let mut previous = ClientShellInput::default();
    state.record_binding(
        crate::input::KeybindMatch::Action(crate::input::KeybindAction::CyclePanePrevious),
        &mut previous,
    );
    assert!(matches!(
        first_endpoint_method(&previous),
        crate::api::schema::Method::PaneFocus(target) if target.pane_id == "pane_2"
    ));
}

#[test]
fn focus_and_swap_toward_dock_are_no_ops() {
    let mut snapshot = dock_snapshot();
    snapshot.focused_pane_id = Some("pane_1".into());
    snapshot.panes[0].focused = true;
    snapshot.panes[1].focused = false;
    let mut state = docked_state(
        snapshot,
        surface_with(vec![
            surface_pane("pane_dock", 0, 20, 10, false),
            surface_pane("pane_1", 20, 60, 10, true),
        ]),
    );

    for action in [
        crate::input::KeybindAction::FocusPaneLeft,
        crate::input::KeybindAction::SwapPaneLeft,
    ] {
        let mut input = ClientShellInput::default();
        state.record_binding(crate::input::KeybindMatch::Action(action), &mut input);
        assert!(
            input
                .actions
                .iter()
                .all(|action| !matches!(action, ClientShellAction::Endpoint { .. })),
            "moving toward the dock must not reach the server"
        );
    }
}

#[test]
fn zoomed_tab_restores_the_chrome_sidebar() {
    let mut docked = dock_snapshot();
    docked.focused_pane_id = Some("pane_1".into());
    docked.panes[0].focused = true;
    docked.panes[1].focused = false;
    let state = docked_state(
        docked,
        surface_with(vec![
            surface_pane("pane_dock", 0, 20, 10, false),
            surface_pane("pane_1", 20, 60, 10, true),
        ]),
    );
    assert!(state.has_docked_sidebar());
    let layout = state.layout(100, 30);
    assert_eq!(layout.sidebar.width, 0);
    assert_eq!(layout.pane_surface.width, 100);

    let mut zoomed = dock_snapshot();
    zoomed.tabs[0].zoomed = true;
    zoomed.focused_pane_id = Some("pane_1".into());
    zoomed.panes[0].focused = true;
    zoomed.panes[1].focused = false;
    let state = docked_state(
        zoomed,
        surface_with(vec![surface_pane("pane_1", 0, 100, 10, true)]),
    );
    assert!(!state.has_docked_sidebar());
    let layout = state.layout(100, 30);
    assert!(layout.sidebar.width > 0);
    assert!(layout.pane_surface.width < 100);
}

#[test]
fn collapsing_a_docked_sidebar_resizes_and_restores_the_split() {
    let mut snapshot = dock_snapshot();
    snapshot.focused_pane_id = Some("pane_1".into());
    snapshot.panes[0].focused = true;
    snapshot.panes[1].focused = false;
    let mut state = docked_state(
        snapshot,
        surface_with(vec![
            surface_pane("pane_dock", 0, 20, 10, false),
            surface_pane("pane_1", 20, 60, 10, true),
        ]),
    );

    let mut collapse = ClientShellInput::default();
    state.toggle_sidebar_collapsed(&mut collapse);
    assert!(state.sidebar_collapsed);
    let method = first_endpoint_method(&collapse);
    let crate::api::schema::Method::PaneResize(params) = method else {
        panic!("expected a pane resize");
    };
    assert_eq!(params.pane_id.as_deref(), Some("pane_dock"));
    assert_eq!(params.direction, crate::api::schema::PaneDirection::Left);
    let amount = params.amount.expect("resize amount");
    assert!((amount - 0.2).abs() < 1e-6, "unexpected amount {amount}");

    // The server reports the compact geometry next.
    let mut compact = surface_with(vec![
        surface_pane("pane_dock", 0, 4, 10, false),
        surface_pane("pane_1", 4, 76, 10, true),
    ]);
    compact.surface_revision = 2;
    state.set_pane_surface(compact);

    let mut expand = ClientShellInput::default();
    state.toggle_sidebar_collapsed(&mut expand);
    assert!(!state.sidebar_collapsed);
    let method = first_endpoint_method(&expand);
    let crate::api::schema::Method::PaneResize(params) = method else {
        panic!("expected a pane resize");
    };
    assert_eq!(params.direction, crate::api::schema::PaneDirection::Right);
    assert!((params.amount.expect("resize amount") - 0.2).abs() < 1e-6);
}

/// Regression: the docked posture clears `hits.sidebar_divider` because the
/// pane split owns the sidebar width, and the spaces/agents drag used to read
/// its extent from that cleared rect, so the section divider never moved.
#[test]
fn docked_sidebar_section_divider_drag_resizes_the_split() {
    let mut snapshot = dock_snapshot();
    snapshot.focused_pane_id = Some("pane_1".into());
    snapshot.panes[0].focused = true;
    snapshot.panes[1].focused = false;
    let mut state = docked_state(
        snapshot,
        surface_sized(
            106,
            30,
            vec![
                surface_pane("pane_dock", 0, 20, 30, false),
                surface_pane("pane_1", 20, 86, 30, true),
            ],
        ),
    );
    state.compose(106, 30).expect("docked sidebar");
    assert!(state.has_docked_sidebar());
    assert_eq!(state.hits.sidebar_divider, Rect::default());
    let area = state.hits.sidebar_section_area;
    assert_eq!(area.height, 30);
    let section_divider = state.hits.sidebar_section_divider;
    assert!(
        !section_divider.is_empty(),
        "docked section divider must register a hit"
    );
    assert_eq!(section_divider.y, area.y + 15);
    assert!((state.sidebar_section_split - 0.5).abs() < f32::EPSILON);

    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: section_divider.x + 2,
        row: section_divider.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(matches!(
        state.chrome_drag,
        Some(ClientChromeDrag::SidebarSection)
    ));

    let split = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: section_divider.x + 2,
        row: area.y + 24,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.sidebar_section_split_manual);
    assert!(
        (state.sidebar_section_split - 0.8).abs() < 1e-6,
        "docked drag must move the split, got {}",
        state.sidebar_section_split
    );
    assert!(split.repaint);
    assert!(!split.resize);

    state.compose(106, 30).expect("docked sidebar after drag");
    assert_eq!(state.hits.sidebar_section_divider.y, area.y + 24);

    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column: section_divider.x + 2,
        row: area.y + 24,
        modifiers: KeyModifiers::empty(),
    })]);
    assert!(state.chrome_drag.is_none());
}
