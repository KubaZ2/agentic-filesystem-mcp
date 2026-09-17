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
    use std::{ffi::OsStr, sync::Arc};

    use anyhow::Result;
    use cap_std::ambient_authority;
    use cap_tempfile::TempDir;

    use crate::{Filesystem, FilesystemData, fs::VfsDirBuilder};

    pub fn setup_test_fs() -> Result<(TempDir, Arc<FilesystemData>)> {
        let tempdir = TempDir::new(ambient_authority())?;

        let builder = VfsDirBuilder::from_root(tempdir.open_dir(".")?);

        let dir = builder.build();

        let filesystem = Filesystem::new(dir);

        Ok((tempdir, filesystem.data.clone()))
    }

    pub fn setup_virtual_fs(dirs: &[&OsStr]) -> Result<(Vec<TempDir>, Arc<FilesystemData>)> {
        let mut builder = VfsDirBuilder::new();

        let mut tempdirs = Vec::new();

        for dir_name in dirs {
            let tempdir = TempDir::new(ambient_authority())?;

            builder.mount_dir(dir_name, tempdir.open_dir(".")?)?;

            tempdirs.push(tempdir);
        }

        let dir = builder.build();

        let filesystem = Filesystem::new(dir);

        Ok((tempdirs, filesystem.data.clone()))
    }
}
