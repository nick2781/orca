use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::types::TaskSpec;

/// Decision on how to isolate a task during execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsolationDecision {
    /// Run in a dedicated git worktree.
    Worktree { path: PathBuf, branch: String },
    /// Run in the same directory (no isolation needed).
    SameDir,
    /// Wait for another task to finish first (file overlap detected).
    Serial { wait_for: String },
    /// Cannot decide automatically; escalate to CC (orchestrator).
    AskCc,
}

/// Result of merging a branch back into a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    /// Merge completed without conflicts.
    Success,
    /// Merge encountered conflicts.
    Conflict(String),
}

/// Manages git worktree isolation for concurrent tasks.
pub struct IsolationManager {
    worktree_base: PathBuf,
    project_dir: PathBuf,
}

impl IsolationManager {
    /// Create a new IsolationManager.
    ///
    /// - `project_dir`: the root git repository directory.
    /// - `worktree_base`: base directory under which worktrees are created.
    pub fn new(project_dir: &Path, worktree_base: &Path) -> Self {
        Self {
            project_dir: project_dir.to_path_buf(),
            worktree_base: worktree_base.to_path_buf(),
        }
    }

    /// Decide how a task should be isolated given currently running tasks.
    pub fn decide(&self, task: &TaskSpec, running_tasks: &[&TaskSpec]) -> IsolationDecision {
        use crate::types::IsolationMode;

        match task.isolation {
            IsolationMode::Worktree => IsolationDecision::Worktree {
                path: self.worktree_base.join(&task.id),
                branch: format!("orca/{}", task.id),
            },
            IsolationMode::Serial => {
                // Check file overlap with any running task.
                for running in running_tasks {
                    if has_file_overlap(task, running) {
                        return IsolationDecision::Serial {
                            wait_for: running.id.clone(),
                        };
                    }
                }
                IsolationDecision::SameDir
            }
            IsolationMode::Auto => {
                // No file info means we cannot reason about overlaps.
                if task.context.files.is_empty() {
                    return IsolationDecision::AskCc;
                }

                // Check for overlap with any running task.
                for running in running_tasks {
                    if has_file_overlap(task, running) {
                        return IsolationDecision::Serial {
                            wait_for: running.id.clone(),
                        };
                    }
                }

                // No overlap — safe to use a worktree.
                IsolationDecision::Worktree {
                    path: self.worktree_base.join(&task.id),
                    branch: format!("orca/{}", task.id),
                }
            }
        }
    }

    /// Create a git worktree at `path` with a new branch `branch`.
    pub fn create_worktree(&self, path: &Path, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["worktree", "add", "-b", branch])
            .arg(path)
            .current_dir(&self.project_dir)
            .output()
            .context("failed to execute git worktree add")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git worktree add failed: {}", stderr.trim());
        }
        Ok(())
    }

    /// Remove a git worktree at `path`.
    pub fn remove_worktree(&self, path: &Path) -> Result<()> {
        let output = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(path)
            .current_dir(&self.project_dir)
            .output()
            .context("failed to execute git worktree remove")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git worktree remove failed: {}", stderr.trim());
        }
        Ok(())
    }

    /// Merge `branch` into `target` using --no-ff.
    ///
    /// Checks out `target`, then merges `branch`. Returns `MergeResult::Conflict`
    /// if the merge produces conflicts.
    pub fn merge_branch(&self, branch: &str, target: &str) -> Result<MergeResult> {
        // Checkout target branch.
        let checkout = Command::new("git")
            .args(["checkout", target])
            .current_dir(&self.project_dir)
            .output()
            .context("failed to execute git checkout")?;

        if !checkout.status.success() {
            let stderr = String::from_utf8_lossy(&checkout.stderr);
            anyhow::bail!("git checkout {} failed: {}", target, stderr.trim());
        }

        // Merge with --no-ff.
        let merge = Command::new("git")
            .args(["merge", "--no-ff", branch])
            .current_dir(&self.project_dir)
            .output()
            .context("failed to execute git merge")?;

        if merge.status.success() {
            Ok(MergeResult::Success)
        } else {
            let stderr = String::from_utf8_lossy(&merge.stderr);
            // Abort the failed merge to leave repo in a clean state.
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(&self.project_dir)
                .output();
            Ok(MergeResult::Conflict(stderr.trim().to_string()))
        }
    }

    /// Delete a local branch.
    pub fn delete_branch(&self, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["branch", "-d", branch])
            .current_dir(&self.project_dir)
            .output()
            .context("failed to execute git branch -d")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git branch -d {} failed: {}", branch, stderr.trim());
        }
        Ok(())
    }
}

/// Check whether two tasks touch any of the same files.
pub fn has_file_overlap(a: &TaskSpec, b: &TaskSpec) -> bool {
    let files_a: HashSet<&str> = a.context.files.iter().map(|s| s.as_str()).collect();
    let files_b: HashSet<&str> = b.context.files.iter().map(|s| s.as_str()).collect();
    !files_a.is_disjoint(&files_b)
}
