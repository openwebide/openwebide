use super::*;
use plan::{Loaded, Original, Plan, Pointer};
use storage::{Area, CANCELLED, COMPLETE, CURRENT, ORIGINAL_ANCHOR, ORIGINAL_DIRECTORY};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Boundary {
    Prepared,
    Intent,
    AnchorBackup,
    DirectoryBackup,
    DirectoryPublished,
    AnchorPublished,
    CompletionLinked,
    Completed,
    CancelPrepared,
    CancellationLinked,
    CancellationUnfenced,
    Cancelled,
}

fn selected(
    home: &Path,
    create: bool,
    exclusive: bool,
) -> Result<(Folder, Dir, Option<Area>), String> {
    let home = Folder::capture_state(home)?;
    let parent = root(&home)?;
    let uid = io(home.handle.as_file().metadata())?.uid();
    // SAFETY: geteuid has no preconditions.
    if uid != unsafe { libc::geteuid() } {
        return Err(UPGRADE_INVALID.into());
    }
    let area = Area::open(&parent, uid, create, exclusive)?;
    Ok((home, parent, area))
}

fn active(home: &Folder, area: &Area) -> Result<Option<Loaded>, String> {
    if storage::exists(&area.dir, CURRENT)? {
        return Loaded::load(home, area, &area.dir, CURRENT).map(Some);
    }
    let uid = io(home.handle.as_file().metadata())?.uid();
    let mut pending = None;
    for id in area.generations()? {
        let generation = area.generation(&id, uid)?;
        if storage::exists(&generation, ORIGINAL_ANCHOR)?
            && storage::exists(&generation, storage::POINTER)?
        {
            if pending.is_some() {
                return Err(UPGRADE_INVALID.into());
            }
            pending = Some(Loaded::load(home, area, &generation, storage::POINTER)?);
        }
    }
    Ok(pending)
}

fn publish_intent(
    home: &Folder,
    area: &Area,
    loaded: &Loaded,
    original: &Original,
    checkpoint: &mut impl FnMut(Boundary) -> Result<(), String>,
) -> Result<(), String> {
    if !storage::exists(&area.dir, CURRENT)? {
        loaded.validate_original(original)?;
        if !original.anchor_current || !original.directory_current || !original.backed_up {
            return Err(UPGRADE_INVALID.into());
        }
        // Reconfirm durability after any lost acknowledgement before publishing
        // the intent. The two-link original anchor already fences old binaries.
        io(original.anchor.sync_all())?;
        io(crate::native_directory::sync(&loaded.generation))?;
        storage::move_entry(
            &loaded.generation,
            storage::POINTER,
            &area.dir,
            CURRENT,
            false,
        )?;
        checkpoint(Boundary::Intent)?;
    }
    loaded.require_intent(home, area)
}

pub(super) fn status(home: &Path) -> Result<Option<Inspection>, String> {
    let (home, parent, area) = selected(home, false, false)?;
    let Some(area) = area else {
        return Ok(None);
    };
    let loaded = active(&home, &area)?;
    let Some(loaded) = loaded else {
        let uid = io(home.handle.as_file().metadata())?.uid();
        for id in area.generations()? {
            let generation = area.generation(&id, uid)?;
            if !storage::exists(&generation, COMPLETE)? {
                continue;
            }
            let loaded = Loaded::load(&home, &area, &generation, COMPLETE)?;
            if !storage::matches(&parent, ANCHOR, &loaded.plan.replacement.anchor)?
                || !storage::matches(&parent, DIRECTORY, &loaded.plan.replacement.directory)?
            {
                continue;
            }
            let original = loaded.original(&home, &parent, Some(false))?;
            loaded.validate_original(&original)?;
            let _replacement = loaded.replacement(&home, &parent, &parent, true)?;
            let mut report = loaded.report("completed");
            report.eligible = false;
            report.reason = "Upgrade is complete; the original registry files are retained".into();
            return Ok(Some(report));
        }
        return Ok(None);
    };
    let original = loaded.original(&home, &parent, Some(false))?;
    if storage::exists(&loaded.generation, "cancel.json")? {
        if !original.anchor_current || !original.directory_current {
            return Err(UPGRADE_INVALID.into());
        }
        if original.backed_up {
            loaded.validate_original(&original)?;
        } else if !storage::exists(&loaded.generation, CANCELLED)? {
            return Err(UPGRADE_INVALID.into());
        }
        let mut report = loaded.report("cancelling");
        report.reason = "Finish the reviewed cancellation with recovery upgrade cancel".into();
        return Ok(Some(report));
    }
    loaded.validate_original(&original)?;
    let anchor_new = storage::matches(&parent, ANCHOR, &loaded.plan.replacement.anchor)?;
    let directory_new = storage::matches(&parent, DIRECTORY, &loaded.plan.replacement.directory)?;
    let _replacement = loaded.replacement(
        &home,
        if anchor_new {
            &parent
        } else {
            &loaded.generation
        },
        if directory_new {
            &parent
        } else {
            &loaded.generation
        },
        anchor_new && directory_new,
    )?;
    Ok(Some(loaded.report(if anchor_new {
        "published"
    } else {
        "prepared"
    })))
}

