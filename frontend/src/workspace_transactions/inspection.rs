//! Remote counterpart of the proven local companion reader. It supplies raw
//! native entries to shared review planning, independently of the file tree.
use crate::backend::Backend;
use openwebide_core::workspace_transaction::companion::{
    FileValue, FolderInspection, FolderInspector, InspectedFolder,
};
use std::rc::Rc;

const INSPECTION_TIMEOUT_MS: u32 = 30_000;

pub struct RemoteFolderInspector {
    backend: Rc<dyn Backend>,
    project: i64,
    current: Rc<dyn Fn() -> bool>,
}
impl FolderInspector for RemoteFolderInspector {
    async fn inspect(&self, request: &FolderInspection) -> Result<FileValue, String> {
        request.validate()?;
        if !(self.current)() {
            return Err("The inspected workspace changed".into());
        }
        let result = crate::util::with_deadline(
            self.backend.inspect_workspace_folder(self.project, request),
            INSPECTION_TIMEOUT_MS,
        )
        .await;
        if !(self.current)() {
            return Err("The inspected workspace changed during its read".into());
        }
        result
            .ok_or_else(|| "Workspace inspection timed out; request the review again".to_string())?
            .map_err(|error| error.to_string())
    }
}

pub async fn remote_files(
    backend: Rc<dyn Backend>,
    project: i64,
    current: Rc<dyn Fn() -> bool>,
) -> Result<InspectedFolder<RemoteFolderInspector>, String> {
    if project <= 0 || !current() {
        return Err("The inspected workspace changed".into());
    }
    let result = crate::util::with_deadline(
        backend.workspace_transaction_root(project),
        INSPECTION_TIMEOUT_MS,
    )
    .await;
    if !current() {
        return Err("The inspected workspace changed during root discovery".into());
    }
    let root = result
        .ok_or_else(|| "Workspace root inspection timed out".to_string())?
        .map_err(|error| error.to_string())?;
    InspectedFolder::new(
        RemoteFolderInspector {
            backend,
            project,
            current,
        },
        root.identity,
    )
}

#[cfg(test)]
mod tests;
