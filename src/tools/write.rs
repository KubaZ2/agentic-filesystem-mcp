use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::Filesystem;

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct WriteParams {
    #[schemars(description = "The path to the file to write.")]
    path: String,

    #[schemars(description = "The complete content to write to the file.")]
    content: String,
}

#[tool_router(router = tool_router_write, vis = "pub")]
impl Filesystem {
    #[tool(
        description = "Writes a file, automatically creating any missing parent directories. Completely overwrites the file if one already exists.\n\nIMPORTANT: Because it overwrites entirely, ensure you have the complete file context before modifying existing files. For partial changes to existing files, prefer using the `edit` tool."
    )]
    async fn write(&self, parameters: Parameters<WriteParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("write", move || Self::try_write(data, parameters)).await
    }

    fn try_write(
        data: std::sync::Arc<crate::FilesystemData>,
        Parameters(WriteParams { path, content }): Parameters<WriteParams>,
    ) -> Result<String> {
        let (dir, rel_path) = data.get_dir(&path)?;

        if let Some(parent) = rel_path.parent() {
            dir.dir
                .create_dir_all(parent)
                .context("Failed to create parent directories for the file")?;
        }

        dir.dir
            .write(rel_path, content)
            .context("Failed to write to the file")?;

        Ok("Successfully wrote the file".to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    use crate::tools::test_utils::setup_test_fs;

    #[test]
    fn test_write_creates_new_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let params = Parameters(WriteParams {
            path: "test_file.txt".to_string(),
            content: "Hello, world!".to_string(),
        });

        let result = Filesystem::try_write(data.clone(), params)?;

        assert_eq!(result, "Successfully wrote the file");

        let mut file = data.dirs[0].dir.open("test_file.txt")?;

        let mut content = String::new();
        file.read_to_string(&mut content)?;
        assert_eq!(content, "Hello, world!");

        Ok(())
    }

    #[test]
    fn test_write_creates_parent_directories() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let params = Parameters(WriteParams {
            path: "deeply/nested/directory/test_file.txt".to_string(),
            content: "Hello from the nest!".to_string(),
        });

        let result = Filesystem::try_write(data.clone(), params)?;
        assert_eq!(result, "Successfully wrote the file");

        let mut file = data.dirs[0]
            .dir
            .open("deeply/nested/directory/test_file.txt")?;

        let mut content = String::new();
        file.read_to_string(&mut content)?;
        assert_eq!(content, "Hello from the nest!");

        Ok(())
    }

    #[test]
    fn test_write_overwrites_existing_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let initial_params = Parameters(WriteParams {
            path: "overwrite_me.txt".to_string(),
            content: "Initial content".to_string(),
        });
        Filesystem::try_write(data.clone(), initial_params)?;

        let overwrite_params = Parameters(WriteParams {
            path: "overwrite_me.txt".to_string(),
            content: "New content".to_string(),
        });
        Filesystem::try_write(data.clone(), overwrite_params)?;

        let mut file = data.dirs[0].dir.open("overwrite_me.txt")?;

        let mut content = String::new();
        file.read_to_string(&mut content)?;
        assert_eq!(content, "New content");

        Ok(())
    }
}
