//! Clap-based CLI dispatch for the `airlock-v2` binary.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::Path;

use crate::git_ops::{
    dirty_count, get_remote_url, primary_branch, snapshot_repo, try_push_or_snapshot,
};
use crate::registry::{load, save, short_ts, upsert_entry, Registry};
use crate::StateRoot;

#[derive(Debug, Parser)]
#[command(name = "airlock-v2")]
#[command(about = "Conservative auto-save / push daemon for git repositories")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Register a repository with the airlock-v2 daemon.
    Register {
        /// Absolute path to a git work-tree.
        repo_path: String,
    },
    /// Remove a repository from the registry.
    Unregister {
        repo_path: String,
    },
    /// List every registered repo.
    List,
    /// Show a one-screen status for a single repo.
    Status {
        repo_path: String,
    },
    /// Create+push a `wip/<date>-<uuid>` snapshot branch.
    Snapshot {
        repo_path: String,
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
    },
    /// Single-shot 15-minute auto-commit pass (used by the launchd wrapper).
    Autocommit {
        /// Do not commit or push; only print what *would* happen.
        #[arg(long)]
        dry_run: bool,
    },
    /// Single-shot 8-hour stash→wip→push pass.
    Cleanup {
        #[arg(long)]
        dry_run: bool,
    },
    /// Long-running scheduler (autocommit or cleanup).
    Daemon {
        /// Which cycle to run forever.
        mode: String,
    },
    /// Audit every registered repo (alias of `list` with verbose output).
    Audit,
    /// Run all cycles once on the live registry. Used by the
    /// `airlock-v2 autocommit` and `airlock-v2 cleanup` subcommands. The
    /// `restore` command restores a `wip/<date>-<uuid>` branch onto a
    /// target ref (a no-op alias of `snapshot` for parity).
    Restore {
        repo_path: String,
        /// Branch or ref to restore into.
        #[arg(short = 'b', long = "branch")]
        branch: String,
    },
    /// Show one-screen status of all registered repos (counts).
    Quickstatus,
}

/// Helper: resolve `repo_path` to an absolute path inside a git work-tree.
fn resolve_repo_path(input: &str) -> Result<std::path::PathBuf> {
    let p = std::path::Path::new(input).expanduser_abs()?;
    Ok(p)
}

trait ExpandUserAbs {
    fn expanduser_abs(&self) -> Result<std::path::PathBuf>;
}

impl ExpandUserAbs for std::path::Path {
    fn expanduser_abs(&self) -> Result<std::path::PathBuf> {
        let s = self.to_string_lossy();
        let expanded = if let Some(rest) = s.strip_prefix("~/") {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .context("HOME not set")?;
            home.join(rest)
        } else if s == "~" {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .context("HOME not set")?
        } else {
            std::path::PathBuf::from(s.as_ref())
        };
        Ok(expanded)
    }
}

/// Run the CLI, returning the process exit code.
pub fn run(cli: &Cli, state_root: &StateRoot) -> Result<i32> {
    state_root.ensure_dirs()?;
    match &cli.command {
        Commands::Register { repo_path } => cmd_register(state_root, repo_path),
        Commands::Unregister { repo_path } => cmd_unregister(state_root, repo_path),
        Commands::List => cmd_list(state_root),
        Commands::Status { repo_path } => cmd_status(state_root, repo_path),
        Commands::Snapshot { repo_path, message } => {
            cmd_snapshot(state_root, repo_path, message.as_deref())
        }
        Commands::Autocommit { dry_run } => cmd_autocommit(state_root, *dry_run),
        Commands::Cleanup { dry_run } => cmd_cleanup(state_root, *dry_run),
        Commands::Daemon { mode } => cmd_daemon(state_root, mode),
        Commands::Audit => cmd_audit(state_root),
        Commands::Restore { repo_path, branch } => {
            cmd_restore(state_root, repo_path, branch)
        }
        Commands::Quickstatus => cmd_quickstatus(state_root),
    }
}

