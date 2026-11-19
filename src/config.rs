use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::Config;

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lbreco")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("lbreco")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn history_path() -> PathBuf {
    cache_dir().join("history.json")
}

pub fn playlist_path() -> PathBuf {
    config_dir().join("playlist.json")
}

pub fn load() -> Config {
    let path = config_path();
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|error| {
            eprintln!("lbreco: could not parse {}: {error}", path.display());
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn save(config: &Config) -> Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = config_path();
    let raw = serde_json::to_string_pretty(config)?;
    fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))?;
    set_private_permissions(&path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub mbid: String,
    pub generated_at: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    pub fn load() -> Self {
        fs::read_to_string(history_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn recent_mbids(&self, days: u32) -> std::collections::HashSet<String> {
        let cutoff = if days == 0 {
            None
        } else {
            Some(chrono::Utc::now() - chrono::Duration::days(i64::from(days)))
        };
        self.entries
            .iter()
            .filter_map(|entry| {
                if let Some(cutoff) = cutoff {
                    let timestamp =
                        chrono::DateTime::parse_from_rfc3339(&entry.generated_at).ok()?;
                    if timestamp < cutoff {
                        return None;
                    }
                }
                Some(entry.mbid.clone())
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&self) -> Result<()> {
        let path = history_path();
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }

    pub fn remember(&mut self, mbids: impl IntoIterator<Item = String>) {
        let now = chrono::Utc::now().to_rfc3339();
        let existing: std::collections::HashSet<String> = self
            .entries
            .iter()
            .map(|entry| entry.mbid.clone())
            .collect();
        for mbid in mbids {
            if !existing.contains(&mbid) {
                self.entries.push(HistoryEntry {
                    mbid,
                    generated_at: now.clone(),
                });
            }
        }
        // Keep the file small.
        if self.entries.len() > 2000 {
            let excess = self.entries.len() - 2000;
            self.entries.drain(0..excess);
        }
    }

    pub fn save(&self) -> Result<()> {
        let dir = cache_dir();
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = history_path();
        fs::write(&path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
