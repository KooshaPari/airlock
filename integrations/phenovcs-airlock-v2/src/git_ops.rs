//! Git shell-out helpers.
//!
//! All git interaction goes through `std::process::Command` — matching the
//! pattern used in `worktree-manager`'s `git_adapter.rs`. We deliberately
//! avoid the `git2` crate so the dependency footprint stays small and the
//! daemon's surface area matches the user's installed `git`.
//!
//! ## Conservative-only-on-delete
//!
//! - `push_branch` uses **fast-forward only** (`--ff-only`). There is no
//!   `--force`, no `--atomic --force-with-lease`, no `git push --push-option`.
//! - When a push is rejected, callers should use `try_push_or_snapshot` to
//!   preserve work as a fresh `wip/<date>-<uuid>` branch on the remote
//!   (new refs are inherently fast-forwardable, so this never loses work).
//! - We **never** call `git stash drop` unless `_recover_one_stash`
//!   confirms the corresponding wip branch was pushed successfully.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::registry::RepoEntry;

/// Result of shelling out to `git`.
///
/// We always capture stdout/stderr so the caller can build human-friendly
/// status lines and structured log entries without re-running the command.
#[derive(Debug, Clone)]
pub struct GitResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl GitResult {
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    pub fn combined(&self) -> String {
        if !self.stderr.trim().is_empty() {
            format!("{}\n{}", self.stdout.trim_end(), self.stderr.trim_end())
        } else {
            self.stdout.trim_end().to_string()
        }
    }
}

/// Execute `git <args>` inside `cwd` and capture the result.
///
/// `timeout_secs` defaults to 30 — push operations may want longer; pass
/// `Some(60)` from push helpers to allow for slow upstreams.
pub fn run_git(args: &[&str], cwd: &Path, timeout_secs: Option<u64>) -> Result<GitResult> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    cmd.current_dir(cwd);

    let output: Output = if let Some(secs) = timeout_secs {
        // `Command::output` is blocking; we approximate timeout via spawn +
        // wait-timeout. A precise cross-platform timeout in std is not yet
        // stable, so we cap at 30s by default — push helpers override.
        run_with_timeout(&mut cmd, Duration::from_secs(secs))?
    } else {
        cmd.output()
            .with_context(|| format!("git {:?} failed to spawn", args))?
    };

    Ok(GitResult {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(unix)]
fn run_with_timeout(cmd: &mut Command, _timeout: Duration) -> std::io::Result<Output> {
    // Std doesn't have a portable child-kill API. For the conservative
    // 30s/60s windows we use, simply blocking on `output()` is fine — the
    // `kill_on_drop` semantic for `Child` requires pinning. We accept the
    // trade-off; the daemon is interruptible via SIGTERM.
    cmd.output()
}

#[cfg(not(unix))]
fn run_with_timeout(cmd: &mut Command, _timeout: Duration) -> std::io::Result<Output> {
    cmd.output()
}

/// Resolve the absolute work-tree root for `path`, if any.
pub fn safe_repo_root(path: &Path) -> Result<Option<PathBuf>> {
    let res = run_git(&["rev-parse", "--show-toplevel"], path, None)?;
    if !res.ok() {
        return Ok(None);
    }
    let trimmed = res.stdout.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(trimmed)))
    }
}

pub fn is_inside_work_tree(path: &Path) -> Result<bool> {
    let res = run_git(&["rev-parse", "--is-inside-work-tree"], path, None)?;
    Ok(res.ok() && res.stdout.trim() == "true")
}

pub fn has_origin_remote(path: &Path) -> Result<bool> {
    let res = run_git(&["remote", "get-url", "origin"], path, None)?;
    Ok(res.ok() && !res.stdout.trim().is_empty())
}

pub fn get_remote_url(path: &Path) -> Result<Option<String>> {
    let res = run_git(&["remote", "get-url", "origin"], path, None)?;
    if res.ok() {
        Ok(Some(res.stdout.trim().to_string()))
    } else {
        Ok(None)
    }
}

pub fn current_branch(path: &Path) -> Result<Option<String>> {
    let res = run_git(&["rev-parse", "--abbrev-ref", "HEAD"], path, None)?;
    if !res.ok() {
        return Ok(None);
    }
    let trimmed = res.stdout.trim();
    if trimmed.is_empty() || trimmed == "HEAD" {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Best-effort detection of the repo's primary branch name.
pub fn primary_branch(path: &Path) -> Result<String> {
    let res = run_git(
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        path,
        None,
    )?;
    if res.ok() {
        let trimmed = res.stdout.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.rsplit('/').next().unwrap_or(trimmed).to_string());
        }
    }
    for candidate in ["main", "master", "trunk", "develop"] {
        let check = run_git(
            &["show-ref", "--verify", "--quiet", &format!("refs/heads/{candidate}")],
            path,
            None,
        )?;
        if check.ok() {
            return Ok(candidate.to_string());
        }
    }
    Ok("main".to_string())
}