fn cmd_register(state_root: &StateRoot, repo_path: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    if !crate::git_ops::is_inside_work_tree(&repo_path)? {
        println!("[SKIP] {} is not inside a git work tree.", repo_path.display());
        return Ok(1);
    }
    let mut registry = load(state_root)?;
    let key = repo_path.to_string_lossy().to_string();
    let remote_url = get_remote_url(&repo_path).unwrap_or(None).unwrap_or_default();
    let primary = primary_branch(&repo_path).unwrap_or_else(|_| "main".to_string());
    upsert_entry(&mut registry, &key, |e| {
        e.remote_url = if remote_url.is_empty() {
            None
        } else {
            Some(remote_url.clone())
        };
        e.primary_branch = Some(primary.clone());
        e.last_dirty_count = Some(dirty_count(&repo_path).unwrap_or(0) as u64);
    });
    save(state_root, &registry)?;
    println!("[OK] Registered {}", repo_path.display());
    Ok(0)
}

fn cmd_unregister(state_root: &StateRoot, repo_path: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    let key = repo_path.to_string_lossy().to_string();
    let mut registry = load(state_root)?;
    if registry.remove(&key).is_none() {
        println!("[INFO] {} not in registry; nothing to do.", repo_path.display());
        return Ok(0);
    }
    save(state_root, &registry)?;
    println!("[OK] Unregistered {}", repo_path.display());
    Ok(0)
}

fn cmd_list(state_root: &StateRoot) -> Result<i32> {
    let registry = load(state_root)?;
    print_registry(&registry);
    Ok(0)
}

fn cmd_status(state_root: &StateRoot, repo_path: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    let registry = load(state_root)?;
    let key = repo_path.to_string_lossy().to_string();
    let meta = registry.get(&key).cloned().unwrap_or_default();
    let snapshot = snapshot_repo(&repo_path, &meta)?;
    print!("{}", snapshot.render());
    // `state_root` is intentionally unused here; the registry probe is enough.
    let _ = state_root;
    Ok(0)
}

fn cmd_snapshot(state_root: &StateRoot, repo_path: &str, message: Option<&str>) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    if !crate::git_ops::is_inside_work_tree(&repo_path)? {
        println!("[FAIL] {} is not a git repo.", repo_path.display());
        return Ok(1);
    }
    let snapshot_branch = format!("wip/{}-{}", short_ts(), crate::cli::short_id());
    let snapshot = match create_snapshot_commit(&repo_path, &snapshot_branch, message)? {
        Some(snapshot) => snapshot,
        None => {
            println!("[INFO] No non-ignored working-tree changes; no snapshot created.");
            return Ok(0);
        }
    };
    let (ok, msg) = crate::git_ops::push_branch_with_upstream(&repo_path, &snapshot_branch)?;
    let _ = state_root;
    if !ok {
        println!(
            "[WARN] Push failed for {snapshot_branch}: {msg}\n       Local branch {snapshot_branch} at {} is preserved.",
            snapshot.commit
        );
        return Ok(2);
    }
    verify_pushed_snapshot(&repo_path, &snapshot)?;
    println!("[OK] Snapshot created and pushed: {snapshot_branch}");
    println!("     {}", msg.trim());
    println!("     commit: {} (parent {})", snapshot.commit, snapshot.head);
    if let Some(m) = message {
        println!("     note: {m}");
    }
    Ok(0)
}

/// A WIP commit made from an isolated index.  The normal index and worktree
/// are never changed by a snapshot: the branch is an immutable recovery point
/// for the exact dirty delta that existed at invocation time.
#[derive(Debug, Clone)]
struct SnapshotCommit {
    branch: String,
    head: String,
    commit: String,
}

