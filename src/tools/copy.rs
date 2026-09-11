use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData, copy_recursive::copy_recursive};

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
            copy_recursive(&src_dir.dir, rel_src_path, &dst_dir.dir, rel_dst_path)?;
        } else {
            src_dir
                .dir
                .copy(rel_src_path, &dst_dir.dir, rel_dst_path)
                .context("Failed to copy the file")?;
        }

        Ok("Successfully copied the file or directory".to_string())
    }
}
