//! Isolated-index creation and remote verification for WIP snapshot commits.

use anyhow::{bail, Context, Result};
use std::path::Path;

/// A WIP commit made from an isolated index. The normal index and worktree
/// are never changed by a snapshot: the branch is an immutable recovery point
/// for the exact dirty delta that existed at invocation time.
#[derive(Debug, Clone)]
pub(crate) struct SnapshotCommit {
    pub(crate) branch: String,
    pub(crate) head: String,
    pub(crate) commit: String,
}

/// Build a commit from a disposable Git index without changing staged state.
/// `git add -A` includes tracked modifications, deletions, and untracked,
/// non-ignored files.
pub(crate) fn create_snapshot_commit(
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

    run_snapshot_git(
        repo_path,
        &["read-tree", "HEAD"],
        &index_env,
        "initialize snapshot index",
    )?;
    run_snapshot_git(
        repo_path,
        &["add", "-A"],
        &index_env,
        "stage working tree in snapshot index",
    )?;
    let tree = git_value_with_env(
        repo_path,
        &["write-tree"],
        &index_env,
        "write snapshot tree",
    )?;
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

/// Confirm the remote received the generated WIP commit and its dirty delta.
pub(crate) fn verify_pushed_snapshot(repo_path: &Path, snapshot: &SnapshotCommit) -> Result<()> {
    let remote_ref = format!("refs/heads/{}", snapshot.branch);
    let remote = crate::git_ops::run_git(
        &["ls-remote", "--heads", "origin", &remote_ref],
        repo_path,
        Some(60),
    )?;
    if !remote.ok() {
        bail!(
            "verify pushed snapshot {}: {}",
            snapshot.branch,
            remote.combined()
        );
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
        require_git(
            dir,
            &[
                "init",
                "-q",
                "-b",
                "main",
                dir.to_str().context("repo path")?,
            ],
            "init repo",
        )?;
        require_git(
            dir,
            &["config", "user.email", "test@example.com"],
            "set email",
        )?;
        require_git(dir, &["config", "user.name", "Test"], "set name")?;
        std::fs::write(dir.join("tracked.txt"), "before\n")?;
        require_git(dir, &["add", "tracked.txt"], "stage initial file")?;
        require_git(
            dir,
            &["commit", "-m", "initial", "--no-verify"],
            "create initial commit",
        )?;
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
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().context("remote path")?,
            ],
            "add bare origin",
        )?;
        require_git(
            &repo,
            &["push", "-u", "origin", "main"],
            "push initial main",
        )?;

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
        let remote_tip = git_value_for_test(&remote, &["rev-parse", "refs/heads/wip/test-dirty"])?;
        assert_eq!(remote_tip, snapshot.commit);

        let status_result = git(&repo, &["status", "--porcelain"])?;
        assert!(
            status_result.ok(),
            "read worktree status: {status_result:?}"
        );
        let status = status_result.stdout;
        assert!(
            status.contains(" M tracked.txt"),
            "snapshot changed the caller index: {status}"
        );
        assert!(
            status.contains("?? untracked.txt"),
            "snapshot lost untracked work: {status}"
        );
        Ok(())
    }
}
