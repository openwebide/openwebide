use super::*;
use crate::{backend::Backend, testing::fake_backend::FakeBackend};
use leptos::prelude::*;
use openwebide_core::workspace_transaction::{ResourceLocation, companion::FileAction};
use std::cell::Cell;

fn handle(state: &str) -> FileSystemDirectoryHandle {
    js_sys::Function::new_with_args(
        "state",
        r#"
        return {queryPermission:async options=> {
            if(options.mode!=='readwrite') throw new Error('wrong permission mode');
            return state;
        },requestPermission:()=>{throw new Error('must not prompt from a filesystem call')}};
    "#,
    )
    .call1(
        &wasm_bindgen::JsValue::NULL,
        &wasm_bindgen::JsValue::from_str(state),
    )
    .unwrap()
    .unchecked_into()
}
fn delegation() -> openwebide_core::workspace_transaction::WorkspaceJournalDelegation {
    use openwebide_core::{WorkspaceMode, editor::EditorRecoveryRoot, workspace_transaction::*};
    WorkspaceJournalDelegation {
        id: "a".repeat(32),
        secret: "b".repeat(32),
        user: 1,
        project: 1,
        scope: WorkspaceTransactionScope {
            id: "c".repeat(32),
            generation: "d".repeat(32),
            revision: 1,
            root_identity: "b".repeat(64),
            root: EditorRecoveryRoot {
                mode: WorkspaceMode::Local,
                path: None,
            },
            worker: None,
            accepted_language_review_digest: None,
        },
    }
}
fn execution() -> FolderAction {
    FolderAction::Execute {
        session: "a".repeat(32),
        api_url: "http://127.0.0.1/api".into(),
        delegation: delegation(),
        request: openwebide_core::workspace_transaction::ExecuteWorkspaceTransaction {
            id: "c".repeat(32),
            generation: "d".repeat(32),
            action: openwebide_core::workspace_transaction::WorkspaceTransactionAction::Apply,
            language_grant: None,
        },
    }
}
#[wasm_bindgen_test::wasm_bindgen_test]
async fn revoked_browser_grants_and_context_changes_stop_before_companion_dispatch() {
    let owner = Owner::new();
    let fake = Rc::new(FakeBackend::default());
    let api: Api = owner.with(|| StoredValue::new_local(fake.clone() as Rc<dyn Backend>));
    let request = FolderRequest {
        user: 1,
        project: 1,
        action: FolderAction::File {
            session: "a".repeat(32),
            sequence: 1,
            action: FileAction::Transfer {
                from: ResourceLocation::Workspace("original".into()),
                to: ResourceLocation::Workspace("moved".into()),
                is_dir: false,
            },
        },
    };
    let requests = [
        FolderRequest {
            user: 1,
            project: 1,
            action: execution(),
        },
        request.clone(),
        FolderRequest {
            user: 1,
            project: 1,
            action: FolderAction::Admit {
                session: "a".repeat(32),
                api_url: "http://127.0.0.1/api".into(),
                delegation: delegation(),
            },
        },
        FolderRequest {
            user: 1,
            project: 1,
            action: FolderAction::Advance {
                session: "a".repeat(32),
                request: openwebide_core::workspace_transaction::AdvanceWorkspaceTransaction {
                    id: "c".repeat(32),
                    generation: "d".repeat(32),
                    revision: 1,
                    progress: Default::default(),
                },
            },
        },
    ];
    for request in &requests {
        for state in ["prompt", "denied"] {
            let transport = BrowserFolderTransport {
                client: PluginBridgeClient::new(
                    "http://127.0.0.1:1".into(),
                    BridgeCredentials::new(api),
                ),
                handle: handle(state),
                current: Rc::new(|| true),
                cleanup: None,
            };
            let error = transport.request(request).await.unwrap_err();
            assert!(
                error.contains(crate::local_fs::PERMISSION_NEEDED),
                "{error}"
            );
            assert!(fake.calls.borrow().is_empty());
        }
    }
    let checks = Rc::new(Cell::new(0));
    let current = {
        let checks = checks.clone();
        Rc::new(move || {
            let count = checks.get();
            checks.set(count + 1);
            count == 0
        })
    };
    let transport = BrowserFolderTransport {
        client: PluginBridgeClient::new("http://127.0.0.1:1".into(), BridgeCredentials::new(api)),
        handle: handle("granted"),
        current,
        cleanup: None,
    };
    assert!(
        transport
            .request(&request)
            .await
            .unwrap_err()
            .contains("changed while checking access")
    );
    assert_eq!(checks.get(), 2);
    assert!(fake.calls.borrow().is_empty());
}

mod deadline;
