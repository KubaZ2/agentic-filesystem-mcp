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
        let (dir, rel_path) = data.get_dir(&path)?;

        let parents = parents.unwrap_or(false);

        if parents {
            dir.dir
                .create_dir_all(&rel_path)
                .context("Failed to create the directory with parents")?;
        } else {
            dir.dir
                .create_dir(&rel_path)
                .context("Failed to create the directory")?;
        }

        Ok("Successfully created the directory".to_string())
    }
}
