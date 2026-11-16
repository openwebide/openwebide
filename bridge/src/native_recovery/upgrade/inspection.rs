use super::*;

pub(super) fn records(connection: &Connection) -> Result<BTreeMap<String, u64>, String> {
    let mut statement = connection
        .prepare("SELECT type,name,tbl_name FROM sqlite_schema")
        .map_err(|_| UPGRADE_INVALID)?;
    let mut rows = statement.query([]).map_err(|_| UPGRADE_INVALID)?;
    let mut tables = BTreeSet::new();
    while let Some(row) = rows.next().map_err(|_| UPGRADE_INVALID)? {
        let kind: String = row.get(0).map_err(|_| UPGRADE_INVALID)?;
        let name: String = row.get(1).map_err(|_| UPGRADE_INVALID)?;
        let table: String = row.get(2).map_err(|_| UPGRADE_INVALID)?;
        let known = |name: &str| {
            matches!(name, "recovery_namespace" | "recovery_revision") || RECORDS.contains(&name)
        };
        if kind == "table" && known(&name) {
            tables.insert(name);
        } else if kind != "index" || !name.starts_with("sqlite_autoindex_") || !known(&table) {
            return Err(UPGRADE_INVALID.into());
        }
    }
    let mut result = BTreeMap::new();
    for table in RECORDS {
        let count: i64 = if tables.contains(table) {
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(|_| UPGRADE_INVALID)?
        } else {
            0
        };
        result.insert(
            table.into(),
            u64::try_from(count).map_err(|_| UPGRADE_INVALID)?,
        );
    }
    if tables.contains("recovery_revision") {
        let rows: i64 = connection
            .query_row("SELECT count(*) FROM recovery_revision", [], |row| {
                row.get(0)
            })
            .map_err(|_| UPGRADE_INVALID)?;
        if rows != 1 {
            return Err(UPGRADE_INVALID.into());
        }
    }
    Ok(result)
}

pub(super) fn head(
    snapshot: &mut Snapshot,
    anchor: &Anchor,
    directory: &Dir,
    files: &BTreeSet<String>,
    uid: u32,
) -> Result<(), String> {
    if !anchor.sealed
        || *files != BTreeSet::from([DATABASE.to_owned(), persistence::HEAD.to_owned()])
    {
        return Err(UPGRADE_INVALID.into());
    }
    let file = child(directory, persistence::HEAD, uid)?;
    let data = bytes(&file, MAX_ANCHOR_BYTES)?;
    let head: persistence::Head = serde_json::from_slice(&data).map_err(|_| UPGRADE_INVALID)?;
    let id = head
        .snapshot
        .name
        .strip_prefix("snapshot-")
        .and_then(|name| name.strip_suffix(".sqlite3"))
        .ok_or(UPGRADE_INVALID)?;
    if head.format != 1
        || head.key != anchor.key
        || head.phase != persistence::Phase::Committed
        || head.revision != snapshot.revision
        || head.digest != snapshot.hashes[DATABASE]
        || !openwebide_core::workspace_transaction::valid_id(id)
        || !head.snapshot.identity.same_device(&anchor.key.home)
        || !head.snapshot.identity.uses_device_number()
        || !head.snapshot.identity.has_birth_time()
    {
        return Err(UPGRADE_INVALID.into());
    }
    observe(
        snapshot,
        persistence::HEAD,
        &head.identity,
        &anchor.key.home,
        &file,
    )?;
    snapshot
        .hashes
        .insert(persistence::HEAD.into(), digest(&data));
    Ok(())
}