pub(crate) fn execute(
    home: &Path,
    expected: &str,
    note: &str,
) -> Result<serde_json::Value, String> {
    execute_observed(home, expected, note, |_| Ok(()))
}

pub(super) fn execute_observed(
    home: &Path,
    expected: &str,
    note: &str,
    mut checkpoint: impl FnMut(Boundary) -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    if !plan::note_valid(note)
        || !openwebide_core::workspace_transaction::companion::fingerprint(expected)
    {
        return Err(UPGRADE_INVALID.into());
    }
    let (folder, parent, area) = selected(home, false, true)?;
    if let Some(area) = area {
        if let Some(loaded) = active(&folder, &area)? {
            if loaded.plan.expected != expected {
                return Err("Native upgrade changed; inspect its original review again".into());
            }
            let original = loaded.original(&folder, &parent, Some(true))?;
            publish_intent(&folder, &area, &loaded, &original, &mut checkpoint)?;
            return resume(&folder, &parent, &area, &loaded, original, &mut checkpoint);
        }
        if let Some(result) = archived(&folder, &parent, &area, expected, COMPLETE)? {
            return Ok(result);
        }
        return prepare_and_resume(&folder, &parent, &area, expected, note, &mut checkpoint);
    }
    // Do not create even the private preparation area for an invalid review.
    let legacy = capture(home, true)?;
    require_review(&legacy, expected)?;
    drop(legacy);
    let area = Area::open(
        &parent,
        io(folder.handle.as_file().metadata())?.uid(),
        true,
        true,
    )?
    .ok_or(UPGRADE_INVALID)?;
    if storage::exists(&area.dir, CURRENT)? {
        return Err(BUSY.into());
    }
    prepare_and_resume(&folder, &parent, &area, expected, note, &mut checkpoint)
}

fn require_review(legacy: &Legacy, expected: &str) -> Result<(), String> {
    let report = legacy.report()?;
    if !report.eligible || report.expected.as_deref() != Some(expected) {
        return Err("Native upgrade changed or contains original records; inspect it again".into());
    }
    Ok(())
}

fn prepare_and_resume(
    home: &Folder,
    parent: &Dir,
    area: &Area,
    expected: &str,
    note: &str,
    checkpoint: &mut impl FnMut(Boundary) -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    let legacy = capture(&home.path, true)?;
    require_review(&legacy, expected)?;
    let loaded = prepare(&legacy, area, expected, note, checkpoint)?;
    let original = Original {
        anchor: legacy.anchor,
        directory: legacy.directory,
        anchor_current: true,
        directory_current: true,
        backed_up: true,
    };
    resume(home, parent, area, &loaded, original, checkpoint)
}

fn prepare(
    legacy: &Legacy,
    area: &Area,
    expected: &str,
    note: &str,
    checkpoint: &mut impl FnMut(Boundary) -> Result<(), String>,
) -> Result<Loaded, String> {
    if area.generations()?.len() >= 16 {
        return Err(
            "Native upgrade preparation capacity is full; preserve the existing audit".into(),
        );
    }
    let uid = io(legacy.home.handle.as_file().metadata())?.uid();
    let id = format!("{:032x}", rand::random::<u128>());
    let generation = storage::create_directory(&area.dir, &id, uid)?;
    let anchor = storage::create(&generation, ANCHOR, uid)?;
    lock(&anchor, true)?;
    Namespace::initialize(&legacy.home, &generation, &anchor, uid)?;
    let record = anchor_record(&anchor)?;
    let namespace = Namespace {
        home: legacy.home.clone(),
        directory: Namespace::directory(&generation, &record.key)?,
        key: record.key.clone(),
        anchor,
    };
    namespace.recover(&generation, &record)?;
    namespace.validate(&generation, &record)?;
    let mut initial = BTreeMap::new();
    for name in [ANCHOR, DATABASE, persistence::HEAD] {
        let selected = if name == ANCHOR {
            &generation
        } else {
            &namespace.directory
        };
        initial.insert(
            name.into(),
            digest(&bytes(&child(selected, name, uid)?, MAX_DATABASE_BYTES)?),
        );
    }
    let mut manifest = storage::create(&generation, storage::PLAN, uid)?;
    let mut pointer = storage::create(&generation, storage::POINTER, uid)?;
    let plan = Plan {
        format: 1,
        id: id.clone(),
        expected: expected.into(),
        note: note.into(),
        recorded_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| UPGRADE_INVALID)?
            .as_secs(),
        legacy: legacy.snapshot.clone(),
        device_changed: legacy.device_changed,
        area: original(&io(area.dir.try_clone())?.into_std_file())?,
        generation: original(&io(generation.try_clone())?.into_std_file())?,
        pointer: original(&pointer)?,
        replacement: namespace.key.clone(),
        initial,
    };
    storage::write(&mut manifest, &plan)?;
    let value = Pointer {
        id,
        digest: digest(&bytes(&manifest, storage::MAX_PLAN)?),
        manifest: original(&manifest)?,
    };
    storage::write(&mut pointer, &value)?;
    io(crate::native_directory::sync(&generation))?;
    drop(namespace);
    drop(manifest);
    drop(pointer);
    checkpoint(Boundary::Prepared)?;
    let parent = root(&legacy.home)?;
    io(parent.hard_link(ANCHOR, &generation, ORIGINAL_ANCHOR))?;
    io(legacy.anchor.sync_all())?;
    io(crate::native_directory::sync(&generation))?;
    checkpoint(Boundary::AnchorBackup)?;
    storage::move_entry(&generation, storage::POINTER, &area.dir, CURRENT, false)?;
    checkpoint(Boundary::Intent)?;
    Loaded::load(&legacy.home, area, &area.dir, CURRENT)
}

