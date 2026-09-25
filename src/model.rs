use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Rebuild,
    Review,
    Destructive,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    AgedFiles {
        targets: Vec<Target>,
        seconds: u64,
    },
    DeletePaths {
        targets: Vec<Target>,
    },
    TimeMachineSnapshot {
        date: String,
    },
    DockerBuilder {
        executable: PathBuf,
        context: String,
        endpoint: String,
        builder: String,
    },
    InspectStorage {
        targets: Vec<Target>,
        guidance: String,
        allocated_bytes: u64,
    },
    UnifiedLogs {
        targets: Vec<Target>,
    },
    Simulator {
        udid: String,
        operation: String,
        data_path: PathBuf,
    },
    Runtime {
        id: String,
    },
    DockerVolumes {
        executable: PathBuf,
        context: String,
        endpoint: String,
        names: Vec<String>,
    },
    Docker {
        executable: PathBuf,
        context: String,
        endpoint: String,
        kind: String,
    },
    AndroidSnapshots {
        targets: Vec<Target>,
        avd: PathBuf,
    },
    AndroidAvd {
        executable: Option<PathBuf>,
        name: String,
        avd: PathBuf,
        identity: Target,
        ini: Target,
    },
}
impl Action {
    pub fn paths(&self) -> Vec<PathBuf> {
        match self {
            Self::AgedFiles { targets, .. }
            | Self::DeletePaths { targets }
            | Self::InspectStorage { targets, .. }
            | Self::UnifiedLogs { targets }
            | Self::AndroidSnapshots { targets, .. } => {
                targets.iter().map(|t| t.path.clone()).collect()
            }
            Self::AndroidAvd { avd, .. } => vec![avd.clone()],
            Self::Simulator { data_path, .. } => vec![data_path.clone()],
            _ => vec![],
        }
    }
    pub fn permanent(&self) -> bool {
        !matches!(self, Self::InspectStorage { .. })
    }
}

pub fn docker_args(kind: &str) -> Vec<&'static str> {
    match kind {
        "Build Cache" => vec!["builder", "prune", "-a", "-f", "--filter", "until=72h"],
        "Images" => vec!["image", "prune", "-a", "-f", "--filter", "until=72h"],
        "Containers" => vec!["container", "prune", "-f", "--filter", "until=72h"],
        _ => vec![],
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub category: String,
    pub name: String,
    pub detail: String,
    pub risk: Risk,
    pub bytes: Option<u64>,
    pub estimated: bool,
    pub files: u64,
    pub action: Action,
    #[serde(default)]
    pub excludes: Vec<PathBuf>,
}
impl Item {
    pub fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        q.is_empty()
            || format!(
                "{} {} {} {:?}",
                self.category,
                self.name,
                self.detail,
                self.action.paths()
            )
            .to_lowercase()
            .contains(&q)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub dry_run: bool,
    pub access_warning: Option<String>,
    pub items: Vec<Item>,
    pub notes: Vec<String>,
    pub roots: Vec<PathBuf>,
    pub elapsed_seconds: f64,
    pub complete: bool,
}
impl Default for Report {
    fn default() -> Self {
        Self {
            dry_run: true,
            access_warning: None,
            items: vec![],
            notes: vec![],
            roots: vec![],
            elapsed_seconds: 0.,
            complete: false,
        }
    }
}
impl Report {
    pub fn bytes(&self) -> u64 {
        self.items.iter().filter_map(|i| i.bytes).sum()
    }
}
pub fn size(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000. && u < 4 {
        v /= 1000.;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", units[u])
    }
}
pub fn selection_bytes(items: &[Item], selected: &HashSet<String>) -> u64 {
    items
        .iter()
        .filter(|i| selected.contains(&i.id))
        .filter_map(|i| i.bytes)
        .sum()
}
pub fn safe_text(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}

pub fn category(category: &str) -> &str {
    match category {
        "Build outputs" | "Dependencies" => "Projects",
        "Package managers" => "Developer caches",
        "Simulator devices" | "Simulator runtimes" => "Simulators",
        "Docker" => "Models & virtual machines",
        "Archives" => "Archives & backups",
        "Logs" => "System cleanup",
        other => other,
    }
}
