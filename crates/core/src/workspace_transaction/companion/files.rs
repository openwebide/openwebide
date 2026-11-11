//! The same serialized companion adapter is used by browser and native contract
//! clients. Dropping a request poisons this session until a fresh folder proof.
use super::*;
use crate::{
    vfs::VfsEntryKind, workspace_entries::WorkspaceEntries, workspace_transaction::TransactionFiles,
};
use std::future::Future;

pub trait FolderTransport {
    fn request(&self, request: &FolderRequest)
    -> impl Future<Output = Result<FolderReply, String>>;
}
struct Cursor {
    sequence: u64,
    usable: bool,
}
pub struct CompanionFiles<T> {
    transport: T,
    user: i64,
    project: i64,
    session: String,
    identity: String,
    worker: WorkspaceTransactionWorkerKey,
    admission: Option<(String, String)>,
    scope: Option<ResourceScope>,
    cursor: futures::lock::Mutex<Cursor>,
}
impl<T: FolderTransport> CompanionFiles<T> {
    pub fn new(
        transport: T,
        user: i64,
        project: i64,
        session: String,
        identity: String,
        worker: WorkspaceTransactionWorkerKey,
    ) -> Result<Self, String> {
        if user <= 0 || project <= 0 || !valid_id(&session) || !fingerprint(&identity) {
            return Err("Invalid proven companion folder".into());
        }
        worker.validate()?;
        Ok(Self {
            transport,
            user,
            project,
            session,
            identity,
            worker,
            admission: None,
            scope: None,
            cursor: futures::lock::Mutex::new(Cursor {
                sequence: 0,
                usable: true,
            }),
        })
    }
    pub async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &super::super::TransactionProgress,
    ) -> Result<u64, String> {
        if self
            .admission
            .as_ref()
            .is_none_or(|admission| admission.0 != id || admission.1 != generation)
        {
            return Err("Paired recovery belongs to another admitted operation".into());
        }
        self.journal_request(
            FolderAction::Advance {
                session: self.session.clone(),
                request: AdvanceWorkspaceTransaction {
                    id: id.into(),
                    generation: generation.into(),
                    revision,
                    progress: progress.clone(),
                },
            },
            |reply| {
                let FolderReply::Advanced { response } = reply else {
                    return Err("Invalid paired cursor acknowledgement".into());
                };
                response.validate(generation, revision)?;
                if response.editor.is_some() {
                    return Err("Paired cursor acknowledgement must contain metadata only".into());
                }
                Ok(response.revision)
            },
        )
        .await
    }
    pub fn worker(&self) -> &WorkspaceTransactionWorkerKey {
        &self.worker
    }
    pub async fn execute(
        &self,
        api_url: String,
        delegation: WorkspaceJournalDelegation,
        request: super::super::ExecuteWorkspaceTransaction,
    ) -> Result<super::super::WorkspaceTransactionOutcome, String> {
        if delegation.user != self.user
            || delegation.project != self.project
            || delegation.scope.root_identity != self.identity
            || self
                .scope
                .as_ref()
                .is_none_or(|scope| scope.transaction != delegation.scope.id)
        {
            return Err("The native transaction belongs to another proven folder or review".into());
        }
        let generation = request.generation.clone();
        self.journal_request(
            FolderAction::Execute {
                session: self.session.clone(),
                api_url,
                delegation,
                request,
            },
            |reply| {
                let FolderReply::Executed { outcome } = reply else {
                    return Err("Invalid native transaction acknowledgement".into());
                };
                outcome.validate(&generation)?;
                Ok(outcome)
            },
        )
        .await
    }
    pub async fn admit(
        &mut self,
        api_url: String,
        delegation: WorkspaceJournalDelegation,
    ) -> Result<(), String> {
        delegation.validate()?;
        if delegation.user != self.user
            || delegation.project != self.project
            || delegation.scope.root_identity != self.identity
        {
            return Err(
                "Paired journal belongs to another account, project or physical folder".into(),
            );
        }
        let identity = (
            delegation.scope.id.clone(),
            delegation.scope.generation.clone(),
        );
        self.journal_request(
            FolderAction::Admit {
                session: self.session.clone(),
                api_url,
                delegation,
            },
            |reply| {
                if matches!(reply, FolderReply::Admitted) {
                    Ok(())
                } else {
                    Err("Invalid paired admission acknowledgement".into())
                }
            },
        )
        .await?;
        self.admission = Some(identity);
        Ok(())
    }
    async fn journal_request<V>(
        &self,
        action: FolderAction,
        decode: impl FnOnce(FolderReply) -> Result<V, String>,
    ) -> Result<V, String> {
        let mut cursor = self.cursor.lock().await;
        if !cursor.usable {
            return Err(
                "A companion acknowledgement was lost; reconnect the folder before recovery".into(),
            );
        }
        let request = FolderRequest {
            user: self.user,
            project: self.project,
            action,
        };
        request.validate()?;
        cursor.usable = false;
        let reply = self.transport.request(&request).await?;
        let value = decode(reply)?;
        cursor.usable = true;
        Ok(value)
    }
    /// Keep authenticated full recovery reads at their original transport;
    /// only cursor writes cross the proven native session.
    pub fn journal<'a, J: super::super::WorkspaceJournal>(
        &'a self,
        readonly: &'a J,
    ) -> impl super::super::WorkspaceJournal + 'a {
        PairedWorkspaceJournal {
            files: self,
            readonly,
        }
    }
    pub async fn bind(&mut self, scope: ResourceScope) -> Result<(), String> {
        self.call(
            FileAction::Bind {
                scope: scope.clone(),
            },
            unit,
        )
        .await?;
        self.scope = Some(scope);
        Ok(())
    }
    pub async fn close(&self) -> Result<(), String> {
        let mut cursor = self.cursor.lock().await;
        cursor.usable = false;
        match self
            .transport
            .request(&FolderRequest {
                user: self.user,
                project: self.project,
                action: FolderAction::Close {
                    session: self.session.clone(),
                },
            })
            .await?
        {
            FolderReply::Closed => Ok(()),
            _ => Err("Invalid companion close acknowledgement".into()),
        }
    }
    async fn call<V>(
        &self,
        action: FileAction,
        decode: impl FnOnce(FileValue) -> Result<V, String>,
    ) -> Result<V, String> {
        action.validate()?;
        let mut cursor = self.cursor.lock().await;
        if !cursor.usable {
            return Err(
                "A companion acknowledgement was lost; reconnect the folder before recovery".into(),
            );
        }
        let sequence = cursor
            .sequence
            .checked_add(1)
            .ok_or("Folder request sequence exhausted")?;
        // Set BEFORE the await: cancellation cannot allow later primitives to
        // overtake a native worker which still owns an ambiguous request.
        cursor.usable = false;
        let reply = self
            .transport
            .request(&FolderRequest {
                user: self.user,
                project: self.project,
                action: FolderAction::File {
                    session: self.session.clone(),
                    sequence,
                    action,
                },
            })
            .await?;
        let FolderReply::File { outcome } = reply else {
            return Err("Invalid companion filesystem acknowledgement".into());
        };
        cursor.sequence = sequence;
        cursor.usable = true;
        match outcome {
            Err(error) => Err(error),
            Ok(value) => decode(value).inspect_err(|_| cursor.usable = false),
        }
    }
    async fn resource<V>(
        &self,
        scope: &ResourceScope,
        action: FileAction,
        decode: impl FnOnce(FileValue) -> Result<V, String>,
    ) -> Result<V, String> {
        if self.scope.as_ref() != Some(scope) {
            return Err("Companion files belong to another reviewed transaction".into());
        }
        self.call(action, decode).await
    }
}
struct PairedWorkspaceJournal<'a, T, J> {
    files: &'a CompanionFiles<T>,
    readonly: &'a J,
}
impl<T: FolderTransport, J: super::super::WorkspaceJournal> super::super::WorkspaceJournal
    for PairedWorkspaceJournal<'_, T, J>
{
    async fn prepare(
        &self,
        plan: &super::super::WorkspacePlan,
    ) -> Result<super::super::PreparedJournal, String> {
        self.readonly.prepare(plan).await
    }
    async fn load(&self, id: Option<&str>) -> Result<Option<super::super::JournalRecord>, String> {
        self.readonly.load(id).await
    }
    async fn advance(
        &self,
        id: &str,
        generation: &str,
        revision: u64,
        progress: &super::super::TransactionProgress,
    ) -> Result<u64, String> {
        self.files.advance(id, generation, revision, progress).await
    }
}
fn wrong() -> String {
    "Invalid companion filesystem result type".into()
}
fn unit(value: FileValue) -> Result<(), String> {
    if matches!(value, FileValue::Unit) {
        Ok(())
    } else {
        Err(wrong())
    }
}
fn matches(value: FileValue) -> Result<bool, String> {
    if let FileValue::Matches(value) = value {
        Ok(value)
    } else {
        Err(wrong())
    }
}
pub(super) fn path(value: FileValue) -> Result<String, String> {
    if let FileValue::Path(value) = value {
        Ok(value)
    } else {
        Err(wrong())
    }
}
pub(super) fn entries(value: FileValue) -> Result<Vec<FileEntry>, String> {
    if let FileValue::Entries(value) = value {
        Ok(value)
    } else {
        Err(wrong())
    }
}
pub(super) fn bytes(value: FileValue) -> Result<Vec<u8>, String> {
    if let FileValue::Bytes(value) = value {
        Ok(value)
    } else {
        Err(wrong())
    }
}

