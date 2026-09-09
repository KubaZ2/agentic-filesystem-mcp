use std::{
    collections::HashMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf, StripPrefixError},
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use grep::printer::{Standard, Summary};
use ignore::{
    DirEntry, WalkBuilder,
    overrides::{Override, OverrideBuilder},
};
use rmcp::{ServerHandler, ServiceExt, handler::server::tool::ToolRouter, tool_handler};
use termcolor::NoColor;
use tokio::io::{stdin, stdout};

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

    // Whether to follow symlinks outside of the root paths
    #[arg(long, default_value_t = false)]
    follow_external_symlinks: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let paths = args.root;

    if paths.is_empty() {
        bail!("No paths provided");
    }

    let paths = paths
        .iter()
        .map(|p| {
            PathBuf::from(p)
                .canonicalize()
                .with_context(|| format!("Error resolving absolute path for {}", p.display()))
        })
        .collect::<Result<Vec<PathBuf>, _>>()?;

    let root = if args.absolute_paths {
        None
    } else {
        get_root_path(&paths)?
    };

    log_info(&format!(
        "Root path: {}",
        root.as_ref()
            .map_or("None".to_string(), |p| p.display().to_string())
    ));

    let filesystem = Filesystem::new(root, paths, args.follow_external_symlinks);

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

#[derive(Clone)]
struct Filesystem {
    tool_router: ToolRouter<Filesystem>,
    root: Option<PathBuf>,
    paths: Vec<PathBuf>,
    media_mime_types: HashMap<&'static str, MimeType>,
    follow_external_symlinks: bool,
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

    fn new(root: Option<PathBuf>, paths: Vec<PathBuf>, follow_external_symlinks: bool) -> Self {
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
            root,
            paths,
            media_mime_types,
            follow_external_symlinks,
        }
    }

    fn safe_path<'a>(
        abs_path: &'a Path,
        root: &Option<PathBuf>,
    ) -> Result<&'a Path, StripPrefixError> {
        match root {
            Some(root) => abs_path.strip_prefix(root),
            None => Ok(abs_path),
        }
    }

    fn safe_join(
        root: &Option<PathBuf>,
        rel_path: &Path,
        follow_external_symlinks: bool,
    ) -> Option<PathBuf> {
        let mut result = root.clone().unwrap_or_default();

        for cmp in rel_path.components() {
            match cmp {
                std::path::Component::Prefix(_) | std::path::Component::RootDir
                    if root.is_none() =>
                {
                    result.push(cmp)
                }
                std::path::Component::Normal(_) => result.push(cmp),
                std::path::Component::CurDir => continue,
                std::path::Component::ParentDir
                    if match root {
                        Some(root) => *root != result,
                        None => true,
                    } =>
                {
                    result.pop();
                }
                _ => return None,
            }

            if !follow_external_symlinks && let Ok(canonicalized) = result.canonicalize() {
                result = canonicalized;
            }
        }

        Some(result)
    }

    fn get_abs_path(&self, path: &str) -> Result<PathBuf> {
        if let Some(abs_path) =
            Self::safe_join(&self.root, Path::new(path), self.follow_external_symlinks)
        {
            for allowed_path in &self.paths {
                if abs_path.starts_with(allowed_path) {
                    return Ok(abs_path);
                }
            }
        }

        bail!("Path is not within the allowed paths");
    }

    fn get_modified_time(entry: &DirEntry) -> Result<SystemTime> {
        let metadata = entry.metadata()?;

        let modified_time = metadata.modified()?;

        Ok(modified_time)
    }

    fn get_maybe_abs_path(&self, path: Option<String>) -> Result<Option<PathBuf>> {
        match path {
            Some(path) => self.get_abs_path(&path).map(Some),
            None => Ok(None),
        }
    }

    fn create_walk_builder(&self, abs_path: &Option<PathBuf>) -> WalkBuilder {
        let mut walk_builder = WalkBuilder::from_iter(match abs_path {
            Some(path) => vec![path.clone()],
            None => self.paths.clone(),
        });

        walk_builder.standard_filters(true).require_git(false);

        walk_builder
    }

    fn walk_builder_add_glob(
        &self,
        walk_builder: &mut WalkBuilder,
        pattern: &str,
        abs_path: &Option<PathBuf>,
    ) -> Result<Override> {
        let mut glob_builder = OverrideBuilder::new(match abs_path {
            Some(abs_path) => abs_path.clone(),
            None => self.root.as_ref().map_or(PathBuf::new(), |p| p.clone()),
        });

        glob_builder
            .add(pattern)
            .context("Failed to add glob a pattern to an override builder")?;

        let glob = glob_builder
            .build()
            .context("Failed to build an override with a glob pattern")?;

        walk_builder.overrides(glob.clone());

        Ok(glob)
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
