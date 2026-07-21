//! 8-hour cleanup cycle.
//!
//! Mirrors the Python `daemon_cleanup` function: walk the registry, for
//! each repo try to recover any stashes (apply -> wip-branch -> push, drop
//! only after push), then check the current branch for unpushed commits
//! and try `try_push_or_snapshot`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::git_ops::{
    ahead_behind, current_branch, is_inside_work_tree, recover_one_stash, run_git, stash_count,
};
use crate::registry::{
    append_event, load, now_iso, save, short_ts, upsert_entry, Registry, RepoEntry,
};
use crate::StateRoot;

#[derive(Debug, Clone, Default)]
pub struct CleanupRecord {
    pub stashes_seen: usize,
    pub stashes_recovered: usize,
    pub stashes_failed: usize,
    pub stash_errors: Vec<(String, String)>,
    pub ahead: usize,
    pub push_ok: Option<bool>,
    pub push_detail: Option<String>,
    pub skip: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CleanupSummary {
    pub visited: usize,
    pub stashes_recovered: usize,
    pub commits_pushed: usize,
    pub errors: usize,
    pub dry_run: bool,
    pub records: Vec<(String, CleanupRecord)>,
}

impl CleanupSummary {
    pub fn render(&self) -> String {
        let mut s = String::new();
        for (path, r) in &self.records {
            s.push_str(&format!(
                "[daemon-cleanup] [OK ] {path}: stashes_seen={} recovered={} failed={} ahead={:?} push_ok={:?}\n",
                r.stashes_seen, r.stashes_recovered, r.stashes_failed, r.ahead, r.push_ok
            ));
        }
        s.push_str(&format!(
            "[done] visited={} stashes_recovered={} commits_pushed={} errors={} dry_run={}\n",
            self.visited,
            self.stashes_recovered,
            self.commits_pushed,
            self.errors,
            self.dry_run,
        ));
        s
    }
}

/// Run a single cleanup pass.
pub fn run(state_root: &StateRoot, dry_run: bool) -> Result<CleanupSummary> {
    state_root.ensure_dirs()?;
    let mut registry = load(state_root)?;
    let mut summary = CleanupSummary {
        dry_run,
        ..CleanupSummary::default()
    };

    let entries: Vec<(String, RepoEntry)> = registry
        .sorted()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    for (path_key, _entry) in entries {
        summary.visited += 1;
        let path = PathBuf::from(&path_key);
        let rec = run_one(state_root, &path, dry_run, &mut registry);
        if rec.stashes_failed > 0 {
            summary.errors += 1;
        }
        if rec.push_ok == Some(true) {
            summary.commits_pushed += 1;
        }
        summary.stashes_recovered += rec.stashes_recovered;
        summary.records.push((path_key.clone(), rec));
    }

    if !dry_run {
        save(state_root, &registry).context("persist registry after cleanup")?;
    }
    Ok(summary)
}

fn run_one(
    state_root: &StateRoot,
    repo_path: &Path,
    dry_run: bool,
    registry: &mut Registry,
) -> CleanupRecord {
    let mut rec = CleanupRecord::default();
    if !is_inside_work_tree(repo_path).unwrap_or(false) {
        rec.skip = Some("not-a-git-repo".into());
        return rec;
    }
    let branch = match current_branch(repo_path) {
        Ok(Some(b)) => b,
        _ => {
            rec.skip = Some("detached-head".into());
            return rec;
        }
    };

    // Stash recovery
    let stashes = stash_count(repo_path).unwrap_or(0);
    rec.stashes_seen = stashes;
    if stashes > 0 {
        let list_res = run_git(&["stash", "list", "--pretty=%gd"], repo_path, None);
        let refs: Vec<String> = match list_res {
            Ok(r) => r
                .stdout
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            Err(_) => Vec::new(),
        };
        for stash_ref in refs {
            if dry_run {
                rec.stashes_recovered += 1;
                continue;
            }
            match recover_one_stash(repo_path, &stash_ref, &short_ts()) {
                Ok((ok, msg, _)) => {
                    if ok {
                        rec.stashes_recovered += 1;
                    } else {
                        rec.stashes_failed += 1;
                        rec.stash_errors.push((stash_ref.clone(), msg.clone()));
                    }
                    let event = serde_json::json!({
                        "event": "stash-recover",
                        "path": repo_path.to_string_lossy(),
                        "ref": stash_ref,
                        "ok": ok,
                        "msg": msg,
                    });
                    let _ = append_event(state_root, "cleanup", &event);
                }
                Err(e) => {
                    rec.stashes_failed += 1;
                    rec.stash_errors.push((stash_ref.clone(), format!("{e:#}")));
                }
            }
        }
    }

    // Stale unpushed commits on the current branch.
    if !branch.is_empty() && branch != "HEAD" {
        let (ahead, _) = ahead_behind(repo_path, &branch).unwrap_or((0, 0));
        rec.ahead = ahead;
        if ahead > 0 && !dry_run {
            let push_result =
                crate::git_ops::try_push_or_snapshot(repo_path, &branch, &short_ts());
            match push_result {
                Ok((ok, msg)) => {
                    rec.push_ok = Some(ok);
                    rec.push_detail = Some(msg.clone());
                    let path_key = repo_path.to_string_lossy().to_string();
                    upsert_entry(registry, &path_key, |e| {
                        if ok {
                            e.last_push_time = Some(now_iso());
                        }
                    });
                    let event = serde_json::json!({
                        "event": "branch-push",
                        "path": repo_path.display().to_string(),
                        "branch": branch,
                        "ahead": ahead,
                        "ok": ok,
                        "msg": msg,
                    });
                    let _ = append_event(state_root, "cleanup", &event);
                }
                Err(e) => {
                    rec.push_ok = Some(false);
                    rec.push_detail = Some(format!("{e:#}"));
                }
            }
        }
    }

    rec
}
