use crate::process;
use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

const RELEASES: &str = "https://github.com/berkinory/cleanix/releases";

fn version(value: &str) -> Result<[u64; 3]> {
    let parts = value
        .strip_prefix('v')
        .unwrap_or(value)
        .split('.')
        .collect::<Vec<_>>();
    ensure!(
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit())),
        "Invalid stable release version: {value}"
    );
    Ok([parts[0].parse()?, parts[1].parse()?, parts[2].parse()?])
}

fn brew_install(executable: &Path) -> bool {
    executable
        .parent()
        .and_then(Path::parent)
        .is_some_and(|keg| {
            keg.join("INSTALL_RECEIPT.json").is_file()
                || keg.parent().is_some_and(|rack| {
                    rack.file_name().is_some_and(|n| n == "cleanix")
                        && rack
                            .parent()
                            .is_some_and(|cellar| cellar.file_name().is_some_and(|n| n == "Cellar"))
                })
        })
}

pub fn run() -> Result<()> {
    let executable = std::env::current_exe()?.canonicalize()?;
    if brew_install(&executable) {
        let brew = process::which("brew")
            .context("This installation is managed by Homebrew, but brew is not on PATH")?;
        let prefix = process::query(&brew, &["--prefix", "cleanix"])?;
        ensure!(
            Path::new(&prefix).canonicalize()?
                == executable.parent().and_then(Path::parent).unwrap(),
            "Homebrew installation does not match this executable; use its owning package manager"
        );
        println!("Updating cleanix through Homebrew…");
        for args in [&["update"][..], &["upgrade", "--formula", "cleanix"][..]] {
            ensure!(
                Command::new(&brew).args(args).status()?.success(),
                "Homebrew update failed; retry with brew upgrade cleanix"
            );
        }
        return Ok(());
    }
    ensure!(
        executable.file_name().is_some_and(|name| name == "cleanix"),
        "The executable must be named cleanix to update in place"
    );
    let curl = process::which("curl").context("curl is required to check for updates")?;
    println!("Checking for updates…");
    let url = process::run(
        &curl,
        &[
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "-fsSL",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
            &format!("{RELEASES}/latest"),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>(),
        std::time::Duration::from_secs(35),
    )?;
    let tag = url
        .strip_prefix(&format!("{RELEASES}/tag/v"))
        .context("Unexpected release URL")?;
    let latest = version(tag)?;
    let current = version(env!("CARGO_PKG_VERSION"))?;
    if latest <= current {
        println!(
            "cleanix {} is {}.",
            env!("CARGO_PKG_VERSION"),
            if latest == current {
                "up to date"
            } else {
                "newer than the published release"
            }
        );
        return Ok(());
    }
    println!("Updating {} → {tag}", env!("CARGO_PKG_VERSION"));
    let destination = executable
        .parent()
        .context("Missing installation directory")?;
    let mut child = Command::new("/bin/sh")
        .args(["-s", "--", tag])
        .env("CLEANIX_INSTALL_DIR", destination)
        .stdin(Stdio::piped())
        .spawn()
        .context("Cannot start the embedded updater")?;
    let written = child
        .stdin
        .take()
        .unwrap()
        .write_all(include_bytes!("../install.sh"));
    let status = child.wait()?;
    written.context("Cannot write the embedded updater")?;
    ensure!(
        status.success(),
        "Update failed; the existing binary is preserved unless replacement completed"
    );
    Ok(())
}
