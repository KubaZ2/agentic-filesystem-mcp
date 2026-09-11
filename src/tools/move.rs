use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData};

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
        let data = self.data.clone();
        Self::run_simple("move", move || Self::try_move(data, parameters)).await
    }

    fn try_move(
        data: Arc<FilesystemData>,
        Parameters(MoveParams { src_path, dst_path }): Parameters<MoveParams>,
    ) -> Result<String> {
        let (src_dir, rel_src_path) = data.get_dir(&src_path)?;
        let (dst_dir, rel_dst_path) = data.get_dir(&dst_path)?;

        src_dir
            .dir
            .rename(rel_src_path, &dst_dir.dir, rel_dst_path)
            .context("Failed to move the file or directory")?;

        Ok("Successfully moved the file or directory".to_string())
    }
}