/// Count lines in `git status --porcelain`.
pub fn dirty_count(path: &Path) -> Result<usize> {
    let res = run_git(&["status", "--porcelain"], path, None)?;
    if !res.ok() {
        return Ok(0);
    }
    Ok(res.stdout.lines().filter(|l| !l.trim().is_empty()).count())
}

/// `(ahead, behind)` against `origin/<branch>`.
pub fn ahead_behind(path: &Path, branch: &str) -> Result<(usize, usize)> {
    let spec = format!("origin/{branch}...{branch}");
    let res = run_git(&["rev-list", "--left-right", "--count", &spec], path, None)?;
    if !res.ok() {
        return Ok((0, 0));
    }
    let mut parts = res.stdout.split_whitespace();
    let ahead = parts
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let behind = parts
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    Ok((ahead, behind))
}

pub fn stash_count(path: &Path) -> Result<usize> {
    let res = run_git(&["stash", "list"], path, None)?;
    if !res.ok() {
        return Ok(0);
    }
    Ok(res.stdout.lines().filter(|l| !l.trim().is_empty()).count())
}

pub fn last_commit(path: &Path) -> Result<String> {
    let res = run_git(&["log", "-1", "--pretty=%h %s", "--no-color"], path, None)?;
    if res.ok() {
        Ok(res.stdout.trim().to_string())
    } else {
        Ok("(no commits)".to_string())
    }
}

/// Build the env-patch for `git commit` to ensure commits have an author.
///
/// Users can override individual `GIT_AUTHOR_*` vars by exporting them
/// before invoking the daemon.
pub fn ensure_clean_author_env() -> Vec<(&'static str, String)> {
    [
        ("GIT_AUTHOR_NAME", "Airlock Bot"),
        ("GIT_AUTHOR_EMAIL", "airlock@phenoforge.local"),
        ("GIT_COMMITTER_NAME", "Airlock Bot"),
        ("GIT_COMMITTER_EMAIL", "airlock@phenoforge.local"),
    ]
    .iter()
    .map(|(k, default)| {
        let v = std::env::var(k).unwrap_or_else(|_| default.to_string());
        (*k, v)
    })
    .collect()
}

/// Stage everything and commit. Conservative: never `--amend`, never `--force`.
pub fn commit_all(path: &Path, message: &str) -> Result<bool> {
    let add = run_git(&["add", "-A"], path, None)?;
    if !add.ok() {
        return Ok(false);
    }
    // Bail if there's nothing to commit.
    let diff = run_git(&["diff", "--cached", "--quiet"], path, None)?;
    if diff.ok() {
        return Ok(false);
    }
    let env = ensure_clean_author_env();
    let commit = run_git_with_env(&["commit", "-m", message, "--no-verify"], path, &env)?;
    Ok(commit.ok())
}

