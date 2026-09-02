// @reliability: normal
// @ai: assisted

use std::path::{Path, PathBuf};

/// One file in an emitted library package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub path: PathBuf,
    pub contents: String,
}

impl SourceFile {
    pub fn new(path: impl Into<PathBuf>, contents: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            contents: contents.into(),
        }
    }
}

/// A reusable library: relative paths plus text. Never a lone `main` / `REQUEST`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourcePackage {
    pub files: Vec<SourceFile>,
}

impl SourcePackage {
    pub fn new(files: Vec<SourceFile>) -> Self {
        Self { files }
    }

    pub fn file(&self, path: &str) -> Option<&SourceFile> {
        self.files.iter().find(|f| f.path.as_os_str() == path)
    }

    pub fn file_text(&self, path: &str) -> Option<&str> {
        self.file(path).map(|f| f.contents.as_str())
    }

    /// Write every file under `dir`, creating parent directories as needed.
    pub fn write_to_dir(&self, dir: impl AsRef<Path>) -> std::io::Result<()> {
        let dir = dir.as_ref();
        for file in &self.files {
            let dest = dir.join(&file.path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(dest, &file.contents)?;
        }
        Ok(())
    }
}
