//! Reviewed edits become unsaved editor documents in one publication. Filesystem
//! writes remain ordinary Save operations, with the same recovery and undo paths.
use super::*;
use crate::state::workspace::{EditorBuffer, EditorText};
use openwebide_core::{
    editor::Document,
    plugins::languages::{intelligence::results::WorkspaceEdit, source_digest},
};

mod resources;
pub use resources::WorkspaceResourceReview;
pub(super) use resources::{ResourceContext, clear_workspace_review};

#[derive(Clone)]
pub struct IntelligenceFile {
    pub path: String,
    pub before: Arc<String>,
    pub after: Document,
    retained: bool,
}

pub struct WorkspaceTextReview {
    pub files: Vec<IntelligenceFile>,
    analysis: Arc<AnalysisResult>,
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
        clear_workspace_review(
            self.review,
            self.resources,
            self.review_epoch,
            self.resource_context,
        );
        let Some(analysis) = self.analysis.get_value() else {
            self.error.set(Some(
                "Request a fresh workspace review before applying these edits".into(),
            ));
            return;
        };
        if let Err(message) = analysis.workspace_edit(&edit) {
            self.error.set(Some(message));
            return;
        }
        if !edit.changes.is_empty() {
            self.prepare_resource_workspace(edit, None, analysis);
            return;
        }
        let snapshot = analysis.snapshot.clone();
        if !self.snapshot_current(&snapshot) {
            return;
        }
        let epoch = self.review_epoch.get_value();
        let Some(actions) = self.workspace_actions else {
            self.error.set(Some("Workspace is unavailable".into()));
            return;
        };
        let Some(workspace) = actions.workspace_for.run(snapshot.presentation.project) else {
            self.error
                .set(Some("Grant folder access to review these edits".into()));
            return;
        };
        self.error.set(None);
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
            if self.review_epoch.try_get_value() != Some(epoch) {
                return;
            }
            self.busy.set(false);
            if !self.snapshot_current(&snapshot) {
                return;
            }
            match prepared {
                Ok(files) => self
                    .review
                    .set(Some(Arc::new(WorkspaceTextReview { files, analysis }))),
                Err(message) => self.error.set(Some(message)),
            }
        });
    }
    pub fn apply_workspace(self, review: Arc<WorkspaceTextReview>, read_only: Signal<bool>) {
        if read_only.get_untracked()
            || self.busy.get_untracked()
            || self
                .editor
                .workspace
                .editor_composition
                .get_untracked()
                .is_some()
        {
            return;
        }
        let owns_review = move |expected: &Arc<WorkspaceTextReview>| {
            self.review
                .try_get_untracked()
                .flatten()
                .is_some_and(|current| Arc::ptr_eq(&current, expected))
        };
        let snapshot = review.analysis.snapshot.clone();
        if !owns_review(&review) || !self.snapshot_current(&snapshot) || review.files.is_empty() {
            return;
        }
        let Some(actions) = self.workspace_actions else {
            return;
        };
        let Some(workspace) = actions.workspace_for.run(snapshot.presentation.project) else {
            return;
        };
        if let Some(previous) = self.application_abort.get_value() {
            previous.abort();
        }
        let (abort, registration) = AbortHandle::new_pair();
        self.application_abort.set_value(Some(abort));
        self.error.set(None);
        self.busy.set(true);
        spawn_local(async move {
            let operation = async {
                let files = &review.files;
                // Re-read unopened files before installing their retained buffer.
                for file in files {
                    if !owns_review(&review) || !self.snapshot_current(&snapshot) {
                        return Err("Workspace review changed".into());
                    }
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
                    if !owns_review(&review) || !self.snapshot_current(&snapshot) {
                        return Err("Workspace review changed".into());
                    }
                }
                review.analysis.validate().await?;
                if !self.snapshot_current(&snapshot) {
                    return Err("Editor context changed".into());
                }
                if read_only.try_get_untracked().unwrap_or(true)
                    || self
                        .editor
                        .workspace
                        .editor_composition
                        .try_get_untracked()
                        .flatten()
                        .is_some()
                {
                    return Err("The editor is read-only".into());
                }
                let mut tabs = self.editor.workspace.editor_tabs.with_untracked(|tabs| {
                    tabs.get(&snapshot.presentation.project)
                        .cloned()
                        .unwrap_or_default()
                });
                for file in files {
                    if !tabs.contains(&file.path) {
                        tabs.push(file.path.clone());
                    }
                }
                if tabs.len() > openwebide_core::editor::MAX_RECOVERY_FILES {
                    return Err("Close some tabs before applying this edit so every file remains recoverable".into());
                }
                for file in files {
                    let retained = self.retained_source(snapshot.presentation.project, &file.path);
                    if retained.as_ref().is_some_and(|(source, read_only)| {
                        *read_only || source.as_str() != file.before.as_str()
                    }) || (file.retained && retained.is_none())
                    {
                        return Err(format!("{} changed. Request the edit again.", file.path));
                    }
                }
                Ok::<_, String>(())
            };
            let prepared = Abortable::new(operation, registration).await;
            if !owns_review(&review) {
                return;
            }
            self.application_abort.set_value(None);
            self.busy.set(false);
            if !self.snapshot_current(&snapshot) {
                return;
            }
            let Ok(prepared) = prepared else {
                return;
            };
            if let Err(message) = prepared {
                self.error.set(Some(message));
                return;
            }
            let files = &review.files;
            let editor = self.editor;
            editor.cancel_native_text();
            editor.typing.set(None);
            let project = snapshot.presentation.project;
            // No await or fallible operation after preflight. Every file enters
            // editor history together; no file is written behind the user's back.
            batch(move || {
                editor.workspace.editor_tabs.update(|tabs| {
                    let paths = tabs.entry(project).or_default();
                    for file in files {
                        if !paths.contains(&file.path) {
                            paths.push(file.path.clone());
                        }
                    }
                });
                editor.workspace.editor_documents.update(|documents| {
                    for file in files {
                        documents.insert((project, file.path.clone()), file.after.clone());
                    }
                });
                editor.workspace.editor_buffers.update(|buffers| {
                    for file in files {
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
                self.review.set(None);
                self.analysis.set_value(None);
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

    pub(super) fn fixture(mode: WorkspaceMode) -> (Owner, EditorIntelligence, Rc<FakeBackend>) {
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
            let mut binding = crate::testing::languages::standard_binding();
            binding.prepared.manifest.compatibility.plugin_api = 9;
            binding.prepared.manifest.executable = None;
            binding.prepared.manifest.contributions.languages = serde_json::from_value(serde_json::json!([
                {"id":"rust","name":"Rust","extensions":["rs"],"features":{"languageServer":{"command":"fixture-lsp"}}}
            ])).unwrap();
            fake.project_plugin_entries.borrow_mut().insert(1, vec![binding.clone()]);
            let registry = LanguageRegistry::from_bindings(&[binding]);
            let scope = ServiceScope { project: 1, account: 0, session: None, host: 1, retry: 0, repository: 0,
                provider: registry.providers[0].clone(), registry, preferences: LanguagePreferences::default() };
            let identity_scope = scope.clone();
            let identity = Memo::new(move |_| Some(identity_scope.clone()));
            let captured = Snapshot { presentation: editor.presentation_scope().unwrap(), revision: editor.source_revision(),
                source: Arc::new("foo".into()), selection: Selection::caret(1), service: scope };
            let intelligence = EditorIntelligence {
                available: Memo::new(|_| true), primary_available:Memo::new(|_|true), identity,
                source_revision: Memo::new(move |_| editor.source_revision()), context_revision: Memo::new(|_| 0),
                request_presentation: StoredValue::new(Presentation::Dialog), queued: RwSignal::new(None),
                popup: RwSignal::new(None), popup_busy: RwSignal::new(false), popup_error: RwSignal::new(None),
                dismiss_popup: Callback::new(|()| ()), review: RwSignal::new(None),
                resources: RwSignal::new(None), review_epoch: StoredValue::new(0), resource_context: None,
                busy: RwSignal::new(false), rename_open: RwSignal::new(false), error: RwSignal::new(None), diagnostics: RwSignal::new(Vec::new()), provider_diagnostics:RwSignal::new(vec![]),
                diagnostics_source: StoredValue::new(None), result: RwSignal::new(None), request: Callback::new(|_| ()),
                dismiss: Callback::new(|()| ()), apply_text: Callback::new(|_| ()), insert: Callback::new(|_| Ok(())), navigate: Callback::new(|_| ()),
                editor, navigation: RwSignal::new(None), workspace_actions: Some(workspace_actions), snapshot: StoredValue::new(Some(captured)),
                analysis: StoredValue::new(None), application_abort: StoredValue::new(None),
            };
            (intelligence, fake)
        });
        (owner, intelligence, fake)
    }
    pub(super) async fn prepare(
        fake: &Rc<FakeBackend>,
        actions: EditorIntelligence,
        edit: WorkspaceEdit,
    ) {
        let retained = retain(
            fake,
            actions,
            Query::Rename {
                position: protocol::Position::default(),
                new_name: "reviewed".into(),
            },
            serde_json::to_value(&edit).unwrap(),
        )
        .await;
        match retained {
            Ok(()) => actions.prepare_workspace(edit),
            Err(message) => actions.error.set(Some(message)),
        }
    }
    pub(super) async fn retain(
        fake: &Rc<FakeBackend>,
        actions: EditorIntelligence,
        query: Query,
        result: serde_json::Value,
    ) -> Result<(), String> {
        *fake.native_language_result.borrow_mut() = Some(result);
        let snapshot = actions.snapshot.get_value().unwrap();
        let workspace_files = if let Some(context) = actions.resource_context {
            Some(context.prepare_files(&snapshot).await)
        } else {
            None
        };
        let expected = snapshot.clone();
        let host = crate::testing::languages::NativeLanguageHost::new(
            fake.clone(),
            snapshot.presentation.project,
            move || actions.snapshot_current(&expected),
        );
        let bindings = fake.project_plugin_entries.borrow()[&snapshot.presentation.project].clone();
        let sequence = u64::try_from(fake.native_analysis_inputs.borrow().len()).unwrap();
        let service = openwebide_agent::plugins::languages::LanguageService::new_with_preferences(
            host,
            bindings,
            snapshot.service.provider.clone(),
            snapshot.service.preferences.clone(),
        )
        .unwrap()
        .with_sequence(Arc::new(std::sync::atomic::AtomicU64::new(sequence)));
        let document = LanguageDocumentRequest {
            document: source_digest(&snapshot.presentation.path),
            path: snapshot.presentation.path.clone(),
            revision: snapshot.revision,
            source: snapshot.source.clone(),
            tab_width: 4,
        };
        let response = service
            .query_intelligence("workspace-review-fixture", document.clone(), query.clone())
            .await
            .unwrap();
        let analysis = service
            .retain_analysis("workspace-review-fixture", document, query, &response)
            .unwrap()
            .unwrap();
        actions.analysis.set_value(Some(Arc::new(
            AnalysisResult::from_analysis(snapshot, analysis, workspace_files).unwrap(),
        )));
        Ok(())
    }
    fn edit() -> WorkspaceEdit {
        WorkspaceEdit {
            changes: vec![],
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
            prepare(&fake, actions, edit()).await;
            settled(actions).await;
            assert_eq!(actions.error.get_untracked(), None);
            assert_eq!(
                actions.review.with_untracked(|review| review
                    .as_ref()
                    .map_or(0, |review| review.files.len())),
                3
            );
            actions.apply_workspace(
                actions.review.get_untracked().unwrap(),
                Signal::derive(|| false),
            );
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
            let (_owner, actions, fake) = fixture(mode);
            prepare(&fake, actions, edit()).await;
            settled(actions).await;
            actions.editor.workspace.editor_buffers.update(|buffers| {
                buffers.get_mut(&(1, "second.rs".into())).unwrap().content = "newer".into();
            });
            actions.apply_workspace(
                actions.review.get_untracked().unwrap(),
                Signal::derive(|| false),
            );
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
            actions.editor.workspace.editor_buffers.update(|buffers| {
                buffers.get_mut(&(1, "second.rs".into())).unwrap().content = "foo!".into();
            });
            actions.error.set(None);
            prepare(&fake, actions, invalid).await;
            settled(actions).await;
            assert!(
                actions
                    .error
                    .get_untracked()
                    .unwrap()
                    .contains("Invalid or repeated workspace edit path")
            );
            assert!(actions.review.with_untracked(Option::is_none));
            assert_eq!(actions.editor.source().as_str(), "foo");
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn old_source_and_read_only_reviews_cannot_apply_to_a_newer_presentation() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, actions, fake) = fixture(mode);
            prepare(&fake, actions, edit()).await;
            settled(actions).await;
            actions.apply_workspace(
                actions.review.get_untracked().unwrap(),
                Signal::derive(|| true),
            );
            assert_eq!(actions.editor.source().as_str(), "foo");
            actions.editor.workspace.content.set("newer".into());
            actions.apply_workspace(
                actions.review.get_untracked().unwrap(),
                Signal::derive(|| false),
            );
            assert_eq!(actions.editor.source().as_str(), "newer");
            assert_eq!(
                actions.editor.workspace.editor_tabs.get_untracked()[&1].len(),
                2
            );
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn diagnostic_navigation_orders_deduplicates_wraps_and_rejects_stale_documents_in_both_modes() {
        for mode in [WorkspaceMode::Local, WorkspaceMode::Remote] {
            let (_owner, actions, _) = fixture(mode);
            let captured = actions.snapshot.get_value().unwrap();
            actions
                .diagnostics_source
                .set_value(Some((captured.presentation, captured.revision)));
            let diagnostic = |character| protocol::Diagnostic {
                range: Range {
                    start: Position { line: 0, character },
                    end: Position { line: 0, character },
                },
                severity: 1,
                message: "Problem".into(),
                source: None,
                code: None,
            };
            actions
                .diagnostics
                .set(vec![diagnostic(2), diagnostic(0), diagnostic(2)]);
            assert!(actions.navigate_next_diagnostic(false));
            assert_eq!(actions.navigation.get_untracked().unwrap().2, 2);
            actions
                .editor
                .record_selection(Selection::caret(2))
                .unwrap();
            assert!(actions.navigate_next_diagnostic(false));
            assert_eq!(actions.navigation.get_untracked().unwrap().2, 0);
            actions
                .editor
                .record_selection(Selection::caret(0))
                .unwrap();
            assert!(actions.navigate_next_diagnostic(true));
            assert_eq!(actions.navigation.get_untracked().unwrap().2, 2);
            actions
                .editor
                .workspace
                .open_file
                .set(Some("other.rs".into()));
            assert!(!actions.navigate_next_diagnostic(false));
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn diagnostic_provider_failures_remain_visible_without_primary_controls() {
        use leptos::mount::mount_to;
        let (_owner, mut actions, _) = fixture(WorkspaceMode::Local);
        actions.primary_available = Memo::new(|_| false);
        let provider = actions.identity.get_untracked().unwrap().provider;
        actions.provider_diagnostics.set(vec![ProviderDiagnostics {
            provider,
            diagnostics: vec![],
            ready: false,
            server: None,
            error: Some("Install the analyzer on the execution host".into()),
        }]);
        let document = web_sys::window().unwrap().document().unwrap();
        let root = document
            .create_element("div")
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>();
        document.body().unwrap().append_child(&root).unwrap();
        let mounted = mount_to(root.clone(), move || {
            view! {
                <crate::components::editor_intelligence::EditorIntelligenceToolbar actions read_only=Signal::derive(||false) />
            }
        });
        crate::util::yield_task().await;
        root.query_selector("button[aria-label='Code intelligence']")
            .unwrap()
            .unwrap()
            .unchecked_into::<web_sys::HtmlElement>()
            .click();
        crate::util::yield_task().await;
        let items = root.query_selector_all("[role=menuitem]").unwrap();
        let mut problems = None;
        for index in 0..items.length() {
            let item = items
                .item(index)
                .unwrap()
                .unchecked_into::<web_sys::HtmlElement>();
            assert!(
                !item
                    .text_content()
                    .unwrap_or_default()
                    .contains("Go to definition")
            );
            if item
                .text_content()
                .unwrap_or_default()
                .starts_with("Problems (")
            {
                problems = Some(item);
            }
        }
        problems.unwrap().click();
        crate::util::yield_task().await;
        let text = root.text_content().unwrap();
        assert!(text.contains("Install the analyzer on the execution host"));
        assert!(!text.contains("No reported problems"));
        drop(mounted);
        root.remove();
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
