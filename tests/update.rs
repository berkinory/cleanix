use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::Command,
};

fn script(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn bad_update_checksum_preserves_executable_and_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let bin = root.join("original install/bin");
    let tools = root.join("tools");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&tools).unwrap();
    let executable = bin.join("cleanix");
    fs::copy(env!("CARGO_BIN_EXE_cleanix"), &executable).unwrap();
    let original = fs::read(&executable).unwrap();
    let link = root.join("cleanix");
    symlink(&executable, &link).unwrap();
    script(
        &tools.join("curl"),
        r#"#!/bin/sh
set -eu
url= output=
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift; output=$1 ;;
    -w) shift ;;
    https://*) url=$1 ;;
  esac
  shift
done
case "$url" in
  */latest) printf '%s\n' 'https://github.com/berkinory/cleanix/releases/tag/v99.0.0' ;;
  */SHA256SUMS)
    for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
      printf '%064d  cleanix-%s.tar.gz\n' 0 "$target"
    done > "$output" ;;
  *.tar.gz) printf 'untrusted archive' > "$output" ;;
  *) exit 22 ;;
esac
"#,
    );
    let output = Command::new(&link)
        .arg("update")
        .env("HOME", &root)
        .env("PATH", format!("{}:/usr/bin:/bin", tools.display()))
        .env("CLEANIX_INSTALL_DIR", root.join("wrong destination"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("SHA-256 mismatch"),
        "{:?}",
        output
    );
    assert_eq!(fs::read(&executable).unwrap(), original);
    assert_eq!(fs::read_link(link).unwrap(), executable);
    assert!(!root.join("wrong destination").exists());
    assert!(!root.join(".local/bin/cleanix").exists());
}

#[test]
fn homebrew_update_delegates_without_replacing_the_keg() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let keg = root.join("Cellar/cleanix/0.0.1");
    let tools = root.join("tools");
    fs::create_dir_all(keg.join("bin")).unwrap();
    fs::create_dir_all(&tools).unwrap();
    fs::write(keg.join("INSTALL_RECEIPT.json"), "{}").unwrap();
    let executable = keg.join("bin/cleanix");
    fs::copy(env!("CARGO_BIN_EXE_cleanix"), &executable).unwrap();
    let original = fs::read(&executable).unwrap();
    script(
        &tools.join("brew"),
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$UPDATE_LOG"
case "$*" in
  '--prefix cleanix') printf '%s\n' "$UPDATE_KEG" ;;
  'update'|'upgrade --formula cleanix') exit 0 ;;
  *) exit 17 ;;
esac
"#,
    );
    let log = root.join("calls");
    let output = Command::new(&executable)
        .arg("update")
        .env("PATH", &tools)
        .env("UPDATE_LOG", &log)
        .env("UPDATE_KEG", &keg)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "--prefix cleanix\nupdate\nupgrade --formula cleanix\n"
    );
    assert_eq!(fs::read(executable).unwrap(), original);
}
