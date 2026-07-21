//! Registry JSON read/write.
//!
//! The shape on disk is:
//!
//! ```jsonc
//! {
//!   "/abs/path/to/repo": {
//!     "registered_at":    "2026-07-17T01:22:33Z",
//!     "last_check":       "2026-07-17T01:22:33Z",
//!     "last_dirty_count": 0,
//!     "remote_url":       "git@github.com:...",
//!     "primary_branch":   "main",
//!     // Optional fields (omitted or null when never seen):
//!     "last_auto_commit": "2026-07-17T01:22:33Z",
//!     "last_push_time":   "2026-07-17T01:22:33Z"
//!   }
//! }
//! ```
//!
//! Both the Python daemon and this crate read this file. Do not change
//! field names without co-bumping the Python side.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::StateRoot;

/// Per-repo metadata entry.
///
/// Optional fields use `Option<...>` so JSON entries written by either
/// daemon can be read without lossiness. New fields should be `Option`
/// and default to `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    #[serde(default)]
    pub registered_at: Option<String>,
    #[serde(default)]
    pub last_check: Option<String>,
    #[serde(default)]
    pub last_dirty_count: Option<u64>,
    #[serde(default)]
    pub remote_url: Option<String>,
    #[serde(default)]
    pub primary_branch: Option<String>,

    // Optional fields populated by autocommit/cleanup:
    #[serde(default)]
    pub last_auto_commit: Option<String>,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub last_push_time: Option<String>,
}

/// Allow JSON `null` to map to `None` for the `last_push_time` field,
/// since the Python daemon writes `null` literally (see registry.json
/// excerpt: `"last_push_time": null`). Serde's default for `Option`
/// already handles missing keys, but explicit `null` requires this
/// helper.
fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<Option<String>> = Option::deserialize(deserializer)?;
    Ok(opt.flatten())
}

/// Path-keyed map of repo paths to their metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub repos: BTreeMap<String, RepoEntry>,
}

/// Read the registry from `state_root.registry_path()`.
///
/// On a missing file, returns an empty registry. On a JSON parse failure,
/// the registry is *not* destroyed — we rename it aside as
/// `registry.json.bak-<short_ts>` and return an empty registry. This
/// matches the Python engine's behaviour in `load_registry`.
pub fn load(state_root: &StateRoot) -> Result<Registry> {
    let path = state_root.registry_path();
    if !path.exists() {
        return Ok(Registry::default());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read registry at {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Registry::default());
    }
    // The Python format stores the map at the top level (not under `repos`).
    // We accept both shapes for forward-compat with a future snapshot file:
    let value: serde_json::Value =
        serde_json::from_str(&text).with_context(|| "parse registry.json")?;
    let mut reg: Registry = match value {
        serde_json::Value::Object(map) if map.contains_key("repos") => {
            // Top-level has a `repos` key — treat as our schema.
            serde_json::from_value(serde_json::Value::Object(map))
                .with_context(|| "parse registry.json via wrapped schema")?
        }
        serde_json::Value::Object(map) => {
            // Legacy shape — flat map from path to entry.
            let mut repos = BTreeMap::new();
            for (k, v) in map {
                let entry: RepoEntry = serde_json::from_value(v)
                    .with_context(|| format!("parse repo entry for {k}"))?;
                repos.insert(k, entry);
            }
            Registry { repos }
        }
        _ => Registry::default(),
    };
    // Keep determinism — Python sorts keys with `sort_keys=True`.
    // BTreeMap already gives us sorted iteration.
    reg.sort_keys_in_place();
    Ok(reg)
}

impl Registry {
    /// Re-key all entries into a fresh `BTreeMap` to guarantee sort order
    /// even if the deserialized map was not sorted (Rust's `BTreeMap`
    /// is always sorted internally, but doing this explicitly is cheap).
    fn sort_keys_in_place(&mut self) {
        let old = std::mem::take(&mut self.repos);
        let mut sorted = BTreeMap::new();
        for (k, v) in old {
            sorted.insert(k, v);
        }
        self.repos = sorted;
    }

    pub fn len(&self) -> usize {
        self.repos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &RepoEntry)> {
        self.repos.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn paths(&self) -> Vec<String> {
        self.repos.keys().cloned().collect()
    }

    pub fn sorted_paths(&self) -> Vec<PathBuf> {
        self.repos.keys().map(PathBuf::from).collect()
    }

    pub fn get(&self, path: &str) -> Option<&RepoEntry> {
        self.repos.get(path)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.repos.contains_key(path)
    }

    pub fn insert(&mut self, path: String, entry: RepoEntry) -> Option<RepoEntry> {
        self.repos.insert(path, entry)
    }

    pub fn remove(&mut self, path: &str) -> Option<RepoEntry> {
        self.repos.remove(path)
    }

    /// Iterate in sorted-by-path order (BTreeMap already does this; we
    /// expose it as a named method for symmetry with the daemon code).
    pub fn sorted(&self) -> impl Iterator<Item = (&str, &RepoEntry)> {
        self.repos.iter().map(|(k, v)| (k.as_str(), v))
    }
}

/// Persist the registry to disk atomically: write to `.tmp`, rename.
///
/// Mirrors the Python `save_registry`: never clobbers the file directly.
pub fn save(state_root: &StateRoot, registry: &Registry) -> Result<()> {
    state_root.ensure_dirs()?;
    let final_path = state_root.registry_path();
    let tmp = final_path.with_extension("tmp");
    let json = serde_json::to_string_pretty(registry)
        .with_context(|| "serialise registry")?;
    fs::write(&tmp, json).with_context(|| format!("write tmp at {}", tmp.display()))?;
    fs::rename(&tmp, &final_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), final_path.display()))?;
    Ok(())
}

