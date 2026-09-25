use crate::{
    filesystem, managed,
    model::{Action, Item, Target, docker_args},
    process, removal,
    scan::Event,
};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::AtomicBool,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, SystemTime},
};

pub fn validate_plan(item: &Item) -> Result<()> {
    match &item.action {
        Action::InspectStorage { .. } => bail!("Storage inventory is not a deletion target"),
        Action::AgedFiles { targets, seconds } => {
            for t in targets {
                filesystem::safe_cleanup_path(&t.path)?;
                filesystem::validate(t)?;
                let m = fs::symlink_metadata(&t.path)?;
                if !m.is_file()
                    || [m.modified()?, m.accessed()?].iter().any(|time| {
                        !SystemTime::now()
                            .duration_since(*time)
                            .is_ok_and(|age| age > Duration::from_secs(*seconds))
                    })
                {
                    bail!("Temporary file was accessed or modified recently; rescan");
                }
            }
        }
        Action::DeletePaths { targets }
        | Action::UnifiedLogs { targets }
        | Action::AndroidSnapshots { targets, .. } => {
            for target in targets {
                filesystem::safe_cleanup_path(&target.path)?;
                filesystem::validate(target)?;
            }
        }
        Action::Simulator {
            udid, operation, ..
        } => {
            if !managed::valid_uuid(udid) || !["erase", "delete"].contains(&operation.as_str()) {
                bail!("Invalid simulator action");
            }
        }
        Action::TimeMachineSnapshot { date } if !crate::stores::valid_snapshot_date(date) => {
            bail!("Invalid snapshot date")
        }
        Action::Runtime { id } if !managed::valid_uuid(id) => bail!("Invalid runtime identifier"),
        Action::AndroidAvd {
            avd, identity, ini, ..
        } => {
            let (current, ..) = managed::avd_targets(&ini.path)?;
            if &current != avd {
                bail!("AVD registration changed since scan");
            }
            filesystem::validate(identity)?;
            filesystem::validate(ini)?;
        }
        Action::Docker {
            kind,
            context,
            endpoint,
            ..
        } => {
            if docker_args(kind).is_empty()
                || context.is_empty()
                || context.starts_with('-')
                || crate::docker::endpoint_key(endpoint).is_none()
            {
                bail!("Invalid Docker target");
            }
        }
        Action::DockerVolumes { names, .. }
            if names.iter().any(|s| !crate::docker::valid_volume_name(s)) =>
        {
            bail!("Invalid volume name");
        }
        _ => {}
    }
    Ok(())
}
fn command(exe: &Path, args: &[String]) -> Result<()> {
    process::run(exe, args, Duration::from_secs(300))?;
    Ok(())
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}
#[cfg(target_os = "macos")]
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
#[cfg(target_os = "macos")]
fn elevated(exe: &Path, arguments: &[String]) -> Result<()> {
    let shell = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(arguments.iter().cloned())
        .map(|s| quote(&s))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "do shell script {} with administrator privileges",
        serde_json::to_string(&shell)?
    );
    command(Path::new("/usr/bin/osascript"), &["-e".into(), script])
}
#[cfg(target_os = "linux")]
fn elevated(exe: &Path, arguments: &[String]) -> Result<()> {
    let pkexec = Path::new("/usr/bin/pkexec");
    if !pkexec.is_file() {
        bail!("Administrator access requires polkit (pkexec) and a running authentication agent");
    }
    let mut values = vec![
        "--disable-internal-agent".into(),
        exe.to_string_lossy().into_owned(),
    ];
    values.extend_from_slice(arguments);
    command(pkexec, &values)
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ElevatedTarget {
    pub target: Target,
    pub home: PathBuf,
}
pub fn elevated_target(request: &str) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("Internal deletion requires administrator authorization");
    }
    let request: ElevatedTarget = serde_json::from_str(request)?;
    filesystem::safe_cleanup_path_for(&request.target.path, &request.home)?;
    filesystem::measure(
        std::slice::from_ref(&request.target),
        &[],
        &AtomicBool::new(false),
    )?;
    removal::remove(&request.target)
}
fn delete(target: &Target) -> Result<()> {
    filesystem::measure(std::slice::from_ref(target), &[], &AtomicBool::new(false))?;
    if removal::writable_parent(&target.path) {
        match removal::remove(target) {
            Ok(()) => return Ok(()),
            Err(e)
                if e.chain().any(|e| {
                    e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
                }) => {}
            Err(e) => return Err(e),
        }
    }
    let request = ElevatedTarget {
        target: target.clone(),
        home: PathBuf::from(std::env::var_os("HOME").context("HOME missing")?),
    };
    elevated(
        &std::env::current_exe()?,
        &["--internal-delete".into(), serde_json::to_string(&request)?],
    )
}
fn stopped_simulator(udid: &str, path: &Path) -> Result<()> {
    let d = process::json(
        Path::new("/usr/bin/xcrun"),
        &["simctl", "list", "devices", "--json"],
    )?;
    let dev = managed::devices(&d)
        .into_iter()
        .find(|v| v["udid"] == udid)
        .context("Simulator disappeared; rescan")?;
    if dev["state"] != "Shutdown" || dev["dataPath"].as_str() != path.to_str() {
        bail!("Simulator state or location changed; rescan");
    }
    Ok(())
}
fn docker_endpoint(exe: &Path, context: &str, expected: &str) -> Result<()> {
    let current = managed::local_endpoint(&process::json(exe, &["context", "inspect", context])?)?;
    if current != expected {
        bail!("Docker endpoint changed; rescan");
    }
    Ok(())
}
pub fn execute(item: &Item, dry: bool) -> Result<()> {
    validate_plan(item)?;
    if dry {
        return Ok(());
    }
    match &item.action {
        Action::DeletePaths { targets }
        | Action::AgedFiles { targets, .. }
        | Action::AndroidSnapshots { targets, .. } => {
            if let Action::AndroidSnapshots { avd, .. } = &item.action
                && managed::android_busy(avd)
            {
                bail!("Stop Android emulators first");
            }
            if item.id == "simulator-caches" {
                let d = process::json(
                    Path::new("/usr/bin/xcrun"),
                    &["simctl", "list", "devices", "--json"],
                )?;
                for target in targets {
                    let dev = managed::devices(&d)
                        .into_iter()
                        .find(|d| {
                            d["dataPath"]
                                .as_str()
                                .is_some_and(|p| target.path.starts_with(p))
                        })
                        .context("Simulator cache owner disappeared")?;
                    if dev["state"] != "Shutdown" {
                        bail!("Stop simulators first");
                    }
                }
            }
            for target in targets {
                if let Action::AgedFiles { seconds, .. } = &item.action {
                    let m = fs::symlink_metadata(&target.path)?;
                    if [m.modified()?, m.accessed()?].iter().any(|t| {
                        !SystemTime::now()
                            .duration_since(*t)
                            .is_ok_and(|d| d > Duration::from_secs(*seconds))
                    }) {
                        bail!("Temporary file changed during cleanup; rescan");
                    }
                }
                delete(target).with_context(|| {
                    format!(
                        "Could not delete {}; operation may be partial, rescan",
                        target.path.display()
                    )
                })?;
            }
        }
        Action::UnifiedLogs { .. } => {
            elevated(Path::new("/usr/bin/log"), &args(&["erase", "--all"]))?
        }
        Action::TimeMachineSnapshot { date } => {
            let output =
                process::query(Path::new("/usr/bin/tmutil"), &["listlocalsnapshots", "/"])?;
            if !crate::stores::snapshot_items(&output)
                .iter()
                .any(|i| matches!(&i.action,Action::TimeMachineSnapshot{date:d} if d==date))
            {
                bail!("Snapshot no longer exists; rescan");
            }
            elevated(
                Path::new("/usr/bin/tmutil"),
                &args(&["deletelocalsnapshots", date]),
            )?;
        }
        Action::Simulator {
            udid,
            operation,
            data_path,
        } => {
            stopped_simulator(udid, data_path)?;
            command(
                Path::new("/usr/bin/xcrun"),
                &args(&["simctl", operation, udid]),
            )?;
        }
        Action::Runtime { id } => {
            let exe = Path::new("/usr/bin/xcrun");
            let r = process::json(exe, &["simctl", "runtime", "list", "--json"])?;
            if r[id]["deletable"] != true {
                bail!("Runtime no longer deletable; rescan");
            }
            let rid = r[id]["runtimeIdentifier"]
                .as_str()
                .context("Missing runtime identity")?;
            let d = process::json(exe, &["simctl", "list", "devices", "--json"])?;
            if d["devices"][rid]
                .as_array()
                .is_some_and(|ds| ds.iter().any(|d| d["state"] != "Shutdown"))
            {
                bail!("Stop runtime's simulators first");
            }
            command(exe, &args(&["simctl", "runtime", "delete", id]))?;
        }
        Action::AndroidAvd {
            executable,
            name,
            avd,
            identity,
            ini,
        } => {
            if managed::android_busy(avd) {
                bail!("Stop Android emulators first");
            }
            if let Some(exe) = executable {
                command(exe, &args(&["delete", "avd", "-n", name]))?;
            } else {
                delete(identity)?;
                delete(ini)?;
            }
        }
        Action::Docker {
            executable,
            context,
            endpoint,
            kind,
        } => {
            docker_endpoint(executable, context, endpoint)?;
            let mut a = args(&["--context", context]);
            a.extend(docker_args(kind).iter().map(|s| s.to_string()));
            command(executable, &a)?;
        }
        Action::DockerBuilder {
            executable,
            context,
            endpoint,
            builder,
        } => {
            docker_endpoint(executable, context, endpoint)?;
            crate::docker::validate_builder(executable, context, endpoint, builder)?;
            command(
                executable,
                &args(&[
                    "--context",
                    context,
                    "buildx",
                    "--builder",
                    builder,
                    "prune",
                    "--all",
                    "--force",
                    "--filter",
                    "until=72h",
                ]),
            )?;
        }
        Action::DockerVolumes {
            executable,
            context,
            endpoint,
            names,
        } => {
            docker_endpoint(executable, context, endpoint)?;
            for name in names {
                if !crate::docker::old_unused_volume(executable, context, name)? {
                    bail!("Volume is new or referenced; rescan");
                }
                command(
                    executable,
                    &args(&["--context", context, "volume", "rm", name]),
                )?;
            }
        }
        Action::InspectStorage { .. } => bail!("Inventory only"),
    }
    Ok(())
}
pub fn start(mut items: Vec<Item>, dry: bool) -> Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    items.sort_by_key(|i| match i.action {
        Action::Runtime { .. } => 2,
        Action::Simulator { .. } | Action::AndroidAvd { .. } => 1,
        _ => 0,
    });
    thread::spawn(move || {
        for item in items {
            let result = execute(&item, dry).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Event::Cleaned(item.id, result));
        }
        let _ = tx.send(Event::CleanupDone);
    });
    rx
}
