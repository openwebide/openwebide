//! Thin descriptor-bound primitives for the shared resource journal. The core
//! owns operation order, acknowledgement, rollback and the durable decision.
use super::rooted::{Identity, Rooted};
use openwebide_core::{
    FileEntry,
    vfs::{self, VfsEntryKind},
    workspace_entries::WorkspaceEntries,
    workspace_transaction::{
        self, MAX_ENTRIES, MAX_RESOURCE_STEPS, OriginalMutation, ResourceLocation, ResourceScope,
        TransactionFiles,
    },
};
use rustix::fs::FileType;
use serde::{Deserialize, Serialize};
use std::{fs::File, io};

mod area;
mod creations;
use area::Area;

const METADATA_LIMIT: u64 = 16 * 1024;

pub(crate) struct ResourceFiles {
    root: Rooted,
    identity: String,
    scope: ResourceScope,
    review: Option<openwebide_core::plugins::languages::intelligence::AnalysisContext>,
}
fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn original_revision(
    identity: &str,
    value: &str,
) -> Result<super::rooted::revision::Revision, String> {
    if !identity.starts_with("u1.") {
        return Err(
            "This filesystem cannot persistently identify reviewed original objects".into(),
        );
    }
    let revision = super::rooted::revision::Revision::decode(value).map_err(text)?;
    if !revision.has_attributes() {
        return Err("Original native attributes are unavailable within the current access or resource limits".into());
    }
    Ok(revision)
}
fn workspace(path: &str) -> Result<String, String> {
    let path = vfs::workspace_path(path).map_err(text)?;
    if path
        .split('/')
        .any(|part| part.eq_ignore_ascii_case(".openwebide"))
    {
        return Err("Resource actions cannot move runtime recovery or backup directories".into());
    }
    Ok(path)
}

impl ResourceFiles {
    pub fn new(root: &File, identity: &str, scope: ResourceScope) -> Result<Self, String> {
        if !root.metadata().map_err(text)?.is_dir()
            || identity.is_empty()
            || identity.chars().any(char::is_control)
            || !workspace_transaction::valid_id(&scope.transaction)
            || scope.fingerprint.len() != 64
            || !scope
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Invalid captured resource filesystem scope".into());
        }
        Ok(Self {
            root: Rooted(root.try_clone().map_err(text)?),
            identity: identity.into(),
            scope,
            review: None,
        })
    }
    pub fn reviewed(
        root: &File,
        identity: &str,
        scope: ResourceScope,
        review: Option<&workspace_transaction::LanguageReview>,
        plan: &workspace_transaction::WorkspacePlan,
    ) -> Result<Self, String> {
        let mut files = Self::new(root, identity, scope)?;
        if let Some(review) = review
            && review.input.version >= 3
        {
            review.context.validate()?;
            if review.context.identities.len() != review.context.files.len()
                || review.context.revisions.len() != review.context.files.len()
            {
                return Err("The native review is missing original object identities".into());
            }
            // Text/command-only plans need no native resource capability. Keep
            // their review strict: an unexpected original-resource access still
            // requires the corresponding authenticated object observation.
            if !plan.operations.is_empty() {
                if review.context.files.first().map(String::as_str) != Some("")
                    || review
                        .context
                        .identities
                        .first()
                        .is_none_or(|identity| !identity.starts_with("u1."))
                {
                    return Err(
                        "This filesystem cannot persistently identify reviewed original objects"
                            .into(),
                    );
                }
                for (identity, revision) in review
                    .context
                    .identities
                    .iter()
                    .zip(&review.context.revisions)
                {
                    if !identity.is_empty() {
                        original_revision(identity, revision)?;
                    }
                }
            }
            files.review = Some(review.context.clone());
        }
        Ok(files)
    }
    fn location_observation(
        &self,
        location: &ResourceLocation,
        suffix: &str,
    ) -> Result<super::rooted::revision::Observation, String> {
        let file = match location {
            ResourceLocation::Workspace(path) => {
                let path = workspace(&format!("{path}{suffix}"))?;
                if path.is_empty() {
                    self.root.0.try_clone().map_err(text)?
                } else {
                    self.root.object(&path).map_err(text)?
                }
            }
            ResourceLocation::Retained(slot) => {
                let path = format!("{slot}{suffix}");
                Self::retained_path(&path).map_err(text)?;
                self.retained()
                    .map_err(text)?
                    .ok_or("The original recovery area is unavailable")?
                    .object(&path)
                    .map_err(text)?
            }
        };
        super::rooted::revision::Observation::of(&file).map_err(text)
    }
    fn authorize(&self, scope: &ResourceScope) -> io::Result<()> {
        if scope != &self.scope {
            return Err(invalid(
                "Recovery objects belong to another reviewed transaction",
            ));
        }
        Ok(())
    }
    fn area(&self, create: bool) -> io::Result<Option<Area>> {
        Area::open(&self.root, &self.identity, &self.scope, create)
    }
    fn retained(&self) -> io::Result<Option<Rooted>> {
        match self.area(false)? {
            Some(area) => area.optional_directory("objects"),
            None => Ok(None),
        }
    }
    fn retained_path(path: &str) -> io::Result<&str> {
        let mut parts = path.split('/');
        let slot = parts.next().unwrap_or("");
        if !path.is_empty()
            && (slot.parse::<usize>().is_err()
                || slot.len() > 3
                || slot.starts_with('0') && slot != "0")
        {
            return Err(invalid("Invalid retained object slot"));
        }
        if slot
            .parse::<usize>()
            .is_ok_and(|slot| slot >= workspace_transaction::MAX_OPERATIONS)
        {
            return Err(invalid("Invalid retained object slot"));
        }
        if vfs::normalize_vfs_path(path)
            .map_err(text)
            .map_err(invalid)?
            != path
        {
            return Err(invalid("Invalid retained object path"));
        }
        Ok(path)
    }
}

