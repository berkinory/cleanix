#![cfg(target_os = "macos")]
use cleanix::uninstall::{inventory, plan};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

fn app(home: &Path, name: &str, id: &str) -> PathBuf {
    let path = home.join("Applications").join(format!("{name}.app"));
    fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
    fs::write(path.join("Contents/MacOS/FixtureExecutable"), "fixture").unwrap();
    fs::write(path.join("Contents/Info.plist"), format!(r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleName</key><string>{name}</string><key>CFBundleExecutable</key><string>FixtureExecutable</string></dict></plist>"#)).unwrap();
    path
}
fn data(home: &Path, path: &str) -> PathBuf {
    let path = home.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "precious").unwrap();
    path
}
fn review(home: &Path) -> plan::Plan {
    let inventory = inventory::scan(home, &[home.join("Applications")]).unwrap();
    assert!(inventory.complete, "{:?}", inventory.notes);
    plan::build(&inventory.apps[0], &inventory).unwrap()
}
#[test]
fn uninstall_dry_and_live_respect_reviewed_paths_and_preserve_other_app_data() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let bundle = app(&home, "CleanixFixture", "io.cleanix.fixture");
    let cache = data(&home, "Library/Caches/io.cleanix.fixture/data");
    let prefs = data(&home, "Library/Preferences/io.cleanix.fixture.plist");
    let neighbor = data(&home, "Library/Caches/io.cleanix.fixture.other/data");
    let shared = data(&home, "Library/Group Containers/io.cleanix.fixture/data");
    let plan = review(&home);
    let cache_index = plan
        .entries
        .iter()
        .position(|e| e.target.path == cache.parent().unwrap())
        .unwrap();
    let selected = HashSet::from([0, cache_index]);
    plan::execute(&plan, &selected, true).unwrap();
    assert!(bundle.exists() && cache.exists());
    plan::execute(&plan, &selected, false).unwrap();
    assert!(!bundle.exists() && !cache.exists());
    for preserved in [prefs, neighbor, shared] {
        assert_eq!(fs::read_to_string(preserved).unwrap(), "precious");
    }
}
#[test]
fn changed_app_identity_or_new_sibling_aborts_before_deleting_any_reviewed_data() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let bundle = app(&home, "CleanixFixture", "io.cleanix.fixture");
    let cache = data(&home, "Library/Caches/io.cleanix.fixture/data");
    let plan = review(&home);
    let selected = (0..plan.entries.len()).collect();
    app(&home, "CleanixFixture", "io.cleanix.replaced");
    assert!(plan::execute(&plan, &selected, false).is_err());
    assert!(bundle.exists() && cache.exists());
    app(&home, "CleanixFixture", "io.cleanix.fixture");
    let plan = review(&home);
    app(&home, "OtherCopy", "io.cleanix.fixture");
    assert!(plan::execute(&plan, &selected, false).is_err());
    assert!(bundle.exists() && cache.exists());
    let restricted = review(&home);
    assert_eq!(restricted.entries.len(), 1);
}

#[test]
fn changed_or_protected_login_helper_aborts_before_app_removal() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let bundle = app(&home, "CleanixFixture", "io.cleanix.fixture");
    let helper = app(&home, "LoginHelper", "io.cleanix.fixture.helper");
    let embedded = bundle.join("Contents/Library/LoginItems/LoginHelper.app");
    fs::create_dir_all(embedded.parent().unwrap()).unwrap();
    fs::rename(helper, &embedded).unwrap();
    let plan = review(&home);
    let plist = embedded.join("Contents/Info.plist");
    let original = fs::read_to_string(&plist).unwrap();
    fs::write(
        &plist,
        original.replace("io.cleanix.fixture.helper", "io.cleanix.unrelated.helper"),
    )
    .unwrap();
    assert!(plan::execute(&plan, &HashSet::from([0]), false).is_err());
    assert!(bundle.exists() && embedded.exists());
    fs::write(
        &plist,
        original.replace("io.cleanix.fixture.helper", "com.apple.protected"),
    )
    .unwrap();
    let inventory = inventory::scan(&home, &[home.join("Applications")]).unwrap();
    assert!(plan::build(&inventory.apps[0], &inventory).is_err());
    assert!(bundle.exists() && embedded.exists());
}
