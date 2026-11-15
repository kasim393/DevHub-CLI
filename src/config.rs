use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const EXAMPLE: &str = include_str!("../config.example.toml");

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Command used by `,` to open this config file.
    pub editor: Option<String>,
    /// `"none"` makes the neon look see-through (for a terminal with acrylic blur).
    pub background: Option<String>,
    /// Folders scanned for projects: every child folder holding `marker` is one.
    pub roots: Vec<Root>,
    /// Projects added by hand, or overrides for discovered ones (matched by path).
    pub repos: Vec<RepoCfg>,
    /// Several commands started in order with one key.
    pub groups: Vec<GroupCfg>,
    /// Product sections, top to bottom: which projects (and groups) each one holds, in order.
    /// Projects named in no section stay in their category's section, below these.
    pub sections: Vec<SectionCfg>,
    /// Extra keys that open the selected project: key -> command (`{path}` is replaced).
    pub open: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Root {
    pub path: PathBuf,
    pub category: String,
    #[serde(default = "default_cmd")]
    pub cmd: String,
    #[serde(default = "default_marker")]
    pub marker: String,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct RepoCfg {
    pub name: Option<String>,
    pub path: PathBuf,
    pub cmd: Option<String>,
    pub category: Option<String>,
    pub port: Option<u16>,
    #[serde(default)]
    pub hidden: bool,
    /// `false` keeps the project out of its section; it still works in groups and `needs`.
    #[serde(default = "yes")]
    pub listed: bool,
    /// Projects (by name) started first when this one starts, unless already running.
    /// devhub waits for each one's `port` when it has one.
    #[serde(default)]
    pub needs: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GroupCfg {
    pub name: String,
    /// The section the group sits in (at its top, unless the section's `items` place it).
    pub section: Option<String>,
    pub steps: Vec<StepCfg>,
}

#[derive(Debug, Deserialize)]
pub struct SectionCfg {
    pub name: String,
    /// Project names and group names, in the order shown.
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct StepCfg {
    /// Use a project from the list (by name) instead of `path` + `cmd`: it is shared,
    /// so it never runs twice, and an already running one is reused.
    pub repo: Option<String>,
    pub name: Option<String>,
    pub path: Option<PathBuf>,
    pub cmd: Option<String>,
    /// Wait for this command to finish (and succeed) before the next step.
    #[serde(default)]
    pub wait_exit: bool,
    /// Wait until this port accepts connections before the next step.
    pub wait_port: Option<u16>,
    /// Port opened by `o` (detected from the output when absent).
    pub port: Option<u16>,
    /// Command run when the group is stopped (e.g. `docker compose stop`).
    pub stop: Option<String>,
}

fn default_cmd() -> String {
    "npm run dev".into()
}

fn yes() -> bool {
    true
}

fn default_marker() -> String {
    "package.json".into()
}

pub fn default_path() -> PathBuf {
    if let Ok(p) = std::env::var("DEVHUB_CONFIG") {
        return p.into();
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("devhub")
        .join("config.toml")
}

/// Loads the config, writing the example first when none exists.
/// Returns the config and whether the file was just created.
pub fn load(path: &Path) -> Result<(Config, bool)> {
    let mut created = false;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, EXAMPLE)?;
        created = true;
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut cfg: Config =
        toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
    for r in &mut cfg.roots {
        r.path = expand(&r.path);
    }
    for r in &mut cfg.repos {
        r.path = expand(&r.path);
    }
    for s in cfg.groups.iter_mut().flat_map(|g| g.steps.iter_mut()) {
        if let Some(p) = &mut s.path {
            *p = expand(p);
        }
    }
    Ok((cfg, created))
}

/// Replaces a leading `~` with the home folder.
fn expand(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match (s.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => p.to_path_buf(),
    }
}
