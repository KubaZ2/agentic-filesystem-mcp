use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct MkdirParams {
    #[schemars(description = "The path to the directory to create.")]
    path: String,

    #[schemars(
        description = "Whether to create parent directories as needed (equivalent to `mkdir -p`). If `true`, no error is thrown if the directory already exists.\n\nDefaults to `false` if not specified."
    )]
    parents: Option<bool>,
}

#[tool_router(router = tool_router_mkdir, vis = "pub")]
impl Filesystem {
    #[tool(
        description = "Creates a new directory.\n\nIMPORTANT: The `write` tool automatically creates missing parent directories. You DO NOT need to call `mkdir` prior to writing a new file with the `write` tool."
    )]
    async fn mkdir(&self, parameters: Parameters<MkdirParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("mkdir", move || Self::try_mkdir(data, parameters)).await
    }

    fn try_mkdir(
        data: Arc<FilesystemData>,
        Parameters(MkdirParams { path, parents }): Parameters<MkdirParams>,
    ) -> Result<String> {
        let parents = parents.unwrap_or(false);

        if parents {
            data.dir
                .create_dir_all(&path)
                .context("Failed to create the directory with parents")?;
        } else {
            data.dir
                .create_dir(&path)
                .context("Failed to create the directory")?;
        }

        Ok("Successfully created the directory".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_utils::setup_test_fs;

    fn test_mkdir_creates_new_directory(parents: Option<bool>) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let params = Parameters(MkdirParams {
            path: "new_dir".to_string(),
            parents,
        });

        let result = Filesystem::try_mkdir(data.clone(), params)?;

        assert_eq!(result, "Successfully created the directory");

        assert!(data.dir.exists("new_dir"));

        Ok(())
    }

    #[test]
    fn test_mkdir_creates_new_directory_parents_none() -> Result<()> {
        test_mkdir_creates_new_directory(None)
    }

    #[test]
    fn test_mkdir_creates_new_directory_parents_false() -> Result<()> {
        test_mkdir_creates_new_directory(Some(false))
    }

    #[test]
    fn test_mkdir_creates_new_directory_parents_true() -> Result<()> {
        test_mkdir_creates_new_directory(Some(true))
    }

    fn test_mkdir_creates_nested_directory(parents: Option<bool>) -> Result<String> {
        let (_tempdir, data) = setup_test_fs()?;

        let params = Parameters(MkdirParams {
            path: "nested/dir".to_string(),
            parents,
        });

        Filesystem::try_mkdir(data.clone(), params)
    }

    fn test_mkdir_creates_nested_directory_should_fail_due_to_parents(
        parents: Option<bool>,
    ) -> Result<()> {
        let result = test_mkdir_creates_nested_directory(parents);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Failed to create the directory".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_mkdir_creates_nested_directory_with_parents_none() -> Result<()> {
        test_mkdir_creates_nested_directory_should_fail_due_to_parents(None)
    }

    #[test]
    fn test_mkdir_creates_nested_directory_with_parents_true() -> Result<()> {
        let result = test_mkdir_creates_nested_directory(Some(true))?;

        assert_eq!(result, "Successfully created the directory");

        Ok(())
    }

    #[test]
    fn test_mkdir_creates_nested_directory_with_parents_false() -> Result<()> {
        test_mkdir_creates_nested_directory_should_fail_due_to_parents(Some(false))
    }

    fn test_mkdir_existing_dir(parents: Option<bool>) -> Result<String> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.create_dir("existing_dir")?;

        let params = Parameters(MkdirParams {
            path: "existing_dir".to_string(),
            parents,
        });

        Filesystem::try_mkdir(data.clone(), params)
    }

    fn test_mkdir_existing_dir_should_fail_due_to_parents(parents: Option<bool>) -> Result<()> {
        let result = test_mkdir_existing_dir(parents);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Failed to create the directory".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_mkdir_existing_dir_with_parents_none() -> Result<()> {
        test_mkdir_existing_dir_should_fail_due_to_parents(None)
    }

    #[test]
    fn test_mkdir_existing_dir_with_parents_true() -> Result<()> {
        let result = test_mkdir_existing_dir(Some(true))?;

        assert_eq!(result, "Successfully created the directory");

        Ok(())
    }

    #[test]
    fn test_mkdir_existing_dir_with_parents_false() -> Result<()> {
        test_mkdir_existing_dir_should_fail_due_to_parents(Some(false))
    }
}
