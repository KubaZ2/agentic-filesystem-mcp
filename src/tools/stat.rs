use std::sync::Arc;

use anyhow::{Context, Result};
use cap_std::fs::{FileType, PermissionsExt};
use chrono::{DateTime, Utc};
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData, fs::VfsMetadata, path_sanitizer::sanitize_path};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct StatParams {
    #[schemars(description = "The path of the file or directory to get information about.")]
    path: String,
}

#[tool_router(router = tool_router_stat, vis = "pub")]
impl Filesystem {
    #[tool]
    async fn stat(&self, parameters: Parameters<StatParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("mkdir", move || Self::try_stat(data, parameters)).await
    }

    fn try_stat(
        data: Arc<FilesystemData>,
        Parameters(StatParams { path }): Parameters<StatParams>,
    ) -> Result<String> {
        let path = sanitize_path(&path)?;

        let metadata = data.dir.symlink_metadata(path)?;

        let metadata = match metadata {
            VfsMetadata::Real(metadata) => metadata,
            VfsMetadata::Virtual => return Ok("This is a virtual directory".to_string()),
        };

        let file_type = match metadata.file_type() {
            _ if metadata.is_dir() => "Directory",
            _ if metadata.is_file() => "File",
            _ if metadata.is_symlink() => "Symlink",
            _ => "Unknown",
        };

        let size = metadata.len();

        let created: DateTime<Utc> = metadata
            .created()
            .context("Failed to get creation time")?
            .into_std()
            .into();

        let modified: DateTime<Utc> = metadata
            .modified()
            .context("Failed to get modification time")?
            .into_std()
            .into();

        let accessed: DateTime<Utc> = metadata
            .accessed()
            .context("Failed to get access time")?
            .into_std()
            .into();

        let permissions = metadata.permissions().mode() & 0o777;

        Ok(format!(
            "Type: {file_type}
Size: {size}
Created: {created}
Modified: {modified}
Accessed: {accessed}
Permissions: {permissions:o}",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_utils::setup_test_fs;
}
