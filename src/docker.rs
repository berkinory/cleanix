use crate::{
    config::Config,
    managed::{item, local_endpoint, parse_docker_size},
    model::{Action, Item},
    process,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

pub fn endpoint_key(endpoint: &str) -> Option<String> {
    let path = Path::new(endpoint.strip_prefix("unix://")?);
    if !path.is_absolute() {
        return None;
    }
    Some(
        path.canonicalize()
            .unwrap_or_else(|_| path.to_owned())
            .display()
            .to_string(),
    )
}
pub fn contexts(output: &str) -> Result<Vec<(String, String)>> {
    let mut seen = HashSet::new();
    let mut result = vec![];
    for line in output.lines() {
        let v: Value = serde_json::from_str(line)?;
        let name = v["Name"].as_str().context("Missing context name")?;
        let endpoint = v["DockerEndpoint"].as_str().unwrap_or("");
        if name.is_empty() || name.starts_with('-') {
            continue;
        }
        if let Some(key) = endpoint_key(endpoint)
            && seen.insert(key)
        {
            result.push((name.into(), endpoint.into()));
        }
    }
    Ok(result)
}
pub fn df_items(output: &str, exe: &Path, context: &str, endpoint: &str) -> Result<Vec<Item>> {
    let mut totals = HashMap::<String, u64>::new();
    for line in output.lines() {
        let v: Value = serde_json::from_str(line)?;
        let kind = v["Type"].as_str().context("Missing Docker resource kind")?;
        if !["Build Cache", "Images", "Containers", "Local Volumes"].contains(&kind) {
            continue;
        }
        let bytes = parse_docker_size(v["Reclaimable"].as_str().unwrap_or(""))
            .context("Unrecognized Docker reclaimable size")?;
        *totals.entry(kind.into()).or_default() += bytes;
    }
    let mut result = vec![];
    for (kind, bytes) in totals {
        if bytes == 0 {
            continue;
        }
        if kind == "Local Volumes" {
            continue;
        }
        let (name, detail) = match kind.as_str() {
            "Build Cache" => (
                "Build cache",
                "Unused build layers; subsequent builds regenerate them.",
            ),
            "Images" => (
                "Unused images",
                "Images unreferenced by containers. Local-only images may be irreplaceable.",
            ),
            "Containers" => (
                "Stopped containers",
                "Stopped containers and writable layers; their data is lost.",
            ),
            _ => (
                "Unused volumes",
                "Unattached named and anonymous volumes; these can contain databases.",
            ),
        };
        result.push(item("Docker",format!("{context} · {name}"),format!("{detail} Local endpoint: {endpoint}. Only unused resources older than 72 hours are removed. Age-filtered reclaimable size is unavailable."),None,Action::Docker{executable:exe.into(),context:context.into(),endpoint:endpoint.into(),kind}));
    }
    Ok(result)
}

pub fn discover(c: &Config) -> (Vec<Item>, Vec<String>) {
    let Some(exe) = process::which("docker") else {
        return (vec![], vec![]);
    };
    let mut notes = vec![];
    let candidates = (|| -> Result<Vec<(String, String)>> {
        if let Some(context) = &c.docker_context {
            if context.is_empty() || context.starts_with('-') {
                bail!("Invalid context");
            }
            let endpoint = local_endpoint(&process::json(&exe, &["context", "inspect", context])?)?;
            Ok(vec![(context.clone(), endpoint)])
        } else {
            contexts(&process::query(
                &exe,
                &["context", "ls", "--format", "{{json .}}"],
            )?)
        }
    })();
    let candidates = match candidates {
        Ok(c) => c,
        Err(e) => return (vec![], vec![format!("Docker context inventory: {e}")]),
    };
    let mut result = vec![];
    let mut daemon_ids = HashSet::new();
    let mut live = HashMap::new();
    for (context, _) in candidates {
        let scan = (|| -> Result<(String, String, Vec<Item>)> {
            let endpoint =
                local_endpoint(&process::json(&exe, &["context", "inspect", &context])?)?;
            let id = process::query(
                &exe,
                &["--context", &context, "info", "--format", "{{.ID}}"],
            )?;
            if id.is_empty() {
                bail!("Missing daemon identity");
            }
            if daemon_ids.contains(&id) {
                return Ok((endpoint, id, vec![]));
            }
            let output = process::query(
                &exe,
                &[
                    "--context",
                    &context,
                    "system",
                    "df",
                    "--format",
                    "{{json .}}",
                ],
            )?;
            Ok((
                endpoint.clone(),
                id,
                df_items(&output, &exe, &context, &endpoint)?,
            ))
        })();
        match scan {
            Ok((endpoint, id, items)) => {
                daemon_ids.insert(id);
                live.insert(context, endpoint);
                result.extend(items);
            }
            Err(e) => notes.push(format!(
                "Docker {context} unavailable; no prune estimate: {e}"
            )),
        }
    }
    for (context, endpoint) in &live {
        match old_volumes(&exe,context) {
            Ok(names) if !names.is_empty()=>result.push(item("Docker",format!("{context} · Unused volumes"),"Unattached volumes created over 72 hours ago. Docker does not expose a last-used timestamp. Volumes can contain databases; review before deleting.".into(),None,Action::DockerVolumes{executable:exe.clone(),context:context.clone(),endpoint:endpoint.clone(),names})),
            Ok(_)=>{},Err(e)=>notes.push(format!("Docker {context} volume inventory: {e}")),
        }
    }
    let docker_home = std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| c.home.join(".docker"));
    let instances = docker_home.join("buildx/instances");
    let mut seen_builders = HashSet::new();
    for entry in fs::read_dir(instances).into_iter().flatten().flatten() {
        let Ok(data) = fs::read(entry.path()) else {
            continue;
        };
        let Ok(builder) = serde_json::from_slice::<Value>(&data) else {
            continue;
        };
        let Some((name, context, endpoint)) = local_builder(&builder, &live) else {
            continue;
        };
        if !seen_builders.insert(name.clone()) {
            continue;
        }
        match process::query(&exe,&["--context",&context,"buildx","--builder",&name,"du","--filter","until=72h","--format=json"]) {
            Ok(output)=>match buildx_private_bytes(&output) {
                Ok(bytes) if bytes>0=>result.push(item("Docker",format!("{context} · builder {name}"),"Unused private BuildKit cache older than 72 hours; shared image layers are excluded to avoid double counting.".into(),Some(bytes),Action::DockerBuilder{executable:exe.clone(),context,endpoint,builder:name})),
                Ok(_)=>{},Err(e)=>notes.push(format!("Builder {name}: {e}")),
            },
            Err(e)=>notes.push(format!("Builder {name} unavailable: {e}")),
        }
    }
    (result, notes)
}
pub fn local_builder(
    v: &Value,
    live: &HashMap<String, String>,
) -> Option<(String, String, String)> {
    if v["Driver"] != "docker-container" {
        return None;
    }
    let name = v["Name"].as_str()?;
    if name.is_empty() || name.starts_with('-') {
        return None;
    }
    let nodes = v["Nodes"].as_array()?;
    if nodes.len() != 1 {
        return None;
    }
    let node = nodes[0]["Endpoint"].as_str()?;
    let (context, endpoint) = live.iter().find(|(c, e)| {
        node == c.as_str()
            || (endpoint_key(node).is_some() && endpoint_key(node) == endpoint_key(e))
    })?;
    Some((name.into(), context.clone(), endpoint.clone()))
}
pub fn buildx_private_bytes(output: &str) -> Result<u64> {
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for line in output.lines() {
        let v: Value = serde_json::from_str(line)?;
        if v["Reclaimable"] != true || v["Shared"] != false {
            continue;
        }
        let id = v["ID"].as_str().context("Missing cache record identity")?;
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let bytes = v["Size"]
            .as_u64()
            .or_else(|| v["Size"].as_str().and_then(|s| s.parse().ok()))
            .context("Invalid BuildKit byte count")?;
        total = total.checked_add(bytes).context("BuildKit size overflow")?;
    }
    Ok(total)
}

