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

use crate::fs::{VfsDir, VfsDirBuilder};

mod copy;
mod fs;
mod tools;
mod walk;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// The root paths to serve
    #[arg(long, num_args = 1..)]
    root: Vec<OsString>,

    /// Whether to use absolute paths instead of relative paths
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

    log_info(&format!(
        "Root path: {}",
        root_path
            .as_ref()
            .map_or("None".to_string(), |p| p.display().to_string())
    ));

    let dir = get_dir(&abs_paths, root_path.as_deref())?;

    // let dirs = get_dirs(&abs_paths, root_path.as_deref())?;

    let filesystem = Filesystem::new(dir);

    let service = filesystem.serve((stdin(), stdout())).await?;

    service.waiting().await?;

    Ok(())
}

fn open_dir(path: &Path) -> Result<Dir> {
    Dir::open_ambient_dir(path, ambient_authority())
        .with_context(|| format!("Error opening directory {}", path.display()))
}

fn get_dir(abs_paths: &[PathBuf], root_path: Option<&Path>) -> Result<VfsDir> {
    if abs_paths.len() == 1
        && let Some(root_path) = root_path
        && abs_paths[0] == root_path
    {
        let dir = open_dir(root_path)?;

        return Ok(VfsDirBuilder::from_root(dir).build());
    }

    let mut builder = VfsDirBuilder::new();

    for abs_path in abs_paths {
        let dir = open_dir(abs_path)?;

        let path = match root_path {
            Some(root_path) => abs_path.strip_prefix(root_path)?,
            None => abs_path,
        };

        builder.mount_dir(path, dir)?;
    }

    Ok(builder.build())
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
    dir: VfsDir,
    media_mime_types: HashMap<&'static str, MimeType>,
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

    fn new(dir: VfsDir) -> Self {
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
                dir,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn get_empty_path() -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from("C:\\")
        }

        #[cfg(not(windows))]
        {
            PathBuf::from("/")
        }
    }

    #[test]
    fn test_get_root_path_empty_root() -> Result<()> {
        let root_path = get_empty_path();

        let abs_paths = vec![
            root_path.join("dir_a/dir_b"),
            root_path.join("dir_a"),
            root_path.join("dir_c"),
        ];

        let root = get_root_path(&abs_paths)?;

        assert_eq!(root, Some(root_path.to_path_buf()));

        Ok(())
    }

    #[test]
    fn test_get_root_path_nested_root() -> Result<()> {
        let root_path = get_empty_path();

        let abs_paths = vec![
            root_path.join("some/nested/dir/dir_a/dir_b"),
            root_path.join("some/nested/dir/dir_a"),
            root_path.join("some/nested/dir/dir_c"),
        ];

        let root = get_root_path(&abs_paths)?;

        assert_eq!(root, Some(root_path.join("some/nested/dir")));

        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn test_get_root_path_no_common_root() -> Result<()> {
        let path_a = PathBuf::from("C:\\dir_a\\dir_b");
        let path_b = PathBuf::from("D:\\dir_c");

        let abs_paths = vec![path_a, path_b];

        let root = get_root_path(&abs_paths)?;

        assert_eq!(root, None);

        Ok(())
    }
}
