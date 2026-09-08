use std::{cmp::Reverse, collections::BinaryHeap, time::SystemTime};

use anyhow::{Context, Result};
use ignore::WalkState;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};

use crate::Filesystem;

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
        match self.try_glob(parameters).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(result)]),
            Err(err) => {
                Self::log_tool_error("glob", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_glob(
        &self,
        Parameters(GlobParams {
            pattern,
            path,
            limit,
            offset,
        }): Parameters<GlobParams>,
    ) -> Result<String> {
        let abs_path = self.get_maybe_abs_path(path)?;

        let mut walk_builder = self.create_walk_builder(&abs_path);

        let glob = self.walk_builder_add_glob(&mut walk_builder, &pattern, &abs_path)?;

        let (sender, mut receiver) = tokio::sync::mpsc::channel::<(SystemTime, String)>(1000);

        let walk = walk_builder.build_parallel();

        let root = self.root.clone();

        let walk_task = tokio::task::spawn_blocking(move || {
            walk.run(|| {
                let sender = sender.clone();
                let root = root.clone();
                let glob = glob.clone();

                Box::new(move |result| {
                    let result = match result {
                        Ok(result) => result,
                        Err(err) => {
                            Self::log_tool_warning("glob", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        }
                    };

                    if result.file_type().map_or(false, |ft| ft.is_dir())
                        && !glob.matched(result.path(), true).is_whitelist()
                    {
                        return WalkState::Continue;
                    }

                    let safe_path = match Self::safe_path(result.path(), &root) {
                        Ok(safe_path) => safe_path.display().to_string(),
                        Err(err) => {
                            Self::log_tool_warning("glob", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        }
                    };

                    let modified_time = match Self::get_modified_time(&result) {
                        Ok(modified_time) => modified_time,
                        Err(err) => {
                            Self::log_tool_warning("glob", &err);
                            return WalkState::Continue;
                        }
                    };

                    if let Err(err) = sender.blocking_send((modified_time, safe_path)) {
                        Self::log_tool_warning("glob", &anyhow::Error::new(err));
                        return WalkState::Quit;
                    }

                    WalkState::Continue
                })
            })
        });

        let mut results = BinaryHeap::new();
        let mut total_results: usize = 0;

        let offset = offset.unwrap_or(0);

        let limit = limit.unwrap_or(Self::DEFAULT_LIMIT);

        let results_limit = offset + limit;

        while let Some(result) = receiver.recv().await {
            total_results += 1;
            results.push(Reverse(result));

            if results.len() > results_limit {
                results.pop();
            }
        }

        walk_task.await.context("Searching files failed")?;

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

        let page = &results.into_sorted_vec()[offset..];

        for Reverse((_, path)) in page {
            response.push_str(path);
            response.push('\n');
        }

        Ok(response)
    }
}
