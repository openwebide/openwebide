//! Creation receipts distinguish an acknowledged or ambiguous publication from
//! an external same-content object. Files keep an anchor until final cleanup;
//! directories require a persistent birth identity. No user file is copied.
use super::super::rooted::revision::{Observation, Revision};
use super::*;

const LIMIT: u64 = METADATA_LIMIT;
mod claims;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Creation {
    ordinal: usize,
    directory: bool,
    object: Identity,
    /// Absent only in receipts written before native metadata ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<Revision>,
    /// Loaded from an immutable namespace intent beside the original receipt.
    #[serde(skip)]
    directory_changed: bool,
}
impl Creation {
    fn kind(&self) -> VfsEntryKind {
        if self.directory {
            VfsEntryKind::Directory
        } else {
            VfsEntryKind::File
        }
    }
    fn matches(&self, observed: &Observation, mut mutation: OriginalMutation) -> bool {
        // Publication itself moves the new object. Its ctime cannot remain the
        // creation ctime, even before any subsequent script move.
        mutation.moved = true;
        mutation.directory_changed |= self.directory_changed;
        observed.identity == self.object
            && self.revision.as_ref().is_none_or(|expected| {
                expected.has_attributes() && expected.matches(&observed.revision, mutation)
            })
    }
    fn matches_without_anchor(&self, observed: &Observation) -> bool {
        if self.directory {
            return self.matches(observed, OriginalMutation::default());
        }
        observed.identity == self.object
            && self.revision.as_ref().is_none_or(|expected| {
                expected.has_attributes()
                    && expected.matches_without_link(
                        &observed.revision,
                        OriginalMutation {
                            moved: true,
                            directory_changed: false,
                        },
                    )
            })
    }
}

