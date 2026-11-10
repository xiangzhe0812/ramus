use gpui::actions;

actions!(
    ramus,
    [
        OpenRepository,
        ToggleBranches,
        ToggleWorktrees,
        DiscardSelected,
        SelectNextFile,
        SelectPreviousFile,
        ToggleSelectedFile,
        ShowChanges,
        ShowHistory,
        ShowStashes,
        TogglePalette,
        CloneRepository,
        ShowAccounts,
        AddRemote,
        NewBranchHere
    ]
);
