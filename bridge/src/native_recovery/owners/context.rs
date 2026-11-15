use super::*;
use openwebide_core::editor::EditorRecoveryRoot;

pub(super) fn validate_origin(origin: &str) -> Result<(), String> {
    let uri = origin
        .parse::<hyper::Uri>()
        .map_err(|_| "Invalid original recovery callback")?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri.authority().is_none()
        || uri.query().is_some()
    {
        return Err("Invalid original recovery callback".into());
    }
    Ok(())
}
impl Owner {
    pub(super) fn authority(&self) -> Result<Option<RecoveryAuthority>, String> {
        let Some(secret) = &self.credential else {
            return Ok(None);
        };
        let (domain, format, user, project, transaction, generation, root_identity, root): (
            String,
            u32,
            i64,
            i64,
            String,
            String,
            String,
            EditorRecoveryRoot,
        ) = serde_json::from_slice(&self.binding).map_err(|_| INVALID)?;
        if domain != openwebide_core::workspace_transaction::RECOVERY_AUTHORITY_DOMAIN
            || format != 1
        {
            return Err(INVALID.into());
        }
        let authority = RecoveryAuthority {
            format,
            user,
            project,
            transaction,
            generation,
            root_identity,
            root,
            secret: secret.clone(),
        };
        authority.validate()?;
        if authority.secret_digest()? != self.credential_digest {
            return Err(INVALID.into());
        }
        Ok(Some(authority))
    }
}
impl Permit {
    pub(crate) fn with_origin(mut self, origin: Option<&str>) -> Result<Self, String> {
        if let Some(origin) = origin {
            let origin = origin.trim_end_matches('/');
            validate_origin(origin)?;
            self.owner.origin = Some(origin.into());
        }
        Ok(self)
    }
}
pub(super) fn save_owner(connection: &Connection, owner: &Owner) -> Result<(), String> {
    owner.validate()?;
    let data = serde_json::to_string(owner).map_err(|_| INVALID)?;
    if data.len() > MAX_OWNER_BYTES {
        return Err("Original native recovery metadata exceeds its capacity".into());
    }
    connection.execute("INSERT INTO recovery_owners (id,record) VALUES (?1,?2) ON CONFLICT(id) DO UPDATE SET record=excluded.record", [&owner.id, &data]).map_err(|_| INVALID)?;
    Ok(())
}
impl Namespace {
    pub(crate) fn files_finished(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
        activity: WorkspaceActivity,
    ) -> Result<(), String> {
        self.enrollment_lease(folder, lease, permit)?;
        if activity.file_recovery {
            return Ok(());
        }
        self.access(true, |parent, anchor, connection| {
            let owners = read(connection, version(connection)?)?;
            self.check(connection, &owners, &permit.owner.folder, Some(permit))?;
            let mut owner = owners
                .into_iter()
                .find(|owner| owner.same(&permit.owner))
                .ok_or(PENDING)?;
            if owner.format == 2 && owner.files != FileStage::Finished {
                owner.files = FileStage::Finished;
                save_owner(connection, &owner)?;
                self.save(parent, anchor, connection)?;
            }
            Ok(())
        })
    }
    pub(crate) fn files_admitted(
        &self,
        folder: &Folder,
        lease: &Lease,
        permit: &Permit,
    ) -> Result<(), String> {
        self.enrollment_lease(folder, lease, permit)?;
        self.access(true, |parent, anchor, connection| {
            let owners = read(connection, version(connection)?)?;
            self.check(connection, &owners, &permit.owner.folder, Some(permit))?;
            let mut owner = owners
                .into_iter()
                .find(|owner| owner.same(&permit.owner))
                .ok_or(PENDING)?;
            if owner.format == 2 && owner.files == FileStage::Reserved {
                owner.files = FileStage::Admitted;
                save_owner(connection, &owner)?;
                self.save(parent, anchor, connection)?;
            }
            Ok(())
        })
    }
}
