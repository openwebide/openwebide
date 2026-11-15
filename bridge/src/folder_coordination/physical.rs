//! OS directory exclusion is an additional primitive below the shared native
//! coordinator. Windows keeps its existing in-process coordination until a
//! native directory-lock adapter has been implemented and verified there.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(super) use unix::Access;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use unix::quarantine_descriptors;

#[cfg(not(unix))]
#[derive(Debug)]
pub(super) struct Access;
#[cfg(not(unix))]
impl Access {
    pub fn for_use(
        folder: &super::Folder,
        _workspace_use: super::WorkspaceUse,
    ) -> Result<Option<Self>, String> {
        if folder.current() {
            Ok(Some(Self))
        } else {
            Err("The physical execution folder changed while waiting".into())
        }
    }
}
