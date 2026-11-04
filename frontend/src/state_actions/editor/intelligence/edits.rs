//! Reviewed edits become unsaved editor documents in one publication. Filesystem
//! writes remain ordinary Save operations, with the same recovery and undo paths.
use super::*;
use crate::state::workspace::{EditorBuffer, EditorText};
use openwebide_core::{
    editor::Document,
    plugins::languages::{intelligence::results::WorkspaceEdit, source_digest},
};

#[derive(Clone)]
pub struct IntelligenceFile {
    pub path: String,
    pub before: Arc<String>,
    pub after: Document,
    retained: bool,
}
impl EditorIntelligence {
    fn retained_source(self, project: i64, path: &str) -> Option<(Arc<String>, bool)> {
        let workspace = self.editor.workspace;
        if self.editor.is_current(project, path) {
            return Some((workspace.content.with_untracked(EditorText::shared), false));
        }
        let key = (project, path.to_string());
        workspace
            .editor_buffers
            .with_untracked(|buffers| {
                buffers
                    .get(&key)
                    .map(|buffer| (buffer.content.shared(), buffer.read_only))
            })
            .or_else(|| {
                workspace.editor_documents.with_untracked(|documents| {
                    documents
                        .get(&key)
                        .map(|document| (document.shared_text(), false))
                })
            })
    }
    pub fn prepare_workspace(self, edit: WorkspaceEdit) {
        self.review.set(Vec::new());
        let Some(snapshot) = self
            .snapshot
            .get_value()
            .filter(|snapshot| self.snapshot_current(snapshot))
        else {
            return;
        };
        let Some(actions) = self.workspace_actions else {
            self.error.set(Some("Workspace is unavailable".into()));
            return;
        };
        let Some(workspace) = actions.workspace_for.run(snapshot.presentation.project) else {
            self.error
                .set(Some("Grant folder access to review these edits".into()));
            return;
        };
        self.busy.set(true);
        spawn_local(async move {
            let prepared = async {
                if edit.files.len() > 64 {
                    return Err("Too many affected files".to_string());
                }
                let mut paths = std::collections::BTreeSet::new();
                let mut files = Vec::new();
                let mut bytes = 0_usize;
                for file in edit.files {
                    if !protocol::relative_path(&file.path) || !paths.insert(file.path.clone()) {
                        return Err("Invalid or repeated workspace edit path".into());
                    }
                    let retained = self.retained_source(snapshot.presentation.project, &file.path);
                    let (before, read_only) = if let Some(source) = &retained {
                        source.clone()
                    } else {
                        (
                            Arc::new(
                                workspace
                                    .read(&file.path)
                                    .await
                                    .map_err(|error| error.to_string())?,
                            ),
                            false,
                        )
                    };
                    if !self.snapshot_current(&snapshot) {
                        return Err("Editor context changed".into());
                    }
                    if read_only {
                        return Err(format!("{} is read-only", file.path));
                    }
                    if source_digest(&before) != file.source_digest {
                        return Err(format!("{} changed. Request the edit again.", file.path));
                    }
                    let edits = results::editor_edits(&before, &file.edits)?;
                    let key = (snapshot.presentation.project, file.path.clone());
                    let mut after = self
                        .editor
                        .workspace
                        .editor_documents
                        .with_untracked(|documents| {
                            documents
                                .get(&key)
                                .filter(|document| document.text() == before.as_str())
                                .cloned()
                        })
                        .unwrap_or_else(|| Document::from_shared_text(before.clone()));
                    let caret = edits
                        .first()
                        .map_or(0, |edit| edit.range.start + edit.text.len());
                    after
                        .apply(edits, vec![Selection::caret(caret)], None)
                        .map_err(|error| error.to_string())?;
                    bytes = bytes
                        .saturating_add(before.len())
                        .saturating_add(after.text().len());
                    if bytes > 64 * 1024 * 1024 {
                        return Err("Workspace edit preview exceeds its memory limit".into());
                    }
                    files.push(IntelligenceFile {
                        path: file.path,
                        before,
                        after,
                        retained: retained.is_some(),
                    });
                }
                Ok::<_, String>(files)
            }
            .await;
            if !self.snapshot_current(&snapshot) {
                return;
            }
            self.busy.set(false);
            match prepared {
                Ok(files) => self.review.set(files),
                Err(message) => self.error.set(Some(message)),
            }
        });
    }
    pub fn apply_workspace(self, read_only: Signal<bool>) {
        if read_only.get_untracked() || self.busy.get_untracked() {
            return;
        }
        let Some(snapshot) = self
            .snapshot
            .get_value()
            .filter(|snapshot| self.snapshot_current(snapshot))
        else {
            return;
        };
        let files = self.review.get_untracked();
        if files.is_empty() {
            return;
        }
        let Some(actions) = self.workspace_actions else {
            return;
        };
        let Some(workspace) = actions.workspace_for.run(snapshot.presentation.project) else {
            return;
        };
        self.busy.set(true);
        spawn_local(async move {
            let prepared = async {
                // Re-read unopened files before installing their retained buffer.
                for file in &files {
                    if !file.retained
                        && workspace
                            .read(&file.path)
                            .await
                            .map_err(|error| error.to_string())?
                            .as_str()
                            != file.before.as_str()
                    {
                        return Err(format!("{} changed outside the editor", file.path));
                    }
                }
                if !self.snapshot_current(&snapshot) {
                    return Err("Editor context changed".into());
                }
                if read_only.try_get_untracked().unwrap_or(true) {
                    return Err("The editor is read-only".into());
                }
                let mut tabs = self.editor.workspace.editor_tabs.with_untracked(|tabs| tabs.get(&snapshot.presentation.project).cloned().unwrap_or_default());
                for file in &files { if !tabs.contains(&file.path) { tabs.push(file.path.clone()); } }
                if tabs.len() > openwebide_core::editor::MAX_RECOVERY_FILES {
                    return Err("Close some tabs before applying this edit so every file remains recoverable".into());
                }
                for file in &files {
                    let retained = self.retained_source(snapshot.presentation.project, &file.path);
                    if retained.as_ref().is_some_and(|(source, read_only)| {
                        *read_only || source.as_str() != file.before.as_str()
                    }) || (file.retained && retained.is_none())
                    {
                        return Err(format!("{} changed. Request the edit again.", file.path));
                    }
                }
                Ok::<_, String>(())
            }
            .await;
            if !self.snapshot_current(&snapshot) {
                return;
            }
            self.busy.set(false);
            if let Err(message) = prepared {
                self.error.set(Some(message));
                return;
            }
            let editor = self.editor;
            editor.cancel_native_text();
            editor.typing.set(None);
            let project = snapshot.presentation.project;
            // No await or fallible operation after preflight. Every file enters
            // editor history together; no file is written behind the user's back.
            batch(move || {
                editor.workspace.editor_tabs.update(|tabs| {
                    let paths = tabs.entry(project).or_default();
                    for file in &files {
                        if !paths.contains(&file.path) {
                            paths.push(file.path.clone());
                        }
                    }
                });
                editor.workspace.editor_documents.update(|documents| {
                    for file in &files {
                        documents.insert((project, file.path.clone()), file.after.clone());
                    }
                });
                editor.workspace.editor_buffers.update(|buffers| {
                    for file in &files {
                        buffers.insert(
                            (project, file.path.clone()),
                            EditorBuffer {
                                content: file.after.shared_text().into(),
                                dirty: file.after.is_dirty(),
                                read_only: false,
                            },
                        );
                    }
                });
                if let Some(file) = files
                    .iter()
                    .find(|file| file.path == snapshot.presentation.path)
                {
                    editor
                        .workspace
                        .content
                        .set(file.after.shared_text().into());
                    editor.workspace.dirty.set(file.after.is_dirty());
                }
                self.result.set(None);
                self.review.set(Vec::new());
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::Backend,
        state::workspace::WorkspaceState,
        state_actions::{editor::EditorCommand, workspace::WorkspaceActions},
        testing::fake_backend::FakeBackend,
        workspace::Workspace,
    };
    use openwebide_core::{
        WorkspaceMode,
        editor::{Edit, Indentation},
        plugins::languages::intelligence::{Position, Range, results::FileEdit},
    };
    use std::rc::Rc;
    use wasm_bindgen::JsCast;

    fn fixture(mode: WorkspaceMode) -> (Owner, EditorIntelligence, Rc<FakeBackend>) {
        crate::testing::initialize_runtime();
        let owner = Owner::new();
        let (intelligence, fake) = owner.with(|| {
            let fake = Rc::new(FakeBackend::default());
            fake.files.borrow_mut().insert((1, "third.rs".into()), "foo".into());
            let api: Api = StoredValue::new_local(fake.clone() as Rc<dyn Backend>);
            let files = match mode {
                WorkspaceMode::Remote => Workspace::Remote { api, project_id: 1 },
                WorkspaceMode::Local => Workspace::Local { handle: js_sys::Function::new_no_args(r#"
                    return {name:'fixture', queryPermission:async()=> 'granted',
                        getFileHandle:async name=> { if(name!=='third.rs') throw new DOMException('missing','NotFoundError');
                            const file=Object.create(FileSystemFileHandle.prototype); Object.defineProperty(file,'getFile',{value:async()=>new File(['foo'],name)}); return file; }};
                "#).call0(&wasm_bindgen::JsValue::NULL).unwrap().unchecked_into() },
            };
            let files = StoredValue::new_local(files);
            let workspace_actions = WorkspaceActions {
                workspace_for: Callback::new(move |_| files.with_value(|files| Some(match files {
                    Workspace::Remote { api, project_id } => Workspace::Remote { api: *api, project_id: *project_id },
                    Workspace::Local { handle } => Workspace::Local { handle: handle.clone() },
                }))),
                on_grant_access: Callback::new(|()| ()), ensure_root: Callback::new(|_| ()),
                refresh_tree: Callback::new(|()| ()), on_open_lossy: Callback::new(|()| ()),
                request_open: Callback::new(|_| ()), open_with_completion: Callback::new(|(_, done): (_, Callback<Result<(), String>>)| done.run(Ok(()))),
                close_file: Callback::new(|_| ()), file_tab_action: Callback::new(|_| ()), on_toggle: Callback::new(|_| ()),
                on_save: Callback::new(|()| ()), on_accept: Callback::new(|()| ()), on_reject: Callback::new(|()| ()),
                on_new_file: Callback::new(|()| ()), on_new_dir: Callback::new(|()| ()),
                on_search_input: Callback::new(|_| ()), on_cancel_search: Callback::new(|()| ()),
                on_search: Callback::new(|_| ()), on_clear_search: Callback::new(|()| ()),
            };
            let workspace = WorkspaceState::new();
            workspace.active_project.set(Some(1));
            workspace.open_file.set(Some("first.rs".into()));
            workspace.content.set("foo".into());
            workspace.editor_tabs.update(|tabs| { tabs.insert(1, vec!["first.rs".into(), "second.rs".into()]); });
            let mut second = Document::from_shared_text(Arc::new("foo".into()));
            second.apply(vec![Edit::replace(3..3, "!")], vec![Selection::caret(4)], None).unwrap();
            workspace.editor_buffers.update(|buffers| { buffers.insert((1, "second.rs".into()), EditorBuffer { content: second.shared_text().into(), dirty: true, read_only: false }); });
            workspace.editor_documents.update(|documents| { documents.insert((1, "second.rs".into()), second); });
            let editor = EditorActions::new(workspace);
            editor.record_selection(Selection::caret(1)).unwrap();
            let registry = LanguageRegistry::from_bindings(&[crate::testing::languages::standard_binding()]);
            let scope = ServiceScope { project: 1, account: 0, session: None, host: 1, retry: 0,
                provider: registry.providers[0].clone(), registry, preferences: LanguagePreferences::default() };
            let identity_scope = scope.clone();
            let identity = Memo::new(move |_| Some(identity_scope.clone()));
            let captured = Snapshot { presentation: editor.presentation_scope().unwrap(), revision: editor.source_revision(),
                source: Arc::new("foo".into()), selection: Selection::caret(1), service: scope };
            let intelligence = EditorIntelligence {
                available: Memo::new(|_| true), identity, review: RwSignal::new(Vec::new()),
                busy: RwSignal::new(false), rename_open: RwSignal::new(false), error: RwSignal::new(None), diagnostics: RwSignal::new(Vec::new()),
                diagnostics_source: StoredValue::new(None), result: RwSignal::new(None), request: Callback::new(|_| ()),
                dismiss: Callback::new(|()| ()), apply_text: Callback::new(|_| ()), navigate: Callback::new(|_| ()),
                editor, navigation: RwSignal::new(None), workspace_actions: Some(workspace_actions), snapshot: StoredValue::new(Some(captured)),
            };
            (intelligence, fake)
        });
        (owner, intelligence, fake)
    }
    fn edit() -> WorkspaceEdit {
        WorkspaceEdit {
            files: [
                ("first.rs", "foo"),
                ("second.rs", "foo!"),
                ("third.rs", "foo"),
            ]
            .into_iter()
            .map(|(path, source)| FileEdit {
                path: path.into(),
                source_digest: source_digest(source),
                revision: None,
                edits: vec![TextEdit {
                    range: Range {
                        start: Position {
                            line: 0,
                            character: 0,
                        },
                        end: Position {
                            line: 0,
                            character: 3,
                        },
                    },
                    new_text: "bar".into(),
                }],
            })
            .collect(),
        }
    }
    async fn settled(actions: EditorIntelligence) {
        for _ in 0..100 {
            if !actions.busy.get_untracked() {
                return;
            }
            crate::util::sleep_ms(10).await;
        }
        panic!("Workspace edit did not settle");
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn reviewed_edits_stage_all_tabs_without_disk_writes_and_preserve_existing_undo_in_both_modes()
     {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, actions, fake) = fixture(mode);
            actions.prepare_workspace(edit());
            settled(actions).await;
            assert_eq!(actions.error.get_untracked(), None);
            assert_eq!(actions.review.with_untracked(Vec::len), 3);
            actions.apply_workspace(Signal::derive(|| false));
            settled(actions).await;
            assert_eq!(actions.error.get_untracked(), None);
            assert_eq!(actions.editor.source().as_str(), "bar");
            assert!(actions.editor.workspace.dirty.get_untracked());
            assert_eq!(
                actions.editor.workspace.editor_tabs.get_untracked()[&1].len(),
                3
            );
            actions
                .editor
                .workspace
                .editor_documents
                .update(|documents| {
                    let second = documents.get_mut(&(1, "second.rs".into())).unwrap();
                    assert_eq!(second.text(), "bar!");
                    assert!(second.undo());
                    assert_eq!(second.text(), "foo!");
                    assert!(second.undo());
                    assert_eq!(second.text(), "foo");
                });
            actions
                .editor
                .command(
                    EditorCommand::Undo,
                    Selection::caret(1),
                    Indentation::default(),
                )
                .unwrap();
            assert_eq!(actions.editor.source().as_str(), "foo");
            assert!(
                !fake.calls.borrow().iter().any(|call| matches!(
                    call,
                    crate::testing::fake_backend::Call::WriteFile { .. }
                ))
            );
            assert_eq!(fake.files.borrow()[&(1, "third.rs".into())], "foo");
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn changed_inactive_buffers_and_invalid_paths_reject_every_file_before_publication() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, actions, _) = fixture(mode);
            actions.prepare_workspace(edit());
            settled(actions).await;
            actions.editor.workspace.editor_buffers.update(|buffers| {
                buffers.get_mut(&(1, "second.rs".into())).unwrap().content = "newer".into();
            });
            actions.apply_workspace(Signal::derive(|| false));
            settled(actions).await;
            assert!(actions.error.get_untracked().unwrap().contains("second.rs"));
            assert_eq!(actions.editor.source().as_str(), "foo");
            assert!(
                !actions
                    .editor
                    .workspace
                    .editor_buffers
                    .with_untracked(|buffers| buffers.contains_key(&(1, "third.rs".into())))
            );
            let mut invalid = edit();
            invalid.files[2].path = "../outside.rs".into();
            actions.error.set(None);
            actions.prepare_workspace(invalid);
            settled(actions).await;
            assert!(actions.error.get_untracked().is_some());
            assert!(actions.review.with_untracked(Vec::is_empty));
            assert_eq!(actions.editor.source().as_str(), "foo");
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn old_source_and_read_only_reviews_cannot_apply_to_a_newer_presentation() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, actions, _) = fixture(mode);
            actions.prepare_workspace(edit());
            settled(actions).await;
            actions.apply_workspace(Signal::derive(|| true));
            assert_eq!(actions.editor.source().as_str(), "foo");
            actions.editor.workspace.content.set("newer".into());
            actions.apply_workspace(Signal::derive(|| false));
            assert_eq!(actions.editor.source().as_str(), "newer");
            assert_eq!(
                actions.editor.workspace.editor_tabs.get_untracked()[&1].len(),
                2
            );
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn first_diagnostics_reveal_marks_and_source_changes_remove_them() {
        use leptos::mount::mount_to;
        let (_owner, actions, _) = fixture(WorkspaceMode::Remote);
        let document = web_sys::window().unwrap().document().unwrap();
        let root = document
            .create_element("div")
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>();
        document.body().unwrap().append_child(&root).unwrap();
        let mounted = mount_to(root.clone(), move || {
            let textarea = NodeRef::<leptos::html::Textarea>::new();
            view! {
                <div class="editor-code" style="position:relative;width:400px;height:100px;font:14px monospace">
                    <span class="editor-source-line" data-line="1" data-paint-length="3">
                        <span class="editor-source-fragment" data-paint-start="0" data-paint-end="3">"foo"</span>
                    </span>
                    <textarea node_ref=textarea />
                    <crate::components::editor_intelligence::DiagnosticOverlay actions textarea ready=RwSignal::new(true) layout_revision=RwSignal::new(0) />
                </div>
            }
        });
        crate::util::yield_task().await;
        assert!(root.query_selector(".editor-diagnostic").unwrap().is_none());
        let snapshot = actions.snapshot.get_value().unwrap();
        actions
            .diagnostics_source
            .set_value(Some((snapshot.presentation, snapshot.revision)));
        actions.diagnostics.set(vec![protocol::Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            severity: 1,
            message: "Fixture error".into(),
            source: None,
            code: None,
        }]);
        for _ in 0..100 {
            if root.query_selector(".editor-diagnostic").unwrap().is_some() {
                break;
            }
            crate::util::sleep_ms(10).await;
        }
        assert!(
            root.query_selector(".editor-diagnostic-error")
                .unwrap()
                .is_some(),
            "The first response must reveal its marks after an initially empty state"
        );
        actions.editor.workspace.content.set("newer".into());
        for _ in 0..100 {
            if root.query_selector(".editor-diagnostic").unwrap().is_none() {
                break;
            }
            crate::util::sleep_ms(10).await;
        }
        assert!(root.query_selector(".editor-diagnostic").unwrap().is_none());
        drop(mounted);
        root.remove();
    }
}
