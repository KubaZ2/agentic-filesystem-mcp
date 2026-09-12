use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData, copy::copy_file, copy::copy_recursive};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct CopyParams {
    #[schemars(description = "The source path to the file or directory to copy.")]
    src_path: String,

    #[schemars(
        description = "The destination path.\n\nIMPORTANT: This must include the target file or directory name, not just the destination folder."
    )]
    dst_path: String,

    #[schemars(
        description = "Whether to recursively copy a directory and its contents.\n\nIMPORTANT: This MUST be set to `true` when copying a directory, otherwise the operation will fail.\n\nDefaults to `false` if not specified."
    )]
    recursive: Option<bool>,
}

#[tool_router(router = tool_router_copy, vis = "pub")]
impl Filesystem {
    #[tool(
        description = "Copies a file or directory to a new location.\n\nIMPORTANT: This operation fails if the destination path already exists."
    )]
    async fn copy(&self, parameters: Parameters<CopyParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("copy", move || Self::try_copy(data, parameters)).await
    }

    fn try_copy(
        data: Arc<FilesystemData>,
        Parameters(CopyParams {
            src_path,
            dst_path,
            recursive,
        }): Parameters<CopyParams>,
    ) -> Result<String> {
        let (src_dir, rel_src_path) = data.get_dir(&src_path)?;
        let (dst_dir, rel_dst_path) = data.get_dir(&dst_path)?;

        if recursive.unwrap_or(false) {
            copy_recursive(&src_dir.dir, rel_src_path, &dst_dir.dir, rel_dst_path)
                .context("Failed to copy the file or directory recursively")?;
        } else {
            copy_file(&src_dir.dir, rel_src_path, &dst_dir.dir, rel_dst_path)
                .context("Failed to copy the file")?;
        }

        Ok("Successfully copied the file or directory".to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::test_utils::setup_test_fs;

    use anyhow::Result;
    use cap_tempfile::TempDir;

    use super::*;

    fn test_copy_file(recursive: Option<bool>) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "Hello, world!")?;

        let params = Parameters(CopyParams {
            src_path: "test.txt".to_string(),
            dst_path: "copy.txt".to_string(),
            recursive,
        });

        let result = Filesystem::try_copy(data.clone(), params)?;

        assert_eq!(result, "Successfully copied the file or directory");

        let original_content = data.dirs[0].dir.read_to_string("test.txt")?;

        assert_eq!(original_content, "Hello, world!");

        let copied_content = data.dirs[0].dir.read_to_string("copy.txt")?;

        assert_eq!(copied_content, "Hello, world!");

        Ok(())
    }

    #[test]
    fn test_copy_file_recursive_none() -> Result<()> {
        test_copy_file(None)
    }

    #[test]
    fn test_copy_file_recursive_false() -> Result<()> {
        test_copy_file(Some(false))
    }

    #[test]
    fn test_copy_file_recursive_true() -> Result<()> {
        test_copy_file(Some(true))
    }

    fn test_copy_empty_dir(
        recursive: Option<bool>,
    ) -> Result<(TempDir, Arc<FilesystemData>, Result<String>)> {
        let (tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.create_dir("empty_dir")?;

        let params = Parameters(CopyParams {
            src_path: "empty_dir".to_string(),
            dst_path: "copy_empty_dir".to_string(),
            recursive,
        });

        Ok((tempdir, data.clone(), Filesystem::try_copy(data, params)))
    }

    fn test_copy_empty_dir_should_fail_due_to_recursive(recursive: Option<bool>) -> Result<()> {
        assert!(
            !recursive.unwrap_or(false),
            "This test is only valid for non-recursive copy"
        );

        let (_, _, result) = test_copy_empty_dir(recursive)?;

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Failed to copy the file".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_copy_empty_dir_recursive_none() -> Result<()> {
        test_copy_empty_dir_should_fail_due_to_recursive(None)
    }

    #[test]
    fn test_copy_empty_dir_recursive_false() -> Result<()> {
        test_copy_empty_dir_should_fail_due_to_recursive(Some(false))
    }

    #[test]
    fn test_copy_empty_dir_recursive_true() -> Result<()> {
        let (_tempdir, data, result) = test_copy_empty_dir(Some(true))?;

        let result = result?;

        assert_eq!(result, "Successfully copied the file or directory");

        assert!(data.dirs[0].dir.exists("copy_empty_dir"));

        assert!(
            data.dirs[0]
                .dir
                .read_dir("copy_empty_dir")?
                .next()
                .is_none()
        );

        Ok(())
    }

    fn test_copy_file_dst_exists_fails(
        recursive: Option<bool>,
        expected_message: &str,
    ) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "Hello, world!")?;
        data.dirs[0].dir.write("copy.txt", "Existing file")?;

        let params = Parameters(CopyParams {
            src_path: "test.txt".to_string(),
            dst_path: "copy.txt".to_string(),
            recursive,
        });

        let result = Filesystem::try_copy(data.clone(), params);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some(expected_message.to_string())
        );

        Ok(())
    }

    #[test]
    fn test_copy_file_dst_exists_recursive_none_fails() -> Result<()> {
        test_copy_file_dst_exists_fails(None, "Failed to copy the file")
    }

    #[test]
    fn test_copy_file_dst_exists_recursive_false_fails() -> Result<()> {
        test_copy_file_dst_exists_fails(Some(false), "Failed to copy the file")
    }

    #[test]
    fn test_copy_file_dst_exists_recursive_true_fails() -> Result<()> {
        test_copy_file_dst_exists_fails(
            Some(true),
            "Failed to copy the file or directory recursively",
        )
    }

    #[test]
    fn test_copy_file_dst_is_dir_fails() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "Hello, world!")?;
        data.dirs[0].dir.create_dir("copy_dir")?;

        let params = Parameters(CopyParams {
            src_path: "test.txt".to_string(),
            dst_path: "copy_dir".to_string(),
            recursive: Some(false),
        });

        let result = Filesystem::try_copy(data.clone(), params);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Failed to copy the file".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_copy_dir_dst_exists_recursive_true_fails() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.create_dir("test_dir")?;
        data.dirs[0].dir.create_dir("copy_test_dir")?;

        let params = Parameters(CopyParams {
            src_path: "test_dir".to_string(),
            dst_path: "copy_test_dir".to_string(),
            recursive: Some(true),
        });

        let result = Filesystem::try_copy(data.clone(), params);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Failed to copy the file or directory recursively".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_copy_dir_dst_is_file_recursive_true_fails() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.create_dir("test_dir")?;
        data.dirs[0].dir.write("copy_test_file", "Existing file")?;

        let params = Parameters(CopyParams {
            src_path: "test_dir".to_string(),
            dst_path: "copy_test_file".to_string(),
            recursive: Some(true),
        });

        let result = Filesystem::try_copy(data.clone(), params);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Failed to copy the file or directory recursively".to_string())
        );

        Ok(())
    }

    fn test_copy_symlink_recursive_none_or_false_follows_links(
        recursive: Option<bool>,
    ) -> Result<()> {
        assert!(
            !recursive.unwrap_or(false),
            "This test is only valid for non-recursive copy"
        );

        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "Hello, world!")?;

        #[cfg(not(windows))]
        {
            data.dirs[0]
                .dir
                .symlink_contents("test.txt", "test_symlink")?;
        }

        #[cfg(windows)]
        {
            data.dirs[0].dir.symlink_file("test.txt", "test_symlink")?;
        }

        let params = Parameters(CopyParams {
            src_path: "test_symlink".to_string(),
            dst_path: "copy_symlink".to_string(),
            recursive,
        });

        let result = Filesystem::try_copy(data.clone(), params)?;

        assert_eq!(result, "Successfully copied the file or directory");

        let original_content = data.dirs[0].dir.read_to_string("test.txt")?;

        assert_eq!(original_content, "Hello, world!");

        let copied_content = data.dirs[0].dir.read_to_string("copy_symlink")?;

        assert_eq!(copied_content, "Hello, world!");

        assert!(
            !data.dirs[0]
                .dir
                .symlink_metadata("copy_symlink")?
                .file_type()
                .is_symlink()
        );

        Ok(())
    }

    #[test]
    fn test_copy_symlink_recursive_none_follows_links() -> Result<()> {
        test_copy_symlink_recursive_none_or_false_follows_links(None)
    }

    #[test]
    fn test_copy_symlink_recursive_false_follows_links() -> Result<()> {
        test_copy_symlink_recursive_none_or_false_follows_links(Some(false))
    }

    #[test]
    fn test_copy_symlink_recursive_true_copies_links() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "Hello, world!")?;

        #[cfg(not(windows))]
        {
            data.dirs[0]
                .dir
                .symlink_contents("test.txt", "test_symlink")?;
        }

        #[cfg(windows)]
        {
            data.dirs[0].dir.symlink_file("test.txt", "test_symlink")?;
        }

        let params = Parameters(CopyParams {
            src_path: "test_symlink".to_string(),
            dst_path: "copy_symlink".to_string(),
            recursive: Some(true),
        });

        let result = Filesystem::try_copy(data.clone(), params)?;

        assert_eq!(result, "Successfully copied the file or directory");

        let original_content = data.dirs[0].dir.read_to_string("test.txt")?;

        assert_eq!(original_content, "Hello, world!");

        let copied_content = data.dirs[0].dir.read_to_string("copy_symlink")?;

        assert_eq!(copied_content, "Hello, world!");

        assert!(
            data.dirs[0]
                .dir
                .symlink_metadata("copy_symlink")?
                .file_type()
                .is_symlink()
        );

        assert_eq!(
            data.dirs[0]
                .dir
                .read_link_contents("copy_symlink")?
                .as_path(),
            "test.txt"
        );

        Ok(())
    }

    #[cfg(not(windows))] // cap std does not support absolute symlinks on Windows
    #[test]
    fn test_copy_symlink_recursive_true_copies_absolute_links() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .symlink_contents("/some/absolute/path/test.txt", "test_symlink")?;

        let params = Parameters(CopyParams {
            src_path: "test_symlink".to_string(),
            dst_path: "copy_symlink".to_string(),
            recursive: Some(true),
        });

        let result = Filesystem::try_copy(data.clone(), params)?;

        assert_eq!(result, "Successfully copied the file or directory");

        assert!(
            data.dirs[0]
                .dir
                .symlink_metadata("copy_symlink")?
                .file_type()
                .is_symlink()
        );

        assert_eq!(
            data.dirs[0]
                .dir
                .read_link_contents("copy_symlink")?
                .as_path(),
            "/some/absolute/path/test.txt"
        );

        Ok(())
    }

    #[cfg(not(windows))]
    // cap std hides Windows-specific metadata extensions behind a feature flag,
    // the current copy implementation follows the symlink to detect whether
    // the symlink targets a directory or a file
    #[test]
    fn test_copy_symlink_recursive_true_copies_broken_links() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .symlink_contents("nonexistent_target.txt", "test_symlink")?;

        let params = Parameters(CopyParams {
            src_path: "test_symlink".to_string(),
            dst_path: "copy_symlink".to_string(),
            recursive: Some(true),
        });

        let result = Filesystem::try_copy(data.clone(), params)?;

        assert_eq!(result, "Successfully copied the file or directory");

        assert!(
            data.dirs[0]
                .dir
                .symlink_metadata("copy_symlink")?
                .file_type()
                .is_symlink()
        );

        assert_eq!(
            data.dirs[0]
                .dir
                .read_link_contents("copy_symlink")?
                .as_path(),
            "nonexistent_target.txt"
        );

        Ok(())
    }
}
