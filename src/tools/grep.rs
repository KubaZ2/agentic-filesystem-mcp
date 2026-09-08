use std::{cmp::Reverse, collections::BinaryHeap, path::PathBuf, time::SystemTime};

use anyhow::{Context, Result};
use grep::{
    printer::{StandardBuilder, SummaryBuilder},
    regex::RegexMatcherBuilder,
    searcher::{BinaryDetection, SearcherBuilder},
};
use ignore::WalkState;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};

use crate::{Filesystem, GrepPrinter};

#[derive(serde::Deserialize, schemars::JsonSchema, Clone)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
#[schemars(extend("type" = "string"))]
pub enum GrepOutputMode {
    Content,
    FilesWithMatches,
    Count,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct GrepParams {
    #[schemars(
        description = "The regular expression pattern to search for in file contents. Uses standard regex syntax.\n\nIMPORTANT: Remember to escape literal characters (e.g., `interface\\{`)."
    )]
    pattern: String,

    #[schemars(
        description = "The directory or file to search in.\n\nDefaults to `\".\"` if not specified."
    )]
    path: Option<String>,

    #[schemars(
        description = "The glob pattern to filter files to be searched (e.g., `*.{ts,tsx}` or `src/**/*.rs`). Extremely useful for narrowing down searches and improving speed.\n\nDefaults to `null` if not specified."
    )]
    glob: Option<String>,

    #[schemars(
        description = "The output mode: `content` (matching lines), `files_with_matches` (paths only), or `count` (match counts per file).\n\nDefaults to `content` if not specified."
    )]
    output_mode: Option<GrepOutputMode>,

    #[schemars(
        description = "The number of lines to show before each match to provide context. Requires `output_mode` to be `content` or omitted. Ignored otherwise.\n\nDefaults to `0` if not specified."
    )]
    before_context: Option<usize>,

    #[schemars(
        description = "The number of lines to show after each match to provide context. Requires `output_mode` to be `content` or omitted. Ignored otherwise.\n\nDefaults to `0` if not specified."
    )]
    after_context: Option<usize>,

    #[schemars(
        description = "The maximum number of files (not matches) to return. Useful for preventing token overflow when a pattern matches thousands of files/lines.\n\nDefaults to `100` if not specified."
    )]
    limit: Option<usize>,

    #[schemars(
        description = "The number of files (not matches) to skip. Used in combination with limit to paginate through large sets of matching files.\n\nDefaults to `0` if not specified."
    )]
    offset: Option<usize>,

    #[schemars(
        description = "Whether to enable multiline mode where `.` matches newlines, `^` and `$` match line boundaries, and patterns can span multiple lines.\n\nDefaults to `false` if not specified."
    )]
    multiline: Option<bool>,

    #[schemars(
        description = "Whether to show line numbers in the output. Requires `output_mode` to be `content` or omitted. Ignored otherwise.\n\nDefaults to `true` if not specified."
    )]
    show_line_numbers: Option<bool>,
}

#[tool_router(router = tool_router_grep, vis = "pub")]
impl Filesystem {
    #[tool(description = "Searches file contents using regular expressions.")]
    async fn grep(&self, parameters: Parameters<GrepParams>) -> CallToolResult {
        match self.try_grep(parameters).await {
            Ok(result) => CallToolResult::success(vec![ContentBlock::text(result)]),
            Err(err) => {
                Self::log_tool_error("grep", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_grep(
        &self,
        Parameters(GrepParams {
            pattern,
            path,
            glob,
            output_mode,
            before_context,
            after_context,
            limit,
            offset,
            multiline,
            show_line_numbers,
        }): Parameters<GrepParams>,
    ) -> Result<String> {
        let abs_path = self.get_maybe_abs_path(path)?;

        let mut walker_builder = self.create_walk_builder(&abs_path);

        if let Some(glob) = glob {
            self.walk_builder_add_glob(&mut walker_builder, &glob, &abs_path)?;
        }

        let walk = walker_builder.build_parallel();

        let mut matcher_builder = RegexMatcherBuilder::new();

        let multiline = multiline.unwrap_or(false);

        if multiline {
            matcher_builder.multi_line(true);
            matcher_builder.dot_matches_new_line(true);
        } else {
            matcher_builder.line_terminator(Some(b'\n'));
        }

        let matcher = matcher_builder
            .build(&pattern)
            .context("Building regex matcher failed")?;

        let mut searcher_builder = SearcherBuilder::new();

        searcher_builder.binary_detection(BinaryDetection::quit(0));
        searcher_builder.before_context(before_context.unwrap_or(0));
        searcher_builder.after_context(after_context.unwrap_or(0));
        searcher_builder.line_number(show_line_numbers.unwrap_or(true));

        if multiline {
            searcher_builder.multi_line(true);
        }

        let searcher = searcher_builder.build();

        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<(SystemTime, PathBuf, String)>(1000);

        let root = self.root.clone();

        let walk_task = tokio::task::spawn_blocking(move || {
            walk.run(|| {
                let matcher = matcher.clone();
                let mut searcher = searcher.clone();
                let sender = sender.clone();
                let output_mode = output_mode.clone();
                let root = root.clone();

                Box::new(move |result| {
                    let result = match result {
                        Ok(result) => result,
                        Err(err) => {
                            Self::log_tool_warning("grep", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        }
                    };

                    if !result.file_type().map_or(false, |ft| ft.is_file()) {
                        return WalkState::Continue;
                    }

                    let path = result.path();

                    let safe_path = match Self::safe_path(path, &root) {
                        Ok(safe_path) => safe_path,
                        Err(err) => {
                            Self::log_tool_warning("grep", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        }
                    };

                    let mut data = Vec::new();

                    let mut printer = match output_mode.as_ref().unwrap_or(&GrepOutputMode::Content)
                    {
                        GrepOutputMode::Content => {
                            GrepPrinter::Standard(StandardBuilder::new().build_no_color(&mut data))
                        }
                        GrepOutputMode::FilesWithMatches => GrepPrinter::Summary(
                            SummaryBuilder::new()
                                .kind(grep::printer::SummaryKind::PathWithMatch)
                                .build_no_color(&mut data),
                        ),
                        GrepOutputMode::Count => GrepPrinter::Summary(
                            SummaryBuilder::new()
                                .kind(grep::printer::SummaryKind::Count)
                                .build_no_color(&mut data),
                        ),
                    };

                    if let Err(err) = match printer {
                        GrepPrinter::Standard(ref mut p) => searcher.search_path(
                            &matcher,
                            path,
                            p.sink_with_path(&matcher, safe_path),
                        ),
                        GrepPrinter::Summary(ref mut p) => searcher.search_path(
                            &matcher,
                            path,
                            p.sink_with_path(&matcher, safe_path),
                        ),
                    } {
                        Self::log_tool_warning("grep", &anyhow::Error::new(err));
                        return WalkState::Continue;
                    }

                    if data.is_empty() {
                        return WalkState::Continue;
                    }

                    let modified_time = match Self::get_modified_time(&result) {
                        Ok(modified_time) => modified_time,
                        Err(err) => {
                            Self::log_tool_warning("grep", &err);
                            return WalkState::Continue;
                        }
                    };

                    let output = String::from_utf8_lossy(&data).into_owned();

                    if let Err(err) =
                        sender.blocking_send((modified_time, safe_path.to_path_buf(), output))
                    {
                        Self::log_tool_warning("grep", &anyhow::Error::new(err));
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

        for Reverse((_, _, output)) in page {
            response.push_str(output);
        }

        Ok(response)
    }
}
