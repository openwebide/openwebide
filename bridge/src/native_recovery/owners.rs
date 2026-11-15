//! Original transaction claims shared by native profiles, projects and accounts.
//! Only private issuer credentials can recover an existing overlapping claim.
use super::*;
use crate::folder_coordination::{Lease, OriginalFolder};
use openwebide_core::workspace_transaction::{
    RecoveryAuthority, WorkspaceActivity, WorkspaceTransactionScope,
};

pub(crate) mod administration;
mod commands;
mod context;
pub(crate) use commands::{
    CommandIntent, PendingCommand, PendingCommandSettlement, ProcessCleanup,
};

const MAX_OWNERS: usize = 128;
const MAX_OWNER_BYTES: usize = 64 * 1024;
const PENDING: &str = "Recover the original native workspace operation before changing this folder";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    format: u32,
    id: String,
    binding: Vec<u8>,
    credential_digest: String,
    folder: OriginalFolder,
    command: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(default)]
    files: FileStage,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FileStage {
    #[default]
    Unknown,
    Reserved,
    Admitted,
    Finished,
}
#[derive(Clone)]
pub(crate) struct Permit {
    key: Key,
    owner: Owner,
    user: i64,
    project: i64,
}
impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryPermit").finish_non_exhaustive()
    }
}
fn fingerprint(value: &str) -> bool {
    openwebide_core::workspace_transaction::companion::fingerprint(value)
}
impl Owner {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.format, 1 | 2)
            || !fingerprint(&self.id)
            || !fingerprint(&self.credential_digest)
            || self.binding.is_empty()
            || self.binding.len() > 16 * 1024
        {
            return Err(INVALID.into());
        }
        if self.format == 1
            && (self.credential.is_some()
                || self.origin.is_some()
                || self.files != FileStage::Unknown)
        {
            return Err(INVALID.into());
        }
        if self.format == 2 && self.authority()?.is_none() {
            return Err(INVALID.into());
        }
        if let Some(origin) = &self.origin {
            context::validate_origin(origin)?;
        }
        self.folder.validate()
    }
    fn same(&self, other: &Self) -> bool {
        self.binding == other.binding
            && self.credential_digest == other.credential_digest
            && self.folder == other.folder
    }
}
fn read(connection: &Connection, version: u32) -> Result<Vec<Owner>, String> {
    if version < 3 {
        return Ok(vec![]);
    }
    let mut statement = connection
        .prepare("SELECT id,record FROM recovery_owners LIMIT 129")
        .map_err(|_| INVALID)?;
    let mut rows = statement.query([]).map_err(|_| INVALID)?;
    let mut result = vec![];
    while let Some(row) = rows.next().map_err(|_| INVALID)? {
        if result.len() >= MAX_OWNERS {
            return Err(INVALID.into());
        }
        let id: String = row.get(0).map_err(|_| INVALID)?;
        let data: String = row.get(1).map_err(|_| INVALID)?;
        if data.len() > MAX_OWNER_BYTES {
            return Err(INVALID.into());
        }
        let owner: Owner = serde_json::from_str(&data).map_err(|_| INVALID)?;
        owner.validate()?;
        if owner.id != id || (version < 6 && owner.format == 2) {
            return Err(INVALID.into());
        }
        result.push(owner);
    }
    Ok(result)
}
pub(super) fn validate(connection: &Connection, version: u32) -> Result<(), String> {
    let owners = read(connection, version)?;
    administration::validate(connection, version, &owners)?;
    for (i, owner) in owners.iter().enumerate() {
        if owners[i + 1..].iter().any(|other| {
            owner.folder.overlaps(&other.folder)
                || (owner.binding == other.binding
                    && owner.credential_digest == other.credential_digest)
        }) {
            return Err(INVALID.into());
        }
    }
    commands::validate(connection, version, &owners)?;
    Ok(())
}
fn version(connection: &Connection) -> Result<u32, String> {
    connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| INVALID.into())
}
impl Namespace {
    pub(crate) fn permit(
        &self,
        folder: &Folder,
        authority: &RecoveryAuthority,
        scope: &WorkspaceTransactionScope,
        activity: WorkspaceActivity,
    ) -> Result<Permit, String> {
        if !authority.matches_scope(authority.user, authority.project, scope) {
            return Err("The original native recovery credential changed".into());
        }
        let binding =
            RecoveryAuthority::binding_for_scope(authority.user, authority.project, scope)?;
        let credential_digest = authority.secret_digest()?;
        let folder = OriginalFolder::capture(folder, true)?;
        let data = serde_json::to_vec(&(&self.key.nonce, &binding, &credential_digest, &folder))
            .map_err(|_| INVALID)?;
        let owner = Owner {
            format: 2,
            id: openwebide_core::plugins::content_digest(&data),
            binding,
            credential_digest,
            folder,
            command: activity.language_command,
            credential: Some(authority.secret.clone()),
            origin: None,
            files: FileStage::Reserved,
        };
        owner.validate()?;
        Ok(Permit {
            key: self.key.clone(),
            owner,
            user: authority.user,
            project: authority.project,
        })
    }
    fn access<T>(
        &self,
        exclusive: bool,
        work: impl FnOnce(&Dir, &Anchor, &Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let parent = root(&self.home)?;
        let anchor = child(&parent, ANCHOR, self.key.uid)?;
        lock(&anchor, exclusive)?;
        let record = anchor_record(&anchor)?;
        if exclusive && self.needs_recovery()? {
            self.recover(&parent, &record)?;
        }
        let connection = self.validated_database(&parent, &record)?;
        work(&parent, &record, &connection)
    }
    fn check(
        &self,
        connection: &Connection,
        owners: &[Owner],
        folder: &OriginalFolder,
        permit: Option<&Permit>,
    ) -> Result<(), String> {
        if let Some(permit) = permit {
            administration::check_retired(connection, &permit.owner)?;
        }
        if permit.is_some_and(|p| p.key != self.key || p.owner.folder != *folder) {
            return Err(PENDING.into());
        }
        for owner in owners {
            if (owner.folder.overlaps(folder)
                || permit.is_some_and(|p| {
                    owner.binding == p.owner.binding
                        && owner.credential_digest == p.owner.credential_digest
                }))
                && permit.is_none_or(|p| !owner.same(&p.owner))
            {
                return Err(PENDING.into());
            }
        }
        Ok(())
    }
    /// Older hosts do not understand compatible ordinary/ancestor admission.
    /// Publish the protocol upgrade before exposing that access, so their next
    /// original-state check fails rather than bypassing a parent ordinary user.
    pub(crate) fn prepare_ordinary_admission(&self) -> Result<(), String> {
        if self.access(false, |_, _, connection| version(connection))? >= 11 {
            return Ok(());
        }
        self.access(true, |parent, anchor, connection| {
            if version(connection)? >= 11 {
                return Ok(());
            }
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS recovery_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS recovery_commands(id TEXT PRIMARY KEY,record TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS recovery_command_settlements(id TEXT PRIMARY KEY,record TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS recovery_retired_owners(id TEXT PRIMARY KEY,record TEXT NOT NULL);
                 PRAGMA user_version=11;",
            ).map_err(|_| INVALID)?;
            self.save(parent, anchor, connection)
        })
    }
    pub(crate) fn admit(&self, folder: &Folder, permit: Option<&Permit>) -> Result<(), String> {
        let folder = OriginalFolder::capture(folder, false)?;
        self.access(false, |_, _, connection| {
            self.check(
                connection,
                &read(connection, version(connection)?)?,
                &folder,
                permit,
            )
        })
    }
    fn save(&self, parent: &Dir, anchor: &Anchor, connection: &Connection) -> Result<(), String> {
        connection
            .execute(
                "UPDATE recovery_revision SET revision=revision+1 WHERE id=1",
                [],
            )
            .map_err(|_| INVALID)?;
        let data = connection.serialize("main").map_err(|_| INVALID)?;
        self.persist(parent, anchor, &data, |_| Ok(()))
    }
    pub(crate) fn enroll(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
    ) -> Result<(), String> {
        self.enrollment_lease(folder, lease, permit)?;
        let folder = OriginalFolder::capture(folder, true)?;
        self.access(true, |parent, anchor, connection| {
            let (_, changed) = self.enroll_owner(connection, &folder, permit)?;
            if changed {
                self.save(parent, anchor, connection)?;
            }
            Ok(())
        })
    }
    fn enrollment_lease(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
    ) -> Result<(), String> {
        if permit.key != self.key
            || !lease.covers(folder)
            || lease
                .recovery()
                .is_none_or(|held| held.key != permit.key || !held.owner.same(&permit.owner))
        {
            return Err("Original recovery enrollment requires the complete folder lease".into());
        }
        Ok(())
    }
    // Mutate only the private in-memory database. The caller publishes this
    // owner and any attached command reservation in one anchored replacement.
    fn enroll_owner(
        &self,
        connection: &Connection,
        folder: &OriginalFolder,
        permit: &Permit,
    ) -> Result<(Owner, bool), String> {
        let owners = read(connection, version(connection)?)?;
        self.check(connection, &owners, folder, Some(permit))?;
        let existing = owners.iter().find(|o| o.same(&permit.owner));
        if existing.is_none() && owners.len() >= MAX_OWNERS {
            return Err("Too many pending native recovery owners".into());
        }
        let mut stored = existing.cloned().unwrap_or_else(|| permit.owner.clone());
        stored.command |= permit.owner.command;
        stored.format = 2;
        stored.credential.clone_from(&permit.owner.credential);
        if let Some(origin) = &permit.owner.origin {
            if stored
                .origin
                .as_ref()
                .is_some_and(|original| original != origin)
            {
                return Err(
                    "Reconnect the original recovery callback before changing this folder".into(),
                );
            }
            stored.origin = Some(origin.clone());
        }
        if existing.is_some_and(|old| {
            old.command == stored.command
                && old.format == stored.format
                && old.credential == stored.credential
                && old.origin == stored.origin
        }) {
            return Ok((stored, false));
        }
        connection.execute_batch("CREATE TABLE IF NOT EXISTS recovery_owners (id TEXT PRIMARY KEY, record TEXT NOT NULL); CREATE TABLE IF NOT EXISTS recovery_commands (id TEXT PRIMARY KEY, record TEXT NOT NULL);").map_err(|_|INVALID)?;
        if version(connection)? < 6 {
            connection
                .execute_batch("PRAGMA user_version=6;")
                .map_err(|_| INVALID)?;
        }
        context::save_owner(connection, &stored)?;
        Ok((stored, true))
    }
    /// File execution alone cannot reconcile an attached native command.
    pub(crate) fn finish_files(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        activity: WorkspaceActivity,
    ) -> Result<(), String> {
        if !lease.covers(folder) {
            return Err("Original recovery release requires the complete folder lease".into());
        }
        let folder = OriginalFolder::capture(folder, true)?;
        self.access(true, |parent, anchor, connection| {
            let owners = read(connection, version(connection)?)?;
            self.check(connection, &owners, &folder, Some(permit))?;
            let owner = owners
                .iter()
                .find(|o| o.same(&permit.owner))
                .ok_or(PENDING)?;
            if !activity.file_recovery && owner.format == 2 && owner.files != FileStage::Finished {
                let mut completed = owner.clone();
                completed.files = FileStage::Finished;
                context::save_owner(connection, &completed)?;
                if owner.command {
                    return self.save(parent, anchor, connection);
                }
            }
            if activity.ensure_idle().is_err() || owner.command {
                return Ok(());
            }
            connection
                .execute("DELETE FROM recovery_owners WHERE id=?1", [&owner.id])
                .map_err(|_| INVALID)?;
            self.save(parent, anchor, connection)
        })
    }
}
#[cfg(test)]
mod tests;
