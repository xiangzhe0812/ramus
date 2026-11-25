use std::collections::BTreeSet;

use git_core::{Choice, ConflictKind, ConflictRegion, OperationKind, Side, conflict_regions};
use gpui::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder, px,
};
use gpui_component::input::{Textarea, TextareaState};

use crate::{
    diff_view::message,
    repo_state::RepoState,
    theme::{MONO_FONT, Theme},
    ui::panel,
};

/// The file being resolved, as read from disk.
struct Loaded {
    path: String,
    kind: ConflictKind,
    /// The text is in the editor; `false` for binary files and delete/modify conflicts.
    text: bool,
}

/// Resolves the conflicts a merge, cherry-pick or revert stopped on.
pub struct ConflictsView {
    repo_state: Entity<RepoState>,
    /// Every path that has been in conflict during this episode, so files already
    /// resolved stay in the list (marked as such) until the operation is finished.
    known: BTreeSet<String>,
    selected: Option<String>,
    loaded: Option<Loaded>,
    editor: Entity<TextareaState>,
    generation: u64,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ConflictsView {
    pub fn new(repo_state: Entity<RepoState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| TextareaState::new(window, cx));
        let _subscriptions = vec![
            cx.observe_in(&repo_state, window, |this, _, window, cx| {
                this.sync(window, cx)
            }),
            cx.observe(&editor, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            repo_state,
            known: BTreeSet::new(),
            selected: None,
            loaded: None,
            editor,
            generation: 0,
            tasks: Vec::new(),
            _subscriptions,
        };
        this.sync(window, cx);
        this
    }

    /// Keeps the file list and the selection in step with the repository.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.repo_state.read(cx);
        let current = state.conflicted_paths();
        if current.is_empty() && state.in_progress().is_none() {
            self.known.clear();
            self.selected = None;
            self.loaded = None;
            cx.notify();
            return;
        }
        self.known.extend(current.iter().cloned());
        // Stay on the file being worked on, unless it is resolved: then move on to the
        // next one that still needs attention.
        let selected_open = self
            .selected
            .as_ref()
            .is_some_and(|path| current.contains(path));
        if !selected_open {
            let next = current.first().cloned().or_else(|| {
                self.selected
                    .clone()
                    .filter(|path| self.known.contains(path))
                    .or_else(|| self.known.iter().next().cloned())
            });
            if next != self.selected || self.loaded.is_none() {
                self.select(next, window, cx);
            }
        } else if self.loaded.is_none() {
            let selected = self.selected.clone();
            self.select(selected, window, cx);
        }
        cx.notify();
    }

