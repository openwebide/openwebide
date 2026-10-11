//! Build offline defaults using the same compiler and export checks as installation.
use anyhow::{Context, Result, bail};
use openwebide_core::plugins::{
    PackageFile, PackageFileKind, PluginSource, package_digest, validate_files,
};
use openwebide_plugin_runtime::{
    Runtime, build,
    bundled::{BundledArtifact, CompiledBundle},
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    repository: String,
    commit: String,
    plugins: Vec<Package>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    path: String,
    files: Vec<File>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    path: String,
    content: String,
    executable: bool,
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        bail!("Usage: openwebide-bundle-plugins <source-snapshots.json> <compiled-bundle.json>");
    }
    let input = PathBuf::from(&args[0]);
    if std::fs::metadata(&input)?.len() > 64 * 1024 * 1024 {
        bail!("Source bundle exceeds its limit");
    }
    let snapshot: Snapshot = serde_json::from_slice(&std::fs::read(input)?)?;
    if snapshot.plugins.is_empty() || snapshot.plugins.len() > 32 {
        bail!("Select between one and 32 bundled plugins");
    }
    let runtime = Runtime::new()?;
    let mut bundle = CompiledBundle::default();
    for package in snapshot.plugins {
        let source = PluginSource {
            repository: snapshot.repository.clone(),
            commit: snapshot.commit.clone(),
            path: package.path,
        };
        source.validate()?;
        if bundle.artifacts.iter().any(|entry| entry.source == source) {
            bail!("Duplicate source in bundle");
        }
        let files: Vec<_> = package
            .files
            .into_iter()
            .map(|file| PackageFile {
                path: file.path,
                content: file.content.into_bytes(),
                kind: if file.executable {
                    PackageFileKind::Executable
                } else {
                    PackageFileKind::File
                },
            })
            .collect();
        let manifest = validate_files(&files)?;
        let executable = manifest
            .executable
            .as_ref()
            .context("Bundled defaults must provide executable behavior")?;
        let workspace = tempfile::tempdir()?;
        let directory = workspace.path().join("source");
        std::fs::create_dir(&directory)?;
        for file in &files {
            let target = directory.join(&file.path);
            std::fs::create_dir_all(target.parent().context("Source file parent")?)?;
            std::fs::write(target, &file.content)?;
        }
        let staging = workspace.path().join("build");
        std::fs::create_dir(&staging)?;
        let bytes = build::compile(&directory, &executable.library, &staging)?;
        runtime.validate_exports(&bytes, &manifest)?;
        bundle
            .artifacts
            .push(BundledArtifact::new(source, package_digest(&files), &bytes));
        eprintln!(
            "Compiled {} {} ({} bytes)",
            manifest.display_name,
            manifest.version,
            bytes.len()
        );
    }
    publish(&bundle, Path::new(&args[1]))?;
    Ok(())
}
fn publish(bundle: &CompiledBundle, output: &Path) -> Result<()> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(file.as_file_mut(), bundle)?;
    file.persist(output)?;
    Ok(())
}
