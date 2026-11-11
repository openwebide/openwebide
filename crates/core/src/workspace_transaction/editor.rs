//! The editor side of an approved resource transaction. Only affected documents
//! travel with the journal; unaffected drafts retain their acknowledged values.
use crate::editor::{EditorRecovery, EditorRecoveryFile, EditorRecoveryRoot, MAX_RECOVERY_FILES};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_EDITOR_PATCH_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRecoveryPatch {
    pub revision: i64,
    pub root: EditorRecoveryRoot,
    pub before: Vec<EditorRecoveryFile>,
    pub after: Vec<EditorRecoveryFile>,
    pub order_before: Vec<String>,
    pub order_after: Vec<String>,
    pub selected_before: Option<String>,
    pub selected_after: Option<String>,
}
impl EditorRecoveryPatch {
    pub fn validate(&self) -> Result<(), String> {
        if self.revision < 0
            || self
                .before
                .iter()
                .chain(&self.after)
                .map(EditorRecoveryFile::recovery_bytes)
                .sum::<usize>()
                > MAX_EDITOR_PATCH_BYTES
        {
            return Err("Workspace editor recovery exceeds its revision or 64 MiB limit".into());
        }
        for files in [&self.before, &self.after] {
            EditorRecovery {
                format: 1,
                root: Some(self.root.clone()),
                selected: None,
                files: files.clone(),
            }
            .validate()?;
        }
        for order in [&self.order_before, &self.order_after] {
            if order.len() > MAX_RECOVERY_FILES
                || order.iter().collect::<BTreeSet<_>>().len() != order.len()
                || order
                    .iter()
                    .any(|path| path.is_empty() || path.len() > 4096 || path.contains('\0'))
            {
                return Err("Invalid workspace editor tab order".into());
            }
        }
        if self
            .selected_before
            .as_ref()
            .is_some_and(|path| !self.order_before.contains(path))
            || self
                .selected_after
                .as_ref()
                .is_some_and(|path| !self.order_after.contains(path))
        {
            return Err("Workspace editor selection is not in its tab order".into());
        }
        Ok(())
    }
    pub fn apply_to(&self, current: &EditorRecovery) -> Result<EditorRecovery, String> {
        self.validate()?;
        current.validate()?;
        if current.root.as_ref().is_some_and(|root| root != &self.root)
            || current.selected != self.selected_before
            || current
                .files
                .iter()
                .map(|file| &file.path)
                .ne(self.order_before.iter())
        {
            return Err("The editor changed before this workspace operation".into());
        }
        let mut files = current
            .files
            .iter()
            .map(|file| (file.path.clone(), file.clone()))
            .collect::<BTreeMap<_, _>>();
        for before in &self.before {
            if before.read_only
                && self.after.iter().find(|file| file.path == before.path) != Some(before)
            {
                return Err(format!("{} is read-only", before.path));
            }
            if files.get(&before.path) != Some(before) {
                return Err(format!("{} has a newer editor draft", before.path));
            }
            files.remove(&before.path);
        }
        for after in &self.after {
            if files.contains_key(&after.path) {
                return Err(format!(
                    "{} would replace an unreviewed editor draft",
                    after.path
                ));
            }
            files.insert(after.path.clone(), after.clone());
        }
        if self.order_after.len() != files.len()
            || self
                .order_after
                .iter()
                .any(|path| !files.contains_key(path))
        {
            return Err("Workspace operation cannot discard unreviewed editor tabs".into());
        }
        let result = EditorRecovery {
            format: 1,
            root: Some(self.root.clone()),
            selected: self.selected_after.clone(),
            files: self
                .order_after
                .iter()
                .map(|path| files.remove(path).unwrap())
                .collect(),
        };
        result.validate()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        WorkspaceMode,
        editor::{Document, RecoveryScroll},
    };
    fn file(path: &str, text: &str) -> EditorRecoveryFile {
        EditorRecoveryFile {
            path: path.into(),
            document: Some(Document::new(text).recovery()),
            scroll: RecoveryScroll::default(),
            read_only: false,
        }
    }
    fn fixture(mode: WorkspaceMode) -> (EditorRecovery, EditorRecoveryPatch) {
        let root = EditorRecoveryRoot {
            mode,
            path: (mode == WorkspaceMode::Remote).then(|| "project".into()),
        };
        let before = file("old.rs", "unsaved 文😀\r\n");
        let mut after = before.clone();
        after.path = "new.rs".into();
        let state = EditorRecovery {
            format: 1,
            root: Some(root.clone()),
            selected: Some("old.rs".into()),
            files: vec![before.clone(), file("other.rs", "untouched")],
        };
        let patch = EditorRecoveryPatch {
            revision: 1,
            root,
            before: vec![before],
            after: vec![after],
            order_before: vec!["old.rs".into(), "other.rs".into()],
            order_after: vec!["new.rs".into(), "other.rs".into()],
            selected_before: Some("old.rs".into()),
            selected_after: Some("new.rs".into()),
        };
        (state, patch)
    }
    #[test]
    fn resource_recovery_moves_drafts_and_preserves_unaffected_documents_in_both_modes() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (state, patch) = fixture(mode);
            let after = patch.apply_to(&state).unwrap();
            assert_eq!(after.files[1], state.files[1]);
            assert_eq!(after.files[0].document, state.files[0].document);
            assert_eq!(after.files[0].path, "new.rs");
            assert_eq!(after.selected.as_deref(), Some("new.rs"));
        }
    }
    #[test]
    fn stale_drafts_folder_changes_and_implicit_tab_deletions_are_rejected() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (state, patch) = fixture(mode);
            let mut changed = state.clone();
            changed.files[0] = file("old.rs", "newer draft");
            assert!(patch.apply_to(&changed).is_err());
            let mut changed = state.clone();
            changed.root = None;
            assert!(patch.apply_to(&changed).is_err());
            let mut changed = patch.clone();
            changed.order_after.pop();
            assert!(changed.apply_to(&state).is_err());
            let mut changed = patch.clone();
            changed.after[0].path = "other.rs".into();
            assert!(changed.apply_to(&state).is_err());
        }
    }
}
