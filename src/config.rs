use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub roots: Vec<PathBuf>,
    pub build_outputs: Vec<PathBuf>,
    pub max_depth: usize,
    pub ignore: Vec<PathBuf>,
    pub docker_context: Option<String>,
    pub min_bytes: u64,
    #[serde(skip)]
    pub dry: bool,
    #[serde(skip)]
    pub home: PathBuf,
    #[serde(skip)]
    pub config_path: PathBuf,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec![],
            build_outputs: vec![],
            max_depth: 16,
            ignore: vec![],
            docker_context: None,
            min_bytes: 1_000_000,
            dry: false,
            home: PathBuf::new(),
            config_path: PathBuf::new(),
        }
    }
}
impl Config {
    pub fn load(roots: Vec<PathBuf>) -> Result<Self> {
        let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?).canonicalize()?;
        let path =
            crate::platform::xdg(&home, "XDG_CONFIG_HOME", ".config").join("cleanix/config.toml");
        let mut c: Self = match fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).context("Invalid cleanix config")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        c.home = home.clone();
        c.config_path = path;
        if !roots.is_empty() {
            c.roots = roots;
        }
        if c.roots.is_empty() {
            c.roots = vec![home.clone()];
        }
        c.roots = c.roots.iter().map(|p| expand(p, &home)).collect();
        c.build_outputs = c.build_outputs.iter().map(|p| expand(p, &home)).collect();
        c.ignore = c.ignore.iter().map(|p| expand(p, &home)).collect();
        if !(1..=64).contains(&c.max_depth) {
            bail!("max_depth must be between 1 and 64");
        }
        c.roots.sort();
        c.roots.dedup();
        Ok(c)
    }
    pub fn ignored(&self, path: &Path) -> bool {
        self.ignore.iter().any(|p| path.starts_with(p))
    }
}
fn expand(p: &Path, home: &Path) -> PathBuf {
    match p.to_str() {
        Some("~") => home.to_owned(),
        Some(s) if s.starts_with("~/") => home.join(&s[2..]),
        _ => p.to_owned(),
    }
}
