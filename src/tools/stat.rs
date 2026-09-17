use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData, fs::VfsMetadata, path_sanitizer::sanitize_path};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct StatParams {
    #[schemars(description = "The path of the file or directory to get information about.")]
    path: String,
}

#[tool_router(router = tool_router_stat, vis = "pub")]
impl Filesystem {
    #[tool]
    async fn stat(&self, parameters: Parameters<StatParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("stat", move || Self::try_stat(data, parameters)).await
    }

    fn try_stat(
        data: Arc<FilesystemData>,
        Parameters(StatParams { path }): Parameters<StatParams>,
    ) -> Result<String> {
        let path = sanitize_path(&path)?;

        let metadata = data.dir.symlink_metadata(path)?;

        let metadata = match metadata {
            VfsMetadata::Real(metadata) => metadata,
            VfsMetadata::Virtual => return Ok("This is a virtual directory".to_string()),
        };

        let file_type = match metadata.file_type() {
            _ if metadata.is_dir() => "Directory",
            _ if metadata.is_file() => "File",
            _ if metadata.is_symlink() => "Symlink",
            _ => "Unknown",
        };

        let size = metadata.len();

        let created: DateTime<Utc> = metadata
            .created()
            .context("Failed to get creation time")?
            .into_std()
            .into();

        let modified: DateTime<Utc> = metadata
            .modified()
            .context("Failed to get modification time")?
            .into_std()
            .into();

        let accessed: DateTime<Utc> = metadata
            .accessed()
            .context("Failed to get access time")?
            .into_std()
            .into();

        let permissions = metadata.permissions();

        let permissions_str = {
            #[cfg(unix)]
            {
                use cap_std::fs::PermissionsExt;

                format!("{:o}", permissions.mode() & 0o777)
            }

            #[cfg(not(unix))]
            {
                if permissions.readonly() {
                    "Read-only"
                } else {
                    "Read-write"
                }
                .to_string()
            }
        };

        Ok(format!(
            "Type: {file_type}
Size: {size}
Created: {created}
Modified: {modified}
Accessed: {accessed}
Permissions: {permissions_str}",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::time::{Duration, UNIX_EPOCH};

    use crate::tools::test_utils::{setup_test_fs, setup_virtual_fs};

    #[test]
    fn test_stat_existing_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        let path = "test_file.txt";
        let content = "Hello, world! This is test content.";
        data.dir.write(path, content)?;

        let modified_time = UNIX_EPOCH + Duration::from_secs(1650000000);
        let accessed_time = UNIX_EPOCH + Duration::from_secs(1660000000);

        let actual_created: DateTime<Utc>;

        {
            let cap_file = data.dir.open(path)?;
            let std_file = cap_file.into_std();

            let times = std::fs::FileTimes::new()
                .set_accessed(accessed_time)
                .set_modified(modified_time);

            std_file.set_times(times)?;

            let metadata = std_file.metadata()?;

            let mut perms = metadata.permissions();

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(0o644);
            }

            #[cfg(not(unix))]
            {
                perms.set_readonly(false);
            }

            std_file.set_permissions(perms)?;

            actual_created = metadata.created()?.into();
        }

        let params = Parameters(StatParams {
            path: path.to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert!(
            result.contains("Type: File"),
            "Expected 'Type: File', got: {result}"
        );
        assert!(
            result.contains(&format!("Size: {}", content.len())),
            "Expected correct size, got: {result}"
        );

        let expected_modified: DateTime<Utc> = modified_time.into();
        let expected_accessed: DateTime<Utc> = accessed_time.into();

        assert!(
            result.contains(&format!("Created: {actual_created}")),
            "Expected 'Created: {actual_created}', got: {result}"
        );

        assert!(
            result.contains(&format!("Modified: {expected_modified}")),
            "Expected 'Modified: {expected_modified}', got: {result}"
        );

        assert!(
            result.contains(&format!("Accessed: {expected_accessed}")),
            "Expected 'Accessed: {expected_accessed}', got: {result}"
        );

        #[cfg(unix)]
        assert!(
            result.contains("Permissions: 644"),
            "Expected 'Permissions: 644', got: {result}"
        );
        #[cfg(not(unix))]
        assert!(
            result.contains("Permissions: Read-write"),
            "Expected 'Permissions: Read-write', got: {result}"
        );

        Ok(())
    }

    #[test]
    fn test_stat_permissions_explicit() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        let path = "perm_file.txt";
        data.dir.write(path, "content")?;

        {
            let cap_file = data.dir.open(path)?;
            let std_file = cap_file.into_std();

            let mut perms = std_file.metadata()?.permissions();

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(0o444);
            }
            #[cfg(not(unix))]
            {
                perms.set_readonly(true);
            }

            std_file.set_permissions(perms)?;
        }

        let params = Parameters(StatParams {
            path: path.to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        #[cfg(unix)]
        assert!(
            result.contains("Permissions: 444"),
            "Expected 'Permissions: 444', got: {result}"
        );
        #[cfg(not(unix))]
        assert!(
            result.contains("Permissions: Read-only"),
            "Expected 'Permissions: Read-only', got: {result}"
        );

        Ok(())
    }

    #[test]
    fn test_stat_empty_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        data.dir.create("empty_file.txt")?;

        let params = Parameters(StatParams {
            path: "empty_file.txt".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert!(
            result.contains("Type: File"),
            "Expected 'Type: File', got: {result}"
        );
        assert!(
            result.contains("Size: 0"),
            "Expected 'Size: 0', got: {result}"
        );

        Ok(())
    }

    #[test]
    fn test_stat_directory() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        data.dir.create_dir("test_dir")?;

        let params = Parameters(StatParams {
            path: "test_dir".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert!(
            result.contains("Type: Directory"),
            "Expected 'Type: Directory', got: {result}"
        );

        Ok(())
    }

    #[test]
    fn test_stat_nonexistent_path() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let params = Parameters(StatParams {
            path: "nonexistent_file.txt".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params);

        assert!(
            result.is_err(),
            "Expected error for nonexistent path, got: {result:?}"
        );

        Ok(())
    }

    #[test]
    fn test_stat_nested_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        data.dir.create_dir_all("subdir")?;
        data.dir.write("subdir/test.txt", "nested content")?;

        let params = Parameters(StatParams {
            path: "subdir/test.txt".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert!(
            result.contains("Type: File"),
            "Expected 'Type: File', got: {result}"
        );
        assert!(
            result.contains("Size: 14"),
            "Expected 'Size: 14', got: {result}"
        );

        Ok(())
    }

    #[cfg(not(windows))]
    #[test]
    fn test_stat_symlink() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        data.dir.write("target.txt", "symlink target")?;
        data.dir.symlink_contents("target.txt", "link.txt")?;

        let params = Parameters(StatParams {
            path: "link.txt".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert!(
            result.contains("Type: Symlink"),
            "Expected 'Type: Symlink', got: {result}"
        );

        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn test_stat_symlink() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        data.dir.write("target.txt", "symlink target")?;
        data.dir.symlink_contents_file("target.txt", "link.txt")?;

        let params = Parameters(StatParams {
            path: "link.txt".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert!(
            result.contains("Type: Symlink"),
            "Expected 'Type: Symlink', got: {result}"
        );

        Ok(())
    }

    #[test]
    fn test_stat_virtual_directory() -> Result<()> {
        let (_tempdirs, data) = setup_virtual_fs(&[OsStr::new("virtual_dir/real_dir")])?;

        let params = Parameters(StatParams {
            path: "virtual_dir".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert_eq!(result, "This is a virtual directory");

        Ok(())
    }

    #[test]
    fn test_stat_file_size_exact() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;
        let content = "a".repeat(1234);
        data.dir.write("sized_file.txt", &content)?;

        let params = Parameters(StatParams {
            path: "sized_file.txt".to_string(),
        });
        let result = Filesystem::try_stat(data.clone(), params)?;

        assert!(
            result.contains("Size: 1234"),
            "Expected 'Size: 1234', got: {result}"
        );

        Ok(())
    }
}
