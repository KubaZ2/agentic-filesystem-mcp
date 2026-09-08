use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};

use crate::Filesystem;

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
        match self.try_mkdir(parameters).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(result)]),
            Err(err) => {
                Self::log_tool_error("mkdir", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_mkdir(
        &self,
        Parameters(MkdirParams { path, parents }): Parameters<MkdirParams>,
    ) -> Result<String> {
        let abs_path = self.get_abs_path(&path)?;

        let parents = parents.unwrap_or(false);

        if parents {
            tokio::fs::create_dir_all(&abs_path).await
        } else {
            tokio::fs::create_dir(&abs_path).await
        }
        .context("Failed to create the directory")?;

        Ok("Successfully created the directory".to_string())
    }
}