/// Build and publish a commit from a disposable Git index, without changing a
/// caller's staged state.  `git add -A` intentionally includes tracked
/// modifications, deletions, and untracked non-ignored files.
fn create_snapshot_commit(
    repo_path: &Path,
    branch: &str,
    requested_message: Option<&str>,
) -> Result<Option<SnapshotCommit>> {
    let head = git_value(repo_path, &["rev-parse", "--verify", "HEAD"])?;
    let index_dir = tempfile::Builder::new()
        .prefix("airlock-v2-index-")
        .tempdir()
        .context("create isolated snapshot index directory")?;
    let index_path = index_dir.path().join("index");
    let index_env = vec![(
        "GIT_INDEX_FILE",
        index_path
            .to_str()
            .context("temporary snapshot index path is not UTF-8")?
            .to_string(),
    )];

    run_snapshot_git(repo_path, &["read-tree", "HEAD"], &index_env, "initialize snapshot index")?;
    run_snapshot_git(repo_path, &["add", "-A"], &index_env, "stage working tree in snapshot index")?;
    let tree = git_value_with_env(repo_path, &["write-tree"], &index_env, "write snapshot tree")?;
    let head_tree = git_value(repo_path, &["rev-parse", "HEAD^{tree}"])?;
    if tree == head_tree {
        return Ok(None);
    }

    let message = requested_message
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("wip: airlock snapshot {branch}"));
    let mut commit_env = index_env;
    commit_env.extend(crate::git_ops::ensure_clean_author_env());
    let commit = git_value_with_env(
        repo_path,
        &["commit-tree", &tree, "-p", &head, "-m", &message],
        &commit_env,
        "create snapshot commit",
    )?;

    let ref_name = format!("refs/heads/{branch}");
    let zero = "0000000000000000000000000000000000000000";
    run_snapshot_git(
        repo_path,
        &["update-ref", &ref_name, &commit, zero],
        &[],
        "create new local snapshot ref",
    )?;

    let actual_ref = git_value(repo_path, &["rev-parse", "--verify", &ref_name])?;
    if actual_ref != commit {
        bail!("snapshot ref {ref_name} does not point at the generated commit");
    }
    let actual_parent = git_value(repo_path, &["rev-parse", "--verify", &format!("{commit}^")])?;
    if actual_parent != head {
        bail!("snapshot commit {commit} is not parented to HEAD {head}");
    }

    Ok(Some(SnapshotCommit {
        branch: branch.to_string(),
        head,
        commit,
    }))
}

/// Confirm that the remote received exactly the generated WIP commit and
/// that it contains a tree delta from the HEAD it was parented to.  This makes
/// a success line evidence-backed rather than merely evidence of a push.
fn verify_pushed_snapshot(repo_path: &Path, snapshot: &SnapshotCommit) -> Result<()> {
    let remote_ref = format!("refs/heads/{}", snapshot.branch);
    let remote = crate::git_ops::run_git(
        &["ls-remote", "--heads", "origin", &remote_ref],
        repo_path,
        Some(60),
    )?;
    if !remote.ok() {
        bail!("verify pushed snapshot {}: {}", snapshot.branch, remote.combined());
    }
    let remote_tip = remote
        .stdout
        .split_whitespace()
        .next()
        .context("remote did not advertise the pushed snapshot ref")?;
    if remote_tip != snapshot.commit {
        bail!(
            "remote snapshot ref {remote_ref} points at {remote_tip}, expected {}",
            snapshot.commit
        );
    }

    let diff = crate::git_ops::run_git(
        &["diff", "--quiet", &snapshot.head, &snapshot.commit],
        repo_path,
        None,
    )?;
    match diff.code {
        1 => Ok(()),
        0 => bail!("snapshot {} has no dirty delta from HEAD", snapshot.branch),
        _ => bail!("could not verify snapshot delta: {}", diff.combined()),
    }
}

fn git_value(repo_path: &Path, args: &[&str]) -> Result<String> {
    git_value_with_env(repo_path, args, &[], "read git value")
}

fn git_value_with_env(
    repo_path: &Path,
    args: &[&str],
    env: &[(&str, String)],
    action: &str,
) -> Result<String> {
    let result = crate::git_ops::run_git_with_env(args, repo_path, env)?;
    if !result.ok() {
        bail!("{action}: {}", result.combined());
    }
    let value = result.stdout.trim();
    if value.is_empty() {
        bail!("{action}: git returned no value");
    }
    Ok(value.to_string())
}

