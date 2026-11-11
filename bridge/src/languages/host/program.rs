//! Native executable selection and lifetime checks. Keep launch aliases intact:
//! multicall executables such as rustup choose their service from argv[0].
use super::{files::FileRevision, resources::owning_root};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

mod links;

const MAX_IMAGES: usize = 64;
const MAX_PROCESSES: usize = 256;
const MAX_PATH_BYTES: usize = 128 * 1024;
static INSPECTIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
const CHANGED: &str = "The language server executable changed or became unavailable; retry to synchronize current buffers with a fresh language service";

struct Image {
    path: PathBuf,
    canonical: PathBuf,
    handle: same_file::Handle,
    revision: FileRevision,
    links: links::Links,
}
impl Image {
    fn read(path: PathBuf) -> Result<Self, String> {
        let links = links::Links::read(&path)?;
        let handle = same_file::Handle::from_path(&path).map_err(|error| error.to_string())?;
        let canonical = std::fs::canonicalize(&path).map_err(|error| error.to_string())?;
        let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("A language executable is not a regular file".into());
        }
        let image = Self {
            path,
            canonical,
            handle,
            revision: FileRevision::from(&metadata),
            links,
        };
        image.check()?;
        Ok(image)
    }
    fn check(&self) -> Result<(), String> {
        if self.links.unchanged()
            && same_file::Handle::from_path(&self.path).is_ok_and(|handle| handle == self.handle)
            && std::fs::canonicalize(&self.path).is_ok_and(|path| path == self.canonical)
            && std::fs::metadata(&self.path)
                .is_ok_and(|metadata| FileRevision::from(&metadata) == self.revision)
        {
            Ok(())
        } else {
            Err(CHANGED.into())
        }
    }
}

#[derive(Clone)]
pub(super) struct Program {
    command: Arc<str>,
    workspace: Arc<Path>,
    declared: Arc<Image>,
    images: Arc<[Image]>,
}
impl Program {
    pub(super) fn cache_identity(&self) -> String {
        openwebide_core::plugins::content_digest(
            format!(
                "{}|{}|{:?}",
                self.declared.path.display(),
                self.declared.canonical.display(),
                self.declared.revision
            )
            .as_bytes(),
        )
    }
    pub(super) async fn prepare(command: &str, workspace: &Path) -> Result<Self, String> {
        let command = command.to_string();
        let workspace = workspace.to_path_buf();
        bounded(move || {
            let path = resolve(&command, &workspace).map_err(|error| format!("Cannot start language server '{command}': {error}. Install the declared server and its toolchain on this execution host."))?;
            Ok(Self {
                command: command.into(), workspace: workspace.into(),
                declared: Arc::new(Image::read(path)?), images: Arc::from([]),
            })
        }).await
    }
    pub(super) fn path(&self) -> &Path {
        &self.declared.path
    }
    pub(super) async fn initialized(mut self, pid: u32) -> Result<Self, String> {
        let current = self.clone();
        self.images = bounded(move || {
            current.check_now()?;
            process_images(pid)?
                .into_iter()
                .map(Image::read)
                .collect::<Result<Vec<_>, _>>()
        })
        .await?
        .into();
        self.check().await?;
        Ok(self)
    }
    pub(super) async fn check(&self) -> Result<(), String> {
        let current = self.clone();
        bounded(move || current.check_now()).await
    }
    fn check_now(&self) -> Result<(), String> {
        if !resolve(&self.command, &self.workspace).is_ok_and(|path| path == self.declared.path) {
            return Err(CHANGED.into());
        }
        self.declared.check()?;
        for image in &*self.images {
            image.check()?;
        }
        Ok(())
    }
}

fn resolve(command: &str, workspace: &Path) -> Result<PathBuf, String> {
    which::which_in(command, std::env::var_os("PATH"), workspace).map_err(|error| error.to_string())
}

// Read executable paths only for the owned process tree. The first pass collects
// parent relationships; command arguments and environments are never requested.
fn process_images(pid: u32) -> Result<BTreeSet<PathBuf>, String> {
    let root = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    let owned = system
        .processes()
        .keys()
        .copied()
        .filter(|pid| owning_root(*pid, system.processes(), &[root]).is_some())
        .collect::<Vec<_>>();
    if owned.len() > MAX_PROCESSES {
        return Err("The language service exceeds its process inventory limit".into());
    }
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&owned),
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_exe(UpdateKind::Always),
    );
    if system
        .process(root)
        .and_then(sysinfo::Process::exe)
        .is_none_or(|path| path.as_os_str().is_empty())
    {
        return Err("Cannot inspect the language server's native executable".into());
    }
    let paths = owned
        .iter()
        .filter_map(|pid| system.process(*pid).and_then(sysinfo::Process::exe))
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>();
    if paths.len() > MAX_IMAGES
        || paths
            .iter()
            .map(|path| path.as_os_str().len())
            .sum::<usize>()
            > MAX_PATH_BYTES
    {
        return Err("The language service exceeds its executable inventory limit".into());
    }
    Ok(paths)
}

async fn bounded<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(5), async move {
        let permit = INSPECTIONS
            .acquire()
            .await
            .map_err(|error| error.to_string())?;
        // A timed-out OS call retains its permit until it actually finishes.
        // Repeated requests therefore cannot accumulate blocking workers.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| error.to_string())?
    })
    .await
    .map_err(|_| "Inspecting language server executables timed out".to_string())?
}
