use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
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
        match self.try_write(parameters).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(result)]),
            Err(err) => {
                Self::log_tool_error("write", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_write(
        &self,
        Parameters(WriteParams { path, content }): Parameters<WriteParams>,
    ) -> Result<String> {
        let abs_path = self.get_abs_path(&path)?;

        if let Some(parent) = abs_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create parent directories for the file")?;
        }

        tokio::fs::write(&abs_path, content)
            .await
            .context("Failed to write to the file")?;

        Ok("Successfully wrote the file".to_string())
    }
}
