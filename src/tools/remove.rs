use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};

use crate::Filesystem;

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
        match self.try_remove(parameters).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(result)]),
            Err(err) => {
                Self::log_tool_error("remove", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_remove(
        &self,
        Parameters(RemoveParams { path, recursive }): Parameters<RemoveParams>,
    ) -> Result<String> {
        let abs_src_path = self.get_abs_path(&path)?;

        let metadata = tokio::fs::metadata(&abs_src_path)
            .await
            .context("Failed to get metadata for the file or directory")?;

        if metadata.is_dir() {
            let recursive = recursive.unwrap_or(false);

            if recursive {
                tokio::fs::remove_dir_all(&abs_src_path)
                    .await
                    .context("Failed to remove the directory recursively")?;
            } else {
                tokio::fs::remove_dir(&abs_src_path).await
                    .context("Failed to remove the directory (consider using recursive option for non-empty directories)")?;
            }

            Ok("Successfully removed the directory".to_string())
        } else {
            tokio::fs::remove_file(&abs_src_path)
                .await
                .context("Failed to remove the file")?;

            Ok("Successfully removed the file".to_string())
        }
    }
}
