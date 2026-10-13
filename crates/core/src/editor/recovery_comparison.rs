//! Complete-only equality for immutable recovery snapshots.
use super::{EditorRecovery, recovery::equal_text};

#[derive(Clone, Copy)]
enum Phase {
    File,
    Text,
    Saved,
    Selections,
    Folds,
}

pub struct EditorRecoveryComparison<'a> {
    left: &'a EditorRecovery,
    right: &'a EditorRecovery,
    file: usize,
    next: usize,
    phase: Phase,
    result: Option<bool>,
}
impl<'a> EditorRecoveryComparison<'a> {
    pub fn new(left: &'a EditorRecovery, right: &'a EditorRecovery) -> Self {
        let equal_header = left.format == right.format
            && left.root == right.root
            && left.selected == right.selected
            && left.files.len() == right.files.len();
        Self {
            left,
            right,
            file: 0,
            next: 0,
            phase: Phase::File,
            result: (!equal_header).then_some(false),
        }
    }
    /// A unit checks one file header, 8 KiB of text or 64 metadata records.
    pub fn advance(&mut self, budget: usize) -> bool {
        for _ in 0..budget {
            if self.result.is_some() {
                break;
            }
            if self.file == self.left.files.len() {
                self.result = Some(true);
                break;
            }
            let left = &self.left.files[self.file];
            let right = &self.right.files[self.file];
            if matches!(self.phase, Phase::File) {
                if left.path != right.path
                    || left.scroll != right.scroll
                    || left.read_only != right.read_only
                {
                    self.result = Some(false);
                    continue;
                }
                match (&left.document, &right.document) {
                    (None, None) => self.file += 1,
                    (Some(_), Some(_)) => self.phase = Phase::Text,
                    _ => self.result = Some(false),
                }
                continue;
            }
            let (left, right) = (
                left.document.as_ref().unwrap(),
                right.document.as_ref().unwrap(),
            );
            let equal = match self.phase {
                Phase::Text => equal_text(&left.text, &right.text, &mut self.next),
                Phase::Saved => equal_text(&left.saved, &right.saved, &mut self.next),
                Phase::Selections => {
                    compare_records(&left.selections, &right.selections, &mut self.next)
                }
                Phase::Folds => compare_records(&left.collapsed, &right.collapsed, &mut self.next),
                Phase::File => unreachable!(),
            };
            match equal {
                Some(false) => self.result = Some(false),
                None => {}
                Some(true) => {
                    self.next = 0;
                    self.phase = match self.phase {
                        Phase::Text => Phase::Saved,
                        Phase::Saved => Phase::Selections,
                        Phase::Selections => Phase::Folds,
                        Phase::Folds => {
                            self.file += 1;
                            Phase::File
                        }
                        Phase::File => unreachable!(),
                    };
                }
            }
        }
        self.result.is_some()
    }
    pub fn finish(self) -> Option<bool> {
        self.result
    }
}

fn compare_records<T: PartialEq>(left: &[T], right: &[T], next: &mut usize) -> Option<bool> {
    if left.len() != right.len() {
        return Some(false);
    }
    let end = next.saturating_add(64).min(left.len());
    if left[*next..end] != right[*next..end] {
        return Some(false);
    }
    *next = end;
    (end == left.len()).then_some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{
        Document, EditorRecoveryFile, EditorRecoveryRoot, FoldRange, RecoveryScroll, Selection,
    };
    #[test]
    fn bounded_comparison_matches_derived_equality_for_both_modes_and_every_field() {
        for mode in [crate::WorkspaceMode::Local, crate::WorkspaceMode::Remote] {
            let mut document = Document::new("文😀\r\n".repeat(2000));
            document.set_fold_ranges(
                (0..500)
                    .map(|index| FoldRange {
                        start_line: index * 2,
                        end_line: index * 2 + 1,
                    })
                    .collect(),
            );
            document.fold_state_mut().collapse_all();
            let left = EditorRecovery {
                root: Some(EditorRecoveryRoot {
                    mode,
                    path: Some("project".into()),
                }),
                selected: Some("a.rs".into()),
                files: vec![EditorRecoveryFile {
                    path: "a.rs".into(),
                    document: Some(document.recovery()),
                    scroll: RecoveryScroll::default(),
                    read_only: false,
                }],
                ..EditorRecovery::default()
            };
            for field in [
                "none",
                "format",
                "root",
                "selected",
                "file",
                "path",
                "scroll",
                "nan",
                "read-only",
                "document",
                "text",
                "saved",
                "selection",
                "fold",
                "text-owner",
            ] {
                let mut right = left.clone();
                match field {
                    "format" => right.format += 1,
                    "root" => right.root = None,
                    "selected" => right.selected = None,
                    "file" => right.files.clear(),
                    "path" => right.files[0].path = "b.rs".into(),
                    "scroll" => right.files[0].scroll.left = 1.0,
                    "nan" => right.files[0].scroll.top = f64::NAN,
                    "read-only" => right.files[0].read_only = true,
                    "document" => right.files[0].document = None,
                    "text" => {
                        right.files[0].document.as_mut().unwrap().text =
                            std::sync::Arc::new("other".into());
                    }
                    "saved" => {
                        right.files[0].document.as_mut().unwrap().saved =
                            std::sync::Arc::new("other".into());
                    }
                    "selection" => {
                        right.files[0].document.as_mut().unwrap().selections =
                            vec![Selection::caret(1)];
                    }
                    "fold" => {
                        right.files[0].document.as_mut().unwrap().collapsed.pop();
                    }
                    "text-owner" => {
                        let document = right.files[0].document.as_mut().unwrap();
                        document.text = std::sync::Arc::new(document.text.as_str().to_owned());
                        document.saved = std::sync::Arc::new(document.saved.as_str().to_owned());
                    }
                    _ => {}
                }
                for budget in [1, 8, usize::MAX] {
                    let mut comparison = EditorRecoveryComparison::new(&left, &right);
                    while !comparison.advance(budget) {}
                    assert_eq!(
                        comparison.finish(),
                        Some(left == right),
                        "{mode:?}/{field}/{budget}"
                    );
                }
            }
            let mut pending = EditorRecoveryComparison::new(&left, &left);
            assert!(!pending.advance(1));
            assert!(pending.finish().is_none());
        }
    }
}
