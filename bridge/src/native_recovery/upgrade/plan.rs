use super::*;
use storage::{Area, ORIGINAL_ANCHOR, ORIGINAL_DIRECTORY};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub format: u32,
    pub id: String,
    pub expected: String,
    pub note: String,
    pub recorded_at: u64,
    pub legacy: Snapshot,
    pub device_changed: bool,
    pub area: Identity,
    pub generation: Identity,
    pub pointer: Identity,
    pub replacement: Key,
    pub initial: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pointer {
    pub id: String,
    pub digest: String,
    pub manifest: Identity,
}

pub(super) struct Loaded {
    pub plan: Plan,
    pub generation: Dir,
    pub manifest_digest: String,
}

pub(super) struct Original {
    pub anchor: File,
    pub directory: Dir,
    pub anchor_current: bool,
    pub directory_current: bool,
    pub backed_up: bool,
}

pub(super) fn note_valid(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096 && !value.contains('\0')
}

impl Loaded {
    pub fn load(home: &Folder, area: &Area, parent: &Dir, name: &str) -> Result<Self, String> {
        let uid = io(home.handle.as_file().metadata())?.uid();
        let marker = storage::linked_file(parent, name, uid)?;
        let pointer: Pointer = storage::read(&marker)?;
        let generation = area.generation(&pointer.id, uid)?;
        let manifest = child(&generation, storage::PLAN, uid)?;
        if !pointer
            .manifest
            .matches(&manifest)
            .map_err(|_| UPGRADE_INVALID)?
            || digest(&bytes(&manifest, storage::MAX_PLAN)?) != pointer.digest
        {
            return Err(UPGRADE_INVALID.into());
        }
        let plan: Plan = storage::read(&manifest)?;
        let mut known_links = 0;
        let mut archived_links = 0;
        for (selected, leaf) in [
            (&area.dir, storage::CURRENT),
            (&generation, storage::POINTER),
            (&generation, storage::COMPLETE),
            (&generation, storage::CANCELLED),
        ] {
            if storage::exists(selected, leaf)? {
                let file = storage::linked_file(selected, leaf, uid)?;
                if !plan.pointer.matches(&file).map_err(|_| UPGRADE_INVALID)? {
                    return Err(UPGRADE_INVALID.into());
                }
                known_links += 1;
                if matches!(leaf, storage::COMPLETE | storage::CANCELLED) {
                    archived_links += 1;
                }
            }
        }
        if io(marker.metadata())?.nlink() != known_links
            || archived_links > 1
            || (storage::exists(&generation, storage::POINTER)? && known_links != 1)
        {
            return Err(UPGRADE_INVALID.into());
        }
        let observations = &plan.legacy.observations;
        let mut names = BTreeSet::from([
            "home".to_owned(),
            ANCHOR.to_owned(),
            DIRECTORY.to_owned(),
            DATABASE.to_owned(),
        ]);
        let mut files = BTreeSet::from([ANCHOR.to_owned(), DATABASE.to_owned()]);
        if plan.legacy.revision > 0 {
            names.insert(persistence::HEAD.into());
            files.insert(persistence::HEAD.into());
        }
        if plan.format != 1
            || plan.id != pointer.id
            || plan.legacy.expected()? != plan.expected
            || !note_valid(&plan.note)
            || plan.recorded_at == 0
            || plan.legacy.home != home.path.as_os_str().as_bytes()
            || observations.keys().cloned().collect::<BTreeSet<_>>() != names
            || plan.legacy.hashes.keys().cloned().collect::<BTreeSet<_>>() != files
            || plan
                .legacy
                .records
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != BTreeSet::from(RECORDS)
            || plan.legacy.records.values().any(|count| *count != 0)
            || !matches!(plan.legacy.schema, 1..=12)
            || (plan.legacy.revision == 0) != (plan.legacy.schema == 1)
            || plan
                .legacy
                .hashes
                .values()
                .any(|value| !openwebide_core::workspace_transaction::companion::fingerprint(value))
            || observations.values().any(|identity| {
                !identity.valid()
                    || !identity.has_birth_time()
                    || !identity.has_persistent_volume()
                    || !identity.same_device(&observations["home"])
            })
            || !observations["home"]
                .matches(home.handle.as_file())
                .map_err(|_| UPGRADE_INVALID)?
            || !plan.pointer.matches(&marker).map_err(|_| UPGRADE_INVALID)?
            || !plan
                .area
                .matches(&io(area.dir.try_clone())?.into_std_file())
                .map_err(|_| UPGRADE_INVALID)?
            || !plan
                .generation
                .matches(&io(generation.try_clone())?.into_std_file())
                .map_err(|_| UPGRADE_INVALID)?
            || plan.replacement.uid != uid
            || plan.replacement.home != observations["home"]
            || !openwebide_core::workspace_transaction::valid_id(&plan.replacement.nonce)
            || plan
                .initial
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != BTreeSet::from([ANCHOR, DATABASE, persistence::HEAD])
            || plan
                .initial
                .values()
                .any(|value| !openwebide_core::workspace_transaction::companion::fingerprint(value))
            || [
                &plan.replacement.anchor,
                &plan.replacement.directory,
                &plan.pointer,
                &plan.generation,
                &plan.area,
            ]
            .iter()
            .any(|identity| {
                !identity.valid()
                    || !identity.has_birth_time()
                    || !identity.has_persistent_volume()
                    || !identity.same_device(&observations["home"])
            })
            || plan.replacement.anchor == observations[ANCHOR]
            || plan.replacement.directory == observations[DIRECTORY]
            || !home.current()
        {
            return Err(UPGRADE_INVALID.into());
        }
        Ok(Self {
            plan,
            generation,
            manifest_digest: pointer.digest,
        })
    }
    pub fn require_intent(&self, home: &Folder, area: &Area) -> Result<(), String> {
        let current = Self::load(home, area, &area.dir, storage::CURRENT)?;
        if current.plan.id != self.plan.id || current.manifest_digest != self.manifest_digest {
            return Err(UPGRADE_INVALID.into());
        }
        Ok(())
    }

