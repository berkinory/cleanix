use crate::{
    config::Config,
    filesystem,
    model::{Action, Item, Risk},
    process,
};
use anyhow::{Result, bail};
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

pub(crate) fn item(
    category: &str,
    name: String,
    detail: String,
    bytes: Option<u64>,
    action: Action,
) -> Item {
    Item {
        id: format!("{category}:{}", serde_json::to_string(&action).unwrap()),
        category: crate::model::category(category).into(),
        name,
        detail,
        risk: Risk::Destructive,
        bytes,
        estimated: true,
        files: 0,
        action,
        excludes: vec![],
    }
}
pub fn valid_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
pub fn devices(json: &Value) -> Vec<&Value> {
    json.get("devices")
        .and_then(Value::as_object)
        .map(|m| m.values().filter_map(Value::as_array).flatten().collect())
        .unwrap_or_default()
}
pub fn sim_items(device_json: &Value, runtime_json: &Value) -> Vec<Item> {
    let mut result = vec![];
    if let Some(runtimes) = device_json.get("devices").and_then(Value::as_object) {
        for (runtime, devs) in runtimes {
            for d in devs.as_array().into_iter().flatten() {
                let Some(id) = d["udid"].as_str().filter(|s| valid_uuid(s)) else {
                    continue;
                };
                if d["state"] != "Shutdown" {
                    continue;
                }
                let Some(data_path) = d["dataPath"]
                    .as_str()
                    .filter(|p| Path::new(p).is_absolute())
                else {
                    continue;
                };
                let name = d["name"].as_str().unwrap_or("Simulator");
                let runtime = runtime
                    .trim_start_matches("com.apple.CoreSimulator.SimRuntime.")
                    .replace('-', " ");
                let available = d["isAvailable"].as_bool() == Some(true);
                let bytes = d["dataPathSize"].as_u64();
                if available && bytes.is_some_and(|n| n < 100 * 1024 * 1024) {
                    continue;
                }
                let verb = if available {
                    "Reset"
                } else {
                    "Remove unavailable"
                };
                result.push(item("Simulator devices",format!("{name} · {runtime}"),format!("{verb} this device using simctl. Installed apps and their data are permanently lost. The device must remain shut down.{}",if available{" The device itself is retained."}else{" Its runtime is unavailable."}),bytes,Action::Simulator{udid:id.into(),operation:if available{"erase"}else{"delete"}.into(),data_path:PathBuf::from(data_path)}));
            }
        }
    }
    if let Some(runtimes) = runtime_json.as_object() {
        for (id, r) in runtimes {
            if !valid_uuid(id) || r["deletable"] != true {
                continue;
            }
            let runtime_id = r["runtimeIdentifier"].as_str().unwrap_or("");
            let devs = device_json["devices"][runtime_id].as_array();
            if devs.is_some_and(|ds| ds.iter().any(|d| d["state"] != "Shutdown")) {
                continue;
            }
            let Some(version) = r["version"].as_str() else {
                continue;
            };
            let platform = r["platformIdentifier"].as_str().unwrap_or("");
            let platform = if platform.contains("iphone") {
                "iOS"
            } else if platform.contains("watch") {
                "watchOS"
            } else if platform.contains("appletv") {
                "tvOS"
            } else if platform.contains("xr") {
                "visionOS"
            } else {
                "Simulator"
            };
            let count = devs.map_or(0, Vec::len);
            result.push(item("Simulator runtimes",format!("{platform} {version} · {}",r["build"].as_str().unwrap_or("")),format!("Uninstall this runtime with simctl. {count} configured devices reference it and will need the runtime reinstalled to boot. Last used: {}.",r["lastUsedAt"].as_str().unwrap_or("unknown")),r["sizeBytes"].as_u64(),Action::Runtime{id:id.clone()}));
        }
    }
    result
}
pub fn simulators(_c: &Config, cancel: &AtomicBool) -> (Vec<Item>, Vec<String>) {
    let exe = Path::new("/usr/bin/xcrun");
    let mut notes = vec![];
    let d = match process::json(exe, &["simctl", "list", "devices", "--json"]) {
        Ok(d) => d,
        Err(e) => return (vec![], vec![format!("iOS simulators not scanned: {e}")]),
    };
    let r = match process::json(exe, &["simctl", "runtime", "list", "--json"]) {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("iOS runtimes not scanned: {e}"));
            Value::Null
        }
    };
    let mut result = sim_items(&d, &r);
    match process::json(exe, &["simctl", "runtime", "match", "list", "-j"]) {
        Ok(matches) => {
            for item in &mut result {
                if let Action::Runtime { id } = &item.action
                    && runtime_unused(&d, &r[id], &matches)
                {
                    item.name.push_str(" · unused");
                    item.detail.push_str(
                        " No configured device or SDK runtime match references this build.",
                    );
                }
            }
        }
        Err(e) => notes.push(format!("Runtime usage classification unavailable: {e}")),
    }
    let mut cache_targets = vec![];
    let mut cache_size = 0;
    for dev in devices(&d) {
        if dev["state"] != "Shutdown" {
            notes.push(format!(
                "Running simulator skipped: {}",
                dev["name"].as_str().unwrap_or("unknown")
            ));
            continue;
        }
        if dev["isAvailable"] != true {
            continue;
        }
        let Some(path) = dev["dataPath"].as_str() else {
            continue;
        };
        let mut device_bytes = 0;
        for relative in ["Library/Caches", "tmp"] {
            if let Ok(t) = filesystem::target(&Path::new(path).join(relative)) {
                match filesystem::measure(std::slice::from_ref(&t), &[], cancel) {
                    Ok(m) => {
                        device_bytes += m.bytes;
                        cache_targets.push(t)
                    }
                    Err(e) => notes.push(format!("Simulator cache skipped: {e}")),
                }
            }
        }
        cache_size += device_bytes;
        if let Some(i)=result.iter_mut().find(|i|matches!(&i.action,Action::Simulator{udid,..} if Some(udid.as_str())==dev["udid"].as_str())){i.bytes=i.bytes.map(|b|b.saturating_sub(device_bytes));}
    }
    if !cache_targets.is_empty() && cache_size > 0 {
        let mut i=item("Simulator devices","Device caches".into(),"Caches and temporary files in shut-down iOS simulators. Rebuilt on boot. Running devices are excluded; cleanup verifies they remain shut down.".into(),Some(cache_size),Action::DeletePaths{targets:cache_targets});
        i.id = "simulator-caches".into();
        i.risk = Risk::Rebuild;
        i.estimated = false;
        result.push(i);
    }
    (result, notes)
}
pub fn local_endpoint(json: &Value) -> Result<String> {
    let endpoint = json[0]["Endpoints"]["docker"]["Host"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Docker endpoint missing"))?;
    if !endpoint.starts_with("unix:///") {
        bail!("Remote Docker endpoints are excluded")
    }
    Ok(endpoint.into())
}
pub fn parse_docker_size(text: &str) -> Option<u64> {
    let s = text.split_whitespace().next()?;
    let split = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let n: f64 = s[..split].parse().ok()?;
    let scale = match &s[split..] {
        "B" | "" => 1.,
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((n * scale) as u64)
}
pub fn android_busy(_path: &Path) -> bool {
    process::running(&["emulator", "qemu-system"]).unwrap_or(true)
}

fn sdk_tool(sdk: &Path, name: &str) -> Option<PathBuf> {
    let preferred = sdk.join("cmdline-tools/latest/bin").join(name);
    if preferred.is_file() {
        return Some(preferred);
    }
    let mut dirs = filesystem::directories(&sdk.join("cmdline-tools")).unwrap_or_default();
    dirs.reverse();
    dirs.into_iter()
        .map(|d| d.join("bin").join(name))
        .find(|p| p.is_file())
        .or_else(|| process::which(name))
}
pub fn android(c: &Config, cancel: &AtomicBool) -> (Vec<Item>, Vec<String>) {
    let sdk = env::var_os("ANDROID_HOME")
        .or_else(|| env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            c.home.join(if cfg!(target_os = "macos") {
                "Library/Android/sdk"
            } else {
                "Android/Sdk"
            })
        });
    let avd_home = env::var_os("ANDROID_AVD_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("ANDROID_USER_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| c.home.join(".android"))
                .join("avd")
        });
    let mut notes = vec![];
    let mut result = vec![];
    let avdm = sdk_tool(&sdk, "avdmanager");
    if let Ok(entries) = fs::read_dir(&avd_home) {
        for entry in entries.flatten() {
            let ini = entry.path();
            if ini.extension().is_none_or(|e| e != "ini") {
                continue;
            }
            let (path, identity, ini_identity) = match avd_targets(&ini) {
                Ok(v) => v,
                Err(e) => {
                    notes.push(format!("Skipped AVD registration {}: {e}", ini.display()));
                    continue;
                }
            };
            if c.ignored(&path) {
                continue;
            }
            let name = ini
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let display = fs::read_to_string(path.join("config.ini"))
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find_map(|l| l.strip_prefix("avd.ini.displayname=").map(str::to_owned))
                })
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| name.clone())
                .replace('_', " ");
            if android_busy(&path) {
                notes.push(format!("Android device {name} is locked; skipped"));
                continue;
            }
            let snapshots = path.join("snapshots");
            let mut snapshot_size = 0;
            if let Ok(t) = filesystem::target(&snapshots)
                && let Ok(m) = filesystem::measure(std::slice::from_ref(&t), &[], cancel)
            {
                snapshot_size = m.bytes;
                if m.bytes > 0 {
                    let mut i=item("Simulator devices",format!("{display} · snapshots"),"Android quick-boot snapshots. Emulator must remain stopped; the next start cold-boots.".into(),Some(m.bytes),Action::AndroidSnapshots{targets:vec![t],avd:path.clone()});
                    i.risk = Risk::Rebuild;
                    i.estimated = false;
                    result.push(i)
                }
            }
            match filesystem::measure(std::slice::from_ref(&identity), &[], cancel) {
                Ok(m) => result.push(item("Simulators", format!("{display} · Android device"), "Remove this stopped AVD and its registration. Installed apps and user data are lost. Snapshot bytes are listed separately; Android SDK installations are excluded.".into(), Some(m.bytes.saturating_sub(snapshot_size)), Action::AndroidAvd { executable: avdm.clone(), name, avd:path, identity, ini:ini_identity })),
                Err(e) => notes.push(format!("Android device {name}: {e}")),
            }
        }
    }
    (result, notes)
}

