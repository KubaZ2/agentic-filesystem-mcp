use std::{cmp::Reverse, collections::BinaryHeap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{
    Filesystem, FilesystemData,
    cap_ignore_walker::{CapIgnoreWalker, RunEntry},
};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct GlobParams {
    #[schemars(
        description = "The glob pattern to match.\n\nIMPORTANT: Patterns like `*.ts` or `src/*.rs` are automatically recursive in this tool. To search ONLY the top-level directory, you MUST use a leading slash (e.g., `/*.ts` or `/src/*.rs`)."
    )]
    pattern: String,

    #[schemars(
        description = "The directory to search in.\n\nDefaults to `\".\"` if not specified."
    )]
    path: Option<String>,

    #[schemars(
        description = "The maximum number of results to return. Useful for preventing token overflow when a pattern matches thousands of files.\n\nDefaults to `100` if not specified."
    )]
    limit: Option<usize>,

    #[schemars(
        description = "The number of results to skip. Used in combination with limit to paginate through large sets of matching files.\n\nDefaults to `0` if not specified."
    )]
    offset: Option<usize>,
}

#[tool_router(router = tool_router_glob, vis = "pub")]
impl Filesystem {
    #[tool(
        description = "Searches for files or directories matching a glob pattern and returns them sorted by modification time."
    )]
    async fn glob(&self, parameters: Parameters<GlobParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("glob", move || Self::try_glob(data, parameters)).await
    }

    fn try_glob(
        data: Arc<FilesystemData>,
        Parameters(GlobParams {
            pattern,
            path,
            limit,
            offset,
        }): Parameters<GlobParams>,
    ) -> Result<String> {
        let dirs = data.get_search_dirs(&path)?;

        let mut override_builder = OverrideBuilder::new(
            path.as_ref()
                .map_or_else(|| Path::new("."), |p| Path::new(p)),
        );

        override_builder
            .add(&pattern)
            .context("Invalid glob pattern")?;

        let r#override = override_builder
            .build()
            .context("Failed to build glob override")?;

        let walk = CapIgnoreWalker::new(vec![r#override], dirs);

        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(Self::DEFAULT_LIMIT);

        let results_limit = offset + limit;

        let mut total_results: usize = 0;

        let mut results = BinaryHeap::new();

        walk.run(|entry| {
            let (entry, entry_path) = match entry {
                RunEntry::Match(entry, path) => (entry, path),
                RunEntry::Error(err) => {
                    Self::log_tool_warning("glob", &err);
                    return Ok(());
                }
            };

            let modified_time = match entry.metadata().and_then(|metadata| metadata.modified()) {
                Ok(time) => time,
                Err(err) => {
                    Self::log_tool_warning("glob", &anyhow::Error::from(err));
                    return Ok(());
                }
            };

            let display_path = match path {
                Some(ref p) => entry_path.strip_prefix(p)?.display().to_string(),
                None => entry_path.display().to_string(),
            };

            total_results += 1;

            results.push(Reverse((modified_time, display_path)));

            if results.len() > results_limit {
                results.pop();
            }

            Ok(())
        })?;

        if total_results == 0 {
            return Ok("No results found regardless of the specified offset".to_string());
        }

        if offset >= results.len() {
            return Ok(format!(
                "No results found at the specified offset (found {} in total)",
                total_results
            ));
        }

        let result_count = results.len() - offset;

        let mut response = format!(
            "Showing {} result(s) (out of {} found in total):\n",
            result_count, total_results
        );

        for Reverse((_, path)) in &results.into_sorted_vec()[offset..] {
            response.push_str(&path);
            response.push('\n');
        }

        Ok(response)
    }
}
