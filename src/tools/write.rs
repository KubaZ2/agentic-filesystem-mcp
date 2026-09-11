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
