use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};

use crate::Filesystem;

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
        description = "Moves or renames a file or directory.\n\nIMPORTANT: This operation fails if the destination path already exists."
    )]
    async fn r#move(&self, parameters: Parameters<MoveParams>) -> CallToolResult {
        match self.try_move(parameters).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(result)]),
            Err(err) => {
                Self::log_tool_error("move", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_move(
        &self,
        Parameters(MoveParams { src_path, dst_path }): Parameters<MoveParams>,
    ) -> Result<String> {
        let abs_src_path = self.get_abs_path(&src_path)?;
        let abs_dst_path = self.get_abs_path(&dst_path)?;

        tokio::fs::rename(&abs_src_path, &abs_dst_path)
            .await
            .context("Failed to move the file or directory")?;

        Ok("Successfully moved the file or directory".to_string())
    }
}