fn resume(
    home: &Folder,
    parent: &Dir,
    area: &Area,
    loaded: &Loaded,
    original: Original,
    checkpoint: &mut impl FnMut(Boundary) -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    if storage::exists(&loaded.generation, "cancel.json")? {
        return Err("Finish the previously requested upgrade cancellation".into());
    }
    loaded.require_intent(home, area)?;
    loaded.validate_original(&original)?;
    let key = &loaded.plan.replacement;
    let anchor_new = storage::matches(parent, ANCHOR, &key.anchor)?;
    let directory_new = storage::matches(parent, DIRECTORY, &key.directory)?;
    if !original.anchor_current && !anchor_new {
        return Err(UPGRADE_INVALID.into());
    }
    if !original.directory_current && storage::exists(parent, DIRECTORY)? && !directory_new {
        return Err(UPGRADE_INVALID.into());
    }
    let replacement = loaded.replacement(
        home,
        if anchor_new {
            parent
        } else {
            &loaded.generation
        },
        if directory_new {
            parent
        } else {
            &loaded.generation
        },
        anchor_new && directory_new,
    )?;
    if !original.backed_up {
        return Err(UPGRADE_INVALID.into());
    }
    io(original.anchor.sync_all())?;
    io(crate::native_directory::sync(&loaded.generation))?;
    loaded.validate_original(&loaded.original(home, parent, None)?)?;
    loaded.require_intent(home, area)?;
    if original.directory_current {
        storage::move_entry(
            parent,
            DIRECTORY,
            &loaded.generation,
            ORIGINAL_DIRECTORY,
            true,
        )?;
        checkpoint(Boundary::DirectoryBackup)?;
    }
    loaded.validate_original(&loaded.original(home, parent, None)?)?;
    loaded.require_intent(home, area)?;
    if !directory_new {
        storage::move_entry(&loaded.generation, DIRECTORY, parent, DIRECTORY, true)?;
        checkpoint(Boundary::DirectoryPublished)?;
    }
    loaded.validate_original(&loaded.original(home, parent, None)?)?;
    loaded.require_intent(home, area)?;
    if !anchor_new {
        if !storage::matches(parent, ANCHOR, &loaded.plan.legacy.observations[ANCHOR])? {
            return Err(UPGRADE_INVALID.into());
        }
        // Replacement is intentional only for the reviewed, backed-up anchor.
        io(loaded.generation.rename(ANCHOR, parent, ANCHOR))?;
        io(crate::native_directory::sync(parent))?;
        io(crate::native_directory::sync(&loaded.generation))?;
        checkpoint(Boundary::AnchorPublished)?;
    }
    loaded.validate_original(&loaded.original(home, parent, None)?)?;
    replacement.validate(parent, &anchor_record(&replacement.anchor)?)?;
    loaded.require_intent(home, area)?;
    finish(home, parent, area, loaded, COMPLETE, checkpoint)?;
    Ok(outcome(&loaded.plan, "completed"))
}

fn outcome(plan: &Plan, state: &str) -> serde_json::Value {
    serde_json::json!({"status":state,"expected":plan.expected,"generation":plan.id,"original_files_preserved":true})
}

