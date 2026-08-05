//! A scratch directory that goes away however the test ends.
//!
//! These tests write a module, its glue and a runner script somewhere, hand the
//! directory to Node, and delete it afterwards. "Afterwards" was a
//! `remove_dir_all` on the last line, which is the one line a failing assertion
//! never reaches — so a red run left its directory behind, and a run that was
//! interrupted left the Node process it had started as well.
//!
//! That is the shape a `Drop` exists for: unwinding runs it, and the assertion
//! is what unwinds.

use std::path::{Path, PathBuf};

/// A directory that is removed when this value is dropped.
pub struct Workspace {
    path: PathBuf,
}

impl Workspace {
    /// A fresh directory named for the test and the process, so two test
    /// binaries running at once cannot collide and a leftover from a previous
    /// run is cleared rather than reused.
    pub fn new(name: &str) -> Workspace {
        let path = std::env::temp_dir().join(format!("kite-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("work directory");
        Workspace { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
