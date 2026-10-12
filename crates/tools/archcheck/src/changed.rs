//! `--changed-files`: limit a report to the files a change touched.
//!
//! Findings cover the whole tree, so once advisory findings exist on the base
//! branch every later pull request would be told about files it never
//! touched. A caller that knows the change, such as CI, passes the paths it
//! touched and only their findings are shown. The tool runs no git itself.

use std::{collections::BTreeSet, io, path::Path, path::PathBuf};

use thiserror::Error;

use crate::{baseline::BASELINE_FILE, config::CONFIG_FILE, finding::Finding};

#[derive(Debug, Error)]
pub enum ChangedFilesError {
    #[error("could not read the changed-file list {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the changed-file list {path} is not valid UTF-8")]
    NonUtf8 { path: PathBuf },
}

/// Repository-relative, `/`-separated paths a change added, modified or
/// deleted, including both sides of a rename.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangedFiles {
    paths: BTreeSet<String>,
}

/// The findings to report, and how many were left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub shown: Vec<Finding>,
    /// Findings in files the change did not touch.
    pub hidden: usize,
}

impl ChangedFiles {
    /// Reads one path per line; blank lines are ignored and a trailing CR is
    /// dropped.
    pub fn load(path: &Path) -> Result<Self, ChangedFilesError> {
        let bytes = std::fs::read(path).map_err(|source| ChangedFilesError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let text = String::from_utf8(bytes).map_err(|_| ChangedFilesError::NonUtf8 {
            path: path.to_path_buf(),
        })?;
        Ok(Self::parse(&text))
    }

    pub fn parse(text: &str) -> Self {
        Self {
            paths: text
                .split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line))
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    /// A change to `ARCHITECTURE.toml` or the baseline changes what every
    /// file is checked against, so the whole tree is its business.
    pub fn changes_rules(&self) -> bool {
        self.contains(CONFIG_FILE) || self.contains(BASELINE_FILE)
    }

    /// Keeps the findings whose path the change touched: a file's own
    /// finding, or a baseline entry's, which is the entry's path, so a
    /// deleted or renamed listed file is matched by its old path.
    pub fn select(&self, findings: Vec<Finding>) -> Selection {
        if self.changes_rules() {
            return Selection {
                shown: findings,
                hidden: 0,
            };
        }
        let total = findings.len();
        let shown: Vec<Finding> = findings
            .into_iter()
            .filter(|finding| self.contains(&finding.path))
            .collect();
        Selection {
            hidden: total - shown.len(),
            shown,
        }
    }
}
