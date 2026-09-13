pub mod copy;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod mkdir;
pub mod r#move;
pub mod read;
pub mod remove;
pub mod write;

#[cfg(test)]
pub(crate) mod test_utils {
    use std::{path::PathBuf, sync::Arc};

    use anyhow::Result;
    use cap_std::ambient_authority;
    use cap_tempfile::TempDir;

    use crate::{DirInfo, Filesystem, FilesystemData};

    pub fn setup_test_fs() -> Result<(TempDir, Arc<FilesystemData>)> {
        let tempdir = TempDir::new(ambient_authority())?;

        let dir = DirInfo {
            path: PathBuf::new(),
            dir: tempdir.open_dir(".")?,
        };

        let filesystem = Filesystem::new(vec![dir]);

        Ok((tempdir, filesystem.data.clone()))
    }
}
