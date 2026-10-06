//! Runner projects creation implementation.

use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct CreatedProjectPaths {
    pub(crate) project_dir_created: Option<PathBuf>,
    pub(crate) paths: Vec<PathBuf>,
}

impl CreatedProjectPaths {
    pub(crate) fn mark_project_dir_created(&mut self, path: PathBuf) {
        self.project_dir_created = Some(path);
    }

    pub(crate) fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    pub(crate) fn cleanup(&self) {
        for path in self.paths.iter().rev() {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else if path.exists() {
                let _ = std::fs::remove_file(path);
            }
        }
        if let Some(dir) = &self.project_dir_created {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

pub(crate) fn write_created_file(
    path: &Path,
    content: &[u8],
    created_paths: &mut CreatedProjectPaths,
) -> Result<(), std::io::Error> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    created_paths.track(path.to_path_buf());
    file.write_all(content)
}
