use std::{cmp::Reverse, collections::BinaryHeap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use grep::{
    printer::{StandardBuilder, SummaryBuilder},
    regex::RegexMatcherBuilder,
    searcher::{BinaryDetection, SearcherBuilder},
};
use ignore::overrides::OverrideBuilder;
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{
    Filesystem, FilesystemData, GrepPrinter,
    cap_ignore_walker::{CapIgnoreWalker, RunEntry},
};

#[derive(serde::Deserialize, schemars::JsonSchema, Clone, Copy)]
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
        let data = self.data.clone();
        Self::run_simple("grep", move || Self::try_grep(data, parameters)).await
    }

    fn try_grep(
        data: Arc<FilesystemData>,
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
        let dirs = data.get_search_dirs(&path)?;

        let mut overrides = Vec::new();

        if let Some(glob) = glob {
            let mut override_builder = OverrideBuilder::new(
                path.as_ref()
                    .map_or_else(|| Path::new("."), |p| Path::new(p)),
            );

            override_builder
                .add(&glob)
                .context("Invalid glob pattern")?;

            let r#override = override_builder
                .build()
                .context("Failed to build glob override")?;

            overrides.push(r#override);
        }

        let walk = CapIgnoreWalker::new(overrides, dirs);

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

        let mut searcher = searcher_builder.build();

        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(Self::DEFAULT_LIMIT);

        let results_limit = offset + limit;

        let mut total_results: usize = 0;

        let mut results = BinaryHeap::new();

        walk.run(|entry| {
            let (entry, entry_path) = match entry {
                RunEntry::Match(entry, path) => (entry, path),
                RunEntry::Error(err) => {
                    Self::log_tool_warning("grep", &err);
                    return Ok(());
                }
            };

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(err) => {
                    Self::log_tool_warning("grep", &anyhow::Error::from(err));
                    return Ok(());
                }
            };

            if !metadata.is_file() {
                return Ok(());
            }

            let mut data = Vec::new();

            let mut printer = match output_mode.unwrap_or(GrepOutputMode::Content) {
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

            let file = match entry.open() {
                Ok(file) => file,
                Err(err) => {
                    Self::log_tool_warning("grep", &anyhow::Error::from(err));
                    return Ok(());
                }
            }
            .into_std();

            let display_path = match path {
                Some(ref p) => entry_path.strip_prefix(p)?,
                None => entry_path,
            };

            if let Err(err) = match printer {
                GrepPrinter::Standard(ref mut p) => {
                    searcher.search_file(&matcher, &file, p.sink_with_path(&matcher, display_path))
                }
                GrepPrinter::Summary(ref mut p) => {
                    searcher.search_file(&matcher, &file, p.sink_with_path(&matcher, display_path))
                }
            } {
                Self::log_tool_warning("grep", &anyhow::Error::new(err));
                return Ok(());
            }

            if data.is_empty() {
                return Ok(());
            }

            let modified_time = match metadata.modified() {
                Ok(time) => time,
                Err(err) => {
                    Self::log_tool_warning("grep", &anyhow::Error::from(err));
                    return Ok(());
                }
            };

            let output = String::from_utf8_lossy(&data).into_owned();

            total_results += 1;

            results.push(Reverse((modified_time, display_path.to_path_buf(), output)));

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

        let page = &results.into_sorted_vec()[offset..];

        for Reverse((_, _, output)) in page {
            response.push_str(output);
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::test_utils::setup_test_fs;

    use super::*;

    use anyhow::Result;

    #[test]
    fn test_grep_basic_content() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "hello world\nfoo bar\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\ntest.txt:1:hello world\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_no_results() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "hello world\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "goodbye".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiple_matches_in_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "hello one\nfoo bar\nhello two\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\ntest.txt:1:hello one\ntest.txt:3:hello two\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiple_files() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nb.txt:1:hello\na.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_files_with_matches() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "hello world\nfoo bar\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: Some(GrepOutputMode::FilesWithMatches),
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\ntest.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_count() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "hello one\nfoo bar\nhello two\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: Some(GrepOutputMode::Count),
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\ntest.txt:2\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_no_line_numbers() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "hello world\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(false),
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\ntest.txt:hello world\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_with_glob_filter() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.rs", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: Some("*.txt".to_string()),
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\na.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_invalid_regex() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "[invalid".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        );

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Building regex matcher failed".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_grep_invalid_glob() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: Some("[invalid".to_string()),
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        );

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Invalid glob pattern".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_dot_matches_newline() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "first line\nsecond line\nthird line\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "first line.*third line".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: Some(true),
                show_line_numbers: None,
            }),
        )?;

        assert!(
            result.contains("first line"),
            "Expected match to contain 'first line', got: {}",
            result
        );
        assert!(
            result.contains("third line"),
            "Expected match to contain 'third line', got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_non_multiline_does_not_match_across_lines() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "first line\nsecond line\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "first line.*second line".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: Some(false),
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_pattern_with_anchors() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "alpha\nbeta\ngamma\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "^beta$".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: Some(true),
                show_line_numbers: None,
            }),
        )?;

        assert!(
            result.contains("beta"),
            "Expected match for ^beta$ in multiline mode, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_no_match_when_pattern_spans_missing_text() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "aaa\nbbb\nccc\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "aaa.*ddd".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: Some(true),
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_with_multiple_matching_lines() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "foo bar\nbaz qux\nhello world\nfoo bar\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "foo bar\nbaz qux".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: None,
                multiline: Some(true),
                show_line_numbers: Some(false),
            }),
        )?;

        assert!(
            result.contains("foo bar"),
            "Expected match to contain 'foo bar', got: {}",
            result
        );
        assert!(
            result.contains("baz qux"),
            "Expected match to contain 'baz qux', got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_before_context() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write(
            "test.txt",
            "line one\nline two\nTARGET\nline four\nline five\n",
        )?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "TARGET".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: Some(2),
                after_context: None,
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(
            result.contains("line one"),
            "Expected before-context 'line one', got: {}",
            result
        );
        assert!(
            result.contains("line two"),
            "Expected before-context 'line two', got: {}",
            result
        );
        assert!(
            result.contains("TARGET"),
            "Expected match 'TARGET', got: {}",
            result
        );
        assert!(
            !result.contains("line four"),
            "Should NOT contain after-context 'line four', got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_after_context() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write(
            "test.txt",
            "line one\nline two\nTARGET\nline four\nline five\n",
        )?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "TARGET".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: Some(2),
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(
            result.contains("TARGET"),
            "Expected match 'TARGET', got: {}",
            result
        );
        assert!(
            result.contains("line four"),
            "Expected after-context 'line four', got: {}",
            result
        );
        assert!(
            result.contains("line five"),
            "Expected after-context 'line five', got: {}",
            result
        );
        assert!(
            !result.contains("line one"),
            "Should NOT contain before-context 'line one', got: {}",
            result
        );
        assert!(
            !result.contains("line two"),
            "Should NOT contain before-context 'line two', got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_both_before_and_after_context() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "alpha\nbeta\nGAMMA\ndelta\nepsilon\nzeta\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "GAMMA".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: Some(2),
                after_context: Some(2),
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(result.contains("alpha"), "Got: {}", result);
        assert!(result.contains("beta"), "Got: {}", result);
        assert!(result.contains("GAMMA"), "Got: {}", result);
        assert!(result.contains("delta"), "Got: {}", result);
        assert!(result.contains("epsilon"), "Got: {}", result);

        Ok(())
    }

    #[test]
    fn test_grep_before_context_at_file_start() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "MATCH_HERE\nline two\nline three\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "MATCH_HERE".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: Some(3),
                after_context: Some(1),
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(
            result.contains("MATCH_HERE"),
            "Expected the match itself, got: {}",
            result
        );
        assert!(
            result.contains("line two"),
            "Expected after-context 'line two', got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_after_context_at_file_end() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "line one\nline two\nFINAL_MATCH\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "FINAL_MATCH".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: Some(1),
                after_context: Some(3),
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(
            result.contains("FINAL_MATCH"),
            "Expected the match itself, got: {}",
            result
        );
        assert!(
            result.contains("line two"),
            "Expected before-context 'line two', got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_context_larger_than_file() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", "a\nb\nC_MATCH\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "C_MATCH".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: Some(10),
                after_context: Some(10),
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(result.contains("a"), "Got: {}", result);
        assert!(result.contains("b"), "Got: {}", result);
        assert!(result.contains("C_MATCH"), "Got: {}", result);

        Ok(())
    }

    #[test]
    fn test_grep_context_zero() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "line one\nTARGET\nline three\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "TARGET".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: Some(0),
                after_context: Some(0),
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(
            result.contains("TARGET"),
            "Expected the match, got: {}",
            result
        );
        assert!(
            !result.contains("line one"),
            "Should NOT contain 'line one' with 0 before-context, got: {}",
            result
        );
        assert!(
            !result.contains("line three"),
            "Should NOT contain 'line three' with 0 after-context, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_context_with_multiple_matches() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0]
            .dir
            .write("test.txt", "aaa\nMATCH1\nbbb\nccc\nMATCH2\nddd\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "MATCH".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: Some(1),
                after_context: Some(1),
                limit: None,
                offset: None,
                multiline: None,
                show_line_numbers: Some(true),
            }),
        )?;

        assert!(result.contains("MATCH1"), "Got: {}", result);
        assert!(result.contains("MATCH2"), "Got: {}", result);
        assert!(result.contains("aaa"), "Got: {}", result);
        assert!(result.contains("bbb"), "Got: {}", result);
        assert!(result.contains("ccc"), "Got: {}", result);
        assert!(result.contains("ddd"), "Got: {}", result);

        Ok(())
    }

    #[test]
    fn test_grep_limit_1() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;
        data.dirs[0].dir.write("c.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(1),
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            result.contains("Showing 1 result(s) (out of 3 found in total)"),
            "Expected 1 result out of 3, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_2() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;
        data.dirs[0].dir.write("c.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(2),
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            result.contains("Showing 2 result(s) (out of 3 found in total)"),
            "Expected 2 results out of 3, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_0() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(0),
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found at the specified offset (found 1 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_0_explicit() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: Some(0),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            result.contains("Showing 2 result(s) (out of 2 found in total)"),
            "Expected both results with offset=0, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_1() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;
        data.dirs[0].dir.write("c.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: Some(1),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            result.contains("Showing 2 result(s) (out of 3 found in total)"),
            "Expected 2 results after offset 1, got: {}",
            result
        );
        assert!(
            !result.contains("c.txt"),
            "Offset 1 should skip c.txt, got: {}",
            result
        );
        assert!(result.contains("b.txt"), "Got: {}", result);
        assert!(result.contains("a.txt"), "Got: {}", result);

        Ok(())
    }

    #[test]
    fn test_grep_offset_beyond_results() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: Some(10),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found at the specified offset (found 2 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_equals_total() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: Some(2),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found at the specified offset (found 2 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_and_offset_pagination() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;
        data.dirs[0].dir.write("b.txt", "hello\n")?;
        data.dirs[0].dir.write("c.txt", "hello\n")?;
        data.dirs[0].dir.write("d.txt", "hello\n")?;
        data.dirs[0].dir.write("e.txt", "hello\n")?;

        let page1 = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(2),
                offset: Some(0),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            page1.contains("Showing 2 result(s) (out of 5 found in total)"),
            "Page 1 expected 2 of 5, got: {}",
            page1
        );

        let page2 = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(2),
                offset: Some(2),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            page2.contains("Showing 2 result(s) (out of 5 found in total)"),
            "Page 2 expected 2 of 5, got: {}",
            page2
        );

        let page3 = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(2),
                offset: Some(4),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            page3.contains("Showing 1 result(s) (out of 5 found in total)"),
            "Page 3 expected 1 of 5, got: {}",
            page3
        );

        let page4 = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(2),
                offset: Some(5),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            page4,
            "No results found at the specified offset (found 5 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_larger_than_results() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "hello\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: Some(10),
                offset: None,
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert!(
            result.contains("Showing 1 result(s) (out of 1 found in total)"),
            "Expected 1 result (limit larger than available), got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_with_no_matches() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "world\n")?;

        let result = Filesystem::try_grep(
            data.clone(),
            Parameters(GrepParams {
                pattern: "hello".to_string(),
                path: None,
                glob: None,
                output_mode: None,
                before_context: None,
                after_context: None,
                limit: None,
                offset: Some(5),
                multiline: None,
                show_line_numbers: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }
}