fn run_snapshot_git(
    repo_path: &Path,
    args: &[&str],
    env: &[(&str, String)],
    action: &str,
) -> Result<()> {
    let result = crate::git_ops::run_git_with_env(args, repo_path, env)?;
    if !result.ok() {
        bail!("{action}: {}", result.combined());
    }
    Ok(())
}

fn cmd_autocommit(state_root: &StateRoot, dry_run: bool) -> Result<i32> {
    let summary = crate::autocommit::run(state_root, dry_run)?;
    print!("{summary}", summary = summary.render());
    Ok(if summary.errors == 0 { 0 } else { 1 })
}

fn cmd_cleanup(state_root: &StateRoot, dry_run: bool) -> Result<i32> {
    let summary = crate::cleanup::run(state_root, dry_run)?;
    print!("{summary}", summary = summary.render());
    Ok(if summary.errors == 0 { 0 } else { 1 })
}

fn cmd_daemon(state_root: &StateRoot, mode: &str) -> Result<i32> {
    let mode = match mode {
        "autocommit" => "autocommit",
        "cleanup" => "cleanup",
        _ => {
            eprintln!("[FAIL] unknown daemon mode: {mode} (use 'autocommit' or 'cleanup')");
            return Ok(2);
        }
    };
    // The long-running loop lives in `examples/daemon.rs`. Re-launching
    // here is an alias that prints a hint — the actual loop is
    // implemented separately so the CLI binary stays single-shot.
    println!(
        "[daemon] mode={mode}: invoke `cargo run --example daemon -- {mode}` to run as a long-lived scheduler."
    );
    let _ = state_root;
    Ok(0)
}

fn cmd_audit(state_root: &StateRoot) -> Result<i32> {
    let registry = load(state_root)?;
    if registry.is_empty() {
        println!("[INFO] No repos registered.");
        return Ok(0);
    }
    println!("[AUDIT] {} registered repo(s):", registry.len());
    for (path, meta) in registry.sorted() {
        let dirty = meta.last_dirty_count.unwrap_or(0);
        let remote = meta
            .remote_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("(no remote)");
        println!("  - {path}");
        println!(
            "      dirty={dirty}  remote={remote}  registered_at={}",
            meta.registered_at.as_deref().unwrap_or("?")
        );
        println!(
            "      primary_branch={}  last_auto_commit={}  last_push_time={}",
            meta.primary_branch.as_deref().unwrap_or("?"),
            meta.last_auto_commit.as_deref().unwrap_or("never"),
            meta.last_push_time.as_deref().unwrap_or("never"),
        );
    }
    let _ = state_root;
    Ok(0)
}

fn cmd_restore(state_root: &StateRoot, repo_path: &str, branch: &str) -> Result<i32> {
    let repo_path = resolve_repo_path(repo_path)?;
    let (ok, msg) = try_push_or_snapshot(&repo_path, branch, &short_ts())?;
    println!("[restore] {branch}: {msg}");
    let _ = state_root;
    Ok(if ok { 0 } else { 2 })
}

fn cmd_quickstatus(state_root: &StateRoot) -> Result<i32> {
    let registry = load(state_root)?;
    let repos = registry.len();
    let mut dirty = 0usize;
    let mut unpushed = 0usize;
    for path in registry.sorted_paths() {
        let meta = registry
            .get(path.to_string_lossy().as_ref())
            .cloned()
            .unwrap_or_default();
        let snap = snapshot_repo(&path, &meta)?;
        if snap.dirty > 0 {
            dirty += 1;
        }
        if snap.ahead > 0 {
            unpushed += 1;
        }
    }
    println!("[quickstatus] repos={repos} dirty={dirty} unpushed={unpushed}");
    let _ = state_root;
    Ok(0)
}

fn print_registry(reg: &Registry) {
    if reg.is_empty() {
        println!("[INFO] No repos registered.");
        return;
    }
    println!("[OK] {} registered repo(s):", reg.len());
    for (path, meta) in reg.sorted() {
        let dirty = meta.last_dirty_count.unwrap_or(0);
        let remote = meta
            .remote_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("(no remote)");
        println!("  - {path}");
        println!("      dirty={dirty}  remote={remote}");
    }
}

