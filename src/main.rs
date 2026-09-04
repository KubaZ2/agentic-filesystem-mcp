use std::{cmp::Reverse, collections::BinaryHeap, ffi::OsString, io::{Error, Write}, path::{Path, PathBuf, StripPrefixError}, time::SystemTime};
use std::fmt::Write as FmtWrite;

use aho_corasick::AhoCorasick;
use grep::{printer::{Standard, StandardBuilder, Summary, SummaryBuilder}, regex::RegexMatcherBuilder, searcher::{BinaryDetection, SearcherBuilder}};
use rmcp::{ServiceExt, handler::server::wrapper::Parameters, schemars, tool, tool_router};
use tempfile::NamedTempFile;
use tokio::{fs::File, io::{AsyncBufReadExt, BufReader, stdin, stdout}};
use clap::Parser;
use ignore::{DirEntry, WalkBuilder, WalkState, overrides::OverrideBuilder};
use termcolor::NoColor;
use anyhow::{Context, Result, bail};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(long, num_args = 1..)]
    root: Vec<OsString>,

    #[arg(long, default_value_t = false)]
    absolute_paths: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let paths = args.root;

    if paths.is_empty() {
        bail!("No paths provided");
    }

    let paths = paths.iter().map(|p| {
        PathBuf::from(p).canonicalize()
            .with_context(|| format!("Error resolving absolute path for {}", p.display()))
    }).collect::<Result<Vec<PathBuf>, _>>()?;

    let root = if args.absolute_paths { None } else { get_root_path(&paths)? };

    log_info(&format!("Root path: {}", root.as_ref().map_or("None".to_string(), |p| p.display().to_string())));

    let filesystem = Filesystem {
        root,
        paths,
    };

    let service = filesystem.serve((stdin(), stdout())).await?;

    service.waiting().await?;

    Ok(())
}

fn get_root_path(paths: &[PathBuf]) -> Result<Option<PathBuf>> {
    let mut root: &Path = &paths[0];

    for path in paths.iter().skip(1) {
        while !path.starts_with(root) {
            match root.parent() {
                Some(parent) => root = parent,
                None => return Ok(None),
            }
        }
    }

    Ok(Some(root.to_path_buf()))
}

fn log_info(message: &str) {
    log("INFO", message);
}

fn log_error(message: &str) {
    log("ERROR", message);
}

fn log_warning(message: &str) {
    log("WARNING", message);
}

fn log(level: &str, message: &str) {
    eprintln!("[{}]: {}", level, message);
}

