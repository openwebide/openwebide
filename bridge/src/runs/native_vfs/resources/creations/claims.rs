//! A durable claim precedes exclusive creation in the destination parent.
//! Recovery discards only that claim's empty, unpublished artifact.
use super::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    #[serde(default = "legacy_version")]
    version: u32,
    ordinal: usize,
    path: String,
    directory: bool,
    parent: Identity,
}
fn legacy_version() -> u32 {
    1
}
impl Claim {
    fn validate(&self, ordinal: usize) -> io::Result<()> {
        if !matches!(self.version, 1 | 2)
            || self.ordinal != ordinal
            || self.path.is_empty()
            || self.path.len() > 4096
            || self.path.chars().any(char::is_control)
            || workspace(&self.path).map_err(invalid)? != self.path
        {
            return Err(invalid("Invalid resource creation ownership record"));
        }
        Ok(())
    }
    fn kind(&self) -> VfsEntryKind {
        if self.directory {
            VfsEntryKind::Directory
        } else {
            VfsEntryKind::File
        }
    }
}
impl ResourceFiles {
    fn claim_header(&self, ordinal: usize) -> io::Result<Vec<u8>> {
        self.creation_header(ordinal)?;
        Ok(format!(
            "openwebide-creation-claim:1\n{}\n{}\n{ordinal}\n",
            self.scope.transaction, self.scope.fingerprint
        )
        .into_bytes())
    }
    fn claim(&self, area: &Area, ordinal: usize) -> io::Result<Option<Claim>> {
        let header = self.claim_header(ordinal)?;
        let receipts = area.directory("receipts")?;
        let name = format!("{ordinal}.claim");
        if receipts.kind(&name)?.is_none() {
            return Ok(None);
        }
        let bytes = receipts.read(&name, LIMIT)?;
        let body = bytes
            .strip_prefix(header.as_slice())
            .ok_or_else(|| invalid("Creation ownership belongs to another intent"))?;
        let claim: Claim = serde_json::from_slice(body).map_err(invalid)?;
        claim.validate(ordinal)?;
        Ok(Some(claim))
    }
    pub(super) fn publish_claim(
        &self,
        area: &Area,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
        parent: Identity,
    ) -> io::Result<()> {
        let claim = Claim {
            version: 2,
            ordinal,
            path: path.into(),
            directory: kind.is_dir(),
            parent,
        };
        claim.validate(ordinal)?;
        let mut bytes = self.claim_header(ordinal)?;
        bytes.extend(serde_json::to_vec(&claim).map_err(invalid)?);
        area::write_metadata(
            &area.directory("receipts")?,
            &format!("{ordinal}.claim.part"),
            &format!("{ordinal}.claim"),
            &bytes,
        )
    }
    pub(super) fn clear_claim(&self, area: &Area, ordinal: usize) -> io::Result<()> {
        let receipts = area.directory("receipts")?;
        let name = format!("{ordinal}.claim");
        if self.claim(area, ordinal)?.is_some() {
            area::write_metadata(
                &receipts,
                &format!("{ordinal}.claim.part"),
                &name,
                &receipts.read(&name, LIMIT)?,
            )?;
            receipts.remove(&name, VfsEntryKind::File)?;
        }
        Ok(())
    }
    pub(super) fn validate_partial_claim(&self, ordinal: usize, bytes: &[u8]) -> io::Result<()> {
        let header = self.claim_header(ordinal)?;
        if header.starts_with(bytes) {
            return Ok(());
        }
        let body = bytes
            .strip_prefix(header.as_slice())
            .ok_or_else(|| invalid("Unknown partial creation ownership was preserved"))?;
        match serde_json::from_slice::<Claim>(body) {
            Ok(claim) => claim.validate(ordinal),
            Err(error) if error.is_eof() => Ok(()),
            Err(error) => Err(invalid(error)),
        }
    }
    pub(in super::super) fn reconcile_creation(
        &self,
        ordinal: usize,
        path: &str,
        kind: VfsEntryKind,
    ) -> io::Result<()> {
        let Some(area) = self.area(false)? else {
            return Ok(());
        };
        let Some(claim) = self.claim(&area, ordinal)? else {
            return Ok(());
        };
        if claim.path != path || claim.kind() != kind {
            return Err(invalid(
                "The creation ownership record changed its destination",
            ));
        }
        let (parent, _) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.root.directory(parent)?;
        if Identity::of(&parent.0)? != claim.parent {
            return Err(invalid("The original creation parent was replaced"));
        }
        let temporary = self.temporary_name(ordinal)?;
        let objects = area.directory("new")?;
        let name = ordinal.to_string();
        if parent.kind(&temporary)?.is_some() {
            if objects.kind(&name)?.is_some() {
                return Err(invalid("Conflicting unpublished creations were preserved"));
            }
            let receipt = self.receipt(&area, ordinal)?;
            if claim.version >= 2
                && receipt
                    .as_ref()
                    .is_none_or(|receipt| receipt.revision.is_none())
            {
                // Exclusive creation and a later metadata sample cannot be one
                // atomic syscall. A restart may not manufacture the missing
                // historical witness from the current, possibly changed object.
                return Err(invalid(format!(
                    "The unpublished object has no durable metadata receipt and was preserved for inspection: {}",
                    if claim.path.contains('/') {
                        format!("{}/{}", claim.path.rsplit_once('/').unwrap().0, temporary)
                    } else {
                        temporary.clone()
                    }
                )));
            }
            let expected_kind = if kind.is_dir() {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            if parent.kind(&temporary)? != Some(expected_kind)
                || !parent.is_empty(&temporary, kind)?
            {
                return Err(invalid("Unpublished creation contents were preserved"));
            }
            let object = parent.identity(&temporary)?;
            if receipt
                .as_ref()
                .is_some_and(|receipt| receipt.kind() != kind || receipt.object != object)
            {
                return Err(invalid("The unpublished creation was replaced"));
            }
            let mut anchored = false;
            if !kind.is_dir() {
                let anchors = area.directory("anchors")?;
                if anchors.kind(&name)?.is_some() {
                    anchored = true;
                    if anchors.identity(&name)? != object {
                        return Err(invalid("An unrelated creation anchor was preserved"));
                    }
                    if receipt.is_none() {
                        self.publish_receipt(
                            &area,
                            &Creation {
                                ordinal,
                                directory: false,
                                object,
                                revision: None,
                                directory_changed: false,
                            },
                        )?;
                    }
                } else if receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.revision.is_none())
                {
                    return Err(invalid("The original creation anchor is unavailable"));
                }
            }
            if let Some(receipt) = &receipt {
                let observed = Observation::of(&parent.object(&temporary)?)?;
                let matches = if anchored || kind.is_dir() {
                    receipt.matches(&observed, OriginalMutation::default())
                } else {
                    receipt.matches_without_anchor(&observed)
                };
                if !matches {
                    return Err(invalid("Unpublished creation metadata was preserved"));
                }
            }
            // A failed create may be precisely a filesystem without hard links
            // or cross-mount rename support. Its recovery must not require a new
            // link or move. The unpublished object is empty and claim-owned.
            parent.remove(&temporary, kind)?;
        }
        if objects.kind(&name)?.is_some() {
            let receipt = self
                .receipt(&area, ordinal)?
                .ok_or_else(|| invalid("An unpublished creation has no receipt"))?;
            if receipt.kind() != kind
                || !receipt.matches(
                    &Observation::of(&objects.object(&name)?)?,
                    OriginalMutation::default(),
                )
                || !objects.is_empty(&name, kind)?
            {
                return Err(invalid(
                    "Unexpected unpublished creation contents were preserved",
                ));
            }
        }
        self.clear_claim(&area, ordinal)
    }
}