impl WorkspaceEntries for ResourceFiles {
    async fn canonicalize(&self, path: &str) -> Result<String, String> {
        let path = vfs::workspace_path(path).map_err(text)?;
        if path.is_empty() {
            return Ok(path);
        }
        match self.root.kind(&path).map_err(text)? {
            None | Some(FileType::Directory | FileType::RegularFile) => Ok(path),
            _ => {
                Err("Resource operations do not follow links or special filesystem objects".into())
            }
        }
    }
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        self.root
            .list(&vfs::workspace_path(path).map_err(text)?, MAX_ENTRIES)
            .map_err(text)
    }
    async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, String> {
        self.root
            .read(
                &vfs::workspace_path(path).map_err(text)?,
                vfs::MAX_READ_BYTES,
            )
            .map_err(text)
    }
    async fn create(&self, _path: &str, _kind: VfsEntryKind) -> Result<(), String> {
        Err("Native resource creation needs its journal intent".into())
    }
    async fn write_bytes(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Native resource originals cannot be rewritten".into())
    }
    async fn delete(&self, _path: &str) -> Result<(), String> {
        Err("Native resource removal needs its journal intent".into())
    }
}

impl TransactionFiles for ResourceFiles {
    fn root_identity(&self) -> &str {
        &self.identity
    }
    fn supports_resource_moves(&self) -> bool {
        self.area(false).is_ok()
    }
    async fn original_resource_matches(
        &self,
        scope: &ResourceScope,
        location: &ResourceLocation,
        suffix: &str,
        original: &str,
        mutation: OriginalMutation,
    ) -> Result<bool, String> {
        self.authorize(scope).map_err(text)?;
        let Some(review) = &self.review else {
            return Ok(true);
        };
        let index = review
            .files
            .binary_search_by(|path| path.as_str().cmp(original))
            .map_err(|_| "The native review omits an original object")?;
        let expected = review
            .identities
            .get(index)
            .filter(|identity| !identity.is_empty())
            .ok_or("The native review omits original object identity")?;
        let revision = review
            .revisions
            .get(index)
            .ok_or("The native review omits original metadata")?;
        let expected_revision = original_revision(expected, revision)?;
        let actual = self.location_observation(location, suffix)?;
        Ok(actual.identity.fingerprint() == *expected
            && expected_revision.matches(&actual.revision, mutation))
    }
    async fn created_resource_matches_at(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        location: &ResourceLocation,
        suffix: &str,
        mutation: OriginalMutation,
    ) -> Result<bool, String> {
        self.authorize(scope).map_err(text)?;
        let observed = self.location_observation(location, suffix)?;
        self.creation_matches(ordinal, &observed, mutation)
            .map_err(text)
    }
    async fn replace_atomic(&self, _path: &str, _bytes: &[u8]) -> Result<(), String> {
        Err("Resource recovery never reconstructs native objects from bytes".into())
    }
    async fn remove_entry(&self, _path: &str, _kind: VfsEntryKind) -> Result<(), String> {
        Err("Native resource removal needs its creation receipt".into())
    }
    async fn create_resource(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.authorize(scope).map_err(text)?;
        self.create_owned(ordinal, &workspace(path)?, kind)
            .map_err(text)
    }
    async fn created_resource_matches(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<bool, String> {
        self.authorize(scope).map_err(text)?;
        self.created_matches(ordinal, &workspace(path)?, kind)
            .map_err(text)
    }
    async fn reconcile_resource_creation(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.authorize(scope).map_err(text)?;
        self.reconcile_creation(ordinal, &workspace(path)?, kind)
            .map_err(text)
    }
    async fn remove_created_resource(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.authorize(scope).map_err(text)?;
        self.remove_created(ordinal, &workspace(path)?, kind)
            .map_err(text)
    }
    async fn removed_resource_matches(
        &self,
        scope: &ResourceScope,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<bool, String> {
        self.authorize(scope).map_err(text)?;
        self.removed_matches(ordinal, &workspace(path)?, kind)
            .map_err(text)
    }
    async fn retained_canonicalize(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<String, String> {
        self.authorize(scope).map_err(text)?;
        Self::retained_path(path).map_err(text)?;
        if let Some(root) = self.retained().map_err(text)?
            && !path.is_empty()
            && !matches!(
                root.kind(path).map_err(text)?,
                None | Some(FileType::Directory | FileType::RegularFile)
            )
        {
            return Err("Retained objects contain a link or special entry".into());
        }
        Ok(path.into())
    }
    async fn retained_list(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<FileEntry>, String> {
        self.authorize(scope).map_err(text)?;
        Self::retained_path(path).map_err(text)?;
        match self.retained().map_err(text)? {
            Some(root) => root.list(path, MAX_ENTRIES).map_err(text),
            None => Ok(vec![]),
        }
    }
    async fn retained_read_bytes(
        &self,
        scope: &ResourceScope,
        path: &str,
    ) -> Result<Vec<u8>, String> {
        self.authorize(scope).map_err(text)?;
        Self::retained_path(path).map_err(text)?;
        self.retained()
            .map_err(text)?
            .ok_or("The original recovery area is unavailable")?
            .read(path, vfs::MAX_READ_BYTES)
            .map_err(text)
    }
    async fn transfer_resource(
        &self,
        scope: &ResourceScope,
        from: &ResourceLocation,
        to: &ResourceLocation,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.authorize(scope).map_err(text)?;
        let area = if matches!(from, ResourceLocation::Retained(_))
            || matches!(to, ResourceLocation::Retained(_))
        {
            Some(
                self.area(matches!(to, ResourceLocation::Retained(_)))
                    .map_err(text)?
                    .ok_or("The original recovery area is unavailable")?,
            )
        } else {
            None
        };
        let objects = area
            .as_ref()
            .map(|area| area.directory("objects"))
            .transpose()
            .map_err(text)?;
        let resolve = |location: &ResourceLocation| -> Result<(&Rooted, String), String> {
            match location {
                ResourceLocation::Workspace(path) => Ok((&self.root, workspace(path)?)),
                ResourceLocation::Retained(slot) => {
                    let path = slot.to_string();
                    Self::retained_path(&path).map_err(text)?;
                    Ok((
                        objects
                            .as_ref()
                            .ok_or("Missing retained object capability")?,
                        path,
                    ))
                }
            }
        };
        let (source, path) = resolve(from)?;
        let (destination, target) = resolve(to)?;
        source
            .transfer(&path, destination, &target, kind)
            .map_err(text)
    }
    async fn remove_retained_entry(
        &self,
        scope: &ResourceScope,
        path: &str,
        kind: VfsEntryKind,
    ) -> Result<(), String> {
        self.authorize(scope).map_err(text)?;
        Self::retained_path(path).map_err(text)?;
        self.retained()
            .map_err(text)?
            .ok_or("The original recovery area is unavailable")?
            .remove(path, kind)
            .map_err(text)
    }
    async fn finish_retained(&self, scope: &ResourceScope) -> Result<(), String> {
        self.authorize(scope).map_err(text)?;
        self.finish_creations().map_err(text)?;
        Area::finish(&self.root, &self.identity, &self.scope).map_err(text)
    }
}

#[cfg(test)]
mod tests;
