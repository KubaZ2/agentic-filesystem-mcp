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
    use std::{collections::HashMap, path::PathBuf, sync::Arc};

    use anyhow::Result;
    use cap_std::ambient_authority;
    use cap_tempfile::TempDir;

    use crate::{DirInfo, FilesystemData, MimeType};

    pub fn setup_test_fs() -> Result<(TempDir, Arc<FilesystemData>)> {
        let tempdir = TempDir::new(ambient_authority())?;

        let dir = DirInfo {
            path: PathBuf::from(""),
            dir: tempdir.open_dir(".")?,
        };

        let filesystem_data = FilesystemData {
            dirs: vec![dir],
            media_mime_types: HashMap::from([("png", MimeType::Image("image/png"))]),
        };

        Ok((tempdir, Arc::new(filesystem_data)))
    }
}