    pub fn original(
        &self,
        home: &Folder,
        parent: &Dir,
        exclusive: Option<bool>,
    ) -> Result<Original, String> {
        let uid = self.plan.replacement.uid;
        let expected = &self.plan.legacy.observations;
        let anchor_current = storage::matches(parent, ANCHOR, &expected[ANCHOR])?;
        let backed_up = storage::exists(&self.generation, ORIGINAL_ANCHOR)?;
        if !anchor_current && !backed_up {
            return Err(UPGRADE_INVALID.into());
        }
        let anchor = storage::anchor(
            if anchor_current {
                parent
            } else {
                &self.generation
            },
            if anchor_current {
                ANCHOR
            } else {
                ORIGINAL_ANCHOR
            },
            uid,
        )?;
        if let Some(exclusive) = exclusive {
            lock(&anchor, exclusive)?;
        }
        if !expected[ANCHOR]
            .matches(&anchor)
            .map_err(|_| UPGRADE_INVALID)?
            || io(anchor.metadata())?.nlink() != if anchor_current && backed_up { 2 } else { 1 }
            || (backed_up
                && !storage::matches(&self.generation, ORIGINAL_ANCHOR, &expected[ANCHOR])?)
        {
            return Err(UPGRADE_INVALID.into());
        }
        let directory_current = storage::matches(parent, DIRECTORY, &expected[DIRECTORY])?;
        let directory_backed_up = storage::exists(&self.generation, ORIGINAL_DIRECTORY)?;
        if directory_current == directory_backed_up {
            return Err(UPGRADE_INVALID.into());
        }
        let directory = storage::directory(
            if directory_current {
                parent
            } else {
                &self.generation
            },
            if directory_current {
                DIRECTORY
            } else {
                ORIGINAL_DIRECTORY
            },
            uid,
        )?;
        if !expected[DIRECTORY]
            .matches(&io(directory.try_clone())?.into_std_file())
            .map_err(|_| UPGRADE_INVALID)?
            || !expected["home"]
                .matches(home.handle.as_file())
                .map_err(|_| UPGRADE_INVALID)?
            || !home.current()
        {
            return Err(UPGRADE_INVALID.into());
        }
        Ok(Original {
            anchor,
            directory,
            anchor_current,
            directory_current,
            backed_up,
        })
    }

    pub fn validate_original(&self, original: &Original) -> Result<(), String> {
        let snapshot = &self.plan.legacy;
        let mut expected_names = snapshot.hashes.keys().cloned().collect::<BTreeSet<_>>();
        expected_names.remove(ANCHOR);
        if inventory(&original.directory)? != expected_names
            || digest(&bytes(&original.anchor, MAX_ANCHOR_BYTES)?) != snapshot.hashes[ANCHOR]
        {
            return Err(UPGRADE_INVALID.into());
        }
        for (name, expected) in &snapshot.hashes {
            if name == ANCHOR {
                continue;
            }
            let file = child(&original.directory, name, self.plan.replacement.uid)?;
            if !snapshot.observations[name]
                .matches(&file)
                .map_err(|_| UPGRADE_INVALID)?
                || digest(&bytes(&file, MAX_DATABASE_BYTES)?) != *expected
            {
                return Err(UPGRADE_INVALID.into());
            }
        }
        let anchor = anchor_record(&original.anchor)?;
        let data = bytes(
            &child(&original.directory, DATABASE, anchor.key.uid)?,
            MAX_DATABASE_BYTES,
        )?;
        let (connection, revision) = database(&data, &anchor.key)?;
        if revision != snapshot.revision || inspection::records(&connection)? != snapshot.records {
            return Err(UPGRADE_INVALID.into());
        }
        Ok(())
    }

    pub fn replacement(
        &self,
        home: &Folder,
        anchor_parent: &Dir,
        directory_parent: &Dir,
        advanced: bool,
    ) -> Result<Namespace, String> {
        let key = &self.plan.replacement;
        let anchor = child(anchor_parent, ANCHOR, key.uid)?;
        lock(&anchor, true)?;
        let record = anchor_record(&anchor)?;
        if record.key != *key {
            return Err(UPGRADE_INVALID.into());
        }
        let directory = Namespace::directory(directory_parent, key)?;
        let namespace = Namespace {
            home: home.clone(),
            directory,
            anchor,
            key: key.clone(),
        };
        namespace.validated_database_at(anchor_parent, directory_parent, &record)?;
        if !advanced {
            for (name, expected) in &self.plan.initial {
                let selected = if name == ANCHOR {
                    anchor_parent
                } else {
                    &namespace.directory
                };
                if digest(&bytes(
                    &child(selected, name, key.uid)?,
                    MAX_DATABASE_BYTES,
                )?) != *expected
                {
                    return Err(UPGRADE_INVALID.into());
                }
            }
        }
        Ok(namespace)
    }

    pub fn report(&self, phase: &str) -> Inspection {
        Inspection {
            eligible: true,
            expected: Some(self.plan.expected.clone()),
            reason: "Resume the reviewed upgrade; original registry files are retained".into(),
            schema: self.plan.legacy.schema,
            revision: self.plan.legacy.revision,
            records: self.plan.legacy.records.clone(),
            device_changed: self.plan.device_changed,
            phase: phase.into(),
        }
    }
}