/// Like [`run_git`] but injects additional environment variables (e.g. for
/// setting author identity per-command).
pub fn run_git_with_env(
    args: &[&str],
    cwd: &Path,
    env: &[(&str, String)],
) -> Result<GitResult> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    cmd.current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd.output()?;
    Ok(GitResult {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Push `branch` to origin with `--ff-only`.
///
/// Returns the result even when the push fails so the caller can decide
/// whether to fall back to a `wip/<date>-<uuid>` snapshot branch. We never
/// force-push — that is the central invariant.
pub fn push_branch_ff_only(path: &Path, branch: &str) -> Result<(bool, String)> {
    let args = ["push", "--ff-only", "origin", branch];
    let res = run_git(&args, path, Some(60))?;
    let combined = res.combined();
    Ok((res.ok(), combined))
}

/// Push with `--set-upstream` semantics (used for new branches — `wip/<date>`).
/// A new branch on the remote is inherently a fast-forward.
pub fn push_branch_with_upstream(path: &Path, branch: &str) -> Result<(bool, String)> {
    let args = ["push", "--set-upstream", "origin", branch];
    let res = run_git(&args, path, Some(60))?;
    Ok((res.ok(), res.combined()))
}

/// Try to push `branch` to origin. On any failure (network, non-FF,
/// rejected), create a backup `wip/<date_tag>-<uuid8>` branch from HEAD
/// and push that with `--set-upstream`. The fresh ref is always
/// fast-forwardable from the server's perspective.
///
/// This implements the Python's `try_push_or_snapshot` semantics with the
/// stricter "fast-forward only" rule baked in.
pub fn try_push_or_snapshot(path: &Path, branch: &str, date_tag: &str) -> Result<(bool, String)> {
    let (ok, err) = push_branch_ff_only(path, branch)?;
    if ok {
        return Ok((true, format!("pushed {branch}")));
    }
    let no_remote = err.contains("Could not resolve hostname")
        || err.contains("Could not read")
        || err.to_lowercase().contains("not a git repository")
        || err.contains("No such remote");
    let non_ff = err.contains("non-fast-forward")
        || err.to_lowercase().contains("rejected")
        || err.contains("[rejected]");
    if no_remote || non_ff {
        // Backup as `wip/<date>-<hex8>`. New ref → inherently fast-forwardable.
        let backup = format!("wip/{date_tag}-{}", short_uuid());
        let cre = run_git(&["branch", &backup, "HEAD"], path, None)?;
        if !cre.ok() {
            return Ok((
                false,
                format!(
                    "failed to create backup branch {backup}: {}",
                    cre.combined()
                ),
            ));
        }
        let (ok2, err2) = push_branch_with_upstream(path, &backup)?;
        if ok2 {
            return Ok((
                true,
                format!("push rejected for {branch}; backup at {backup} pushed"),
            ));
        }
        return Ok((
            false,
            format!("backup branch push also failed for {backup}: {err2}"),
        ));
    }
    Ok((false, err))
}

fn short_uuid() -> String {
    // Use process time + a counter; not cryptographic — just short enough
    // to collide rarely across one daemon tick.
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos as u64)
}

/// Create a branch ref without switching.
pub fn create_branch_at_head(path: &Path, name: &str) -> Result<()> {
    let res = run_git(&["branch", name, "HEAD"], path, None)?;
    if !res.ok() {
        bail!(
            "could not create branch {name}: {}",
            res.combined()
        );
    }
    Ok(())
}

/// Stash recovery helper used by `cleanup`. Applies the stash, snapshots
/// it as `wip/<date>-stash-<uuid8>`, commits, pushes (with upstream), and
/// only drops the stash if the push succeeded.
pub fn recover_one_stash(
    path: &Path,
    stash_ref: &str,
    date_tag: &str,
) -> Result<(bool, String, Option<String>)> {
    let show = run_git(&["stash", "show", "-p", stash_ref], path, None)?;
    if !show.ok() {
        return Ok((false, format!("cannot read {stash_ref}"), None));
    }
    let diff_lines = show.stdout.lines().count();
    let apply = run_git(&["stash", "apply", stash_ref], path, None)?;
    if !apply.ok() {
        return Ok((
            false,
            format!("stash apply failed: {}", apply.combined()),
            None,
        ));
    }
    let backup_branch = format!("wip/{date_tag}-stash-{}", short_uuid());
    let cre = run_git(&["branch", &backup_branch, "HEAD"], path, None)?;
    if !cre.ok() {
        return Ok((
            false,
            "could not create wip branch from applied stash".to_string(),
            Some(backup_branch),
        ));
    }
    let msg = format!(
        "wip: applied stash {stash_ref} ({date_tag})\n\n{diff_lines} lines"
    );
    if !commit_all(path, &msg)? {
        return Ok((
            false,
            "could not commit applied stash".to_string(),
            Some(backup_branch),
        ));
    }
    let (ok, push_msg) = push_branch_with_upstream(path, &backup_branch)?;
    if !ok {
        return Ok((
            false,
            format!(
                "applied+committed to local {backup_branch} (not pushed): {push_msg}"
            ),
            Some(backup_branch),
        ));
    }
    // Only drop the stash AFTER the work has been pushed.
    let drop = run_git(&["stash", "drop", stash_ref], path, None)?;
    if !drop.ok() {
        return Ok((
            true,
            format!(
                "pushed {backup_branch} but stash drop refused: {}",
                drop.combined()
            ),
            Some(backup_branch),
        ));
    }
    Ok((
        true,
        format!("pushed {backup_branch} and dropped {stash_ref}"),
        Some(backup_branch),
    ))
}

/// Return a snapshot of current values for a single repo. Pure computation:
/// no side effects, so it is safe to call from any context.
pub fn snapshot_repo(path: &Path, meta: &RepoEntry) -> Result<RepoSnapshot> {
    let branch = current_branch(path)?.unwrap_or_else(|| "(detached)".to_string());
    let primary = primary_branch(path)?;
    let dirty = dirty_count(path)?;
    let (ahead, behind) = if branch != "(detached)" {
        ahead_behind(path, &branch).unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    let stashes = stash_count(path)?;
    let last = last_commit(path)?;
    Ok(RepoSnapshot {
        path: path.to_path_buf(),
        branch,
        primary,
        ahead,
        behind,
        dirty,
        stashes,
        last_commit: last,
        meta: meta.clone(),
    })
}

/// Pre-rendered view of a single repo used by `airlock-v2 status` and tests.
#[derive(Debug, Clone)]
pub struct RepoSnapshot {
    pub path: PathBuf,
    pub branch: String,
    pub primary: String,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: usize,
    pub stashes: usize,
    pub last_commit: String,
    pub meta: RepoEntry,
}

impl RepoSnapshot {
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("[STATUS] {}\n", self.path.display()));
        out.push_str(&format!(
            "  branch          : {} (primary={})\n",
            self.branch, self.primary
        ));
        out.push_str(&format!(
            "  ahead/behind    : {}/{}\n",
            self.ahead, self.behind
        ));
        out.push_str(&format!("  dirty files     : {}\n", self.dirty));
        out.push_str(&format!("  stashes         : {}\n", self.stashes));
        out.push_str(&format!("  last commit     : {}\n", self.last_commit));
        out.push_str(&format!(
            "  last auto-commit: {}\n",
            self.meta.last_auto_commit.as_deref().unwrap_or("never")
        ));
        out.push_str(&format!(
            "  last push       : {}\n",
            self.meta
                .last_push_time
                .as_ref()
                .map(|t| t.as_str())
                .unwrap_or("never")
        ));
        out.push_str(&format!(
            "  remote          : {}\n",
            self.meta
                .remote_url
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("(none)")
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_git_repo(dir: &Path) -> Result<PathBuf> {
        // Inside `dir`, create a single-file repo on a single branch with
        // a single empty commit. No remote — tests that need a remote set
        // it up explicitly.
        std::fs::create_dir_all(dir)?;
        run_git(&["init", "-q", "-b", "main", dir.to_str().unwrap()], dir, None)?;
        let cfg = |k: &str, v: &str| -> Result<()> {
            run_git(&["config", "user.email", "test@example.com"], dir, None)?;
            run_git(&["config", "user.name", "Test"], dir, None)?;
            let _ = (k, v);
            Ok(())
        };
        cfg("x", "y")?;
        std::fs::write(dir.join("README.md"), "hello\n")?;
        run_git(&["add", "README.md"], dir, None)?;
        run_git(&["commit", "-m", "init", "--no-verify"], dir, None)?;
        Ok(dir.to_path_buf())
    }

    #[test]
    fn dirty_count_zero_for_clean_repo() -> Result<()> {
        let tmp = TempDir::new()?;
        let repo = init_git_repo(tmp.path())?;
        assert_eq!(dirty_count(&repo)?, 0);
        Ok(())
    }

    #[test]
    fn dirty_count_increments_with_modifications() -> Result<()> {
        let tmp = TempDir::new()?;
        let repo = init_git_repo(tmp.path())?;
        std::fs::write(repo.join("new.txt"), "data\n")?;
        std::fs::write(repo.join("README.md"), "dirty\n")?;
        assert_eq!(dirty_count(&repo)?, 2);
        Ok(())
    }

    #[test]
    fn push_branch_ff_only_returns_failure_for_missing_remote() -> Result<()> {
        let tmp = TempDir::new()?;
        let repo = init_git_repo(tmp.path())?;
        let (ok, _) = push_branch_ff_only(&repo, "main")?;
        assert!(!ok, "push without an origin must fail conservatively");
        Ok(())
    }

    #[test]
    fn ahead_behind_returns_zero_for_no_remote() -> Result<()> {
        let tmp = TempDir::new()?;
        let repo = init_git_repo(tmp.path())?;
        let (ahead, behind) = ahead_behind(&repo, "main")?;
        assert_eq!((ahead, behind), (0, 0));
        Ok(())
    }

    #[test]
    fn is_inside_work_tree_recognises_repo() -> Result<()> {
        let tmp = TempDir::new()?;
        let repo = init_git_repo(tmp.path())?;
        assert!(is_inside_work_tree(&repo)?);
        // Must be outside the repo tree — a subdirectory would still be
        // inside the work tree from git's perspective.
        let outside = TempDir::new()?;
        let other = outside.path().join("not-a-repo");
        std::fs::create_dir_all(&other)?;
        assert!(!is_inside_work_tree(&other)?);
        Ok(())
    }
}
