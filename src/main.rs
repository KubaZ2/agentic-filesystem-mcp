use std::{
    collections::HashMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use cap_std::{ambient_authority, fs::Dir};
use clap::Parser;
use grep::printer::{Standard, Summary};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::tool::ToolRouter,
    model::{CallToolResult, ContentBlock},
    tool_handler,
};
use termcolor::NoColor;
use tokio::{
    io::{stdin, stdout},
    task::spawn_blocking,
};

mod cap_ignore_walker;
mod copy_recursive;
mod tools;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    // The root paths to serve
    #[arg(long, num_args = 1..)]
    root: Vec<OsString>,

    // Whether to use absolute paths instead of relative paths
    #[arg(long, default_value_t = false)]
    absolute_paths: bool,
}

struct DirInfo {
    /// Path relative to the root path
    path: PathBuf,

    dir: Dir,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let paths = args.root;

    if paths.is_empty() {
        bail!("No paths provided");
    }

    let abs_paths = paths
        .iter()
        .map(|p| {
            PathBuf::from(p)
                .canonicalize()
                .with_context(|| format!("Error resolving absolute path for {}", p.display()))
        })
        .collect::<Result<Vec<PathBuf>>>()?;

    let root_path = if args.absolute_paths {
        None
    } else {
        get_root_path(&abs_paths)?
    };

    let mut dirs = abs_paths
        .iter()
        .map(|abs_path| {
            let dir = Dir::open_ambient_dir(abs_path, ambient_authority())
                .with_context(|| format!("Error opening directory {}", abs_path.display()))?;

            let path = match root_path {
                Some(ref root_path) => abs_path.strip_prefix(root_path)?,
                None => abs_path,
            }
            .to_path_buf();

            Ok((path.components().count(), DirInfo { path, dir }))
        })
        .collect::<Result<Vec<_>>>()?;

    dirs.sort_unstable_by(|(count_left, _), (count_right, _)| count_right.cmp(count_left));

    let dirs = dirs.into_iter().map(|(_, dir)| dir).collect::<Vec<_>>();

    log_info(&format!(
        "Root path: {}",
        root_path
            .as_ref()
            .map_or("None".to_string(), |p| p.display().to_string())
    ));

    let filesystem = Filesystem::new(dirs);

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
enum MimeType {
    Image(&'static str),
    Audio(&'static str),
}

struct FilesystemData {
    dirs: Vec<DirInfo>,
    media_mime_types: HashMap<&'static str, MimeType>,
}

impl FilesystemData {
    fn get_dir<'a>(&self, path: &'a impl AsRef<Path>) -> Result<(&DirInfo, &'a Path)> {
        let path = path.as_ref();

        for dir in &self.dirs {
            if dir.path == path {
                return Ok((&dir, Path::new(".")));
            } else if let Ok(rel_path) = path.strip_prefix(&dir.path) {
                return Ok((&dir, rel_path));
            }
        }

        bail!("Path is not within the allowed paths");
    }

    fn get_search_dirs<'a>(
        &self,
        path: &'a Option<impl AsRef<Path>>,
    ) -> Result<Vec<(&DirInfo, &'a Path)>> {
        let path = match path {
            Some(p) => p.as_ref(),
            None => return Ok(self.dirs.iter().map(|dir| (dir, Path::new(""))).collect()),
        };

        let mut search_dirs = Vec::new();

        for dir in &self.dirs {
            // example:
            // dir.path: dir_a/dir_b
            // path: dir_a
            if dir.path.starts_with(path) {
                search_dirs.push((dir, Path::new("")));

            // example:
            // dir.path: dir_a
            // path: dir_a/dir_b
            } else if let Ok(rel_path) = path.strip_prefix(&dir.path) {
                search_dirs.push((dir, rel_path));
            }
        }

        if search_dirs.is_empty() {
            bail!("Path is not within the allowed paths");
        }

        Ok(search_dirs)
    }
}

struct Filesystem {
    tool_router: ToolRouter<Filesystem>,
    data: Arc<FilesystemData>,
}

#[derive(Clone)]
enum GrepPrinter<W>
where
    W: Write,
{
    Standard(Standard<NoColor<W>>),
    Summary(Summary<NoColor<W>>),
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Filesystem {}

impl Filesystem {
    const DEFAULT_LIMIT: usize = 100;

    fn new(dirs: Vec<DirInfo>) -> Self {
        let media_mime_types = HashMap::from([
            ("png", MimeType::Image("image/png")),
            ("jpg", MimeType::Image("image/jpeg")),
            ("jpeg", MimeType::Image("image/jpeg")),
            ("gif", MimeType::Image("image/gif")),
            ("webp", MimeType::Image("image/webp")),
            ("bmp", MimeType::Image("image/bmp")),
            ("svg", MimeType::Image("image/svg+xml")),
            ("mp3", MimeType::Audio("audio/mpeg")),
            ("wav", MimeType::Audio("audio/wav")),
            ("ogg", MimeType::Audio("audio/ogg")),
            ("opus", MimeType::Audio("audio/ogg")),
            ("flac", MimeType::Audio("audio/flac")),
        ]);

        let tool_router = Self::tool_router_glob()
            + Self::tool_router_grep()
            + Self::tool_router_read()
            + Self::tool_router_write()
            + Self::tool_router_mkdir()
            + Self::tool_router_edit()
            + Self::tool_router_move()
            + Self::tool_router_copy()
            + Self::tool_router_remove();

        Self {
            tool_router,
            data: Arc::new(FilesystemData {
                dirs,
                media_mime_types,
            }),
        }
    }

    async fn run_simple<F>(tool_name: &str, f: F) -> CallToolResult
    where
        F: FnOnce() -> Result<String> + Send + 'static,
    {
        spawn_blocking(f).await.map_or_else(
            |err| {
                let err = anyhow::Error::new(err);
                Self::log_tool_error(tool_name, &err);
                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            },
            |result| {
                result.map_or_else(
                    |err| {
                        Self::log_tool_error(tool_name, &err);
                        CallToolResult::error(vec![ContentBlock::text(err.to_string())])
                    },
                    |s| CallToolResult::success(vec![ContentBlock::text(s)]),
                )
            },
        )
    }

    async fn run<F>(tool_name: &str, f: F) -> CallToolResult
    where
        F: FnOnce() -> Result<CallToolResult> + Send + 'static,
    {
        spawn_blocking(f)
            .await
            .unwrap_or_else(|err| {
                let err = anyhow::Error::new(err);
                Self::log_tool_error(tool_name, &err);
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    err.to_string(),
                )]))
            })
            .unwrap_or_else(|err| {
                Self::log_tool_error(tool_name, &err);
                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            })
    }

    fn log_tool_error(tool: &str, err: &anyhow::Error) {
        log_error(&format!("'{}' failed: {:#}", tool, err));
    }

    fn log_tool_warning(tool: &str, err: &anyhow::Error) {
        log_warning(&format!(
            "'{}' handled an unexpected error: {:#}",
            tool, err
        ));
    }
}
