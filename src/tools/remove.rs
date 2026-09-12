use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RemoveParams {
    #[schemars(description = "The path to the file or directory to remove.")]
    path: String,

    #[schemars(
        description = "Whether to recursively remove a directory and all its contents.\n\nIMPORTANT: This MUST be set to `true` to remove a non-empty directory.\n\nDefaults to `false` if not specified."
    )]
    recursive: Option<bool>,
}

#[tool_router(router = tool_router_remove, vis = "pub")]
impl Filesystem {
    #[tool(
        description = "Removes a file or directory.\n\nIMPORTANT: This action is permanent. Always verify the path before calling."
    )]
    async fn remove(&self, parameters: Parameters<RemoveParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("remove", move || Self::try_remove(data, parameters)).await
    }

    fn try_remove(
        data: Arc<FilesystemData>,
        Parameters(RemoveParams { path, recursive }): Parameters<RemoveParams>,
    ) -> Result<String> {
        let (dir, rel_path) = data.get_dir(&path)?;

        let metadata = dir
            .dir
            .symlink_metadata(rel_path)
            .context("Failed to retrieve metadata for the specified path")?;

        if metadata.is_file() || metadata.is_symlink() {
            dir.dir
                .remove_file(rel_path)
                .context("Failed to remove the file")?;

            Ok("Successfully removed the file".to_string())
        } else if metadata.is_dir() {
            let recursive = recursive.unwrap_or(false);

            if recursive {
                dir.dir
                    .remove_dir_all(rel_path)
                    .context("Failed to remove the directory recursively")?;
            } else {
                dir.dir
                    .remove_dir(rel_path)
                    .context("Failed to remove the directory (consider using recursive option for non-empty directories)")?;
            }

            Ok("Successfully removed the directory".to_string())
        } else {
            Ok("The specified path is neither a file nor a directory".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_utils::setup_test_fs;

    fn test_remove_file(recursive: Option<bool>) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test_file.txt", "Hello, world!")?;

        let params = Parameters(RemoveParams {
            path: "test_file.txt".to_string(),
            recursive,
        });

        let result = Filesystem::try_remove(data.clone(), params)?;

        assert_eq!(result, "Successfully removed the file");

        assert!(!data.dirs[0].dir.exists("test_file.txt"));

        Ok(())
    }

    #[test]
    fn test_remove_file_recursive_none() -> Result<()> {
        test_remove_file(None)
    }

    #[test]
    fn test_remove_file_recursive_false() -> Result<()> {
        test_remove_file(Some(false))
    }

    #[test]
    fn test_remove_file_recursive_true() -> Result<()> {
        test_remove_file(Some(true))
    }

    fn test_remove_empty_directory(recursive: Option<bool>) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.create_dir("empty_dir")?;

        let params = Parameters(RemoveParams {
            path: "empty_dir".to_string(),
            recursive,
        });

        let result = Filesystem::try_remove(data.clone(), params)?;

        assert_eq!(result, "Successfully removed the directory");

        assert!(!data.dirs[0].dir.exists("empty_dir"));

        Ok(())
    }

    #[test]
    fn test_remove_empty_directory_recursive_none() -> Result<()> {
        test_remove_empty_directory(None)
    }

    #[test]
    fn test_remove_empty_directory_recursive_false() -> Result<()> {
        test_remove_empty_directory(Some(false))
    }

    #[test]
    fn test_remove_empty_directory_recursive_true() -> Result<()> {
        test_remove_empty_directory(Some(true))
    }

    fn test_remove_non_empty_directory(recursive: Option<bool>) -> Result<String> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.create_dir("non_empty_dir")?;
        data.dirs[0]
            .dir
            .write("non_empty_dir/test_file.txt", "Hello, world!")?;

        let params = Parameters(RemoveParams {
            path: "non_empty_dir".to_string(),
            recursive,
        });

        Filesystem::try_remove(data.clone(), params)
    }

    fn test_remove_non_empty_directory_should_fail_due_to_recursive(
        recursive: Option<bool>,
    ) -> Result<()> {
        assert!(
            recursive != Some(true),
            "This test should not be run with recursive set to true"
        );

        let result = test_remove_non_empty_directory(recursive);

        assert!(
            result.err().map(|e| e.to_string())
                == Some(
                    "Failed to remove the directory (consider using recursive option for non-empty directories)".to_string()
                )
        );

        Ok(())
    }

    #[test]
    fn test_remove_non_empty_directory_recursive_none() -> Result<()> {
        test_remove_non_empty_directory_should_fail_due_to_recursive(None)
    }

    #[test]
    fn test_remove_non_empty_directory_recursive_false() -> Result<()> {
        test_remove_non_empty_directory_should_fail_due_to_recursive(Some(false))
    }

    #[test]
    fn test_remove_non_empty_directory_recursive_true() -> Result<()> {
        let result = test_remove_non_empty_directory(Some(true))?;

        assert_eq!(result, "Successfully removed the directory");

        Ok(())
    }
}
