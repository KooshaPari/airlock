//! 15-minute autocommit cycle.
//!
//! Mirrors the Python `daemon_autocommit` function: walk the registry,
//! for each repo check `git status --porcelain`, throttle by last
//! `last_auto_commit`, commit + push (FF-only, with `wip/<date>-<uuid>`
//! fallback on push rejection), update metadata, append NDJSON log.

use anyhow::{Context, Result};
use std::path::Path;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::git_ops::{commit_all, dirty_count, is_inside_work_tree, try_push_or_snapshot};
use crate::registry::{RepoEntry, Registry};
use crate::registry::{append_event, load, now_iso, parse_iso, save, short_ts, upsert_entry};
use crate::{StateRoot, AUTOCOMMIT_INTERVAL};

/// Per-repo decision record.
#[derive(Debug, Clone, Default)]
pub struct AutocommitRecord {
    pub branch: String,
    pub dirty_before: usize,
    pub since_last_sec: u64,
    pub committed: bool,
    pub pushed: bool,
    pub skip: Option<String>,
    pub error: Option<String>,
    pub commit_message: Option<String>,
    pub push_detail: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default)]
pub struct AutocommitSummary {
    pub visited: usize,
    pub dirty_count_seen: usize,
    pub committed: usize,
    pub errors: usize,
    pub dry_run: bool,
    pub records: Vec<(String, AutocommitRecord)>,
}

impl AutocommitSummary {
    pub fn render(&self) -> String {
        let mut s = String::new();
        for (path, r) in &self.records {
            let marker = if r.error.is_some() { "ERR" } else { "OK " };
            let mut line = format!("[daemon-autocommit] [{marker}] {path}: ");
            line.push_str(&format!(
                "branch={} dirty_before={} since_last={}s committed={} pushed={}",
                r.branch, r.dirty_before, r.since_last_sec, r.committed, r.pushed,
            ));
            if let Some(skip) = &r.skip {
                line.push_str(&format!(" skip={skip}"));
            }
            if let Some(err) = &r.error {
                line.push_str(&format!(" error={err}"));
            }
            s.push_str(&line);
            s.push('\n');
        }
        s.push_str(&format!(
            "[done] visited={} dirty={} committed={} errors={} dry_run={}\n",
            self.visited, self.dirty_count_seen, self.committed, self.errors, self.dry_run,
        ));
        s
    }
}

/// Run a single autocommit pass.
pub fn run(state_root: &StateRoot, dry_run: bool) -> Result<AutocommitSummary> {
    state_root.ensure_dirs()?;
    let mut registry = load(state_root)?;
    let mut summary = AutocommitSummary { dry_run, ..Default::default() };

    // Collect entries first to avoid borrow conflict with &mut registry
    let entries: Vec<(String, RepoEntry)> = registry
        .sorted()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();

    for (path_key, entry) in &entries {
        summary.visited += 1;
        let path = PathBuf::from(path_key);
        let result = run_one(state_root, &path, entry, dry_run, &mut registry);
        match result {
            Ok(rec) => {
                if rec.error.is_some() {
                    summary.errors += 1;
                }
                if rec.dirty_before > 0 {
                    summary.dirty_count_seen += 1;
                }
                if rec.committed {
                    summary.committed += 1;
                }
                summary.records.push((path_key.clone(), rec));
            }
            Err(e) => {
                summary.errors += 1;
                let rec = AutocommitRecord {
                    skip: Some("error".into()),
                    error: Some(format!("{e:#}")),
                    dry_run,
                    ..Default::default()
                };
                summary.records.push((path_key.clone(), rec));
            }
        }
    }

    if !dry_run {
        save(state_root, &registry).context("persist registry after autocommit")?;
    }
    Ok(summary)
}

fn run_one(
    state_root: &StateRoot,
    repo_path: &Path,
    meta: &RepoEntry,
    dry_run: bool,
    registry: &mut Registry,
) -> Result<AutocommitRecord> {
    if !is_inside_work_tree(repo_path)? {
        return Ok(AutocommitRecord {
            skip: Some("not-a-git-repo".into()),
            ..Default::default()
        });
    }
    let branch = match crate::git_ops::current_branch(repo_path)? {
        Some(b) => b,
        None => {
            return Ok(AutocommitRecord {
                skip: Some("detached-head".into()),
                ..Default::default()
            });
        }
    };
    let dirty = dirty_count(repo_path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Seconds since last auto-commit; u64::MAX means never committed (no throttle).
    let since_last_sec = match meta.last_auto_commit.as_deref().and_then(parse_iso) {
        Some(dt) => {
            let last = dt.timestamp().max(0) as u64;
            now.saturating_sub(last)
        }
        None => u64::MAX,
    };

    let mut rec = AutocommitRecord {
        branch: branch.clone(),
        dirty_before: dirty,
        since_last_sec,
        ..Default::default()
    };

    if dirty == 0 {
        rec.skip = Some("clean".into());
        return Ok(rec);
    }
    if since_last_sec < AUTOCOMMIT_INTERVAL.as_secs() {
        rec.skip = Some("throttled".into());
        return Ok(rec);
    }
    if dry_run {
        rec.dry_run = true;
        return Ok(rec);
    }

    let msg = format!("wip: auto-commit daemon {ts}", ts = now_iso());
    if !commit_all(repo_path, &msg)? {
        rec.error = Some("commit failed".into());
        upsert_entry(registry, repo_path.to_str().unwrap_or(""), |e| {
            e.last_check = Some(now_iso());
        });
        return Ok(rec);
    }
    rec.committed = true;
    rec.commit_message = Some(msg.clone());

    let (ok, push_msg) = try_push_or_snapshot(repo_path, &branch, &short_ts())?;
    rec.pushed = ok;
    rec.push_detail = Some(push_msg.clone());

    let path_key = repo_path.to_str().unwrap_or("").to_string();
    upsert_entry(registry, &path_key, |e| {
        e.last_check = Some(now_iso());
        e.last_dirty_count = Some(dirty as u64);
        e.last_auto_commit = Some(now_iso());
        if ok {
            e.last_push_time = Some(now_iso());
        }
    });

    let log_event = serde_json::json!({
        "event": "autocommit",
        "path": path_key,
        "branch": rec.branch,
        "dirty_before": rec.dirty_before,
        "since_last_sec": rec.since_last_sec,
        "committed": rec.committed,
        "pushed": rec.pushed,
        "push_detail": push_msg,
        "commit_message": msg,
    });
    let _ = append_event(state_root, "autocommit", &log_event);
    Ok(rec)
}
