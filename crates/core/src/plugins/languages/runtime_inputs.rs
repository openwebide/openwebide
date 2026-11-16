//! Public native implementation-file declarations, independent of transport.
use super::{LanguageServer, text};
use std::collections::BTreeSet;

pub fn validate_runtime_inputs(server: &LanguageServer) -> Result<(), String> {
    let Some(inputs) = &server.runtime_inputs else {
        return Ok(());
    };
    let count = inputs
        .argument_files
        .len()
        .saturating_add(inputs.files.len());
    let mut arguments = BTreeSet::new();
    let mut files = BTreeSet::new();
    if count == 0
        || count > 64
        || inputs.argument_files.iter().any(|index| {
            !arguments.insert(*index)
                || server
                    .args
                    .get(*index)
                    .is_none_or(|path| !text(path, 4096) || path.contains("${cache}"))
        })
        || inputs
            .files
            .iter()
            .any(|path| !text(path, 4096) || path.contains("${cache}") || !files.insert(path))
    {
        return Err("Runtime inputs require 1–64 distinct implementation files, valid argument positions and paths outside derived-cache bindings".into());
    }
    Ok(())
}
