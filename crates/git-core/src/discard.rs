//! Throwing changes away, safely: every discard first saves what it is about to destroy.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    ChangeKind, Error, FileChange, Repo,
    runner::{run_git, run_git_with_env},
};

/// What a discard saved, so it can be undone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscardBackup {
    /// Names this discard in the backup folder and the hidden ref.
    pub id: String,
    /// A commit holding the discarded files as they were, for tracked changes.
    pub commit: Option<String>,
    /// Tracked paths that were discarded.
    pub tracked: Vec<String>,
    /// Untracked paths moved aside, relative to the repository root.
    pub moved: Vec<String>,
}

/// `root/relative`, after checking that every folder on the way is a real directory.
///
/// A folder that has become a symlink since git listed the path could lead anywhere, and
/// writing or deleting through it would reach outside the repository. With
/// `create_parents`, missing folders are created one at a time, with the same check.
fn checked_path(root: &Path, relative: &str, create_parents: bool) -> Result<PathBuf, Error> {
    let unsafe_path = || Error::UnsafePath(relative.to_owned());
    let relative_path = Path::new(relative);
    // Only plain names joined by single slashes, exactly as git lists them: no `..`, no `.`,
    // no empty parts, and not empty (which would mean the repository root itself).
    let names: Option<Vec<&std::ffi::OsStr>> = relative_path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let is_plain = names.as_ref().is_some_and(|names| {
        !names.is_empty()
            && names
                .iter()
                .map(|name| name.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
                == relative
    });
    if !is_plain {
        return Err(unsafe_path());
    }

    let mut current = root.to_owned();
    let mut components = relative_path.components().peekable();
    while let Some(component) = components.next() {
        current.push(component);
        if components.peek().is_none() {
            break; // The last part is the file itself, not a folder to walk through.
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(unsafe_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound && create_parents => {
                fs::create_dir(&current)?
            }
            // Nothing exists below here, so there is nothing to follow.
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root.join(relative_path))
}

impl DiscardBackup {
    pub fn file_count(&self) -> usize {
        self.tracked.len() + self.moved.len()
    }
}

impl Repo {
    /// `.git/ramus`, resolved the way git would (it may be a worktree's private folder).
    fn ramus_dir(&self) -> Result<PathBuf, Error> {
        let output = run_git(self.root(), &["rev-parse", "--git-path", "ramus"])?;
        let path = PathBuf::from(String::from_utf8_lossy(&output).trim());
        Ok(if path.is_absolute() {
            path
        } else {
            self.root().join(path)
        })
    }

    /// Discards the working-tree and staged changes of `changes`. Nothing is lost for
    /// good: tracked files are saved as a commit under `refs/ramus/discarded/`, untracked
    /// ones are moved into `.git/ramus/discarded/`. Undo with [`Repo::undo_discard`].
    pub fn discard(&self, changes: &[&FileChange]) -> Result<DiscardBackup, Error> {
        if changes.iter().any(|c| c.kind() == ChangeKind::Conflicted) {
            return Err(Error::Parse(
                "conflicted files can't be discarded yet".into(),
            ));
        }
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let id = millis.to_string();

        let (untracked, tracked): (Vec<&FileChange>, Vec<&FileChange>) = changes
            .iter()
            .copied()
            .partition(|c| c.worktree == Some(ChangeKind::Untracked));
        let tracked_paths: Vec<&str> = tracked.iter().flat_map(|c| c.pathspecs()).collect();

        // 1. Save everything first. If this fails, nothing is touched.
        let commit = if tracked_paths.is_empty() {
            None
        } else {
            Some(self.save_tracked(&id, &tracked_paths)?)
        };
        let mut moved = Vec::new();
        for change in &untracked {
            self.move_aside(&id, &change.path)?;
            moved.push(change.path.clone());
        }

        // 2. Then throw the tracked changes away.
        for change in &tracked {
            self.restore_to_head(change)?;
        }

        Ok(DiscardBackup {
            id,
            commit,
            tracked: tracked_paths.iter().map(|p| (*p).to_owned()).collect(),
            moved,
        })
    }

    /// Commits the current state of `paths` (working tree, including deletions) without
    /// touching the real index, and points a hidden ref at it.
    fn save_tracked(&self, id: &str, paths: &[&str]) -> Result<String, Error> {
        let dir = self.ramus_dir()?;
        fs::create_dir_all(&dir)?;
        let index = dir.join(format!("index-{id}.tmp"));
        let run = |args: &[&str]| {
            let mut vars = vec![("GIT_INDEX_FILE", index.as_os_str())];
            vars.extend([
                ("GIT_AUTHOR_NAME", std::ffi::OsStr::new("Ramus")),
                ("GIT_AUTHOR_EMAIL", std::ffi::OsStr::new("ramus@localhost")),
                ("GIT_COMMITTER_NAME", std::ffi::OsStr::new("Ramus")),
                (
                    "GIT_COMMITTER_EMAIL",
                    std::ffi::OsStr::new("ramus@localhost"),
                ),
            ]);
            run_git_with_env(self.root(), args, &vars)
        };

        let has_head = run_git(self.root(), &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok();
        let result = (|| {
            if has_head {
                run(&["read-tree", "HEAD"])?;
            }
            let mut add = vec!["add", "-A", "--"];
            add.extend(paths);
            run(&add)?;
            let tree = String::from_utf8_lossy(&run(&["write-tree"])?)
                .trim()
                .to_owned();
            let mut commit_args = vec![
                "commit-tree",
                &tree,
                "-m",
                "Ramus: backup of discarded changes",
            ];
            if has_head {
                commit_args.extend(["-p", "HEAD"]);
            }
            let commit = String::from_utf8_lossy(&run(&commit_args)?)
                .trim()
                .to_owned();
            run_git(
                self.root(),
                &["update-ref", &format!("refs/ramus/discarded/{id}"), &commit],
            )?;
            Ok(commit)
        })();
        let _ = fs::remove_file(&index);
        result
    }

    fn move_aside(&self, id: &str, path: &str) -> Result<(), Error> {
        let from = checked_path(self.root(), path, false)?;
        let to = self.ramus_dir()?.join("discarded").join(id).join(path);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from, &to)?;
        Ok(())
    }

    fn restore_to_head(&self, change: &FileChange) -> Result<(), Error> {
        let in_head = |kind: Option<ChangeKind>| kind != Some(ChangeKind::Added);
        match (change.index, change.original_path.as_deref()) {
            // A rename: the new name doesn't exist in HEAD, the old one does.
            (Some(ChangeKind::Renamed | ChangeKind::Copied), Some(old)) => {
                run_git(self.root(), &["rm", "-f", "-q", "--", &change.path])?;
                if change.index == Some(ChangeKind::Renamed) {
                    run_git(
                        self.root(),
                        &[
                            "restore",
                            "--source=HEAD",
                            "--staged",
                            "--worktree",
                            "--",
                            old,
                        ],
                    )?;
                }
                Ok(())
            }
            // New in the index: there is nothing in HEAD to go back to, so remove it.
            _ if !in_head(change.index) => {
                run_git(self.root(), &["rm", "-f", "-q", "--", &change.path]).map(drop)
            }
            _ => run_git(
                self.root(),
                &[
                    "restore",
                    "--source=HEAD",
                    "--staged",
                    "--worktree",
                    "--",
                    &change.path,
                ],
            )
            .map(drop),
        }
    }

    /// Puts back what `backup` saved. Files changed again since are overwritten by the
    /// saved version only if they don't exist yet; existing untracked copies are kept.
    pub fn undo_discard(&self, backup: &DiscardBackup) -> Result<(), Error> {
        if let Some(commit) = &backup.commit {
            for path in &backup.tracked {
                let saved = run_git(self.root(), &["ls-tree", "--name-only", commit, "--", path])?;
                if saved.is_empty() {
                    // It was deleted when discarded; it came back, so delete it again.
                    match fs::remove_file(checked_path(self.root(), path, false)?) {
                        Err(error) if error.kind() != io::ErrorKind::NotFound => {
                            return Err(error.into());
                        }
                        _ => {}
                    }
                } else {
                    run_git(
                        self.root(),
                        &[
                            "restore",
                            &format!("--source={commit}"),
                            "--worktree",
                            "--",
                            path,
                        ],
                    )?;
                }
            }
        }
        let aside = self.ramus_dir()?.join("discarded").join(&backup.id);
        for path in &backup.moved {
            let to = checked_path(self.root(), path, true)?;
            // Something is there now (even a dangling symlink): keep it, don't overwrite.
            if fs::symlink_metadata(&to).is_ok() {
                continue;
            }
            fs::rename(aside.join(path), to)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn paths_that_climb_out_or_are_absolute_are_refused() {
        let root = TempDir::new().unwrap();
        for bad in ["../outside", "a/../../outside", "/etc/passwd", "a/./b", ""] {
            assert!(
                checked_path(root.path(), bad, true).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_plain_nested_path_is_accepted_and_missing_folders_are_created_on_request() {
        let root = TempDir::new().unwrap();
        let path = checked_path(root.path(), "a/b/c.txt", true).unwrap();
        assert_eq!(path, root.path().join("a/b/c.txt"));
        assert!(root.path().join("a/b").is_dir());
        // Without the request, nothing is created.
        let other = checked_path(root.path(), "x/y/z.txt", false).unwrap();
        assert_eq!(other, root.path().join("x/y/z.txt"));
        assert!(!root.path().join("x").exists());
    }

    #[test]
    fn a_symlink_in_the_middle_is_refused_even_if_it_points_inside() {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join("real")).unwrap();
        std::os::unix::fs::symlink(root.path().join("real"), root.path().join("alias")).unwrap();
        assert!(matches!(
            checked_path(root.path(), "alias/file.txt", true),
            Err(Error::UnsafePath(_))
        ));
    }
}
