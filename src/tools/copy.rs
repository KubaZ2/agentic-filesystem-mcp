use anyhow::{Context, Result};
use parcopy::CopyBuilder;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};

use crate::Filesystem;

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
        match self.try_copy(parameters).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(result)]),
            Err(err) => {
                Self::log_tool_error("copy", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_copy(
        &self,
        Parameters(CopyParams {
            src_path,
            dst_path,
            recursive,
        }): Parameters<CopyParams>,
    ) -> Result<String> {
        let abs_src_path = self.get_abs_path(&src_path)?;
        let abs_dst_path = self.get_abs_path(&dst_path)?;

        let builder = CopyBuilder::new(&abs_src_path, &abs_dst_path).error_on_conflict();

        if recursive.unwrap_or(false) && abs_src_path.is_dir() {
            builder
                .run_dir()
                .context("Failed to copy the directory recursively")?;

            Ok("Successfully copied the directory recursively".to_string())
        } else {
            builder.run_file().context("Failed to copy the file")?;

            Ok("Successfully copied the file".to_string())
        }
    }
}
