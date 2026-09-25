use cleanix::{
    cleanup, filesystem,
    model::{Action, Item, Risk},
    providers,
};
use std::{
    fs,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime},
};
fn file(root: &Path, path: &str) {
    let p = root.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, b"keep this data").unwrap();
}
fn item(path: &Path) -> Item {
    Item {
        id: "fixture".into(),
        category: "test".into(),
        name: "fixture".into(),
        detail: String::new(),
        risk: Risk::Review,
        bytes: None,
        estimated: false,
        files: 0,
        action: Action::DeletePaths {
            targets: vec![filesystem::target(path).unwrap()],
        },
        excludes: vec![],
    }
}
#[test]
fn real_delete_removes_only_selected_tree_and_never_follows_links() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "selected/nested/data");
    file(&h, "outside/precious");
    std::os::unix::fs::symlink(h.join("outside"), h.join("selected/link")).unwrap();
    cleanup::execute(&item(&h.join("selected")), false).unwrap();
    assert!(!h.join("selected").exists());
    assert_eq!(
        fs::read(h.join("outside/precious")).unwrap(),
        b"keep this data"
    );
}
#[test]
fn dry_mode_never_deletes_or_starts_commands() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "cache/data");
    let mut i = item(&h.join("cache"));
    cleanup::execute(&i, true).unwrap();
    assert!(h.join("cache/data").exists());
    let exe = h.join("command");
    let marker = h.join("called");
    fs::write(&exe, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    i.action = Action::Docker {
        executable: exe,
        context: "local".into(),
        endpoint: "unix:///tmp/local.sock".into(),
        kind: "Images".into(),
    };
    cleanup::execute(&i, true).unwrap();
    assert!(!marker.exists());
    i.action = Action::TimeMachineSnapshot {
        date: "2026-09-24-120000".into(),
    };
    cleanup::execute(&i, true).unwrap();
}
#[test]
fn replaced_root_and_redirected_parent_cannot_delete_unreviewed_data() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "parent/cache/data");
    let i = item(&h.join("parent/cache"));
    fs::rename(h.join("parent/cache"), h.join("old")).unwrap();
    file(&h, "parent/cache/precious");
    assert!(cleanup::execute(&i, false).is_err());
    assert!(h.join("parent/cache/precious").exists());
    let i = item(&h.join("parent/cache"));
    fs::rename(h.join("parent"), h.join("moved")).unwrap();
    std::os::unix::fs::symlink(h.join("moved"), h.join("parent")).unwrap();
    assert!(cleanup::execute(&i, false).is_err());
    assert!(h.join("moved/cache/precious").exists());
}
#[cfg(target_os = "macos")]
#[test]
fn protected_descendants_fail_before_any_deletion() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "cache/ordinary");
    file(&h, "cache/protected");
    let i = item(&h.join("cache"));
    let path = std::ffi::CString::new(h.join("cache/protected").to_str().unwrap()).unwrap();
    assert_eq!(
        unsafe { libc::chflags(path.as_ptr(), libc::UF_IMMUTABLE) },
        0
    );
    let result = cleanup::execute(&i, false);
    assert_eq!(unsafe { libc::chflags(path.as_ptr(), 0) }, 0);
    assert!(result.is_err());
    assert!(h.join("cache/ordinary").exists());
    assert!(h.join("cache/protected").exists());
}
#[test]
fn aged_files_are_rechecked_at_deletion_and_fresh_files_survive() {
    use std::fs::{File, FileTimes};
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "temp/old");
    file(&h, "temp/fresh");
    let old = SystemTime::now() - Duration::from_secs(4 * 86400);
    File::options()
        .write(true)
        .open(h.join("temp/old"))
        .unwrap()
        .set_times(FileTimes::new().set_accessed(old).set_modified(old))
        .unwrap();
    let mut notes = vec![];
    let paths = cleanix::extra::aged_files(
        &h.join("temp"),
        SystemTime::now(),
        &AtomicBool::new(false),
        &mut notes,
    );
    assert_eq!(paths, vec![h.join("temp/old")]);
    let mut i = item(&paths[0]);
    i.action = Action::AgedFiles {
        targets: vec![filesystem::target(&paths[0]).unwrap()],
        seconds: 3 * 86400,
    };
    File::options()
        .write(true)
        .open(&paths[0])
        .unwrap()
        .set_times(FileTimes::new().set_accessed(SystemTime::now()))
        .unwrap();
    assert!(cleanup::execute(&i, false).is_err());
    File::options()
        .write(true)
        .open(&paths[0])
        .unwrap()
        .set_times(FileTimes::new().set_accessed(old).set_modified(old))
        .unwrap();
    cleanup::execute(&i, false).unwrap();
    assert!(!paths[0].exists());
    assert!(h.join("temp/fresh").exists());
}
#[test]
fn broad_roots_and_conversation_state_are_not_deletion_targets() {
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    for path in [
        home.clone(),
        home.join("Library"),
        home.join(".codex/sessions"),
        home.join(".claude/projects"),
        "/private/var".into(),
        "/Library/Caches".into(),
        "/".into(),
    ] {
        assert!(
            filesystem::safe_cleanup_path(&path).is_err(),
            "{}",
            path.display()
        );
    }
    assert!(cleanup::elevated_target("{}").is_err());
}
#[test]
fn overlapping_groups_and_ignores_do_not_expand_deletion_scope() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "a/nested/data");
    file(&h, "b/precious");
    let mut items = vec![
        item(&h.join("a/nested")),
        item(&h.join("a")),
        item(&h.join("b")),
    ];
    providers::normalize(&mut items, &[h.join("b/precious")]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].action.paths(), vec![h.join("a")]);
}
#[test]
fn android_registration_cannot_redirect_removal_to_another_directory() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "Pixel.avd/config.ini");
    file(&h, "Pixel.avd/data");
    file(&h, "other/precious");
    let ini = h.join("Pixel.ini");
    fs::write(&ini, format!("path={}\n", h.join("Pixel.avd").display())).unwrap();
    let (avd, identity, registration) = cleanix::managed::avd_targets(&ini).unwrap();
    let mut i = item(&h.join("Pixel.avd"));
    i.action = Action::AndroidAvd {
        executable: None,
        name: "Pixel".into(),
        avd,
        identity,
        ini: registration,
    };
    fs::write(&ini, format!("path={}\n", h.join("other").display())).unwrap();
    assert!(cleanup::execute(&i, false).is_err());
    assert!(h.join("other/precious").exists());
    assert!(h.join("Pixel.avd/data").exists());
}
#[test]
fn docker_rejects_remote_or_changed_endpoints_before_pruning() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "fixture");
    let exe = h.join("docker");
    let marker = h.join("pruned");
    fs::write(&exe,format!("#!/bin/sh\nif [ \"$1\" = context ]; then printf '%s' '[{{\"Endpoints\":{{\"docker\":{{\"Host\":\"ssh://remote\"}}}}}}]'; else touch '{}'; fi\n",marker.display())).unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    let mut i = item(&h.join("fixture"));
    i.action = Action::Docker {
        executable: exe,
        context: "local".into(),
        endpoint: "unix:///tmp/local.sock".into(),
        kind: "Images".into(),
    };
    assert!(cleanup::execute(&i, false).is_err());
    assert!(!marker.exists());
}
#[test]
fn volume_age_filter_rejects_new_or_unparseable_dates() {
    let now = 1_800_000_000;
    assert!(cleanix::docker::older_than_72h("2020-01-01T00:00:00Z", now));
    assert!(!cleanix::docker::older_than_72h(
        "2099-01-01T00:00:00Z",
        now
    ));
    assert!(!cleanix::docker::older_than_72h("unknown", now));
    assert!(!cleanix::docker::valid_volume_name("--all"));
    let live =
        std::collections::HashMap::from([("local".into(), "unix:///tmp/docker.sock".into())]);
    assert!(cleanix::docker::local_builder(&serde_json::json!({"Name":"b","Driver":"docker-container","Nodes":[{"Endpoint":"ssh://prod"}]}),&live).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_special_entries_fail_before_any_deletion() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    file(&h, "cache/ordinary");
    let socket = std::os::unix::net::UnixListener::bind(h.join("cache/session.sock")).unwrap();
    assert!(cleanup::execute(&item(&h.join("cache")), false).is_err());
    assert!(h.join("cache/ordinary").exists());
    drop(socket);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_inventory_honors_xdg_and_preserves_state() {
    use std::process::Command;
    let t = tempfile::tempdir().unwrap();
    let h = t.path().canonicalize().unwrap();
    for path in [
        "cache/pip/data",
        "cache/spotify/data",
        "cache/codex/sessions/chat",
        "cache/mystery/auth.json",
        "config/Code/Cache/data",
        "config/Code/User/settings.json",
        "projects/app/node_modules/package/data",
        "Library/Developer/Xcode/DerivedData/keep/data",
    ] {
        file(&h, path);
    }
    file(&h, "projects/app/package.json");
    fs::create_dir_all(h.join("config/cleanix")).unwrap();
    fs::write(h.join("config/cleanix/config.toml"), "min_bytes = 0\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cleanix"))
        .args(["--json", "--root"])
        .arg(h.join("projects"))
        .env_clear()
        .env("HOME", &h)
        .env("PATH", "/usr/bin:/bin")
        .env("XDG_CACHE_HOME", h.join("cache"))
        .env("XDG_CONFIG_HOME", h.join("config"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let items: Vec<Item> = serde_json::from_value(report["items"].clone()).unwrap();
    let paths: Vec<_> = items.iter().flat_map(|i| i.action.paths()).collect();
    for path in [
        "cache/pip",
        "cache/spotify",
        "config/Code/Cache",
        "projects/app/node_modules",
    ] {
        assert!(paths.contains(&h.join(path)), "missing {path}: {paths:?}");
    }
    for path in [
        "cache/codex/sessions/chat",
        "cache/mystery/auth.json",
        "config/Code/User/settings.json",
        "Library/Developer/Xcode/DerivedData/keep/data",
    ] {
        assert!(!paths.iter().any(|p| h.join(path).starts_with(p)));
        assert!(h.join(path).exists());
    }
    assert_eq!(
        items.iter().filter(|i| i.category == "App caches").count(),
        1
    );
    assert!(!report["notes"].as_array().unwrap().iter().any(|n| {
        let n = n.as_str().unwrap();
        n.contains("Time Machine") || n.contains("iOS simulator")
    }));
}