pub fn valid_volume_name(name: &str) -> bool {
    !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
pub fn older_than_72h(created: &str, now: i64) -> bool {
    time::OffsetDateTime::parse(created, &time::format_description::well_known::Rfc3339)
        .is_ok_and(|t| now.saturating_sub(t.unix_timestamp()) > 72 * 3600)
}
pub fn old_unused_volume(exe: &Path, context: &str, name: &str) -> Result<bool> {
    if !valid_volume_name(name) {
        bail!("Invalid volume name");
    }
    let output = process::query(
        exe,
        &[
            "--context",
            context,
            "volume",
            "ls",
            "--filter",
            "dangling=true",
            "--format",
            "{{.Name}}",
        ],
    )?;
    if !output.lines().any(|s| s == name) {
        return Ok(false);
    }
    let v = process::json(exe, &["--context", context, "volume", "inspect", name])?;
    let created = v[0]["CreatedAt"]
        .as_str()
        .context("Missing volume creation date")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    Ok(older_than_72h(created, now))
}
fn old_volumes(exe: &Path, context: &str) -> Result<Vec<String>> {
    let output = process::query(
        exe,
        &[
            "--context",
            context,
            "volume",
            "ls",
            "--filter",
            "dangling=true",
            "--format",
            "{{.Name}}",
        ],
    )?;
    let mut names = vec![];
    for name in output.lines() {
        if old_unused_volume(exe, context, name)? {
            names.push(name.into());
        }
    }
    Ok(names)
}
pub fn validate_builder(_exe: &Path, context: &str, endpoint: &str, name: &str) -> Result<()> {
    if !valid_volume_name(name) {
        bail!("Invalid builder name");
    }
    let root = std::env::var_os("DOCKER_CONFIG")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".docker")))
        .context("Docker configuration unavailable")?;
    let v: Value = serde_json::from_slice(&fs::read(root.join("buildx/instances").join(name))?)?;
    let live = HashMap::from([(context.into(), endpoint.into())]);
    if local_builder(&v, &live).is_none_or(|(n, _, _)| n != name) {
        bail!("Builder endpoint changed; rescan");
    }
    Ok(())
}