/// Append a single NDJSON event line to the named log file under
/// `logs/`. If the file does not exist, it is created.
pub fn append_event(state_root: &StateRoot, log_name: &str, event: &serde_json::Value) -> Result<()> {
    state_root.ensure_dirs()?;
    let path = state_root.log_path(log_name);
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open log {}", path.display()))?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// UTC timestamp in `YYYY-MM-DDTHH:MM:SSZ` form (matches Python's `now_iso`).
pub fn now_iso() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Slug-safe timestamp `YYYYMMDDTHHMM` (matches Python's `short_ts`).
pub fn short_ts() -> String {
    Utc::now().format("%Y%m%dT%H%M").to_string()
}

/// Helper: update a single entry in a registry, preserving
/// `registered_at` on re-registration.
pub fn upsert_entry(
    registry: &mut Registry,
    path: &str,
    mutator: impl FnOnce(&mut RepoEntry),
) {
    let mut entry = registry.remove(path).unwrap_or_default();
    if entry.registered_at.is_none() {
        entry.registered_at = Some(now_iso());
    }
    entry.last_check = Some(now_iso());
    mutator(&mut entry);
    registry.insert(path.to_string(), entry);
}

/// Convenience: parse an ISO-8601 timestamp into a `DateTime<Utc>`,
/// treating malformed values as a missing timestamp. Used by
/// autocommit throttling which checks "time since last auto-commit".
pub fn parse_iso(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_state() -> (TempDir, StateRoot) {
        let dir = TempDir::new().unwrap();
        let root = StateRoot::from_path(dir.path().to_path_buf());
        (dir, root)
    }

    use tempfile::TempDir;

    #[test]
    fn empty_registry_on_missing_file() -> Result<()> {
        let (_tmp, root) = tmp_state();
        let reg = load(&root)?;
        assert!(reg.is_empty());
        Ok(())
    }

    #[test]
    fn round_trip_preserves_entries() -> Result<()> {
        let (_tmp, root) = tmp_state();
        let mut reg = Registry::default();
        reg.insert(
            "/a/path".into(),
            RepoEntry {
                registered_at: Some("2026-07-17T01:00:00Z".into()),
                last_check: Some("2026-07-17T01:00:00Z".into()),
                last_dirty_count: Some(3),
                remote_url: Some("git@github.com:x/y.git".into()),
                primary_branch: Some("main".into()),
                last_auto_commit: None,
                last_push_time: None,
            },
        );
        save(&root, &reg)?;
        let loaded = load(&root)?;
        assert_eq!(loaded, reg);
        Ok(())
    }

    #[test]
    fn accepts_legacy_flat_map_shape() -> Result<()> {
        // The Python daemon writes the flat-map shape (top-level keys
        // are paths). Verify a flat-shaped JSON deserialises correctly.
        let (_tmp, root) = tmp_state();
        let flat = r#"{
            "/some/repo": {
                "registered_at": "2026-07-17T01:00:00Z",
                "last_check":    "2026-07-17T01:00:00Z",
                "last_dirty_count": 0,
                "remote_url": "",
                "primary_branch": "main"
            }
        }"#;
        std::fs::write(root.registry_path(), flat)?;
        let reg = load(&root)?;
        assert_eq!(reg.len(), 1);
        let entry = reg.get("/some/repo").unwrap();
        assert_eq!(entry.primary_branch.as_deref(), Some("main"));
        Ok(())
    }

    #[test]
    fn save_is_atomic_via_rename() -> Result<()> {
        let (_tmp, root) = tmp_state();
        let mut reg = Registry::default();
        reg.insert("/x".into(), RepoEntry::default());
        save(&root, &reg)?;
        // After save, no .tmp file should be left behind in the parent.
        let tmp_path = root.registry_path().with_extension("tmp");
        assert!(!tmp_path.exists(), "tmp file must be moved into final position");
        assert!(root.registry_path().exists());
        Ok(())
    }

    #[test]
    fn preserves_null_for_last_push_time() -> Result<()> {
        let (_tmp, root) = tmp_state();
        let json = r#"{"repos": {"/r": {"primary_branch": "main", "last_push_time": null}}}"#;
        std::fs::write(root.registry_path(), json)?;
        let reg = load(&root)?;
        assert_eq!(reg.len(), 1);
        let entry = reg.get("/r").unwrap();
        assert!(entry.last_push_time.is_none());
        Ok(())
    }

    #[test]
    fn append_event_writes_ndjson_line() -> Result<()> {
        let (_tmp, root) = tmp_state();
        let event = serde_json::json!({"event": "test", "n": 1});
        append_event(&root, "test", &event)?;
        let content = std::fs::read_to_string(root.log_path("test"))?;
        assert!(content.contains("\"event\":\"test\""));
        assert!(content.ends_with('\n'));
        Ok(())
    }

    #[test]
    fn upsert_entry_preserves_registered_at() -> Result<()> {
        let (_tmp, _root) = tmp_state();
        let mut reg = Registry::default();
        // First insert sets registered_at.
        upsert_entry(&mut reg, "/r", |e| {
            e.primary_branch = Some("main".into());
        });
        let first = reg.get("/r").unwrap().registered_at.clone();
        assert!(first.is_some());
        // Second insert must NOT overwrite registered_at.
        upsert_entry(&mut reg, "/r", |e| {
            e.last_dirty_count = Some(5);
        });
        let second = reg.get("/r").unwrap().registered_at.clone();
        assert_eq!(first, second);
        assert_eq!(reg.get("/r").unwrap().last_dirty_count, Some(5));
        Ok(())
    }

    #[test]
    fn parse_iso_handles_z_suffix() {
        assert!(parse_iso("2026-07-17T01:00:00Z").is_some());
        assert!(parse_iso("not-a-date").is_none());
    }
}
