//! Git context and metadata generations retained by the original service across
//! client handoff. Both authenticated transports and commands share this check.
use super::*;

pub(super) struct Repository {
    revision: Option<crate::exec::git::RepositoryRevision>,
    metadata: Option<(
        generation::Tracker<generation::MetadataFiles>,
        generation::Observation,
    )>,
    directories: Vec<(PathBuf, same_file::Handle)>,
}

const CHANGED: &str = "The project's Git revision changed; retry to synchronize current buffers with a fresh language service";

impl Repository {
    pub async fn new(workspace: &std::path::Path) -> Result<Self, String> {
        let revision = crate::exec::git::repository_revision(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut directories = Vec::new();
        let metadata = if let Some(revision) = &revision {
            let paths = crate::exec::git::repository_metadata(workspace, revision)
                .await
                .map_err(|error| error.to_string())?;
            for path in &paths.directories {
                let handle =
                    same_file::Handle::from_path(path).map_err(|error| error.to_string())?;
                if !handle
                    .as_file()
                    .metadata()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                {
                    return Err("The Git metadata folder is unavailable".into());
                }
                directories.push((path.clone(), handle));
            }
            let tracker = generation::Tracker::new(generation::MetadataFiles {
                files: paths.files,
                parents: paths.parents,
                boundaries: paths.boundaries,
            });
            let observed = tracker.observe(workspace).await?;
            Some((tracker, observed))
        } else {
            None
        };
        Ok(Self {
            revision,
            metadata,
            directories,
        })
    }

    pub async fn check(&self, workspace: &std::path::Path) -> Result<(), String> {
        if crate::exec::git::repository_revision(workspace)
            .await
            .map_err(|error| error.to_string())?
            != self.revision
        {
            return Err(CHANGED.into());
        }
        self.check_directories()?;
        if let Some((tracker, before)) = &self.metadata
            && tracker.observe(workspace).await? != *before
        {
            return Err(CHANGED.into());
        }
        self.check_directories()
    }

    fn check_directories(&self) -> Result<(), String> {
        if self.directories.iter().any(|(path, handle)| {
            !same_file::Handle::from_path(path).is_ok_and(|current| current == *handle)
        }) {
            return Err(CHANGED.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
