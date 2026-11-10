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
mod history_view;
mod palette;
mod palette_view;
mod repo_state;
mod secrets;
mod settings;
mod stash_view;
mod syntax;
#[cfg(test)]
mod testutil;
mod theme;
mod title_bar;
mod ui;
mod welcome;
mod workspace;
mod worktree_popover;

use gpui::{AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};

use crate::{
    accounts::AccountsState,
    actions::{
        CloneRepository, DiscardSelected, OpenRepository, SelectNextFile, SelectPreviousFile,
        ShowChanges, ShowHistory, ShowStashes, ToggleBranches, TogglePalette, ToggleSelectedFile,
        ToggleWorktrees,
    },
    github::Endpoints,
    repo_state::RepoState,
    secrets::Keychain,
    theme::Theme,
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

fn main() {
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx| {
            gpui_component::init(cx);
            let theme = Theme::dark();
            component_theme::apply(&theme, cx);
            cx.set_global(theme);
            cx.bind_keys([
                KeyBinding::new("cmd-o", OpenRepository, None),
                KeyBinding::new("cmd-b", ToggleBranches, None),
                KeyBinding::new("cmd-shift-b", ToggleWorktrees, None),
                KeyBinding::new("cmd-shift-o", CloneRepository, None),
                KeyBinding::new("cmd-k", TogglePalette, None),
                KeyBinding::new("cmd-1", ShowChanges, None),
                KeyBinding::new("cmd-2", ShowHistory, None),
                KeyBinding::new("cmd-3", ShowStashes, None),
                KeyBinding::new("cmd-backspace", DiscardSelected, Some("Workspace")),
                KeyBinding::new("down", SelectNextFile, Some("ChangesList")),
                KeyBinding::new("up", SelectPreviousFile, Some("ChangesList")),
                KeyBinding::new("space", ToggleSelectedFile, Some("ChangesList")),
            ]);

            let repo_state = cx.new(|_| RepoState::persistent());
            if let Some(path) = repository_argument(std::env::args_os().skip(1)) {
                repo_state.update(cx, |state, cx| state.open(path, cx));
            }

            let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
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
                    let workspace = cx.new(|cx| Workspace::new(repo_state, accounts, window, cx));
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

    use super::repository_argument;

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

    #[test]
    fn flags_macos_adds_when_launching_are_not_paths() {
        assert_eq!(repository_argument(args(&["-psn_0_12345"])), None);
        assert_eq!(
            repository_argument(args(&["-psn_0_12345", "/work/repo"])),
            Some(PathBuf::from("/work/repo"))
        );
    }
}
