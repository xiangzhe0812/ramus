use gpui::actions;

actions!(
    ramus,
    [
        OpenRepository,
        ToggleBranches,
        ToggleWorktrees,
        ToggleRepositories,
        DiscardSelected,
        SelectNextFile,
        SelectPreviousFile,
        ToggleSelectedFile,
        ShowChanges,
        ShowHistory,
        ShowStashes,
        ShowWorktrees,
        TogglePalette,
        CloneRepository,
        ShowAccounts,
        AddRemote,
        NewBranchHere
    ]
);
