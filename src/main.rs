use std::{cmp::Reverse, collections::BinaryHeap, ffi::OsString, io::Write, path::{Path, PathBuf, StripPrefixError}, time::SystemTime, usize};

use grep::{printer::{Standard, StandardBuilder, Summary, SummaryBuilder}, regex::RegexMatcherBuilder, searcher::{BinaryDetection, SearcherBuilder}};
use rmcp::{ServiceExt, handler::server::wrapper::Parameters, schemars, tool, tool_router};
use tokio::io::{stdin, stdout};
use clap::Parser;
use ignore::{DirEntry, WalkBuilder, WalkState, overrides::OverrideBuilder};
use termcolor::NoColor;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(long, num_args = 1..)]
    root: Vec<OsString>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let paths = args.root.iter().map(|p| {
        match PathBuf::from(p).canonicalize() {
            Ok(abs_path) => {
                abs_path
            },
            Err(err) => {
                eprintln!("Error resolving absolute path for {:?}: {}", p, err);
                std::process::exit(1);
            }
        }
    }).collect::<Vec<PathBuf>>();

    let mut root: &Path = &paths[0];

    for path in paths.iter().skip(1) {
        while !path.starts_with(root) {
            match root.parent() {
                Some(parent) => root = parent,
                None => break,
            }
        }
    }

    eprintln!("Root directory: {}", root.display());

    let filesystem = Filesystem {
        root: root.to_path_buf(),
        paths: paths,
    };

    let service = filesystem.serve((stdin(), stdout())).await?;

    service.waiting().await?;

    Ok(())
}

#[derive(Clone)]
struct Filesystem {
    root: PathBuf,
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
    line_number: Option<bool>,
}

fn safe_join(root: &Path, rel_path: &Path) -> Result<PathBuf, String> {
    let mut result = root.to_path_buf();

    for cmp in rel_path.components() {
        match cmp {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(format!("Error: Path {} is not within the allowed paths", rel_path.display()));
            },
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => {
                if result == root {
                    return Err(format!("Error: Path {} is not within the allowed paths", rel_path.display()));
                }

                result.pop();
            }
            std::path::Component::Normal(part) => result.push(part),
        }
    }

    return Ok(result);
}

fn safe_path<'a>(abs_path: &'a Path, root: &Path) -> Result<&'a Path, StripPrefixError> {
    abs_path.strip_prefix(root)
}

