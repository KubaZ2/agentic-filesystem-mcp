use std::{cmp::Reverse, collections::BinaryHeap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{
    Filesystem, FilesystemData,
    walk::{self, RunEntry},
};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct GlobParams {
    #[schemars(
        description = "The glob pattern to match.\n\nIMPORTANT: Patterns like `*.ts` or `src/*.rs` are automatically recursive in this tool. To search ONLY the top-level directory, you MUST use a leading slash (e.g., `/*.ts` or `/src/*.rs`)."
    )]
    pattern: String,

    #[schemars(description = "The directory to search in.\n\nDefaults to empty if not specified.")]
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
        let maybe_empty_path = Path::new(path.as_deref().unwrap_or(""));

        let mut override_builder = OverrideBuilder::new(maybe_empty_path);

        override_builder
            .add(&pattern)
            .context("Invalid glob pattern")?;

        let r#override = override_builder
            .build()
            .context("Failed to build glob override")?;

        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(Self::DEFAULT_LIMIT);

        let results_limit = offset + limit;

        let mut total_results: usize = 0;

        let mut results = BinaryHeap::new();

        walk::run(&[r#override], &data.dir, maybe_empty_path, |entry| {
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
                    Self::log_tool_warning("glob", &err);
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
            response.push_str(path);
            response.push('\n');
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

    fn execute_glob_with_content_and_times(
        files: &[(&str, &str, u64)],
        params: GlobParams,
    ) -> Result<String> {
        let (_tempdir, data) = setup_test_fs()?;
        let now = SystemTime::now();

        for (file_path, content, time_offset) in files {
            let path = Path::new(file_path);
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                data.dir.create_dir_all(parent)?;
            }

            let mut file = data.dir.create(file_path)?;

            file.write_all(content.as_bytes())?;

            let mod_time = now + Duration::from_secs(*time_offset);
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

        Filesystem::try_glob(data.clone(), Parameters(params))
    }

    fn execute_glob_with_times(files: &[(&str, u64)], params: GlobParams) -> Result<String> {
        let files_with_content_and_times: Vec<(&str, &str, u64)> = files
            .iter()
            .map(|(path, time)| (*path, "content", *time))
            .collect();

        execute_glob_with_content_and_times(&files_with_content_and_times, params)
    }

    fn execute_glob_with_content(files: &[(&str, &str)], params: GlobParams) -> Result<String> {
        let files_with_content_and_times: Vec<(&str, &str, u64)> = files
            .iter()
            .enumerate()
            .map(|(i, &(path, content))| (path, content, i as u64))
            .collect();

        execute_glob_with_content_and_times(&files_with_content_and_times, params)
    }

    fn execute_glob(files: &[&str], params: GlobParams) -> Result<String> {
        let files_with_times: Vec<(&str, u64)> = files
            .iter()
            .enumerate()
            .map(|(i, &path)| (path, i as u64))
            .collect();

        execute_glob_with_times(&files_with_times, params)
    }

    fn default_glob_params(pattern: &str) -> GlobParams {
        GlobParams {
            pattern: pattern.to_string(),
            path: None,
            limit: None,
            offset: None,
        }
    }

    fn run_pagination_test(
        files: &[&str],
        pattern: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<String> {
        let mut params = default_glob_params(pattern);
        params.limit = limit;
        params.offset = offset;

        execute_glob(files, params)
    }

    const FILES_3: &[&str] = &["a.txt", "b.txt", "c.txt"];
    const FILES_5: &[&str] = &["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];

    #[test]
    fn test_glob_sorting_by_modified_time() -> Result<()> {
        let result = execute_glob_with_times(
            &[("a.txt", 10), ("b.txt", 2), ("c.txt", 5)],
            default_glob_params("*.txt"),
        )?;

        assert_eq!(
            result,
            "Showing 3 result(s) (out of 3 found in total):\na.txt\nc.txt\nb.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_single_match() -> Result<()> {
        let result = execute_glob(&["a.txt", "b.rs"], default_glob_params("*.txt"))?;
        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\na.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_multiple_matches() -> Result<()> {
        let result = execute_glob(&["a.txt", "b.txt", "c.rs"], default_glob_params("*.txt"))?;
        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nb.txt\na.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_no_results() -> Result<()> {
        let result = execute_glob(&["a.txt"], default_glob_params("*.cs"))?;
        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );
        Ok(())
    }

    #[test]
    fn test_glob_brace_expansion() -> Result<()> {
        let result = execute_glob(
            &["a.ts", "b.tsx", "c.js"],
            default_glob_params("*.{ts,tsx}"),
        )?;
        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nb.tsx\na.ts\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_recursive_by_default() -> Result<()> {
        let result = execute_glob(
            &["a.txt", "subdir/b.txt", "subdir/nested/c.txt"],
            default_glob_params("*.txt"),
        )?;
        assert_eq!(
            result,
            format!(
                "Showing 3 result(s) (out of 3 found in total):\nsubdir{}nested{}c.txt\nsubdir{}b.txt\na.txt\n",
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            )
        );
        Ok(())
    }

    #[test]
    fn test_glob_top_level_only() -> Result<()> {
        let result = execute_glob(&["a.txt", "subdir/b.txt"], default_glob_params("/*.txt"))?;
        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\na.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_with_path_parameter() -> Result<()> {
        let mut params = default_glob_params("*.txt");
        params.path = Some("subdir".to_string());

        let result = execute_glob(&["other.txt", "subdir/a.txt", "subdir/b.txt"], params)?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nb.txt\na.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_invalid_pattern() -> Result<()> {
        let result = execute_glob(&["a.txt"], default_glob_params("[invalid"));
        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Invalid glob pattern".to_string())
        );
        Ok(())
    }

    #[test]
    fn test_glob_limit_1() -> Result<()> {
        let result = run_pagination_test(FILES_3, "*.txt", Some(1), None)?;
        assert_eq!(
            result,
            "Showing 1 result(s) (out of 3 found in total):\nc.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_limit_2() -> Result<()> {
        let result = run_pagination_test(FILES_3, "*.txt", Some(2), None)?;
        assert_eq!(
            result,
            "Showing 2 result(s) (out of 3 found in total):\nc.txt\nb.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_limit_0() -> Result<()> {
        let result = run_pagination_test(FILES_3, "*.txt", Some(0), None)?;
        assert_eq!(
            result,
            "No results found at the specified offset (found 3 in total)"
        );
        Ok(())
    }

    #[test]
    fn test_glob_offset_0_explicit() -> Result<()> {
        let result = run_pagination_test(FILES_3, "*.txt", None, Some(0))?;
        assert_eq!(
            result,
            "Showing 3 result(s) (out of 3 found in total):\nc.txt\nb.txt\na.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_offset_1() -> Result<()> {
        let result = run_pagination_test(FILES_3, "*.txt", None, Some(1))?;
        assert_eq!(
            result,
            "Showing 2 result(s) (out of 3 found in total):\nb.txt\na.txt\n"
        );
        Ok(())
    }

    #[test]
    fn test_glob_offset_past_results() -> Result<()> {
        let result = run_pagination_test(FILES_3, "*.txt", None, Some(5))?;
        assert_eq!(
            result,
            "No results found at the specified offset (found 3 in total)"
        );
        Ok(())
    }

    #[test]
    fn test_glob_offset_equals_total() -> Result<()> {
        let result = run_pagination_test(FILES_3, "*.txt", None, Some(3))?;
        assert_eq!(
            result,
            "No results found at the specified offset (found 3 in total)"
        );
        Ok(())
    }

    #[test]
    fn test_glob_limit_and_offset_pagination() -> Result<()> {
        let page1 = run_pagination_test(FILES_5, "*.txt", Some(2), Some(0))?;
        assert_eq!(
            page1,
            "Showing 2 result(s) (out of 5 found in total):\ne.txt\nd.txt\n"
        );

        let page2 = run_pagination_test(FILES_5, "*.txt", Some(2), Some(2))?;
        assert_eq!(
            page2,
            "Showing 2 result(s) (out of 5 found in total):\nc.txt\nb.txt\n"
        );

        let page3 = run_pagination_test(FILES_5, "*.txt", Some(2), Some(4))?;
        assert_eq!(
            page3,
            "Showing 1 result(s) (out of 5 found in total):\na.txt\n"
        );

        let page4 = run_pagination_test(FILES_5, "*.txt", Some(2), Some(5))?;
        assert_eq!(
            page4,
            "No results found at the specified offset (found 5 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_glob_respects_gitignore() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.create_dir("ignored_dir")?;

        data.dir.write("ignored_dir/file.txt", "content")?;

        data.dir.write(".gitignore", "ignored_dir")?;

        data.dir.write("kept.txt", "content")?;

        let result = Filesystem::try_glob(data.clone(), Parameters(default_glob_params("*.txt")))?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\nkept.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_respects_hidden_files() -> Result<()> {
        let result = execute_glob(
            &[".hidden_dir/hidden.txt", "visible.txt"],
            default_glob_params("*.txt"),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\nvisible.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_glob_overrides_gitignore_and_hidden() -> Result<()> {
        let result = execute_glob_with_content(
            &[
                ("visible.txt", "content"),
                ("ignored_dir/file.txt", "content"),
                (".gitignore", "ignored_dir"),
            ],
            default_glob_params("*"),
        )?;

        assert_eq!(
            result,
            format!(
                "Showing 4 result(s) (out of 4 found in total):\n.gitignore\nignored_dir{}file.txt\nignored_dir\nvisible.txt\n",
                std::path::MAIN_SEPARATOR,
            )
        );

        Ok(())
    }

    #[test]
    fn test_glob_complex_glob_overrides_gitignore() -> Result<()> {
        let result = execute_glob_with_content(
            &[
                ("visible.txt", "content"),
                ("ignored.txt", "content"),
                (".gitignore", "ignored.txt"),
            ],
            default_glob_params("*.txt"),
        )?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nignored.txt\nvisible.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_complex_glob_overrides_hidden() -> Result<()> {
        let result = execute_glob(
            &["visible.txt", ".hidden.txt"],
            default_glob_params("*.txt"),
        )?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\n.hidden.txt\nvisible.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_invalid_gitignore_line_ignored() -> Result<()> {
        let result = execute_glob_with_content(
            &[
                ("a/visible.txt", "content"),
                ("b/ignored.txt", "content"),
                (".gitignore", "[z-a]\nb/"),
            ],
            default_glob_params("*.txt"),
        )?;

        assert_eq!(
            result,
            format!(
                "Showing 1 result(s) (out of 1 found in total):\na{}visible.txt\n",
                std::path::MAIN_SEPARATOR,
            )
        );

        Ok(())
    }

    #[test]
    fn test_glob_trailing_slash_means_directory() -> Result<()> {
        let result = execute_glob(&["a/a", "b"], default_glob_params("/*/"))?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\na\n"
        );

        Ok(())
    }
}