fn archived(
    home: &Folder,
    parent: &Dir,
    area: &Area,
    expected: &str,
    name: &str,
) -> Result<Option<serde_json::Value>, String> {
    let uid = io(home.handle.as_file().metadata())?.uid();
    for id in area.generations()? {
        let generation = area.generation(&id, uid)?;
        if !storage::exists(&generation, name)? {
            continue;
        }
        let loaded = Loaded::load(home, area, &generation, name)?;
        if loaded.plan.expected != expected {
            continue;
        }
        if name == COMPLETE {
            let original = loaded.original(home, parent, Some(false))?;
            loaded.validate_original(&original)?;
            let _namespace = loaded.replacement(home, parent, parent, true)?;
        }
        return Ok(Some(outcome(
            &loaded.plan,
            if name == COMPLETE {
                "completed"
            } else {
                "cancelled"
            },
        )));
    }
    Ok(None)
}

pub(crate) fn cancel(home: &Path, expected: &str, note: &str) -> Result<serde_json::Value, String> {
    cancel_observed(home, expected, note, |_| Ok(()))
}
pub(super) fn cancel_observed(
    home: &Path,
    expected: &str,
    note: &str,
    mut checkpoint: impl FnMut(Boundary) -> Result<(), String>,
) -> Result<serde_json::Value, String> {
    if !plan::note_valid(note)
        || !openwebide_core::workspace_transaction::companion::fingerprint(expected)
    {
        return Err(UPGRADE_INVALID.into());
    }
    let (home, parent, area) = selected(home, false, true)?;
    let area = area.ok_or(UPGRADE_INVALID)?;
    let Some(loaded) = active(&home, &area)? else {
        return archived(&home, &parent, &area, expected, CANCELLED)?.ok_or(UPGRADE_INVALID.into());
    };
    if loaded.plan.expected != expected {
        return Err(UPGRADE_INVALID.into());
    }
    let original = loaded.original(&home, &parent, Some(true))?;
    if !original.anchor_current || !original.directory_current {
        return Err("Resume this upgrade; original authority publication has already begun".into());
    }
    if original.backed_up {
        loaded.validate_original(&original)?;
    } else if !storage::exists(&loaded.generation, CANCELLED)? {
        return Err(UPGRADE_INVALID.into());
    }
    publish_intent(&home, &area, &loaded, &original, &mut checkpoint)?;
    let uid = loaded.plan.replacement.uid;
    let _replacement = loaded.replacement(&home, &loaded.generation, &loaded.generation, false)?;
    if !storage::exists(&loaded.generation, "cancel.json")? {
        let mut audit = storage::create(&loaded.generation, "cancel.json", uid)?;
        storage::write(
            &mut audit,
            &serde_json::json!({"expected":expected,"note":note}),
        )?;
        io(crate::native_directory::sync(&loaded.generation))?;
    }
    let audit: Cancellation = storage::read(&child(&loaded.generation, "cancel.json", uid)?)?;
    if audit.expected != expected || !plan::note_valid(&audit.note) {
        return Err(UPGRADE_INVALID.into());
    }
    checkpoint(Boundary::CancelPrepared)?;
    loaded.require_intent(&home, &area)?;
    finish(&home, &parent, &area, &loaded, CANCELLED, &mut checkpoint)?;
    Ok(outcome(&loaded.plan, "cancelled"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cancellation {
    expected: String,
    note: String,
}

fn finish(
    home: &Folder,
    parent: &Dir,
    area: &Area,
    loaded: &Loaded,
    name: &str,
    checkpoint: &mut impl FnMut(Boundary) -> Result<(), String>,
) -> Result<(), String> {
    loaded.require_intent(home, area)?;
    if !storage::exists(&loaded.generation, name)? {
        io(area.dir.hard_link(CURRENT, &loaded.generation, name))?;
    }
    let marker = storage::linked_file(&area.dir, CURRENT, loaded.plan.replacement.uid)?;
    io(marker.sync_all())?;
    io(crate::native_directory::sync(&loaded.generation))?;
    checkpoint(if name == COMPLETE {
        Boundary::CompletionLinked
    } else {
        Boundary::CancellationLinked
    })?;
    loaded.require_intent(home, area)?;
    if name == CANCELLED {
        let original = loaded.original(home, parent, None)?;
        if !original.anchor_current || !original.directory_current {
            return Err(UPGRADE_INVALID.into());
        }
        if original.backed_up {
            loaded.validate_original(&original)?;
            io(loaded.generation.remove_file(ORIGINAL_ANCHOR))?;
        }
        // The durable cancellation receipt permits only metadata cleanup after
        // this point. Old publishers may advance the restored original state.
        io(original.anchor.sync_all())?;
        io(crate::native_directory::sync(&loaded.generation))?;
        checkpoint(Boundary::CancellationUnfenced)?;
    }
    loaded.require_intent(home, area)?;
    io(area.dir.remove_file(CURRENT))?;
    io(crate::native_directory::sync(&area.dir))?;
    checkpoint(if name == COMPLETE {
        Boundary::Completed
    } else {
        Boundary::Cancelled
    })
}
