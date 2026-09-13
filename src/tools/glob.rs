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

    #[test]
    fn test_glob_single_match() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "content")?;
        data.dirs[0].dir.write("b.rs", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.txt".to_string(),
                path: None,
                limit: None,
                offset: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 1 found in total):\na.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_multiple_matches() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "content")?;
        data.dirs[0].dir.write("b.txt", "content")?;
        data.dirs[0].dir.write("c.rs", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.txt".to_string(),
                path: None,
                limit: None,
                offset: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nb.txt\na.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_no_results() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.cs".to_string(),
                path: None,
                limit: None,
                offset: None,
            }),
        )?;

        assert_eq!(
            result,
            "No results found regardless of the specified offset"
        );

        Ok(())
    }

    #[test]
    fn test_glob_subdirectory_match() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.create_dir("subdir")?;
        data.dirs[0].dir.write("a.txt", "content")?;
        data.dirs[0].dir.write("subdir/b.txt", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.txt".to_string(),
                path: None,
                limit: None,
                offset: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nsubdir/b.txt\na.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_with_limit() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "content")?;
        data.dirs[0].dir.write("b.txt", "content")?;
        data.dirs[0].dir.write("c.txt", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.txt".to_string(),
                path: None,
                limit: Some(1),
                offset: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 1 result(s) (out of 3 found in total):\nc.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_with_offset() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "content")?;
        data.dirs[0].dir.write("b.txt", "content")?;
        data.dirs[0].dir.write("c.txt", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.txt".to_string(),
                path: None,
                limit: None,
                offset: Some(1),
            }),
        )?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 3 found in total):\nb.txt\na.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_offset_past_results() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "content")?;
        data.dirs[0].dir.write("b.txt", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.txt".to_string(),
                path: None,
                limit: None,
                offset: Some(5),
            }),
        )?;

        assert_eq!(
            result,
            "No results found at the specified offset (found 2 in total)"
        );

        Ok(())
    }

    #[test]
    fn test_glob_with_path() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.create_dir("subdir")?;
        data.dirs[0].dir.write("other.txt", "content")?;
        data.dirs[0].dir.write("subdir/a.txt", "content")?;
        data.dirs[0].dir.write("subdir/b.txt", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "*.txt".to_string(),
                path: Some("subdir".to_string()),
                limit: None,
                offset: None,
            }),
        )?;

        assert_eq!(
            result,
            "Showing 2 result(s) (out of 2 found in total):\nb.txt\na.txt\n"
        );

        Ok(())
    }

    #[test]
    fn test_glob_invalid_pattern() -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("a.txt", "content")?;

        let result = Filesystem::try_glob(
            data.clone(),
            Parameters(GlobParams {
                pattern: "[invalid".to_string(),
                path: None,
                limit: None,
                offset: None,
            }),
        );

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Invalid glob pattern".to_string())
        );

        Ok(())
    }
}
