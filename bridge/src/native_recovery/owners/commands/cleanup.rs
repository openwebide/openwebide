//! Original process cleanup is enrolled before descriptors leave the command
//! owner. Only that private capability can certify cleanup from the watcher.
use super::*;
use std::os::{fd::OwnedFd, unix::ffi::OsStringExt};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    token: String,
    pub(super) confirmed: bool,
}
impl Receipt {
    pub(super) fn validate(&self) -> Result<(), String> {
        if !openwebide_core::workspace_transaction::valid_id(&self.token) {
            return Err(INVALID.into());
        }
        Ok(())
    }
}
/// Private socket payload, never an HTTP permission or a new command claim.
/// Reopening it cannot initialize or replace native state.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessCleanup {
    home: Vec<u8>,
    key: Key,
    owner: String,
    token: String,
}
impl ProcessCleanup {
    fn namespace(&self) -> Result<Namespace, String> {
        if self.home.len() > 16 * 1024
            || self.home.contains(&0)
            || !fingerprint(&self.owner)
            || !openwebide_core::workspace_transaction::valid_id(&self.token)
            // SAFETY: geteuid has no preconditions.
            || self.key.uid != unsafe { libc::geteuid() }
        {
            return Err(INVALID.into());
        }
        let path = PathBuf::from(std::ffi::OsString::from_vec(self.home.clone()));
        if !path.is_absolute() {
            return Err(INVALID.into());
        }
        let home = Folder::capture_state(&path)?;
        let parent = root(&home)?;
        let anchor = child(&parent, ANCHOR, self.key.uid)?;
        if original(&anchor)? != self.key.anchor
            || original(home.handle.as_file())? != self.key.home
        {
            return Err(INVALID.into());
        }
        let directory = Namespace::directory(&parent, &self.key)?;
        Ok(Namespace {
            home,
            directory,
            anchor,
            key: self.key.clone(),
        })
    }
    fn update(
        &self,
        descriptors: &[OwnedFd],
        required: bool,
        change: impl FnOnce(&mut CommandRecord) -> Result<bool, String>,
    ) -> Result<(), String> {
        let namespace = self.namespace()?;
        namespace.access(true, |parent, anchor, connection| {
            let owners = super::super::read(connection, version(connection)?)?;
            let owner = owners.iter().find(|owner| owner.id == self.owner);
            let Some(owner) = owner else {
                // Ends only this old watcher's local work. It neither writes a
                // receipt nor grants access when its original record is absent.
                return if required {
                    Err(PENDING.into())
                } else {
                    Ok(())
                };
            };
            let mut record = read(connection, version(connection)?)?
                .into_iter()
                .find(|record| record.owner == self.owner)
                .ok_or(PENDING)?;
            if !owner.command
                || record
                    .cleanup
                    .as_ref()
                    .is_none_or(|c| c.token != self.token)
            {
                return Err(PENDING.into());
            }
            owner.folder.retained_by(descriptors)?;
            if change(&mut record)? {
                save(connection, &record)?;
                namespace.save(parent, anchor, connection)?;
            }
            Ok(())
        })
    }
    pub(crate) fn validate(&self, descriptors: &[OwnedFd]) -> Result<(), String> {
        self.update(descriptors, true, |_| Ok(false))
    }
    /// A successful warm release requires the exact outcome to be durable first.
    pub(crate) fn release(&self, descriptors: &[OwnedFd]) -> Result<(), String> {
        self.update(descriptors, false, |record| record.confirm_cleanup(true))
    }
    /// Called only after the original managed group is positively quiet.
    pub(crate) fn stopped(&self, descriptors: &[OwnedFd]) -> Result<(), String> {
        self.update(descriptors, false, |record| record.confirm_cleanup(false))
    }
}
impl CommandRecord {
    fn confirm_cleanup(&mut self, require_outcome: bool) -> Result<bool, String> {
        if require_outcome && self.outcome.is_none() {
            return Err("Persist the original command outcome before releasing its watcher".into());
        }
        let receipt = self.cleanup.as_mut().ok_or(PENDING)?;
        let changed = !receipt.confirmed || (self.outcome.is_some() && !self.cleaned);
        receipt.confirmed = true;
        self.cleaned = self.outcome.is_some();
        Ok(changed)
    }
}
impl Namespace {
    pub(crate) fn command_process_cleanup(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
    ) -> Result<ProcessCleanup, String> {
        if !lease.covers(folder) {
            return Err(PENDING.into());
        }
        self.command_access(folder, lease, permit, |connection, owner, record| {
            let mut record = record.ok_or(PENDING)?;
            if owner.files != FileStage::Finished
                || record.stage != CommandStage::Admitted
                || record.worker.id != worker.id
                || record.worker.secret != worker.secret
            {
                return Err(PENDING.into());
            }
            if record.cleanup.is_none() {
                record.format = 3;
                record.cleanup = Some(Receipt {
                    token: format!("{:032x}", rand::random::<u128>()),
                    confirmed: false,
                });
                if version(connection)? < 8 {
                    connection
                        .execute_batch("PRAGMA user_version=8;")
                        .map_err(|_| INVALID)?;
                }
                save(connection, &record)?;
            }
            Ok(ProcessCleanup {
                home: self.home.path.as_os_str().as_bytes().into(),
                key: self.key.clone(),
                owner: record.owner,
                token: record.cleanup.ok_or(PENDING)?.token,
            })
        })
    }
    /// The live original producer may also confirm its positively awaited
    /// process cleanup while it still holds the complete original lease.
    pub(crate) fn command_process_stopped(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        worker: &WorkspaceTransactionWorker,
    ) -> Result<(), String> {
        self.command_change(folder, lease, permit, worker, |record| {
            if record.cleanup.is_some() {
                record.confirm_cleanup(false)?;
            }
            Ok(())
        })
    }
}