    fn select(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = path.clone();
        self.loaded = None;
        self.generation += 1;
        let generation = self.generation;
        let (Some(path), Some(repo)) = (path, self.repo_state.read(cx).repo().cloned()) else {
            cx.notify();
            return;
        };
        // A file that is already resolved has nothing to resolve; just show that.
        if !self.repo_state.read(cx).conflicted_paths().contains(&path) {
            cx.notify();
            return;
        }
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move {
                        let kind = repo.conflict_kind(&path)?;
                        let text = if kind == ConflictKind::BothChanged {
                            repo.conflict_text(&path)?
                        } else {
                            None
                        };
                        Ok::<_, git_core::Error>((kind, text))
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok((kind, text)) => {
                        let editable = text.is_some();
                        if let Some(text) = text {
                            this.editor
                                .update(cx, |editor, cx| editor.set_value(text, window, cx));
                        }
                        this.loaded = Some(Loaded {
                            path,
                            kind,
                            text: editable,
                        });
                    }
                    Err(error) => this
                        .repo_state
                        .update(cx, |state, cx| state.report(error.to_string(), cx)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn choose(
        &mut self,
        index: usize,
        choice: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.editor.read(cx).value().to_string();
        let resolved = git_core::resolve_region(&text, index, choice);
        self.editor
            .update(cx, |editor, cx| editor.set_value(resolved, window, cx));
        cx.notify();
    }

    fn mark_resolved(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let (path, text) = (
            loaded.path.clone(),
            self.editor.read(cx).value().to_string(),
        );
        // Never stage a file that still has conflict markers in it.
        if !conflict_regions(&text).is_empty() {
            return;
        }
        self.repo_state
            .update(cx, |state, cx| state.resolve_with_text(path, text, cx));
    }

    fn whole_file(&mut self, action: WholeFile, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let path = loaded.path.clone();
        self.repo_state.update(cx, |state, cx| match action {
            WholeFile::Side(side) => state.resolve_with_side(path, side, cx),
            WholeFile::Keep => state.resolve_keep(path, cx),
            WholeFile::Delete => state.resolve_delete(path, cx),
        });
    }

    fn region_card(
        &self,
        index: usize,
        total: usize,
        region: &ConflictRegion,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let side = |title: &str, label: &str, lines: &[String], tint, fg| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(theme.metrics.row_height))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(SharedString::from(title.to_owned()))
                        .child(
                            div()
                                .px(px(8.))
                                .rounded(px(theme.metrics.radius_md))
                                .bg(tint)
                                .text_color(fg)
                                .font_family(MONO_FONT)
                                .text_size(px(theme.metrics.text_xs))
                                .child(SharedString::from(label.to_owned())),
                        ),
                )
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(theme.metrics.text_sm))
                        .children(lines.iter().map(|line| {
                            div()
                                .px(px(12.))
                                .h(px(22.))
                                .bg(tint)
                                .whitespace_nowrap()
                                .child(SharedString::from(
                                    line.trim_end_matches(['\n', '\r']).to_owned(),
                                ))
                        })),
                )
        };
        let pick = |id: (&'static str, usize), label: &'static str, choice: Choice| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(theme.metrics.radius_md))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|b| b.bg(theme.surface_selected))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.choose(index, choice, window, cx)),
                )
                .child(label)
        };
        div()
            .flex_none()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(theme.metrics.row_height))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_color(theme.text_muted).child(format!(
                        "Conflict {} of {}",
                        index + 1,
                        total
                    )))
                    .child(pick(("use-ours", index), "Use ours", Choice::Ours))
                    .child(pick(("use-theirs", index), "Use theirs", Choice::Theirs))
                    .child(pick(("use-both", index), "Use both", Choice::Both)),
            )
            .child(
                div()
                    .flex()
                    .child(side(
                        "Ours",
                        &region.ours_label,
                        &region.ours,
                        theme.renamed.background,
                        theme.renamed.text,
                    ))
                    .child(side(
                        "Theirs",
                        &region.theirs_label,
                        &region.theirs,
                        theme.diff_hunk_background,
                        theme.diff_hunk_text,
                    )),
            )
            .into_any_element()
    }

    fn resolver(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(loaded) = &self.loaded else {
            let resolved = self
                .selected
                .as_ref()
                .is_some_and(|path| self.known.contains(path));
            return message(
                theme,
                if resolved {
                    "This file is resolved"
                } else {
                    "Select a file to resolve"
                },
            );
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(theme.metrics.row_height))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(theme.metrics.radius_md))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|b| b.bg(theme.surface_selected))
                .child(label)
        };

        if loaded.text {
            let text = self.editor.read(cx).value().to_string();
            let regions = conflict_regions(&text);
            let total = regions.len();
            let cards: Vec<AnyElement> = regions
                .iter()
                .enumerate()
                .map(|(ix, region)| self.region_card(ix, total, region, theme, cx))
                .collect();
            let done = regions.is_empty();
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_none()
                        .max_h(px(300.))
                        .overflow_hidden()
                        .children(cards),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(40.))
                                .flex_none()
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .border_b_1()
                                .border_color(theme.border)
                                .child(div().font_weight(gpui::FontWeight::MEDIUM).child("Result"))
                                .child(
                                    div()
                                        .px(px(9.))
                                        .py(px(2.))
                                        .rounded(px(theme.metrics.radius_md))
                                        .text_size(px(theme.metrics.text_xs))
                                        .bg(if done {
                                            theme.added.background
                                        } else {
                                            theme.modified.background
                                        })
                                        .text_color(if done {
                                            theme.added.text
                                        } else {
                                            theme.modified.text
                                        })
                                        .child(if done {
                                            "Ready".to_owned()
                                        } else {
                                            format!("{total} to resolve")
                                        }),
                                )
                                .child(div().flex_1())
                                .child(button("take-ours", "Take all ours").on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.whole_file(WholeFile::Side(Side::Ours), cx)
                                    },
                                )))
                                .child(button("take-theirs", "Take all theirs").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.whole_file(WholeFile::Side(Side::Theirs), cx)
                                    }),
                                ))
                                .child(
                                    button("mark-resolved", "Mark as resolved")
                                        .bg(theme.accent)
                                        .border_color(theme.accent)
                                        .text_color(theme.accent_text)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .when(!done, |b| b.opacity(0.5))
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.mark_resolved(cx)),
                                        ),
                                ),
                        )
                        .child(
                            div().flex_1().min_h_0().p(px(8.)).child(
                                Textarea::new(&self.editor)
                                    .aria_label("Resolved file")
                                    .h_full(),
                            ),
                        ),
                )
                .into_any_element();
        }

        // Not a text merge: the choice is about the whole file.
        let explanation = match loaded.kind {
            ConflictKind::BothChanged => {
                "This file can't be shown as text. Choose which version to keep."
            }
            ConflictKind::DeletedByThem => "You changed this file, but the other side deleted it.",
            ConflictKind::DeletedByUs => "The other side changed this file, but you deleted it.",
            ConflictKind::BothDeleted => "Both sides deleted this file.",
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .child(div().child(explanation))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .when(loaded.kind == ConflictKind::BothChanged, |row| {
                        row.child(button("take-ours", "Keep ours").on_click(cx.listener(
                            |this, _, _, cx| this.whole_file(WholeFile::Side(Side::Ours), cx),
                        )))
                        .child(
                            button("take-theirs", "Take theirs").on_click(cx.listener(
                                |this, _, _, cx| this.whole_file(WholeFile::Side(Side::Theirs), cx),
                            )),
                        )
                    })
                    .when(loaded.kind != ConflictKind::BothChanged, |row| {
                        row.when(loaded.kind != ConflictKind::BothDeleted, |row| {
                            row.child(button("keep-file", "Keep the file").on_click(
                                cx.listener(|this, _, _, cx| this.whole_file(WholeFile::Keep, cx)),
                            ))
                        })
                        .child(
                            button("delete-file", "Delete the file").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.whole_file(WholeFile::Delete, cx)
                                }),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
enum WholeFile {
    Side(Side),
    Keep,
    Delete,
}

/// The banner's sentence: what is waiting, and how much is left.
pub fn operation_summary(
    kind: OperationKind,
    merging: Option<&str>,
    branch: &str,
    open_conflicts: usize,
) -> String {
    let what = match (kind, merging) {
        (OperationKind::Merge, Some(from)) => format!("Merging {from} into {branch}"),
        (OperationKind::Merge, None) => format!("Merging into {branch}"),
        (OperationKind::CherryPick, _) => format!("Cherry-picking onto {branch}"),
        (OperationKind::Revert, _) => format!("Reverting on {branch}"),
    };
    match open_conflicts {
        0 => format!("{what} · every conflict is resolved; continue to finish"),
        1 => format!("{what} · 1 file still has conflicts"),
        n => format!("{what} · {n} files still have conflicts"),
    }
}

/// "Abort merge", "Abort cherry-pick" or "Abort revert".
pub fn abort_label(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Merge => "Abort merge",
        OperationKind::CherryPick => "Abort cherry-pick",
        OperationKind::Revert => "Abort revert",
    }
}

/// "Continue merge", "Continue cherry-pick" or "Continue revert".
pub fn continue_label(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Merge => "Continue merge",
        OperationKind::CherryPick => "Continue cherry-pick",
        OperationKind::Revert => "Continue revert",
    }
}