/// Short process-time hex used by snapshot branch names.
///
/// Mirrors the Python's `uuid.uuid4().hex[:8]` — collision-safe enough for
/// one daemon tick across hundreds of repos.
pub fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn git(repo: &Path, args: &[&str]) -> Result<crate::git_ops::GitResult> {
        crate::git_ops::run_git(args, repo, None)
    }

    fn require_git(repo: &Path, args: &[&str], action: &str) -> Result<()> {
        let result = git(repo, args)?;
        if !result.ok() {
            bail!("{action}: {}", result.combined());
        }
        Ok(())
    }

    fn git_value_for_test(repo: &Path, args: &[&str]) -> Result<String> {
        let result = git(repo, args)?;
        if !result.ok() {
            bail!("git {:?}: {}", args, result.combined());
        }
        Ok(result.stdout.trim().to_string())
    }

    fn initialize_repo(dir: &Path) -> Result<()> {
        require_git(dir, &["init", "-q", "-b", "main", dir.to_str().context("repo path")?], "init repo")?;
        require_git(dir, &["config", "user.email", "test@example.com"], "set email")?;
        require_git(dir, &["config", "user.name", "Test"], "set name")?;
        std::fs::write(dir.join("tracked.txt"), "before\n")?;
        require_git(dir, &["add", "tracked.txt"], "stage initial file")?;
        require_git(dir, &["commit", "-m", "initial", "--no-verify"], "create initial commit")?;
        Ok(())
    }

    #[test]
    fn snapshot_commit_captures_tracked_and_untracked_dirty_delta() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo)?;
        initialize_repo(&repo)?;

        let remote = temp.path().join("remote.git");
        require_git(
            temp.path(),
            &["init", "--bare", remote.to_str().context("remote path")?],
            "initialize bare remote",
        )?;
        require_git(
            &repo,
            &["remote", "add", "origin", remote.to_str().context("remote path")?],
            "add bare origin",
        )?;
        require_git(&repo, &["push", "-u", "origin", "main"], "push initial main")?;

        let head = git_value_for_test(&repo, &["rev-parse", "HEAD"])?;
        std::fs::write(repo.join("tracked.txt"), "after\n")?;
        std::fs::write(repo.join("untracked.txt"), "preserve me\n")?;
        let snapshot = create_snapshot_commit(&repo, "wip/test-dirty", Some("test snapshot"))?
            .context("dirty repository unexpectedly produced no snapshot")?;
        assert_ne!(snapshot.commit, head);
        assert_eq!(snapshot.head, head);

        let local_tip = git_value_for_test(&repo, &["rev-parse", "refs/heads/wip/test-dirty"])?;
        assert_eq!(local_tip, snapshot.commit);
        let parent = git_value_for_test(&repo, &["rev-parse", "wip/test-dirty^"])?;
        assert_eq!(parent, head);
        let files = git_value_for_test(&repo, &["diff", "--name-only", &head, "wip/test-dirty"])?;
        assert!(files.lines().any(|path| path == "tracked.txt"), "{files}");
        assert!(files.lines().any(|path| path == "untracked.txt"), "{files}");

        let (pushed, detail) = crate::git_ops::push_branch_with_upstream(&repo, &snapshot.branch)?;
        assert!(pushed, "snapshot push failed: {detail}");
        verify_pushed_snapshot(&repo, &snapshot)?;
        let remote_tip = git_value_for_test(
            &remote,
            &["rev-parse", "refs/heads/wip/test-dirty"],
        )?;
        assert_eq!(remote_tip, snapshot.commit);

        let status_result = git(&repo, &["status", "--porcelain"])?;
        assert!(status_result.ok(), "read worktree status: {status_result:?}");
        let status = status_result.stdout;
        assert!(status.contains(" M tracked.txt"), "snapshot changed the caller index: {status}");
        assert!(status.contains("?? untracked.txt"), "snapshot lost untracked work: {status}");
        Ok(())
    }
}
