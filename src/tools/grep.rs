use std::{cmp::Reverse, collections::BinaryHeap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use grep::{
    printer::{StandardBuilder, SummaryBuilder},
    regex::RegexMatcherBuilder,
    searcher::{BinaryDetection, LineIter, SearcherBuilder},
};
use ignore::overrides::OverrideBuilder;
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{
    Filesystem, FilesystemData, GrepPrinter,
    path_sanitizer::sanitize_path_option,
    walk::{self, RunEntry},
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
        description = "The directory or file to search in.\n\nDefaults to empty if not specified."
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
        description = "The maximum number of lines to return. Useful for preventing token overflow when a pattern matches thousands of files/lines.\n\nDefaults to `100` if not specified."
    )]
    limit: Option<usize>,

    #[schemars(
        description = "The number of lines to skip. Used in combination with limit to paginate through large sets of matching files.\n\nDefaults to `0` if not specified."
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
        let path = sanitize_path_option(path.as_deref())?;

        let mut overrides = Vec::new();

        if let Some(glob) = glob {
            let mut override_builder = OverrideBuilder::new(path);

            override_builder
                .add(&glob)
                .context("Invalid glob pattern")?;

            let r#override = override_builder
                .build()
                .context("Failed to build glob override")?;

            overrides.push(r#override);
        }

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

        let mut total_lines: usize = 0;

        type ResultEntry = (
            Reverse<cap_std::time::SystemTime>,
            Arc<Path>,
            usize,
            Vec<u8>,
        );

        let mut results: BinaryHeap<ResultEntry> = BinaryHeap::new();

        walk::run(&overrides, &data.dir, path, |entry| {
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
                    Self::log_tool_warning("grep", &err);
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
                    Self::log_tool_warning("grep", &err);
                    return Ok(());
                }
            }
            .into_std();

            let display_path = entry_path.strip_prefix(path)?;

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
                    Self::log_tool_warning("grep", &err);
                    return Ok(());
                }
            };

            let mut display_path_arc: Option<Arc<Path>> = None;

            for (i, line) in LineIter::new(b'\n', &data).enumerate() {
                total_lines += 1;

                if results.len() == results_limit
                    && let Some(worst) = results.peek()
                {
                    let current = (Reverse(modified_time), display_path, i);

                    let worst = (worst.0, worst.1.as_ref(), worst.2);

                    if current >= worst {
                        continue;
                    }
                }

                let display_path_arc =
                    display_path_arc.get_or_insert_with(|| Arc::from(display_path.to_path_buf()));

                results.push((
                    Reverse(modified_time),
                    display_path_arc.clone(),
                    i,
                    line.to_vec(),
                ));

                if results.len() > results_limit {
                    results.pop();
                }
            }

            Ok(())
        })?;

        if total_lines == 0 {
            return Ok("No results found regardless of the specified offset".to_string());
        }

        if offset >= results.len() {
            return Ok(format!(
                "No results found at the specified offset (found {} in total)",
                total_lines,
            ));
        }

        let line_count = results.len() - offset;

        let mut response = format!(
            "Showing {} line(s) (out of {} found in total):\n",
            line_count, total_lines
        );

        let page = &results.into_sorted_vec()[offset..];

        for (_, _, _, line) in page {
            for chunk in line.utf8_chunks() {
                response.push_str(chunk.valid());

                if !chunk.invalid().is_empty() {
                    response.push(char::REPLACEMENT_CHARACTER);
                }
            }
        }

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::test_utils::setup_test_fs;

    use super::*;

    use anyhow::Result;
    use cap_fs_ext::SystemTimeSpec;
    use std::{
        io::Write,
        time::{Duration, SystemTime},
    };

    fn execute_grep(files: &[(&str, &str)], params: GrepParams) -> Result<String> {
        let (_tempdir, data) = setup_test_fs()?;
        let now = SystemTime::now();

        for (i, &(name, content)) in files.iter().enumerate() {
            let path = std::path::Path::new(name);
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                data.dir.create_dir_all(parent)?;
            }

            let mut file = data.dir.create(name)?;
            file.write_all(content.as_bytes())?;

            let mod_time = now + Duration::from_secs(i as u64);
            file.into_std().set_modified(mod_time)?;

            for ancestor in path.ancestors().skip(1) {
                if ancestor.as_os_str().is_empty() {
                    continue;
                }
                data.dir.set_mtime(
                    ancestor,
                    SystemTimeSpec::Absolute(cap_primitives::time::SystemTime::from_std(mod_time)),
                )?;
            }
        }

        Filesystem::try_grep(data.clone(), Parameters(params))
    }

    fn default_grep_params(pattern: &str) -> GrepParams {
        GrepParams {
            pattern: pattern.to_string(),
            path: None,
            glob: None,
            output_mode: None,
            before_context: None,
            after_context: None,
            limit: None,
            offset: None,
            multiline: None,
            show_line_numbers: None,
        }
    }

    fn run_context_test(
        file_content: &str,
        pattern: &str,
        before: Option<usize>,
        after: Option<usize>,
    ) -> Result<String> {
        let mut params = default_grep_params(pattern);
        params.before_context = before;
        params.after_context = after;
        params.show_line_numbers = Some(true);

        execute_grep(&[("test.txt", file_content)], params)
    }

    fn run_multiline_test(
        file_content: &str,
        pattern: &str,
        multiline: Option<bool>,
        show_line_numbers: Option<bool>,
    ) -> Result<String> {
        let mut params = default_grep_params(pattern);
        params.multiline = multiline;
        params.show_line_numbers = show_line_numbers;

        execute_grep(&[("test.txt", file_content)], params)
    }

    fn run_pagination_test(
        files: &[(&str, &str)],
        pattern: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<String> {
        let mut params = default_grep_params(pattern);
        params.limit = limit;
        params.offset = offset;

        execute_grep(files, params)
    }

    const FILES_3: &[(&str, &str)] = &[
        ("a.txt", "hello\n"),
        ("b.txt", "hello\n"),
        ("c.txt", "hello\n"),
    ];

    const FILES_5: &[(&str, &str)] = &[
        ("a.txt", "hello\n"),
        ("b.txt", "hello\n"),
        ("c.txt", "hello\n"),
        ("d.txt", "hello\n"),
        ("e.txt", "hello\n"),
    ];

    #[test]
    fn test_grep_basic_content() -> Result<()> {
        let result = execute_grep(
            &[("test.txt", "hello world\nfoo bar\n")],
            default_grep_params("hello"),
        )?;

        assert_eq!(
            result,
            "Showing 1 line(s) (out of 1 found in total):\ntest.txt:1:hello world\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_no_results() -> Result<()> {
        let result = execute_grep(
            &[("test.txt", "hello world\n")],
            default_grep_params("goodbye"),
        )?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiple_matches_in_file() -> Result<()> {
        let result = execute_grep(
            &[("test.txt", "hello one\nfoo bar\nhello two\n")],
            default_grep_params("hello"),
        )?;

        assert_eq!(
            result,
            "Showing 2 line(s) (out of 2 found in total):\ntest.txt:1:hello one\ntest.txt:3:hello two\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiple_files() -> Result<()> {
        let result = execute_grep(
            &[("a.txt", "hello\n"), ("b.txt", "hello\n")],
            default_grep_params("hello"),
        )?;

        assert_eq!(
            result,
            "Showing 2 line(s) (out of 2 found in total):\nb.txt:1:hello\na.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_files_with_matches() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.output_mode = Some(GrepOutputMode::FilesWithMatches);

        let result = execute_grep(&[("test.txt", "hello world\nfoo bar\n")], params)?;

        assert_eq!(
            result,
            "Showing 1 line(s) (out of 1 found in total):\ntest.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_count() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.output_mode = Some(GrepOutputMode::Count);

        let result = execute_grep(&[("test.txt", "hello one\nfoo bar\nhello two\n")], params)?;

        assert_eq!(
            result,
            "Showing 1 line(s) (out of 1 found in total):\ntest.txt:2\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_no_line_numbers() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.show_line_numbers = Some(false);

        let result = execute_grep(&[("test.txt", "hello world\n")], params)?;

        assert_eq!(
            result,
            "Showing 1 line(s) (out of 1 found in total):\ntest.txt:hello world\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_with_glob_filter() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.glob = Some("*.txt".to_string());

        let result = execute_grep(&[("a.txt", "hello\n"), ("b.rs", "hello\n")], params)?;

        assert_eq!(
            result,
            "Showing 1 line(s) (out of 1 found in total):\na.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_invalid_regex() -> Result<()> {
        let result = execute_grep(&[("test.txt", "hello\n")], default_grep_params("[invalid"));

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Building regex matcher failed".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_grep_invalid_glob() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.glob = Some("[invalid".to_string());

        let result = execute_grep(&[("test.txt", "hello\n")], params);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Invalid glob pattern".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_dot_matches_newline() -> Result<()> {
        let result = run_multiline_test(
            "first line\nsecond line\nthird line\n",
            "first line.*third line",
            Some(true),
            None,
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
        let result = run_multiline_test(
            "first line\nsecond line\n",
            "first line.*second line",
            Some(false),
            None,
        )?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_pattern_with_anchors() -> Result<()> {
        let result = run_multiline_test("alpha\nbeta\ngamma\n", "^beta$", Some(true), None)?;

        assert!(
            result.contains("beta"),
            "Expected match for ^beta$ in multiline mode, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_no_match_when_pattern_spans_missing_text() -> Result<()> {
        let result = run_multiline_test("aaa\nbbb\nccc\n", "aaa.*ddd", Some(true), None)?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_grep_multiline_with_multiple_matching_lines() -> Result<()> {
        let result = run_multiline_test(
            "foo bar\nbaz qux\nhello world\nfoo bar\n",
            "foo bar\nbaz qux",
            Some(true),
            Some(false),
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
        let result = run_context_test(
            "line one\nline two\nTARGET\nline four\nline five\n",
            "TARGET",
            Some(2),
            None,
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
        let result = run_context_test(
            "line one\nline two\nTARGET\nline four\nline five\n",
            "TARGET",
            None,
            Some(2),
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
        let result = run_context_test(
            "alpha\nbeta\nGAMMA\ndelta\nepsilon\nzeta\n",
            "GAMMA",
            Some(2),
            Some(2),
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
        let result = run_context_test(
            "MATCH_HERE\nline two\nline three\n",
            "MATCH_HERE",
            Some(3),
            Some(1),
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
        let result = run_context_test(
            "line one\nline two\nFINAL_MATCH\n",
            "FINAL_MATCH",
            Some(1),
            Some(3),
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
        let result = run_context_test("a\nb\nC_MATCH\n", "C_MATCH", Some(10), Some(10))?;

        assert!(result.contains("a"), "Got: {}", result);
        assert!(result.contains("b"), "Got: {}", result);
        assert!(result.contains("C_MATCH"), "Got: {}", result);

        Ok(())
    }

    #[test]
    fn test_grep_context_zero() -> Result<()> {
        let result =
            run_context_test("line one\nTARGET\nline three\n", "TARGET", Some(0), Some(0))?;

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
        let result = run_context_test(
            "aaa\nMATCH1\nbbb\nccc\nMATCH2\nddd\n",
            "MATCH",
            Some(1),
            Some(1),
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
        let result = run_pagination_test(FILES_3, "hello", Some(1), None)?;

        assert!(
            result.contains("Showing 1 line(s) (out of 3 found in total)"),
            "Expected 1 line out of 3, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_2() -> Result<()> {
        let result = run_pagination_test(FILES_3, "hello", Some(2), None)?;

        assert!(
            result.contains("Showing 2 line(s) (out of 3 found in total)"),
            "Expected 2 lines out of 3, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_0() -> Result<()> {
        let result = run_pagination_test(&[("a.txt", "hello\n")], "hello", Some(0), None)?;

        assert_eq!(
            result,
            "No results found at the specified offset (found 1 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_0_explicit() -> Result<()> {
        let result = run_pagination_test(
            &[("a.txt", "hello\n"), ("b.txt", "hello\n")],
            "hello",
            None,
            Some(0),
        )?;

        assert!(
            result.contains("Showing 2 line(s) (out of 2 found in total)"),
            "Expected both lines with offset=0, got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_1() -> Result<()> {
        let result = run_pagination_test(FILES_3, "hello", None, Some(1))?;

        assert!(
            result.contains("Showing 2 line(s) (out of 3 found in total)"),
            "Expected 2 lines after offset 1, got: {}",
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
        let result = run_pagination_test(
            &[("a.txt", "hello\n"), ("b.txt", "hello\n")],
            "hello",
            None,
            Some(10),
        )?;

        assert_eq!(
            result,
            "No results found at the specified offset (found 2 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_equals_total() -> Result<()> {
        let result = run_pagination_test(
            &[("a.txt", "hello\n"), ("b.txt", "hello\n")],
            "hello",
            None,
            Some(2),
        )?;

        assert_eq!(
            result,
            "No results found at the specified offset (found 2 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_and_offset_pagination() -> Result<()> {
        let page1 = run_pagination_test(FILES_5, "hello", Some(2), Some(0))?;
        assert!(
            page1.contains("Showing 2 line(s) (out of 5 found in total)"),
            "Page 1 expected 2 of 5, got: {}",
            page1
        );

        let page2 = run_pagination_test(FILES_5, "hello", Some(2), Some(2))?;
        assert!(
            page2.contains("Showing 2 line(s) (out of 5 found in total)"),
            "Page 2 expected 2 of 5, got: {}",
            page2
        );

        let page3 = run_pagination_test(FILES_5, "hello", Some(2), Some(4))?;
        assert!(
            page3.contains("Showing 1 line(s) (out of 5 found in total)"),
            "Page 3 expected 1 of 5, got: {}",
            page3
        );

        let page4 = run_pagination_test(FILES_5, "hello", Some(2), Some(5))?;
        assert_eq!(
            page4,
            "No results found at the specified offset (found 5 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_grep_limit_larger_than_results() -> Result<()> {
        let result = run_pagination_test(&[("a.txt", "hello\n")], "hello", Some(10), None)?;

        assert!(
            result.contains("Showing 1 line(s) (out of 1 found in total)"),
            "Expected 1 line (limit larger than available), got: {}",
            result
        );

        Ok(())
    }

    #[test]
    fn test_grep_offset_with_no_matches() -> Result<()> {
        let result = run_pagination_test(&[("a.txt", "world\n")], "hello", None, Some(5))?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_grep_respects_gitignore() -> Result<()> {
        let result = execute_grep(
            &[
                ("ignored_dir/file.txt", "hello"),
                (".gitignore", "ignored_dir\n"),
                ("kept.txt", "hello"),
            ],
            default_grep_params("hello"),
        )?;

        assert_eq!(
            result,
            "Showing 1 line(s) (out of 1 found in total):\nkept.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_respects_hidden_files() -> Result<()> {
        let result = execute_grep(
            &[
                (".hidden_dir/hidden.txt", "hello"),
                ("visible.txt", "hello"),
            ],
            default_grep_params("hello"),
        )?;

        assert_eq!(
            result,
            "Showing 1 line(s) (out of 1 found in total):\nvisible.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_glob_overrides_gitignore_and_hidden() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.glob = Some("*".to_string());

        let result = execute_grep(
            &[
                ("visible.txt", "hello"),
                ("ignored_dir/file.txt", "hello"),
                (".gitignore", "ignored_dir\nhello"),
            ],
            params,
        )?;

        assert_eq!(
            result,
            format!(
                "Showing 3 line(s) (out of 3 found in total):\n.gitignore:2:hello\nignored_dir{}file.txt:1:hello\nvisible.txt:1:hello\n",
                std::path::MAIN_SEPARATOR,
            )
        );

        Ok(())
    }

    #[test]
    fn test_grep_complex_glob_overrides_gitignore() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.glob = Some("*.txt".to_string());
        let result = execute_grep(
            &[
                ("visible.txt", "hello"),
                ("ignored.txt", "hello"),
                (".gitignore", "ignored.txt\n"),
            ],
            params,
        )?;

        assert_eq!(
            result,
            "Showing 2 line(s) (out of 2 found in total):\nignored.txt:1:hello\nvisible.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_grep_complex_glob_overrides_hidden() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.glob = Some("*.txt".to_string());
        let result = execute_grep(
            &[("visible.txt", "hello"), (".hidden.txt", "hello")],
            params,
        )?;

        assert_eq!(
            result,
            "Showing 2 line(s) (out of 2 found in total):\n.hidden.txt:1:hello\nvisible.txt:1:hello\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_invalid_gitignore_line_ignored() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.glob = Some("*.txt".to_string());
        let result = execute_grep(
            &[
                ("a/visible.txt", "hello"),
                ("b/ignored.txt", "hello"),
                (".gitignore", "[z-a]\nb/"),
            ],
            params,
        )?;

        assert_eq!(
            result,
            format!(
                "Showing 1 line(s) (out of 1 found in total):\na{}visible.txt:1:hello\n",
                std::path::MAIN_SEPARATOR,
            )
        );

        Ok(())
    }

    #[test]
    fn test_grep_trailing_slash_means_directory() -> Result<()> {
        let mut params = default_grep_params("hello");
        params.glob = Some("/*/".to_string());
        let result = execute_grep(&[("a/a", "hello"), ("b", "hello")], params)?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }
}
