use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData};

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
        let data = self.data.clone();
        Self::run_simple("remove", move || Self::try_remove(data, parameters)).await
    }

    fn try_remove(
        data: Arc<FilesystemData>,
        Parameters(RemoveParams { path, recursive }): Parameters<RemoveParams>,
    ) -> Result<String> {
        let (dir, rel_path) = data.get_dir(&path)?;

        let metadata = dir
            .dir
            .symlink_metadata(&rel_path)
            .context("Failed to retrieve metadata for the specified path")?;

        if metadata.is_file() || metadata.is_symlink() {
            dir.dir
                .remove_file(&rel_path)
                .context("Failed to remove the file")?;

            Ok("Successfully removed the file".to_string())
        } else if metadata.is_dir() {
            let recursive = recursive.unwrap_or(false);

            if recursive {
                dir.dir
                    .remove_dir_all(&rel_path)
                    .context("Failed to remove the directory recursively")?;
            } else {
                dir.dir
                    .remove_dir(&rel_path)
                    .context("Failed to remove the directory (consider using recursive option for non-empty directories)")?;
            }

            Ok("Successfully removed the directory".to_string())
        } else {
            Ok("The specified path is neither a file nor a directory".to_string())
        }
    }
}
