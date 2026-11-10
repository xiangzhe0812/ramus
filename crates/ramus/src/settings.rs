//! Settings that outlive a session, stored as JSON in the user's Application Support folder.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Repositories the user has said may run their own configured programs
    /// (hooks, credential helpers, ssh commands). Stored as resolved paths: a repository's
    /// shared git folder, or a worktree folder for records from before trust was per
    /// repository.
    pub trusted_repositories: BTreeSet<PathBuf>,
}

/// `~/Library/Application Support/Ramus/settings.json`.
pub fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/Ramus")
            .join("settings.json"),
    )
}

/// The path a repository is remembered by: symlinks resolved, so the same folder reached
/// two ways is one repository.
fn identity(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_owned())
}

impl Settings {
    /// Reads settings from `path`. A missing file is a fresh start. A file that can't be
    /// parsed is moved aside to `settings.json.broken` rather than overwritten, so nothing
    /// the user had is silently lost, and defaults are used.
    pub fn load(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => return Self::default(),
        };
        match serde_json::from_str(&text) {
            Ok(settings) => settings,
            Err(_) => {
                let _ = fs::rename(path, path.with_extension("json.broken"));
                Self::default()
            }
        }
    }

    /// Writes settings to `path`, creating its folder. The write goes to a temporary file
    /// first so a crash can't leave a half-written settings file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temporary, path)
    }

    /// Trusted if the shared git folder, or (records from before per-repository trust)
    /// the worktree's own folder, is on record.
    pub fn is_trusted(&self, root: &Path, common_dir: &Path) -> bool {
        self.trusted_repositories.contains(&identity(common_dir))
            || self.trusted_repositories.contains(&identity(root))
    }

    /// Records trust for the whole repository, by its shared git folder.
    pub fn trust(&mut self, common_dir: &Path) {
        self.trusted_repositories.insert(identity(common_dir));
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn a_missing_file_gives_defaults() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            Settings::load(&dir.path().join("nope.json")),
            Settings::default()
        );
    }

    #[test]
    fn settings_survive_a_save_and_load() {
        let dir = TempDir::new().unwrap();
        let (root, common) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let path = dir.path().join("nested/Ramus/settings.json");

        let mut settings = Settings::default();
        settings.trust(common.path());
        settings.save(&path).unwrap();

        let loaded = Settings::load(&path);
        assert!(loaded.is_trusted(root.path(), common.path()));
        assert_eq!(loaded, settings);
    }

    #[test]
    fn a_repository_is_not_trusted_until_it_is_trusted() {
        let (root, common) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut settings = Settings::default();
        assert!(!settings.is_trusted(root.path(), common.path()));

        settings.trust(common.path());

        assert!(settings.is_trusted(root.path(), common.path()));
    }

    #[test]
    fn trusting_a_repository_trusts_every_worktree_that_shares_its_git_folder() {
        let common = TempDir::new().unwrap();
        let (main, linked) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut settings = Settings::default();

        settings.trust(common.path());

        assert!(settings.is_trusted(main.path(), common.path()));
        assert!(settings.is_trusted(linked.path(), common.path()));
    }

    #[test]
    fn a_folder_trusted_before_the_upgrade_is_still_trusted() {
        let (root, common) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut settings = Settings::default();
        settings
            .trusted_repositories
            .insert(root.path().canonicalize().unwrap());

        assert!(settings.is_trusted(root.path(), common.path()));
    }

    #[test]
    fn another_repository_stays_untrusted() {
        let common = TempDir::new().unwrap();
        let (other_root, other_common) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut settings = Settings::default();

        settings.trust(common.path());

        assert!(!settings.is_trusted(other_root.path(), other_common.path()));
    }

    #[test]
    fn the_same_folder_reached_through_a_symlink_is_one_repository() {
        let (root, common) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let links = TempDir::new().unwrap();
        let link = links.path().join("alias");
        std::os::unix::fs::symlink(common.path(), &link).unwrap();

        let mut settings = Settings::default();
        settings.trust(&link);

        assert!(settings.is_trusted(root.path(), common.path()));
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_not_overwritten() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{ this is not json").unwrap();

        assert_eq!(Settings::load(&path), Settings::default());
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("settings.json.broken")).unwrap(),
            "{ this is not json"
        );
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"from_a_newer_version": true}"#).unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        Settings::default().save(&path).unwrap();
        assert!(!dir.path().join("settings.json.tmp").exists());
    }
}