fn get_modification_time(entry: &DirEntry) -> Result<SystemTime, ignore::Error> {
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
    fn get_abs_path(&self, path: Option<String>) -> Result<Option<PathBuf>, String> {
        match path {
            Some(path) => {
                match safe_join(&self.root, Path::new(&path)) {
                    Ok(joined_path) => Ok(Some(joined_path)),
                    Err(err_msg) => Err(err_msg),
                }
            },
            None => Ok(None),
        }
    }

    fn create_walk_builder(&self, abs_path: &Option<PathBuf>) -> Result<WalkBuilder, String> {
        let mut walk_builder = WalkBuilder::from_iter(match abs_path {
            Some(path) => vec![path.clone()],
            None => self.paths.clone(),
        });

        walk_builder.standard_filters(true);

        walk_builder.require_git(false);

        Ok(walk_builder)
    }

    fn walk_builder_add_glob(&self, walk_builder: &mut WalkBuilder, pattern: &str, abs_path: &Option<PathBuf>) -> Result<(), String> {
        let mut override_builder = OverrideBuilder::new(match abs_path {
            Some(path) => path,
            None => &self.root,
        });

        if let Err(err) = override_builder.add(pattern) {
            return Err(format!("Error: {}", err));
        }

        let r#override = match override_builder.build() {
            Ok(o) => o,
            Err(err) => return Err(format!("Error: {}", err)),
        };

        walk_builder.overrides(r#override);

        Ok(())
    }

    #[tool(description = "Searches the filesystem for files matching a specific glob pattern.")]
    pub async fn glob(&self,
                Parameters(GlobParams {
                    pattern,
                    path,
                    head_limit,
                    offset,
                }): Parameters<GlobParams>) -> String {
        let abs_path = match self.get_abs_path(path) {
            Ok(abs_path) => abs_path,
            Err(err_msg) => return err_msg,
        };

        let mut walk_builder = match self.create_walk_builder(&abs_path) {
            Ok(walk_builder) => walk_builder,
            Err(err_msg) => return err_msg,
        };

        if let Err(err_msg) = self.walk_builder_add_glob(&mut walk_builder, &pattern, &abs_path) {
            return err_msg;
        };

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

                    let modified_time = match get_modification_time(&result) {
                        Ok(modified_time) => modified_time,
                        Err(err_msg) => {
                            eprintln!("{}", err_msg);
                            return WalkState::Continue;
                        },
                    };

                    let safe_path = match safe_path(result.path(), &root) {
                        Ok(safe_path) => safe_path.display().to_string(),
                        Err(err_msg) => {
                            eprintln!("Error: {}", err_msg);
                            return WalkState::Continue;
                        },
                    };

                    if sender.blocking_send((modified_time, safe_path)).is_err() {
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

        if let Err(err_msg) = walk_task.await {
            eprintln!("Error: {}", err_msg);
            return "Error occurred during file search".to_string();
        }

        if total_results == 0 {
            return "No results found regardless of the specified offset".to_string();
        }

        if offset >= results.len() {
            return format!("No results found at the specified offset (found {} in total)", total_results);
        }

        let result_count = results.len() - offset;

        let mut response = format!("Showing {} result(s) (out of {} found in total):\n", result_count, total_results);

        for Reverse((_, path)) in &results.into_sorted_vec()[offset..] {
            response.push_str(&path);
            response.push('\n');
        }

        response
    }

    #[tool(description = "Searches file contents.")]
    pub async fn grep(&self,
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
                        line_number,
                    }): Parameters<GrepParams>) -> String {
        let abs_path = match self.get_abs_path(path) {
            Ok(abs_path) => abs_path,
            Err(err_msg) => return err_msg,
        };

        let mut walker_builder = match self.create_walk_builder(&abs_path) {
            Ok(walk_builder) => walk_builder,
            Err(err_msg) => return err_msg,
        };

        if let Some(glob) = glob {
            if let Err(err_msg) = self.walk_builder_add_glob(&mut walker_builder, &glob, &abs_path) {
                return err_msg;
            };
        }

        let walk = walker_builder.build_parallel();

        let mut matcher_builder = RegexMatcherBuilder::new();
        if multiline.unwrap_or(false) {
            matcher_builder.multi_line(true);
            matcher_builder.dot_matches_new_line(true);
        }
        else {
            matcher_builder.line_terminator(Some(b'\n'));
        }

        let matcher = match matcher_builder.build(&pattern) {
            Ok(matcher) => matcher,
            Err(err) => return format!("Error building matcher: {}", err),
        };

        let mut searcher_builder = SearcherBuilder::new();

        searcher_builder.binary_detection(BinaryDetection::quit(0));
        searcher_builder.before_context(before_context.unwrap_or(0));
        searcher_builder.after_context(after_context.unwrap_or(0));
        searcher_builder.line_number(line_number.unwrap_or(true));

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
                            eprintln!("Error: {}", err);
                            return WalkState::Continue;
                        },
                    };

                    if !result.file_type().map_or(false, |ft| ft.is_file()) {
                        return WalkState::Continue;
                    }

                    let path = result.path();

                    let safe_path = match safe_path(path, &root) {
                        Ok(safe_path) => safe_path,
                        Err(err_msg) => {
                            eprintln!("Error: {}", err_msg);
                            return WalkState::Continue;
                        },
                    };

                    let mut data = Vec::new();

                    let mut printer = match output_mode.clone().unwrap_or(GrepOutputMode::Content) {
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
                        eprintln!("Error searching file: {}: {}", path.display(), err);
                    }

                    if data.is_empty() {
                        return WalkState::Continue;
                    }

                    let metadata = match result.metadata() {
                        Ok(metadata) => metadata,
                        Err(err) => {
                            eprintln!("Error getting file metadata: {}: {}", path.display(), err);
                            return WalkState::Continue;
                        },
                    };

                    let modified_time = match metadata.modified() {
                        Ok(time) => time,
                        Err(err) => {
                            eprintln!("Error getting modified time: {}: {}", path.display(), err);
                            return WalkState::Continue;
                        },
                    };

                    let output = String::from_utf8_lossy(&data).to_string();

                    if sender.blocking_send((modified_time, safe_path.to_path_buf(), output)).is_err() {
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

        if let Err(err_msg) = walk_task.await {
            eprintln!("Error: {}", err_msg);
            return "Error occurred during file search".to_string();
        }

        if total_results == 0 {
            return "No results found regardless of the specified offset".to_string();
        }

        if offset >= results.len() {
            return format!("No results found at the specified offset (found {} in total)", total_results);
        }

        let result_count = results.len() - offset;

        let mut response = format!("Showing {} result(s) (out of {} found in total):\n", result_count, total_results);

        for Reverse((_, _, output)) in &results.into_sorted_vec()[offset..] {
            response.push_str(&output);
        }

        response
    }
}
