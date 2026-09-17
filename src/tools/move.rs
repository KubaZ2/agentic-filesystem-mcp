use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData, path_sanitizer::sanitize_path};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct MoveParams {
    #[schemars(description = "The source path to the file or directory to move or rename.")]
    src_path: String,

    #[schemars(
        description = "The destination path.\n\nIMPORTANT: This must include the target file or directory name, not just the destination folder."
    )]
    dst_path: String,
}

#[tool_router(router = tool_router_move, vis = "pub")]
impl Filesystem {
    #[tool(
        name = "move",
        description = "Moves or renames a file or directory.\n\nIMPORTANT: If the destination path already exists, it will be overwritten. Always verify the destination path before calling."
    )]
    async fn r#move(&self, parameters: Parameters<MoveParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("move", move || Self::try_move(data, parameters)).await
    }

    fn try_move(
        data: Arc<FilesystemData>,
        Parameters(MoveParams { src_path, dst_path }): Parameters<MoveParams>,
    ) -> Result<String> {
        let src_path = sanitize_path(&src_path).context("Failed to sanitize the source path")?;
        let dst_path =
            sanitize_path(&dst_path).context("Failed to sanitize the destination path")?;

        data.dir
            .rename(src_path, &data.dir, dst_path)
            .context("Failed to move the file or directory")?;

        Ok("Successfully moved the file or directory".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_utils::setup_test_fs;

    #[test]
    fn test_move_renames_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.write("source.txt", "Hello, world!")?;

        let params = Parameters(MoveParams {
            src_path: "source.txt".to_string(),
            dst_path: "destination.txt".to_string(),
        });

        let result = Filesystem::try_move(data.clone(), params)?;

        assert_eq!(result, "Successfully moved the file or directory");

        assert!(!data.dir.exists("source.txt"));

        let content = data.dir.read_to_string("destination.txt")?;

        assert_eq!(content, "Hello, world!");

        Ok(())
    }

    #[test]
    fn test_move_file_to_subdirectory() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.create_dir("subdirectory")?;
        data.dir.write("source.txt", "Hello, world!")?;

        let params = Parameters(MoveParams {
            src_path: "source.txt".to_string(),
            dst_path: "subdirectory/source.txt".to_string(),
        });

        let result = Filesystem::try_move(data.clone(), params)?;

        assert_eq!(result, "Successfully moved the file or directory");

        assert!(!data.dir.exists("source.txt"));

        let content = data.dir.read_to_string("subdirectory/source.txt")?;

        assert_eq!(content, "Hello, world!");

        Ok(())
    }

    #[test]
    fn test_move_directory() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.create_dir("source_dir")?;
        data.dir.write("source_dir/inner.txt", "Hello, world!")?;

        let params = Parameters(MoveParams {
            src_path: "source_dir".to_string(),
            dst_path: "destination_dir".to_string(),
        });

        let result = Filesystem::try_move(data.clone(), params)?;

        assert_eq!(result, "Successfully moved the file or directory");

        assert!(!data.dir.exists("source_dir"));

        let content = data.dir.read_to_string("destination_dir/inner.txt")?;

        assert_eq!(content, "Hello, world!");

        Ok(())
    }

    #[test]
    fn test_move_fails_when_source_missing() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let params = Parameters(MoveParams {
            src_path: "does_not_exist.txt".to_string(),
            dst_path: "destination.txt".to_string(),
        });

        let result = Filesystem::try_move(data.clone(), params);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Failed to move the file or directory".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_move_overwrites_when_destination_exists() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.write("source.txt", "New content")?;
        data.dir.write("destination.txt", "Old content")?;

        let params = Parameters(MoveParams {
            src_path: "source.txt".to_string(),
            dst_path: "destination.txt".to_string(),
        });

        let result = Filesystem::try_move(data.clone(), params)?;

        assert_eq!(result, "Successfully moved the file or directory");

        assert!(!data.dir.exists("source.txt"));

        let content = data.dir.read_to_string("destination.txt")?;

        assert_eq!(content, "New content");

        Ok(())
    }
}
