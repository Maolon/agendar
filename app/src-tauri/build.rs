fn main() {
    // The command list is the frozen application ACL
    // Keeping it here (and comparing it with the invoke registrations and the
    // capability files in a test) prevents a command from silently gaining or
    // losing its generated `allow-<name>` permission.
    const COMMANDS: &[&str] = &[
        "get_snapshot",
        "get_project",
        "search",
        "prepare_answer",
        "answer_decision",
        "review_delivery",
        "resolve_rescue",
        "update_project",
        "update_feature",
        "drop_steps",
        "set_policy",
        "set_rule",
        "read_report",
        "approve_check",
        "run_check",
        "set_preferences",
        "snooze_need",
        "dismiss_need",
        "configure_vault",
        "get_setup_locations",
        "get_local_config",
        "get_storage_usage",
        "test_notice",
        "open_report",
        "get_bindings",
        "set_binding",
        "install_local",
        "link_repo",
        "unlink_repo",
        "show_main",
        "hide_popover",
        "open_preview",
        "window_chrome",
        "set_glass_regions",
        "check_updates",
        "open_release",
    ];
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("Tauri build failed");
}
