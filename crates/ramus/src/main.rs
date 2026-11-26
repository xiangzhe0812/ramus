mod accounts;
mod accounts_view;
mod actions;
mod add_remote_dialog;
mod assets;
mod branch_popover;
mod changes_view;
mod clone_dialog;
mod component_theme;
mod conflicts_view;
mod diff_rows;
mod diff_view;
mod error_banner;
mod github;
mod graph;
mod history_view;
mod palette;
mod palette_view;
mod pull_requests;
mod repo_state;
mod repository_popover;
mod secrets;
mod settings;
mod stash_view;
mod syntax;
#[cfg(test)]
mod testutil;
mod theme;
mod title_bar;
mod ui;
mod update_check;
mod welcome;
mod workspace;
mod worktree_popover;
mod worktrees_view;

use gpui::{
    AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};

use crate::{
    accounts::AccountsState,
    actions::{
        CloneRepository, DiscardSelected, OpenRepository, SelectNextFile, SelectPreviousFile,
        ShowChanges, ShowHistory, ShowStashes, ShowWorktrees, ToggleBranches, TogglePalette,
        ToggleRepositories, ToggleSelectedFile, ToggleWorktrees,
    },
    github::Endpoints,
    repo_state::RepoState,
    secrets::Keychain,
    update_check::UpdateCheck,
    workspace::Workspace,
};

/// The repository path given on the command line, if any. Anything starting with `-` is
/// not a path: macOS can launch an app with a `-psn_…` argument, which must not be
/// mistaken for a folder to open.
fn repository_argument(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    args.into_iter()
        .find(|arg| !arg.to_string_lossy().starts_with('-'))
        .map(Into::into)
}

/// The folder to open at launch: the one named on the command line, otherwise the most
/// recent repository whose folder still exists. `None` shows Welcome.
fn launch_path(
    argument: Option<std::path::PathBuf>,
    recent: &[settings::RecentRepository],
) -> Option<std::path::PathBuf> {
    argument.or_else(|| {
        recent
            .iter()
            .map(|entry| &entry.open_path)
            .find(|path| path.exists())
            .cloned()
    })
}

fn main() {
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx| {
            cx.text_system()
                .add_fonts(assets::fonts())
                .expect("the bundled fonts load");
            gpui_component::init(cx);
            cx.bind_keys([
                KeyBinding::new("cmd-o", OpenRepository, None),
                KeyBinding::new("cmd-b", ToggleBranches, None),
                KeyBinding::new("cmd-shift-b", ToggleWorktrees, None),
                KeyBinding::new("cmd-shift-o", CloneRepository, None),
                KeyBinding::new("cmd-t", ToggleRepositories, None),
                KeyBinding::new("cmd-k", TogglePalette, None),
                KeyBinding::new("cmd-1", ShowChanges, None),
                KeyBinding::new("cmd-2", ShowHistory, None),
                KeyBinding::new("cmd-3", ShowStashes, None),
                KeyBinding::new("cmd-4", ShowWorktrees, None),
                KeyBinding::new("cmd-backspace", DiscardSelected, Some("Workspace")),
                KeyBinding::new("down", SelectNextFile, Some("ChangesList")),
                KeyBinding::new("up", SelectPreviousFile, Some("ChangesList")),
                KeyBinding::new("space", ToggleSelectedFile, Some("ChangesList")),
            ]);

            let repo_state = cx.new(|_| RepoState::persistent());
            let argument = repository_argument(std::env::args_os().skip(1));
            let launch = launch_path(argument, repo_state.read(cx).recent_repositories());
            if let Some(path) = launch {
                repo_state.update(cx, |state, cx| state.open(path, cx));
            }

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // No native title bar: our own bar is the top of the window, with the
                    // traffic lights inside it, and it moves the window itself.
                    titlebar: Some(TitlebarOptions {
                        title: Some("Ramus".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(
                            px(title_bar::TRAFFIC_LIGHTS_POSITION.0),
                            px(title_bar::TRAFFIC_LIGHTS_POSITION.1),
                        )),
                    }),
                    app_owns_titlebar_drag: true,
                    ..Default::default()
                },
                |window, cx| {
                    let accounts = cx.new(|_| {
                        AccountsState::new(
                            std::sync::Arc::new(Keychain::github()),
                            Endpoints::default(),
                            crate::github::client_id(),
                        )
                    });
                    let update_check = cx.new(|cx| {
                        let mut check = UpdateCheck::new(Endpoints::default());
                        check.start(cx);
                        check
                    });
                    let workspace =
                        cx.new(|cx| Workspace::new(repo_state, accounts, update_check, window, cx));
                    cx.new(|cx| gpui_component::Root::new(workspace, window, cx))
                },
            )
            .expect("failed to open window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::PathBuf};

    use super::{launch_path, repository_argument};
    use crate::settings::RecentRepository;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_first_plain_argument_is_the_repository() {
        assert_eq!(
            repository_argument(args(&["/work/repo"])),
            Some(PathBuf::from("/work/repo"))
        );
        assert_eq!(repository_argument(args(&[])), None);
    }

    fn entry(open_path: &std::path::Path) -> RecentRepository {
        RecentRepository {
            repository: open_path.join(".git"),
            open_path: open_path.to_owned(),
        }
    }

    #[test]
    fn the_argument_wins_over_the_list() {
        let remembered = tempfile::TempDir::new().unwrap();
        assert_eq!(
            launch_path(Some("/work/repo".into()), &[entry(remembered.path())]),
            Some(PathBuf::from("/work/repo"))
        );
    }

    #[test]
    fn launch_skips_entries_whose_folder_is_gone() {
        let kept = tempfile::TempDir::new().unwrap();
        let gone = entry(std::path::Path::new("/nowhere/at/all"));
        assert_eq!(
            launch_path(None, &[gone, entry(kept.path())]),
            Some(kept.path().to_owned())
        );
    }

    #[test]
    fn nothing_to_reopen_gives_none() {
        assert_eq!(launch_path(None, &[]), None);
    }

    #[test]
    fn flags_macos_adds_when_launching_are_not_paths() {
        assert_eq!(repository_argument(args(&["-psn_0_12345"])), None);
        assert_eq!(
            repository_argument(args(&["-psn_0_12345", "/work/repo"])),
            Some(PathBuf::from("/work/repo"))
        );
    }
}