impl Render for ConflictsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let state = self.repo_state.read(cx);
        let open = state.conflicted_paths();
        let in_progress = state.in_progress().cloned();
        let total = self.known.len();
        let resolved = total - self.known.iter().filter(|path| open.contains(path)).count();
        let can_continue = in_progress.is_some() && open.is_empty();

        let rows: Vec<AnyElement> = self
            .known
            .iter()
            .enumerate()
            .map(|(ix, path)| {
                let is_open = open.contains(path);
                let selected = self.selected.as_ref() == Some(path);
                let (name, dir) = match path.rsplit_once('/') {
                    Some((dir, name)) => (name.to_owned(), dir.to_owned()),
                    None => (path.clone(), String::new()),
                };
                let target = path.clone();
                div()
                    .id(("conflict-file", ix))
                    .h(px(44.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.surface_selected))
                    .hover(|row| row.bg(theme.surface_selected))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(Some(target.clone()), window, cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .truncate()
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                div()
                                    .text_size(px(theme.metrics.text_xs))
                                    .text_color(theme.text_muted)
                                    .truncate()
                                    .child(SharedString::from(dir)),
                            ),
                    )
                    .child(
                        div()
                            .px(px(9.))
                            .py(px(2.))
                            .rounded(px(theme.metrics.radius_md))
                            .text_size(px(theme.metrics.text_xs))
                            .bg(if is_open {
                                theme.modified.background
                            } else {
                                theme.added.background
                            })
                            .text_color(if is_open {
                                theme.modified.text
                            } else {
                                theme.added.text
                            })
                            .child(if is_open { "Conflict" } else { "Resolved" }),
                    )
                    .into_any_element()
            })
            .collect();

        let resolver = self.resolver(&theme, cx);
        let label = in_progress
            .as_ref()
            .map(|p| continue_label(p.kind))
            .unwrap_or("Continue");

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap(px(12.))
                    .px(px(12.))
                    .pb(px(12.))
                    .child(
                        panel(&theme)
                            .w(px(300.))
                            .flex_none()
                            .child(
                                div()
                                    .h(px(40.))
                                    .flex_none()
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Conflicted files"),
                            )
                            .children(rows),
                    )
                    .child(panel(&theme).flex_1().min_w_0().child(resolver)),
            )
            .child(
                div()
                    .h(px(44.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child(format!("{resolved} of {total} files resolved")),
                    )
                    .child(div().flex_1())
                    .when(in_progress.is_some(), |bar| {
                        bar.child(
                            div()
                                .id("continue-operation")
                                .h(px(theme.metrics.control_height))
                                .px(px(20.))
                                .flex()
                                .items_center()
                                .rounded(px(theme.metrics.radius_md))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .when(can_continue, |b| {
                                    b.bg(theme.accent)
                                        .text_color(theme.accent_text)
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.repo_state.update(cx, |state, cx| {
                                                state.continue_operation(cx)
                                            })
                                        }))
                                })
                                .when(!can_continue, |b| {
                                    b.bg(theme.border).text_color(theme.text_muted)
                                })
                                .child(label),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use tempfile::TempDir;

    use super::*;
    use crate::testutil::git_ok as git;

    /// `main` and `feature` both change lines 2 and 5 of `f.txt` (two regions) and
    /// `feature` deletes `gone.txt` while `main` edits it. The merge is started.
    fn merging(dir: &Path) {
        assert!(git(dir, &["init", "-q", "-b", "main"]));
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        fs::write(dir.join("f.txt"), base).unwrap();
        fs::write(dir.join("gone.txt"), "keep\n").unwrap();
        assert!(git(dir, &["add", "."]));
        assert!(git(dir, &["commit", "-qm", "base"]));
        assert!(git(dir, &["switch", "-qc", "feature"]));
        fs::write(dir.join("f.txt"), "1\nF2\n3\n4\n5\n6\n7\n8\nF9\n10\n").unwrap();
        assert!(git(dir, &["rm", "-q", "gone.txt"]));
        assert!(git(dir, &["commit", "-qam", "feature"]));
        assert!(git(dir, &["switch", "-q", "main"]));
        fs::write(dir.join("f.txt"), "1\nM2\n3\n4\n5\n6\n7\n8\nM9\n10\n").unwrap();
        fs::write(dir.join("gone.txt"), "edited\n").unwrap();
        assert!(git(dir, &["commit", "-qam", "main"]));
        assert!(
            !git(dir, &["merge", "feature"]),
            "the merge should conflict"
        );
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        dir: &Path,
    ) -> (
        Entity<RepoState>,
        Entity<ConflictsView>,
        &'a mut VisualTestContext,
    ) {
        merging(dir);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(Theme::dark());
        });
        let state = cx.new(|_| RepoState::default());
        state.update(cx, |state, cx| state.open(dir.to_owned(), cx));
        cx.run_until_parked();
        state.update(cx, |state, cx| state.trust(cx));
        let (view, cx) = cx.add_window_view({
            let state = state.clone();
            move |window, cx| ConflictsView::new(state, window, cx)
        });
        cx.run_until_parked();
        (state, view, cx)
    }

    fn editor_text(view: &Entity<ConflictsView>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |v, cx| v.editor.read(cx).value().to_string())
    }

    #[gpui::test]
    fn it_lists_the_conflicted_files_and_loads_the_first(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (_state, view, cx) = open(cx, dir.path());
        view.read_with(cx, |v, _| {
            assert_eq!(
                v.known.iter().cloned().collect::<Vec<_>>(),
                ["f.txt", "gone.txt"]
            );
            assert_eq!(v.selected.as_deref(), Some("f.txt"));
            let loaded = v.loaded.as_ref().expect("loaded");
            assert!(loaded.text && loaded.kind == ConflictKind::BothChanged);
        });
        assert_eq!(conflict_regions(&editor_text(&view, cx)).len(), 2);
    }

    #[gpui::test]
    fn choosing_for_each_region_then_marking_resolved_stages_the_file(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());

        view.update_in(cx, |v, window, cx| v.choose(0, Choice::Ours, window, cx));
        assert_eq!(conflict_regions(&editor_text(&view, cx)).len(), 1);
        view.update_in(cx, |v, window, cx| v.choose(0, Choice::Theirs, window, cx));
        assert_eq!(editor_text(&view, cx), "1\nM2\n3\n4\n5\n6\n7\n8\nF9\n10\n");

        view.update(cx, |v, cx| v.mark_resolved(cx));
        cx.run_until_parked();

        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "1\nM2\n3\n4\n5\n6\n7\n8\nF9\n10\n"
        );
        state.read_with(cx, |s, _| assert_eq!(s.conflicted_paths(), ["gone.txt"]));
        view.read_with(cx, |v, _| {
            // The finished file stays listed, and the view moved on to the one left.
            assert!(v.known.contains("f.txt"));
            assert_eq!(v.selected.as_deref(), Some("gone.txt"));
            assert_eq!(
                v.loaded.as_ref().map(|l| l.kind),
                Some(ConflictKind::DeletedByThem)
            );
            assert!(!v.loaded.as_ref().unwrap().text);
        });
    }

    #[gpui::test]
    fn resolving_everything_and_continuing_finishes_the_merge_and_clears_the_list(
        cx: &mut TestAppContext,
    ) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());

        view.update(cx, |v, cx| v.whole_file(WholeFile::Side(Side::Theirs), cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| v.whole_file(WholeFile::Delete, cx));
        cx.run_until_parked();
        state.read_with(cx, |s, _| {
            assert!(s.conflicted_paths().is_empty());
            assert!(s.in_progress().is_some(), "waiting to be continued");
        });
        view.read_with(cx, |v, _| {
            assert_eq!(v.known.len(), 2, "both stay listed as resolved")
        });

        state.update(cx, |s, cx| s.continue_operation(cx));
        cx.run_until_parked();

        state.read_with(cx, |s, _| {
            assert!(s.error().is_none(), "{:?}", s.error());
            assert!(s.in_progress().is_none());
        });
        view.read_with(cx, |v, _| assert!(v.known.is_empty()));
        assert!(!dir.path().join("gone.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "1\nF2\n3\n4\n5\n6\n7\n8\nF9\n10\n"
        );
    }

    #[gpui::test]
    fn a_file_that_still_has_markers_cannot_be_marked_resolved(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());
        view.update_in(cx, |v, window, cx| v.choose(0, Choice::Ours, window, cx));
        assert_eq!(conflict_regions(&editor_text(&view, cx)).len(), 1);

        view.update(cx, |v, cx| v.mark_resolved(cx));
        cx.run_until_parked();

        state.read_with(cx, |s, _| {
            assert!(s.conflicted_paths().contains(&"f.txt".to_owned()))
        });
        assert!(
            fs::read_to_string(dir.path().join("f.txt"))
                .unwrap()
                .contains("<<<<<<<")
        );
    }

    #[gpui::test]
    fn a_delete_modify_conflict_can_keep_the_file(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());
        let gone = "gone.txt".to_owned();
        view.update_in(cx, |v, window, cx| v.select(Some(gone), window, cx));
        cx.run_until_parked();

        view.update(cx, |v, cx| v.whole_file(WholeFile::Keep, cx));
        cx.run_until_parked();

        state.read_with(cx, |s, _| assert_eq!(s.conflicted_paths(), ["f.txt"]));
        assert_eq!(
            fs::read_to_string(dir.path().join("gone.txt")).unwrap(),
            "edited\n"
        );
    }

    #[gpui::test]
    fn aborting_empties_the_view(cx: &mut TestAppContext) {
        let dir = TempDir::new().unwrap();
        let (state, view, cx) = open(cx, dir.path());
        state.update(cx, |s, cx| s.abort_operation(cx));
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(v.known.is_empty() && v.selected.is_none() && v.loaded.is_none())
        });
        assert_eq!(
            fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "1\nM2\n3\n4\n5\n6\n7\n8\nM9\n10\n"
        );
    }

    #[test]
    fn the_banner_says_what_is_waiting_and_how_much_is_left() {
        use OperationKind::*;
        assert_eq!(
            operation_summary(Merge, Some("main"), "feat/x", 2),
            "Merging main into feat/x · 2 files still have conflicts"
        );
        assert_eq!(
            operation_summary(CherryPick, None, "main", 1),
            "Cherry-picking onto main · 1 file still has conflicts"
        );
        assert!(operation_summary(Revert, None, "main", 0).contains("continue to finish"));
        assert_eq!(abort_label(Merge), "Abort merge");
        assert_eq!(continue_label(Revert), "Continue revert");
    }
}
