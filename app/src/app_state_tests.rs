use super::*;

#[test]
fn test_has_horizontal_split() {
    let single_leaf = PaneNodeSnapshot::Leaf(LeafSnapshot {
        is_focused: false,
        custom_vertical_tabs_title: None,
        contents: LeafContents::Code(CodePaneSnapShot::Local {
            tabs: vec![CodePaneTabSnapshot {
                path: Some(PathBuf::new()),
            }],
            active_tab_index: 0,
            source: None,
        }),
    });
    assert!(!single_leaf.has_horizontal_split());

    let horizontal_split = PaneNodeSnapshot::Branch(BranchSnapshot {
        direction: SplitDirection::Horizontal,
        children: vec![
            (
                PaneFlex(1.),
                PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: false,
                    custom_vertical_tabs_title: None,
                    contents: LeafContents::Code(CodePaneSnapShot::Local {
                        tabs: vec![CodePaneTabSnapshot {
                            path: Some(PathBuf::new()),
                        }],
                        active_tab_index: 0,
                        source: None,
                    }),
                }),
            ),
            (
                PaneFlex(1.),
                PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: false,
                    custom_vertical_tabs_title: None,
                    contents: LeafContents::Code(CodePaneSnapShot::Local {
                        tabs: vec![CodePaneTabSnapshot {
                            path: Some(PathBuf::new()),
                        }],
                        active_tab_index: 0,
                        source: None,
                    }),
                }),
            ),
        ],
    });
    assert!(horizontal_split.has_horizontal_split());
}

#[test]
fn test_code_pane_snapshot_single_tab() {
    let snapshot = CodePaneSnapShot::Local {
        tabs: vec![CodePaneTabSnapshot {
            path: Some(PathBuf::from("/tmp/test.rs")),
        }],
        active_tab_index: 0,
        source: Some(CodeSource::FileTree {
            location: crate::code::buffer_location::LocalOrRemotePath::Local(PathBuf::from(
                "/tmp/test.rs",
            )),
        }),
    };
    let CodePaneSnapShot::Local {
        tabs,
        active_tab_index,
        source,
    } = &snapshot;
    assert_eq!(tabs.len(), 1);
    assert_eq!(*active_tab_index, 0);
    assert_eq!(tabs[0].path, Some(PathBuf::from("/tmp/test.rs")));
    assert!(matches!(source, Some(CodeSource::FileTree { .. })));
}

#[test]
fn test_code_pane_snapshot_with_multiple_tabs() {
    let snapshot = CodePaneSnapShot::Local {
        tabs: vec![
            CodePaneTabSnapshot {
                path: Some(PathBuf::from("/tmp/main.rs")),
            },
            CodePaneTabSnapshot {
                path: Some(PathBuf::from("/tmp/lib.rs")),
            },
            CodePaneTabSnapshot { path: None },
        ],
        active_tab_index: 1,
        source: Some(CodeSource::Link {
            path: PathBuf::from("/tmp/main.rs"),
            range_start: None,
            range_end: None,
        }),
    };
    let CodePaneSnapShot::Local {
        tabs,
        active_tab_index,
        source,
    } = &snapshot;
    assert_eq!(tabs.len(), 3);
    assert_eq!(*active_tab_index, 1);
    assert_eq!(tabs[0].path, Some(PathBuf::from("/tmp/main.rs")));
    assert_eq!(tabs[1].path, Some(PathBuf::from("/tmp/lib.rs")));
    assert_eq!(tabs[2].path, None);
    assert!(matches!(source, Some(CodeSource::Link { .. })));
}

#[test]
fn test_rmux_terminal_pane_is_persisted() {
    let rmux_leaf = LeafContents::RmuxTerminal(RmuxTerminalPaneSnapshot {
        uuid: vec![1, 2, 3, 4],
        session_name: "shell-1".to_string(),
        pane_id: Some(1),
        cwd: Some("/tmp".to_string()),
        warp_created: true,
    });
    assert!(rmux_leaf.is_persisted());
}

#[test]
fn test_network_log_is_not_persisted() {
    let network_log = LeafContents::NetworkLog;
    assert!(!network_log.is_persisted());
}

#[test]
fn test_environment_management_is_not_persisted() {
    let env_mgmt = LeafContents::EnvironmentManagement(EnvironmentManagementPaneSnapshot {
        mode: crate::settings_view::environments_page::EnvironmentsPage::default(),
    });
    assert!(!env_mgmt.is_persisted());
}

#[test]
fn test_terminal_pane_is_persisted() {
    let terminal = LeafContents::Terminal(TerminalPaneSnapshot {
        uuid: vec![1, 2, 3],
        cwd: Some("/tmp".to_string()),
        shell_launch_data: None,
        is_active: true,
        is_read_only: false,
        input_config: None,
        llm_model_override: None,
        active_profile_id: None,
        conversation_ids_to_restore: vec![],
        active_conversation_id: None,
    });
    assert!(terminal.is_persisted());
}

#[test]
fn test_rmux_terminal_snapshot_fields() {
    let snapshot = RmuxTerminalPaneSnapshot {
        uuid: vec![1, 2, 3, 4],
        session_name: "codex-1".to_string(),
        pane_id: Some(42),
        cwd: Some("/home/user".to_string()),
        warp_created: true,
    };
    assert_eq!(snapshot.session_name, "codex-1");
    assert_eq!(snapshot.pane_id, Some(42));
    assert_eq!(snapshot.cwd, Some("/home/user".to_string()));
    assert!(snapshot.warp_created);
}
