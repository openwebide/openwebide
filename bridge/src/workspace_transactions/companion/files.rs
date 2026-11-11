//! Wire-to-filesystem adaptation only. No journal or feature workflow lives here.
use super::*;
use openwebide_core::{
    vfs::VfsEntryKind, workspace_entries::WorkspaceEntries, workspace_transaction::TransactionFiles,
};

fn kind(directory: bool) -> VfsEntryKind {
    if directory {
        VfsEntryKind::Directory
    } else {
        VfsEntryKind::File
    }
}
pub(super) async fn execute(
    files: &ResourceFiles,
    scope: Option<&ResourceScope>,
    action: &FileAction,
) -> Result<FileValue, String> {
    match action {
        FileAction::Canonicalize { path } => {
            return files.canonicalize(path).await.map(FileValue::Path);
        }
        FileAction::List { path } => return files.list(path).await.map(FileValue::Entries),
        FileAction::Read { path } => return files.read_bytes(path).await.map(FileValue::Bytes),
        _ => {}
    }
    let scope = scope.ok_or("The reviewed resource scope is unavailable")?;
    match action {
        FileAction::Create {
            ordinal,
            path,
            is_dir,
        } => {
            files
                .create_resource(scope, *ordinal, path, kind(*is_dir))
                .await?;
        }
        FileAction::ReconcileCreation {
            ordinal,
            path,
            is_dir,
        } => {
            files
                .reconcile_resource_creation(scope, *ordinal, path, kind(*is_dir))
                .await?;
        }
        FileAction::CreatedMatches {
            ordinal,
            path,
            is_dir,
        } => {
            return files
                .created_resource_matches(scope, *ordinal, path, kind(*is_dir))
                .await
                .map(FileValue::Matches);
        }
        FileAction::RemoveCreated {
            ordinal,
            path,
            is_dir,
        } => {
            files
                .remove_created_resource(scope, *ordinal, path, kind(*is_dir))
                .await?;
        }
        FileAction::RemovedMatches {
            ordinal,
            path,
            is_dir,
        } => {
            return files
                .removed_resource_matches(scope, *ordinal, path, kind(*is_dir))
                .await
                .map(FileValue::Matches);
        }
        FileAction::RetainedCanonicalize { path } => {
            return files
                .retained_canonicalize(scope, path)
                .await
                .map(FileValue::Path);
        }
        FileAction::RetainedList { path } => {
            return files
                .retained_list(scope, path)
                .await
                .map(FileValue::Entries);
        }
        FileAction::RetainedRead { path } => {
            return files
                .retained_read_bytes(scope, path)
                .await
                .map(FileValue::Bytes);
        }
        FileAction::Transfer { from, to, is_dir } => {
            files
                .transfer_resource(scope, from, to, kind(*is_dir))
                .await?;
        }
        FileAction::RemoveRetained { path, is_dir } => {
            files
                .remove_retained_entry(scope, path, kind(*is_dir))
                .await?;
        }
        FileAction::Finish => files.finish_retained(scope).await?,
        _ => return Err("Unsupported native filesystem call".into()),
    }
    Ok(FileValue::Unit)
}