#[derive(Clone)]
struct Filesystem {
    root: Option<PathBuf>,
    paths: Vec<PathBuf>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct GlobParams {
    pattern: String,
    path: Option<String>,
    head_limit: Option<usize>,
    offset: Option<usize>,
}

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
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    output_mode: Option<GrepOutputMode>,
    before_context: Option<usize>,
    after_context: Option<usize>,
    head_limit: Option<usize>,
    offset: Option<usize>,
    multiline: Option<bool>,
    show_line_numbers: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ReadParams {
    path: String,
    head_limit: Option<usize>,
    offset: Option<usize>,
    show_line_numbers: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct WriteParams {
    path: String,
    content: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct MkdirParams {
    path: String,
    parents: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct EditParams {
    path: String,
    old_string: String,
    new_string: String,
    replace_all: Option<bool>,
}

fn safe_join(root: &Option<PathBuf>, rel_path: &Path) -> Option<PathBuf> {
    let mut result = root.clone().unwrap_or_default();

    for cmp in rel_path.components() {
        match cmp {
            std::path::Component::Prefix(_) | std::path::Component::RootDir if root.is_none() => result.push(cmp),
            std::path::Component::Normal(_) => result.push(cmp),
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir if match root {
                Some(root) => *root != result,
                None => true,
            } => { result.pop(); },
            _ => return None,
        }
    }

    Some(result)
}

fn safe_path<'a>(abs_path: &'a Path, root: &Option<PathBuf>) -> Result<&'a Path, StripPrefixError> {
    match root {
        Some(root) => abs_path.strip_prefix(root),
        None => Ok(abs_path),
    }
}

fn get_modified_time(entry: &DirEntry) -> Result<SystemTime, ignore::Error> {
    let metadata = entry.metadata()?;

    let modified_time = metadata.modified()?;

    Ok(modified_time)
}

#[derive(Clone)]
enum GrepPrinter<W> where W: Write {
    Standard(Standard<NoColor<W>>),
    Summary(Summary<NoColor<W>>),
}

const DEFAULT_HEAD_LIMIT: usize = 100;

#[tool_router(server_handler)]
impl Filesystem {
    fn get_abs_path(&self, path: String) -> Result<PathBuf> {
        if let Some(abs_path) = safe_join(&self.root, Path::new(&path)) {
            for allowed_path in &self.paths {
                if abs_path.starts_with(allowed_path) {
                    return Ok(abs_path);
                }
            }
        }

        bail!("Path is not within the allowed paths");
    }

    fn get_maybe_abs_path(&self, path: Option<String>) -> Result<Option<PathBuf>> {
        match path {
            Some(path) => self.get_abs_path(path).map(Some),
            None => Ok(None),
        }
    }

    fn create_walk_builder(&self, abs_path: &Option<PathBuf>) -> WalkBuilder {
        let mut walk_builder = WalkBuilder::from_iter(match abs_path {
            Some(path) => vec![path.clone()],
            None => self.paths.clone(),
        });

        walk_builder.standard_filters(true);

        walk_builder.require_git(false);

        walk_builder
    }

    fn walk_builder_add_glob(&self, walk_builder: &mut WalkBuilder, pattern: &str, abs_path: &Option<PathBuf>) -> Result<()> {
        let mut override_builder = OverrideBuilder::new(match abs_path {
            Some(abs_path) => abs_path.clone(),
            None => self.root.as_ref().map_or(PathBuf::new(), |p| p.clone()),
        });

        override_builder.add(pattern)
            .context("Failed to add glob a pattern to an override builder")?;

        let r#override = override_builder.build()
            .context("Failed to build an override with a glob pattern")?;

        walk_builder.overrides(r#override);

        Ok(())
    }

    fn log_tool_error(tool: &str, err: &anyhow::Error) {
        log_error(&format!("'{}' failed: {:#}", tool, err));
    }

    fn log_tool_warning(tool: &str, err: &anyhow::Error) {
        log_warning(&format!("'{}' handled an unexpected error: {:#}", tool, err));
    }

    #[tool(description = "Searches the filesystem for files matching a specific glob pattern.")]
    pub async fn glob(&self, parameters: Parameters<GlobParams>) -> String {
        match self.try_glob(parameters).await {
            Ok(result) => result,
            Err(err) => {
                Self::log_tool_error("glob", &err);

                err.to_string()
            },
        }
    }

    async fn try_glob(&self,
                      Parameters(
                          GlobParams {
                              pattern,
                              path,
                              head_limit,
                              offset,
                          }
                      ): Parameters<GlobParams>) -> Result<String> {
        let abs_path = self.get_maybe_abs_path(path)?;

        let mut walk_builder = self.create_walk_builder(&abs_path);

        self.walk_builder_add_glob(&mut walk_builder, &pattern, &abs_path)?;

        let (sender, mut receiver) = tokio::sync::mpsc::channel::<(SystemTime, String)>(1000);

        let walk = walk_builder.build_parallel();

        let root = self.root.clone();

        let walk_task = tokio::task::spawn_blocking(move || {
            walk.run(|| {
                let sender = sender.clone();
                let root = root.clone();

                Box::new(move |result| {
                    let result = match result {
                        Ok(result) => result,
                        Err(err) => {
                            eprintln!("Error: {}", err);
                            return WalkState::Continue;
                        },
                    };

                    if !result.file_type().map_or(false, |ft| ft.is_file()) {
                        return WalkState::Continue;
                    }

                    let safe_path = match safe_path(result.path(), &root) {
                        Ok(safe_path) => safe_path.display().to_string(),
                        Err(err) => {
                            Self::log_tool_warning("glob", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        },
                    };

                    let modified_time = match get_modified_time(&result) {
                        Ok(modified_time) => modified_time,
                        Err(err) => {
                            Self::log_tool_warning("glob", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        },
                    };

                    if let Err(err) = sender.blocking_send((modified_time, safe_path)) {
                        Self::log_tool_warning("glob", &anyhow::Error::new(err));
                        return WalkState::Quit;
                    }

                    ignore::WalkState::Continue
                })
            })
        });

        let mut results = BinaryHeap::new();
        let mut total_results: usize = 0;

        let offset = offset.unwrap_or(0);

        let head_limit = head_limit.unwrap_or(DEFAULT_HEAD_LIMIT);

        let results_limit = offset + head_limit;

        while let Some(result) = receiver.recv().await {
            total_results += 1;
            results.push(Reverse(result));

            if results.len() > results_limit {
                results.pop();
            }
        }

        walk_task.await
            .context("Searching files failed")?;

        if total_results == 0 {
            return Ok("No results found regardless of the specified offset".to_string());
        }

        if offset >= results.len() {
            return Ok(format!("No results found at the specified offset (found {} in total)", total_results));
        }

        let result_count = results.len() - offset;

        let mut response = format!("Showing {} result(s) (out of {} found in total):\n", result_count, total_results);

        for Reverse((_, path)) in &results.into_sorted_vec()[offset..] {
            response.push_str(&path);
            response.push('\n');
        }

        Ok(response)
    }

    #[tool(description = "Searches file contents.")]
    pub async fn grep(&self, parameters: Parameters<GrepParams>) -> String {
        match self.try_grep(parameters).await {
            Ok(result) => result,
            Err(err) => {
                Self::log_tool_error("grep", &err);

                err.to_string()
            },
        }
    }

    async fn try_grep(&self,
                      Parameters(
                          GrepParams {
                              pattern,
                              path,
                              glob,
                              output_mode,
                              before_context,
                              after_context,
                              head_limit,
                              offset,
                              multiline,
                              show_line_numbers,
                          }
                      ): Parameters<GrepParams>) -> Result<String> {
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
        }
        else {
            matcher_builder.line_terminator(Some(b'\n'));
        }

        let matcher = matcher_builder.build(&pattern)
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

        let (sender, mut receiver) = tokio::sync::mpsc::channel::<(SystemTime, PathBuf, String)>(1000);

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
                        },
                    };

                    if !result.file_type().map_or(false, |ft| ft.is_file()) {
                        return WalkState::Continue;
                    }

                    let path = result.path();

                    let safe_path = match safe_path(path, &root) {
                        Ok(safe_path) => safe_path,
                        Err(err) => {
                            Self::log_tool_warning("grep", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        },
                    };

                    let mut data = Vec::new();

                    let mut printer = match output_mode.as_ref().unwrap_or(&GrepOutputMode::Content) {
                        GrepOutputMode::Content => {
                            GrepPrinter::Standard(StandardBuilder::new()
                                .build_no_color(&mut data))
                        }
                        GrepOutputMode::FilesWithMatches => {
                            GrepPrinter::Summary(SummaryBuilder::new()
                                .kind(grep::printer::SummaryKind::PathWithMatch)
                                .build_no_color(&mut data))
                        },
                        GrepOutputMode::Count => {
                            GrepPrinter::Summary(SummaryBuilder::new()
                                .kind(grep::printer::SummaryKind::Count)
                                .build_no_color(&mut data))
                        },
                    };

                    if let Err(err) = match printer {
                        GrepPrinter::Standard(ref mut p) => {
                            searcher.search_path(&matcher, path, p.sink_with_path(&matcher, safe_path))
                        },
                        GrepPrinter::Summary(ref mut p) => {
                            searcher.search_path(&matcher, path, p.sink_with_path(&matcher, safe_path))
                        },
                    } {
                        Self::log_tool_warning("grep", &anyhow::Error::new(err));
                        return WalkState::Continue;
                    }

                    if data.is_empty() {
                        return WalkState::Continue;
                    }

                    let modified_time = match get_modified_time(&result) {
                        Ok(modified_time) => modified_time,
                        Err(err) => {
                            Self::log_tool_warning("grep", &anyhow::Error::new(err));
                            return WalkState::Continue;
                        },
                    };

                    let output = String::from_utf8_lossy(&data).into_owned();

                    if let Err(err) = sender.blocking_send((modified_time, safe_path.to_path_buf(), output)) {
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

        let head_limit = head_limit.unwrap_or(DEFAULT_HEAD_LIMIT);

        let results_limit = offset + head_limit;

        while let Some(result) = receiver.recv().await {
            total_results += 1;
            results.push(Reverse(result));

            if results.len() > results_limit {
                results.pop();
            }
        }

        walk_task.await
            .context("Searching files failed")?;

        if total_results == 0 {
            return Ok("No results found regardless of the specified offset".to_string());
        }

        if offset >= results.len() {
            return Ok(format!("No results found at the specified offset (found {} in total)", total_results));
        }

        let result_count = results.len() - offset;

        let mut response = format!("Showing {} result(s) (out of {} found in total):\n", result_count, total_results);

        for Reverse((_, _, output)) in &results.into_sorted_vec()[offset..] {
            response.push_str(&output);
        }

        Ok(response)
    }

    #[tool(description = "Reads the contents of a file.")]
    pub async fn read(&self, parameters: Parameters<ReadParams>) -> String {
        match self.try_read(parameters).await {
            Ok(result) => result,
            Err(err) => {
                Self::log_tool_error("read", &err);

                err.to_string()
            },
        }
    }

    async fn try_read(&self,
                      Parameters(
                          ReadParams {
                              path,
                              head_limit,
                              offset ,
                              show_line_numbers,
                          }
                      ): Parameters<ReadParams>) -> Result<String> {
        let abs_path = self.get_abs_path(path)?;

        let head_limit = head_limit.unwrap_or(DEFAULT_HEAD_LIMIT);
        let offset = offset.unwrap_or(0);

        let file = File::open(&abs_path).await
            .context("Failed to open the file")?;

        let mut reader = BufReader::new(file);

        let mut result = String::new();

        let mut total_lines: usize = 0;

        let show_line_numbers = show_line_numbers.unwrap_or(true);

        let mut raw_line = Vec::new();

        loop {
            let bytes_read = reader.read_until(b'\n', &mut raw_line).await
                .context("Failed to read the file")?;

            if bytes_read == 0 {
                break;
            }

            if total_lines >= offset && total_lines < offset + head_limit {
                if show_line_numbers {
                    write!(&mut result, "{}:", total_lines + 1)?;
                }
                result.push_str(&String::from_utf8_lossy(&raw_line));
            }

            total_lines += 1;
            raw_line.clear();
        }

        if total_lines == 0 {
            return Ok("No results found regardless of the specified offset".to_string());
        }

        if offset >= total_lines {
            return Ok(format!("No results found at the specified offset (file has {} lines in total)", total_lines));
        }

        result.insert_str(0, &format!("Showing lines {} to {} (out of {} lines in total):\n", offset + 1, (offset + head_limit).min(total_lines), total_lines));

        Ok(result)
    }

    #[tool(description = "Writes content to a file.")]
    pub async fn write(&self, parameters: Parameters<WriteParams>) -> String {
        match self.try_write(parameters).await {
            Ok(result) => result,
            Err(err) => {
                Self::log_tool_error("write", &err);

                err.to_string()
            },
        }
    }

    async fn try_write(&self,
                       Parameters(
                           WriteParams {
                               path,
                               content,
                           }
                       ): Parameters<WriteParams>) -> Result<String> {
        let abs_path = self.get_abs_path(path)?;

        if let Some(parent) = abs_path.parent() {
            tokio::fs::create_dir_all(parent).await
                .context("Failed to create parent directories for the file")?;
        }

        tokio::fs::write(&abs_path, content).await
            .context("Failed to write to the file")?;

        Ok("Successfully wrote the file".to_string())
    }

    #[tool(description = "Creates a new directory.")]
    pub async fn mkdir(&self, parameters: Parameters<MkdirParams>) -> String {
        match self.try_mkdir(parameters).await {
            Ok(result) => result,
            Err(err) => {
                Self::log_tool_error("mkdir", &err);

                err.to_string()
            },
        }
    }

    async fn try_mkdir(&self,
                       Parameters(
                           MkdirParams {
                               path,
                               parents,
                           }
                       ): Parameters<MkdirParams>) -> anyhow::Result<String> {
        let abs_path = self.get_abs_path(path)?;

        let parents = parents.unwrap_or(false);

        if parents {
            tokio::fs::create_dir_all(&abs_path).await
        } else {
            tokio::fs::create_dir(&abs_path).await
        }.context("Failed to create the directory")?;

        Ok("Successfully created the directory".to_string())
    }

    #[tool(description = "Edits a file.")]
    pub fn edit(&self, parameters: Parameters<EditParams>) -> String {
        match self.try_edit(parameters) {
            Ok(result) => result,
            Err(err) => {
                Self::log_tool_error("edit", &err);

                err.to_string()
            },
        }
    }

    fn try_edit(&self,
                Parameters(
                    EditParams {
                        path,
                        old_string,
                        new_string,
                        replace_all,
                    }
                ): Parameters<EditParams>) -> Result<String> {
        let abs_path = self.get_abs_path(path)?;

        let mut file = std::fs::File::open(&abs_path)?;

        let file_permissions = file.metadata()
            .context("Failed to get file metadata")?
            .permissions();

        let ac = AhoCorasick::new([&old_string])
            .context("Failed to create Aho-Corasick automaton")?;

        let tempfile = NamedTempFile::new()
            .context("Failed to create a temporary file")?;

        let mut writer = std::io::BufWriter::new(tempfile);

        let mut replacements: usize = 0;

        let replace_all = replace_all.unwrap_or(false);

        if replace_all {
            ac.try_stream_replace_all_with(&mut file, &mut writer, |_, _, writer| {
                    replacements += 1;
                    writer.write_all(new_string.as_bytes())
            })
        } else {
            ac.try_stream_replace_all_with(&mut file, &mut writer, |_, _, writer| {
                if replacements >= 1 {
                    return Err(Error::new(std::io::ErrorKind::Other, "Too many matches found for single replacement"));
                }
                replacements += 1;
                writer.write_all(new_string.as_bytes())
            })
        }.context("Failed to perform string replacement")?;

        if replacements == 0 {
            return Ok("No matches found for the specified string".to_string());
        }

        let tempfile = writer.into_inner()
            .context("Failed to flush the temporary file")?;

        tempfile.as_file().set_permissions(file_permissions)
            .context("Failed to set permissions on the temporary file")?;

        tempfile.persist(&abs_path)
            .context("Failed to replace the original file with the edited file")?;

        Ok(format!("Successfully edited the file ({} replacement(s) made)", replacements))
    }
}
