//! Explicit object watches above thin kernel adapters. Limits, cancellation,
//! handle validation and reconciliation are shared across supported platforms.
use super::{Budgets, Stamp};
use std::{
    collections::BTreeMap,
    fs::File,
    path::PathBuf,
    sync::{Arc, LazyLock, atomic::AtomicBool},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as native;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as native;

const DESCRIPTORS: usize = 1024;
#[cfg(windows)]
static BUFFER_BYTES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(16 * 1024 * 1024)));
static AVAILABLE: LazyLock<Budgets> = LazyLock::new(|| Budgets::new(descriptor_budget()));

#[cfg(unix)]
fn descriptor_budget() -> usize {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // This initialized output lives for the synchronous read-only limit query.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return 0;
    }
    usize::try_from(limit.rlim_cur / 4)
        .unwrap_or(DESCRIPTORS)
        .min(DESCRIPTORS)
}
#[cfg(windows)]
fn descriptor_budget() -> usize {
    DESCRIPTORS
}

fn descriptor(available: &Arc<Semaphore>) -> Result<OwnedSemaphorePermit, String> {
    available
        .clone()
        .try_acquire_owned()
        .map_err(|_| "Project-source event descriptor budget is full".into())
}

#[cfg(windows)]
fn buffer(bytes: usize) -> Result<OwnedSemaphorePermit, String> {
    BUFFER_BYTES
        .clone()
        .try_acquire_many_owned(
            u32::try_from(bytes).map_err(|_| "Invalid notification buffer size")?,
        )
        .map_err(|_| "Project-source event buffer budget is full".into())
}

struct Watched {
    // Close the descriptor before releasing its resource permit.
    file: File,
    _permit: OwnedSemaphorePermit,
    id: native::Id,
}
pub(super) struct Events {
    kernel: native::Kernel,
    _permit: OwnedSemaphorePermit,
    files: BTreeMap<PathBuf, Watched>,
    available: Arc<Semaphore>,
}

impl Events {
    pub fn new() -> Result<Self, String> {
        Self::with_budget(AVAILABLE.sources.clone())
    }

    pub fn metadata() -> Result<Self, String> {
        Self::with_budget(AVAILABLE.metadata.clone())
    }

    fn with_budget(available: Arc<Semaphore>) -> Result<Self, String> {
        let permit = descriptor(&available)?;
        let kernel = native::Kernel::new(available.clone())?;
        Ok(Self {
            kernel,
            _permit: permit,
            files: BTreeMap::new(),
            available,
        })
    }

    pub fn reconcile(
        &mut self,
        paths: &BTreeMap<PathBuf, Stamp>,
        cancelled: &AtomicBool,
    ) -> Result<bool, String> {
        super::check(cancelled)?;
        if paths.len() >= DESCRIPTORS {
            return Err("Project-source event inventory exceeds its watch limit".into());
        }
        let mut obsolete = Vec::new();
        for (path, watched) in &self.files {
            super::check(cancelled)?;
            let current = paths.get(path).is_some_and(|expected| {
                Stamp::from_file(&watched.file).is_ok_and(|metadata| metadata == *expected)
            });
            if !current {
                obsolete.push(path.clone());
            }
        }
        let mut changed = !obsolete.is_empty();
        for path in obsolete {
            super::check(cancelled)?;
            let watched = self.files.remove(&path).expect("collected registration");
            self.kernel.remove(watched.id)?;
        }
        for (path, expected) in paths {
            super::check(cancelled)?;
            if self.files.contains_key(path) {
                continue;
            }
            let permit = descriptor(&self.available)?;
            let file = native::open(path)?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !(metadata.is_dir() || metadata.is_file()) || Stamp::from_file(&file)? != *expected {
                return Err("A project-source object changed while installing its watch".into());
            }
            let id = self.kernel.register(&file, metadata.is_dir(), path)?;
            self.files.insert(
                path.clone(),
                Watched {
                    file,
                    _permit: permit,
                    id,
                },
            );
            changed = true;
        }
        Ok(changed)
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        self.kernel.poll()
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(test)]
#[test]
fn speculative_sources_cannot_exhaust_reserved_metadata_watch_capacity() {
    let budgets = Budgets::new(16);
    let sources = budgets.sources.clone().try_acquire_many_owned(12).unwrap();
    assert!(budgets.sources.try_acquire().is_err());
    let metadata = Events::with_budget(budgets.metadata.clone()).unwrap();
    assert_eq!(budgets.metadata.available_permits(), 3);
    drop(metadata);
    drop(sources);
    assert_eq!(budgets.metadata.available_permits(), 4);
    assert_eq!(budgets.sources.available_permits(), 12);
}
