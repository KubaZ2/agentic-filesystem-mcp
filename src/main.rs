use std::{collections::HashMap, ffi::OsString, io::Write, path::Path, sync::Arc};

use anyhow::{Context, Result};
use cap_std::{ambient_authority, fs::Dir};
use clap::{ArgAction, ArgGroup, Parser};
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

use crate::{
    fs::{VfsDir, VfsDirBuilder},
    path_sanitizer::sanitize_path,
};

mod copy;
mod fs;
mod path_sanitizer;
mod tools;
mod walk;

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    group(ArgGroup::new("mode").required(true).args(["root", "mount"])))]
struct Args {
    /// The root path to serve
    #[arg(long, value_name = "ROOT_PATH")]
    root: Option<OsString>,

    /// The mount points to serve
    #[arg(long, num_args = 2, action = ArgAction::Append, value_names = ["MOUNT_POINT", "ROOT_PATH"])]
    mount: Option<Vec<OsString>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let dir = match (args.root, args.mount) {
        (Some(root), None) => {
            let dir = open_dir(root.as_ref())?;

            VfsDirBuilder::from_root(dir).build()
        }
        (None, Some(mount)) => {
            let mut builder = VfsDirBuilder::new();

            let (chunks, []) = mount.as_chunks::<2>() else {
                unreachable!(
                    "Clap should ensure that --mount is specified in pairs of mount point and root path"
                );
            };

            for chunk in chunks {
                let mount_point = &chunk[0];
                let root_path = &chunk[1];

                let dir = open_dir(root_path.as_ref())?;

                let mount_path = sanitize_path(mount_point)?;

                builder.mount_dir(mount_path, dir)?;
            }

            builder.build()
        }
        _ => unreachable!(
            "Clap should ensure that either --root or --mount is specified, but not both"
        ),
    };

    let filesystem = Filesystem::new(dir);

    let service = filesystem.serve((stdin(), stdout())).await?;

    log_info("Started...");

    service.waiting().await?;

    Ok(())
}

fn open_dir(path: &Path) -> Result<Dir> {
    Dir::open_ambient_dir(path, ambient_authority())
        .with_context(|| format!("Error opening directory {}", path.display()))
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