pub fn runtime_unused(devices: &Value, runtime: &Value, matches: &Value) -> bool {
    let (Some(devices), Some(matches), Some(build), Some(id)) = (
        devices["devices"].as_object(),
        matches.as_object(),
        runtime["build"].as_str(),
        runtime["runtimeIdentifier"].as_str(),
    ) else {
        return false;
    };
    if matches.is_empty()
        || matches
            .values()
            .any(|m| m["chosenRuntimeBuild"].as_str().is_none_or(|b| b == build))
    {
        return false;
    }
    devices
        .get(id)
        .is_none_or(|d| d.as_array().is_some_and(|d| d.is_empty()))
}

pub fn avd_targets(ini: &Path) -> Result<(PathBuf, crate::model::Target, crate::model::Target)> {
    if ini.extension().is_none_or(|e| e != "ini") {
        bail!("Not an AVD registration");
    }
    let content = fs::read_to_string(ini)?;
    let locations: Vec<_> = content
        .lines()
        .filter_map(|l| l.strip_prefix("path="))
        .collect();
    if locations.len() != 1 {
        bail!("Missing or ambiguous AVD path");
    }
    let path = PathBuf::from(locations[0]);
    if path.extension().is_none_or(|e| e != "avd") || !path.join("config.ini").is_file() {
        bail!("Not a recognized AVD directory");
    }
    filesystem::safe_cleanup_path(&path)?;
    filesystem::safe_cleanup_path(ini)?;
    Ok((
        path.clone(),
        filesystem::target(&path)?,
        filesystem::target(ini)?,
    ))
}
