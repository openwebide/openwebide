//! Folder proof files are private native writes too. Abandoned proofs wait for
//! their original physical gate; losing an observer or runtime never authorizes
//! an unguarded unlink.
use super::super::CapturedRoot;
use crate::folder_coordination::{Coordinator, Folder, Lease};
use crate::runs::native_vfs::rooted::{Identity, Rooted};
use openwebide_core::{vfs::VfsEntryKind, workspace_transaction::companion::challenge_name};
use std::{fs::File, io::Write};

pub(super) struct Challenge {
    file: Option<Proof>,
    coordinator: Coordinator,
    folder: Folder,
}
struct Proof {
    root: Rooted,
    name: String,
    identity: Identity,
    secret: String,
    complete: bool,
}
impl Challenge {
    pub(super) fn new(
        root: &CapturedRoot,
        session: &str,
        coordinator: Coordinator,
        _lease: &Lease,
    ) -> Result<Self, String> {
        use rustix::fs::{Mode, OFlags, openat};
        let name = challenge_name(session)?;
        let captured = Rooted(
            root.handle
                .as_file()
                .try_clone()
                .map_err(|e| e.to_string())?,
        );
        let mut file = File::from(
            openat(
                root.handle.as_file(),
                name.as_str(),
                OFlags::CREATE | OFlags::EXCL | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|error| error.to_string())?,
        );
        let mut challenge = Self {
            file: Some(Proof {
                root: captured,
                name,
                identity: Identity::of(&file).map_err(|error| error.to_string())?,
                secret: format!(
                    "{:032x}{:032x}",
                    rand::random::<u128>(),
                    rand::random::<u128>()
                ),
                complete: false,
            }),
            coordinator,
            folder: root.folder.clone(),
        };
        let proof = challenge.file.as_mut().unwrap();
        file.write_all(proof.secret.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        root.handle
            .as_file()
            .sync_all()
            .map_err(|error| error.to_string())?;
        proof.complete = true;
        Ok(challenge)
    }
    pub(super) fn name(&self) -> &str {
        &self.file.as_ref().unwrap().name
    }
    pub(super) fn proves(&self, secret: &str) -> bool {
        self.file.as_ref().is_some_and(|proof| {
            crate::secret::constant_time_eq(secret.as_bytes(), proof.secret.as_bytes())
                && proof.current()
        })
    }
    /// Only the request holding this proof's original folder gate calls this.
    /// Successful explicit cleanup disarms Drop; failed cleanup stays guarded.
    pub(super) fn clear(&mut self, lease: &Lease) -> Result<(), String> {
        if let Some(proof) = &self.file {
            proof.clear(lease)?;
        }
        self.file = None;
        Ok(())
    }
}
impl Proof {
    fn current(&self) -> bool {
        self.root
            .identity(&self.name)
            .is_ok_and(|identity| identity == self.identity)
            && self
                .root
                .read(&self.name, 64)
                .is_ok_and(|bytes| bytes == self.secret.as_bytes())
    }
    fn clear(&self, _lease: &Lease) -> Result<(), String> {
        if self
            .root
            .kind(&self.name)
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Ok(());
        }
        if self
            .root
            .identity(&self.name)
            .map_err(|error| error.to_string())?
            != self.identity
        {
            return Err("Folder proof was replaced; preserving the replacement".into());
        }
        let bytes = self
            .root
            .read(&self.name, 64)
            .map_err(|error| error.to_string())?;
        if bytes != self.secret.as_bytes()
            && (self.complete || !self.secret.as_bytes().starts_with(&bytes))
        {
            return Err("Folder proof contents changed; preserving the later data".into());
        }
        self.root
            .remove(&self.name, VfsEntryKind::File)
            .map_err(|error| error.to_string())
    }
}
impl Drop for Challenge {
    fn drop(&mut self) {
        let Some(proof) = self.file.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("Folder proof preserved because its cleanup runtime is unavailable");
            return;
        };
        let registration = match self.coordinator.worker() {
            Ok(registration) => registration,
            Err(error) => {
                tracing::warn!(%error, "Folder proof preserved because cleanup admission failed");
                return;
            }
        };
        let coordinator = self.coordinator.clone();
        let folder = self.folder.clone();
        // Register before spawning. Shutdown observes queued cleanup, and all
        // capacity/closure/identity failures preserve the file without writing.
        runtime.spawn(async move {
            let _registration = registration;
            let result = match coordinator.acquire(folder).await {
                Ok(lease) => proof.clear(&lease),
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                tracing::warn!(%error, "Folder proof cleanup needs attention");
            }
        });
    }
}