impl<T: FolderTransport> WorkspaceEntries for CompanionFiles<T> {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        self.call(FileAction::Canonicalize { path: path.into() }, self::path)
            .await
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        self.call(FileAction::List { path: path.into() }, entries)
            .await
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.call(FileAction::Read { path: path.into() }, bytes)
            .await
    }
    async fn create(&self, _path: &str, _kind: VfsEntryKind) -> Result<(), String> {
        Err("Companion resource creation requires its reviewed intent".into())
    }
    async fn write_bytes(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Companion originals cannot be reconstructed from bytes".into())
    }
    async fn delete(&self, _path: &str) -> Result<(), String> {
        Err("Companion resource removal requires its reviewed intent".into())
    }
}
impl<T: FolderTransport> TransactionFiles for CompanionFiles<T> {
    fn root_identity(&self) -> &str {
        &self.identity
    }
    fn supports_resource_moves(&self) -> bool {
        self.scope.is_some()
    }
    async fn replace_atomic(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Companion resource recovery never reconstructs originals".into())
    }
    async fn remove_entry(&self, _path: &str, _kind: VfsEntryKind) -> Result<(), String> {
        Err("Companion resource removal requires its creation receipt".into())
    }
    async fn create_resource(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.resource(
            scope,
            FileAction::Create {
                ordinal,
                path: path.into(),
                is_dir: kind.is_dir(),
            },
            unit,
        )
        .await
    }
    async fn reconcile_resource_creation(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.resource(
            scope,
            FileAction::ReconcileCreation {
                ordinal,
                path: path.into(),
                is_dir: kind.is_dir(),
            },
            unit,
        )
        .await
    }
    async fn created_resource_matches(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<bool, String> {
        self.resource(
            scope,
            FileAction::CreatedMatches {
                ordinal,
                path: path.into(),
                is_dir: kind.is_dir(),
            },
            matches,
        )
        .await
    }
    async fn remove_created_resource(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.resource(
            scope,
            FileAction::RemoveCreated {
                ordinal,
                path: path.into(),
                is_dir: kind.is_dir(),
            },
            unit,
        )
        .await
    }
    async fn removed_resource_matches(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<bool, String> {
        self.resource(
            scope,
            FileAction::RemovedMatches {
                ordinal,
                path: path.into(),
                is_dir: kind.is_dir(),
            },
            matches,
        )
        .await
    }
    async fn retained_canonicalize(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<String, String> {
        self.resource(
            scope,
            FileAction::RetainedCanonicalize { path: path.into() },
            self::path,
        )
        .await
    }
    async fn retained_list(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<FileEntry>, String> {
        self.resource(
            scope,
            FileAction::RetainedList { path: path.into() },
            entries,
        )
        .await
    }
    async fn retained_read_bytes(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<u8>, String> {
        self.resource(scope, FileAction::RetainedRead { path: path.into() }, bytes)
            .await
    }
    async fn transfer_resource(
        &self,
        scope: &ResourceScope,
        from: &ResourceLocation,
        to: &ResourceLocation,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.resource(
            scope,
            FileAction::Transfer {
                from: from.clone(),
                to: to.clone(),
                is_dir: kind.is_dir(),
            },
            unit,
        )
        .await
    }
    async fn remove_retained_entry(
        &self,
        scope: &ResourceScope,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.resource(
            scope,
            FileAction::RemoveRetained {
                path: path.into(),
                is_dir: kind.is_dir(),
            },
            unit,
        )
        .await
    }
    async fn finish_retained(&self, scope: &ResourceScope) -> Result<(), String> {
        self.resource(scope, FileAction::Finish, unit).await
    }
}