impl ResourceFiles {
    fn namespace_header(&self, receipt: &Creation) -> io::Result<Vec<u8>> {
        if !receipt.directory {
            return Err(invalid("A namespace intent must identify a directory"));
        }
        let mut bytes = self.creation_header(receipt.ordinal)?;
        bytes.extend_from_slice(b"directory-namespace:1\n");
        bytes.extend_from_slice(receipt.object.fingerprint().as_bytes());
        bytes.push(b'\n');
        Ok(bytes)
    }
    fn creation_parent_changed(&self, area: &Area, parent: &Identity) -> io::Result<()> {
        // Creating an unpublished temporary changes its destination parent even
        // when publication never happens. Keep that fact with a created parent's
        // receipt before the syscall, so it survives abandoning the child step.
        if Identity::of(&self.root.0)? == *parent {
            return Ok(());
        }
        for entry in area
            .directory("receipts")?
            .list("", 6 * MAX_RESOURCE_STEPS)?
        {
            let Some(ordinal) = entry.name.strip_suffix(".json") else {
                continue;
            };
            let ordinal = ordinal.parse::<usize>().map_err(invalid)?;
            let receipt = self
                .receipt(area, ordinal)?
                .ok_or_else(|| invalid("Creation metadata disappeared"))?;
            if receipt.directory && receipt.object == *parent && !receipt.directory_changed {
                area::write_metadata(
                    &area.directory("receipts")?,
                    &format!("{ordinal}.namespace.part"),
                    &format!("{ordinal}.namespace"),
                    &self.namespace_header(&receipt)?,
                )?;
            }
        }
        Ok(())
    }
    fn creation_header(&self, ordinal: usize) -> io::Result<Vec<u8>> {
        if ordinal >= MAX_RESOURCE_STEPS {
            return Err(invalid("Invalid resource creation intent"));
        }
        Ok(format!(
            "openwebide-created:1\n{}\n{}\n{ordinal}\n",
            self.scope.transaction, self.scope.fingerprint
        )
        .into_bytes())
    }
    fn receipt(&self, area: &Area, ordinal: usize) -> io::Result<Option<Creation>> {
        let header = self.creation_header(ordinal)?;
        let receipts = area.directory("receipts")?;
        let path = format!("{ordinal}.json");
        if receipts.kind(&path)?.is_none() {
            return Ok(None);
        }
        let bytes = receipts.read(&path, LIMIT)?;
        let body = bytes
            .strip_prefix(header.as_slice())
            .ok_or_else(|| invalid("Creation receipt belongs to another intent"))?;
        let mut receipt: Creation = serde_json::from_slice(body).map_err(invalid)?;
        if receipt.ordinal != ordinal {
            return Err(invalid("Creation receipt changed its intent"));
        }
        let namespace = format!("{ordinal}.namespace");
        if receipts.kind(&namespace)?.is_some() {
            if receipts.read(&namespace, LIMIT)? != self.namespace_header(&receipt)? {
                return Err(invalid("Creation namespace intent changed its directory"));
            }
            receipt.directory_changed = true;
        }
        Ok(Some(receipt))
    }
    fn publish_receipt(&self, area: &Area, receipt: &Creation) -> io::Result<()> {
        let mut bytes = self.creation_header(receipt.ordinal)?;
        bytes.extend(serde_json::to_vec(receipt).map_err(invalid)?);
        area::write_metadata(
            &area.directory("receipts")?,
            &format!("{}.part", receipt.ordinal),
            &format!("{}.json", receipt.ordinal),
            &bytes,
        )
    }
    fn temporary_name(&self, ordinal: usize) -> io::Result<String> {
        self.creation_header(ordinal)?;
        Ok(format!(
            ".openwebide-create-{}-{ordinal}",
            self.scope.transaction
        ))
    }
    fn record_creation(
        &self,
        area: &Area,
        ordinal: usize,
        source: &Rooted,
        name: &str,
        kind: VfsEntryKind,
        checkpoint: &mut impl FnMut() -> io::Result<()>,
    ) -> io::Result<Creation> {
        let expected = if kind.is_dir() {
            FileType::Directory
        } else {
            FileType::RegularFile
        };
        if source.kind(name)? != Some(expected) || !source.is_empty(name, kind)? {
            return Err(invalid("An unpublished creation contains unreviewed data"));
        }
        let observed = Observation::of(&source.object(name)?)?;
        let object = observed.identity;
        if kind.is_dir() && !object.has_birth_time() {
            return Err(invalid(
                "This filesystem cannot persistently identify newly created directories",
            ));
        }
        let receipt = Creation {
            ordinal,
            directory: kind.is_dir(),
            object,
            directory_changed: false,
            // Persist before creating the anchor. Recovery can distinguish a
            // missing anchor from an interrupted publication without sampling
            // a fresh metadata baseline after the interruption.
            revision: Some(if kind.is_dir() {
                observed.revision
            } else {
                observed.revision.with_added_link()?
            }),
        };
        if let Some(previous) = self.receipt(area, ordinal)?
            && previous != receipt
        {
            return Err(invalid("The unpublished creation was replaced"));
        }
        self.publish_receipt(area, &receipt)?;
        checkpoint()?;
        if !kind.is_dir() {
            let anchors = area.directory("anchors")?;
            let anchor = ordinal.to_string();
            if anchors.kind(&anchor)?.is_none() {
                source.link(name, &anchors, &anchor)?;
            }
            if !receipt.matches(
                &Observation::of(&anchors.object(&anchor)?)?,
                OriginalMutation::default(),
            ) {
                return Err(invalid("A creation anchor belongs to another object"));
            }
            checkpoint()?;
        }
        Ok(receipt)
    }
    pub(super) fn create_owned(
        &self,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> io::Result<()> {
        self.create_owned_observed(ordinal, path, kind, || Ok(()))
    }
    pub(super) fn create_owned_observed(
        &self,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
        mut checkpoint: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        self.creation_header(ordinal)?;
        if self.root.kind(path)?.is_some() {
            if self.created_matches(ordinal, path, kind)? {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "A different object occupies the new resource path",
            ));
        }
        let (parent, leaf) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.root.directory(parent)?;
        let identity = Identity::of(&parent.0)?;
        if !Identity::of(&self.root.0)?.same_device(&identity) {
            return Err(invalid("Resource creation crosses a filesystem boundary"));
        }
        if kind.is_dir() && !identity.has_birth_time() {
            return Err(invalid(
                "This filesystem cannot persistently identify newly created directories",
            ));
        }
        let temporary = self.temporary_name(ordinal)?;
        if parent.kind(&temporary)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "An unowned entry collides with the creation staging name",
            ));
        }
        let area = self
            .area(true)?
            .ok_or_else(|| invalid("Missing creation recovery area"))?;
        let objects = area.directory("new")?;
        let receipts = area.directory("receipts")?;
        let name = ordinal.to_string();
        if objects.kind(&name)?.is_some() || area.directory("anchors")?.kind(&name)?.is_some() {
            return Err(invalid(
                "An earlier creation must be recovered before another publication",
            ));
        }
        for suffix in [
            "json",
            "part",
            "claim",
            "claim.part",
            "namespace",
            "namespace.part",
        ] {
            if receipts.kind(&format!("{ordinal}.{suffix}"))?.is_some() {
                return Err(invalid(
                    "An earlier creation must be recovered before another publication",
                ));
            }
        }
        self.publish_claim(&area, ordinal, path, kind, identity.clone())?;
        checkpoint()?;
        self.creation_parent_changed(&area, &identity)?;
        if let Err(error) = parent.create(&temporary, kind, false) {
            if error.kind() == io::ErrorKind::AlreadyExists {
                self.clear_claim(&area, ordinal)?;
            }
            return Err(error);
        }
        checkpoint()?;
        let receipt =
            self.record_creation(&area, ordinal, &parent, &temporary, kind, &mut checkpoint)?;
        parent.transfer(&temporary, &objects, &name, kind)?;
        checkpoint()?;
        if !receipt.matches(
            &Observation::of(&objects.object(&name)?)?,
            OriginalMutation::default(),
        ) {
            return Err(invalid("The staged creation was replaced"));
        }
        self.clear_claim(&area, ordinal)?;
        checkpoint()?;
        objects.transfer(&name, &parent, leaf, kind)?;
        checkpoint()?;
        if !self.created_matches(ordinal, path, kind)? {
            return Err(invalid(
                "Resource publication could not confirm its native identity",
            ));
        }
        Ok(())
    }
    pub(super) fn creation_matches(
        &self,
        ordinal: usize,
        observed: &Observation,
        mutation: OriginalMutation,
    ) -> io::Result<bool> {
        let Some(area) = self.area(false)? else {
            return Ok(false);
        };
        let Some(receipt) = self.receipt(&area, ordinal)? else {
            return Ok(false);
        };
        if !receipt.kind().is_dir() {
            let anchors = area.directory("anchors")?;
            let path = ordinal.to_string();
            if anchors.kind(&path)?.is_none() || anchors.identity(&path)? != receipt.object {
                return Ok(false);
            }
        }
        Ok(receipt.matches(observed, mutation))
    }
    pub(super) fn created_matches(
        &self,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> io::Result<bool> {
        let Some(area) = self.area(false)? else {
            return Ok(false);
        };
        let Some(receipt) = self.receipt(&area, ordinal)? else {
            return Ok(false);
        };
        if receipt.kind() != kind || self.root.kind(path)?.is_none() {
            return Ok(false);
        }
        // Namespace-sensitive comparison also runs in the shared interpreter,
        // with the exact prior child operations. This primitive additionally
        // protects attributes, ownership and mode immediately before removal.
        if !receipt.matches(
            &Observation::of(&self.root.object(path)?)?,
            OriginalMutation {
                moved: true,
                directory_changed: kind.is_dir(),
            },
        ) {
            return Ok(false);
        }
        if !kind.is_dir() {
            let anchors = area.directory("anchors")?;
            let name = ordinal.to_string();
            if anchors.kind(&name)?.is_none() || anchors.identity(&name)? != receipt.object {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(super) fn remove_created(
        &self,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> io::Result<()> {
        let area = self
            .area(false)?
            .ok_or_else(|| invalid("Missing creation receipt"))?;
        let receipt = self
            .receipt(&area, ordinal)?
            .ok_or_else(|| invalid("Missing creation receipt"))?;
        if receipt.kind() != kind {
            return Err(invalid("The created resource kind changed"));
        }
        let objects = area.directory("new")?;
        let name = ordinal.to_string();
        if objects.kind(&name)?.is_none() {
            if !self.created_matches(ordinal, path, kind)? {
                return Err(invalid("A later object replaced the created resource"));
            }
            self.root.transfer(path, &objects, &name, kind)?;
        }
        if !receipt.matches(
            &Observation::of(&objects.object(&name)?)?,
            OriginalMutation {
                moved: true,
                directory_changed: kind.is_dir(),
            },
        ) || !objects.is_empty(&name, kind)?
        {
            // Preserve the unexpected object, preferably at its original path.
            if self.root.kind(path)?.is_none() {
                let _ = objects.transfer(&name, &self.root, path, kind);
            }
            return Err(invalid(
                "Later resource contents were preserved instead of removed",
            ));
        }
        objects.remove(&name, kind)
    }
    pub(super) fn removed_matches(
        &self,
        ordinal: usize,
        _path: &str,
        kind: VfsEntryKind,
    ) -> io::Result<bool> {
        let Some(area) = self.area(false)? else {
            return Ok(false);
        };
        let Some(receipt) = self.receipt(&area, ordinal)? else {
            return Ok(false);
        };
        if receipt.kind() != kind {
            return Ok(false);
        }
        let objects = area.directory("new")?;
        let name = ordinal.to_string();
        if objects.kind(&name)?.is_none() {
            return Ok(true);
        }
        Ok(receipt.matches(
            &Observation::of(&objects.object(&name)?)?,
            OriginalMutation {
                moved: true,
                directory_changed: kind.is_dir(),
            },
        ) && objects.is_empty(&name, kind)?)
    }
    pub(super) fn finish_creations(&self) -> io::Result<()> {
        let Some(area) = self.area(false)? else {
            return Ok(());
        };
        let folders = [
            area.optional_directory("receipts")?,
            area.optional_directory("new")?,
            area.optional_directory("anchors")?,
        ];
        if folders.iter().any(Option::is_none) {
            // Control folders are removed only after all their contents. A
            // restarted finalization may see any empty suffix of that cleanup.
            for folder in folders.iter().flatten() {
                if !folder.list("", 1)?.is_empty() {
                    return Err(invalid(
                        "An incomplete recovery area still contains creation payloads",
                    ));
                }
            }
            return Ok(());
        }
        let [Some(receipts), Some(objects), Some(anchors)] = folders else {
            unreachable!()
        };
        let mut ordinals = std::collections::BTreeSet::new();
        for entry in receipts.list("", 6 * MAX_RESOURCE_STEPS)? {
            let (stem, suffix) = entry
                .name
                .split_once('.')
                .ok_or_else(|| invalid("Unknown creation metadata was preserved"))?;
            if !matches!(
                suffix,
                "json" | "part" | "claim" | "claim.part" | "namespace" | "namespace.part"
            ) {
                return Err(invalid("Unknown creation metadata was preserved"));
            }
            let ordinal = stem.parse::<usize>().map_err(invalid)?;
            self.creation_header(ordinal)?;
            if ordinal.to_string() != stem {
                return Err(invalid("Invalid creation metadata name"));
            }
            ordinals.insert(ordinal);
        }
        for root in [&objects, &anchors] {
            for entry in root.list("", MAX_RESOURCE_STEPS)? {
                let ordinal = entry.name.parse::<usize>().map_err(invalid)?;
                self.creation_header(ordinal)?;
                if ordinal.to_string() != entry.name {
                    return Err(invalid("Unknown staged creation was preserved"));
                }
                ordinals.insert(ordinal);
            }
        }
        for ordinal in ordinals {
            let name = ordinal.to_string();
            if receipts.kind(&format!("{ordinal}.claim"))?.is_some() {
                return Err(invalid("An unpublished creation still needs recovery"));
            }
            let receipt = self.receipt(&area, ordinal)?;
            let namespace = format!("{ordinal}.namespace");
            let namespace_part = format!("{ordinal}.namespace.part");
            if receipts.kind(&namespace_part)?.is_some() {
                let expected = self.namespace_header(
                    receipt
                        .as_ref()
                        .ok_or_else(|| invalid("A creation namespace intent has no receipt"))?,
                )?;
                if !expected.starts_with(&receipts.read(&namespace_part, LIMIT)?) {
                    return Err(invalid("Unknown creation namespace metadata was preserved"));
                }
            }
            if objects.kind(&name)?.is_some() {
                let kind = match objects.kind(&name)? {
                    Some(FileType::Directory) => VfsEntryKind::Directory,
                    Some(FileType::RegularFile) => VfsEntryKind::File,
                    _ => return Err(invalid("A staged resource became a link or special entry")),
                };
                let expected = receipt
                    .as_ref()
                    .ok_or_else(|| invalid("An unknown staged resource was preserved"))?;
                if expected.kind() != kind
                    || !expected.matches(
                        &Observation::of(&objects.object(&name)?)?,
                        OriginalMutation {
                            moved: true,
                            directory_changed: kind.is_dir(),
                        },
                    )
                    || !objects.is_empty(&name, kind)?
                {
                    return Err(invalid("Unexpected staged resource data was preserved"));
                }
                objects.remove(&name, kind)?;
            }
            if anchors.kind(&name)?.is_some() {
                use std::os::unix::fs::MetadataExt;
                let file = anchors.regular(&name)?;
                let expected = receipt
                    .as_ref()
                    .ok_or_else(|| invalid("An unknown creation anchor was preserved"))?;
                if expected.directory
                    || Identity::of(&file)? != expected.object
                    || (file.metadata()?.nlink() == 1
                        && (file.metadata()?.len() != 0
                            || !expected.matches_without_anchor(&Observation::of(&file)?)))
                {
                    return Err(invalid("Unexpected creation anchor data was preserved"));
                }
                anchors.remove(&name, VfsEntryKind::File)?;
            }
            let part = format!("{ordinal}.part");
            if receipts.kind(&part)?.is_some() {
                let bytes = receipts.read(&part, LIMIT)?;
                let header = self.creation_header(ordinal)?;
                if !header.starts_with(&bytes) {
                    let body = bytes.strip_prefix(header.as_slice()).ok_or_else(|| {
                        invalid("Unknown partial creation metadata was preserved")
                    })?;
                    match serde_json::from_slice::<Creation>(body) {
                        Ok(receipt) if receipt.ordinal == ordinal => {}
                        Err(error) if error.is_eof() => {}
                        _ => {
                            return Err(invalid("Unknown partial creation metadata was preserved"));
                        }
                    }
                }
                receipts.remove(&part, VfsEntryKind::File)?;
            }
            let part = format!("{ordinal}.claim.part");
            if receipts.kind(&part)?.is_some() {
                self.validate_partial_claim(ordinal, &receipts.read(&part, LIMIT)?)?;
                receipts.remove(&part, VfsEntryKind::File)?;
            }
            let json = format!("{ordinal}.json");
            for name in [&namespace_part, &namespace] {
                if receipts.kind(name)?.is_some() {
                    if receipt.is_none() {
                        return Err(invalid("A creation namespace intent has no receipt"));
                    }
                    receipts.remove(name, VfsEntryKind::File)?;
                }
            }
            if receipts.kind(&json)?.is_some() {
                receipts.remove(&json, VfsEntryKind::File)?;
            }
        }
        Ok(())
    }
}
