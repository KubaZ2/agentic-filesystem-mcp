use std::{
    io::{BufRead, BufReader},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result};
use cap_std::fs::File;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};
use std::fmt::Write as _;

use crate::{Filesystem, FilesystemData, MimeType};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ReadParams {
    #[schemars(description = "The path to the file to read.")]
    path: String,

    #[schemars(
        description = "The maximum number of lines to read. Useful for preventing token overflow when reading very large text files. Ignored for media files.\n\nDefaults to `100` if not specified."
    )]
    limit: Option<usize>,

    #[schemars(
        description = "The number of lines to skip before starting to read. Used in combination with limit to paginate through large files. Ignored for media files.\n\nDefaults to `0` if not specified."
    )]
    offset: Option<usize>,

    #[schemars(
        description = "Whether to prepend 1-indexed line numbers to each line (e.g., `1:`, `2:`). Setting this to `false` can save tokens when line numbers are strictly not needed. Ignored for media files.\n\nDefaults to `true` if not specified."
    )]
    show_line_numbers: Option<bool>,
}

#[tool_router(router = tool_router_read, vis = "pub")]
impl Filesystem {
    #[tool(
        description = "Reads the contents of a file. Supports text files and media files (images and audio)."
    )]
    async fn read(&self, parameters: Parameters<ReadParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run("read", move || Self::try_read(data, parameters)).await
    }

    fn try_read(
        data: Arc<FilesystemData>,
        parameters: Parameters<ReadParams>,
    ) -> Result<CallToolResult> {
        let path = &parameters.0.path;
        let (dir, rel_path) = data.get_dir(&path)?;

        let file = dir.dir.open(&rel_path)?;

        if let Some(extension) = Path::new(path).extension()
            && let Some(extension) = extension.to_str()
            && let Some(media_mime_type) =
                data.media_mime_types.get(extension.to_lowercase().as_str())
        {
            return Self::try_read_media(media_mime_type, file);
        }

        Self::try_read_text(file, parameters)
    }

    fn try_read_media(mime_type: &MimeType, mut file: File) -> Result<CallToolResult> {
        let data = Vec::new();

        let mut encoder =
            base64::write::EncoderWriter::new(data, &base64::engine::general_purpose::STANDARD);

        std::io::copy(&mut file, &mut encoder).context("Failed to encode the media file")?;

        let data = encoder
            .finish()
            .context("Failed to finish encoding the media file")?;

        let data = String::from_utf8(data)?;

        Ok(CallToolResult::success(vec![match mime_type {
            MimeType::Image(mime) => ContentBlock::image(&data, *mime),
            MimeType::Audio(mime) => ContentBlock::audio(&data, *mime),
        }]))
    }

    fn try_read_text(
        file: File,
        Parameters(ReadParams {
            path: _,
            limit,
            offset,
            show_line_numbers,
        }): Parameters<ReadParams>,
    ) -> Result<CallToolResult> {
        let mut reader = BufReader::new(file);

        let limit = limit.unwrap_or(Self::DEFAULT_LIMIT);
        let offset = offset.unwrap_or(0);

        let mut content = String::new();

        let mut total_lines: usize = 0;

        let show_line_numbers = show_line_numbers.unwrap_or(true);

        let mut raw_line = Vec::new();

        loop {
            let bytes_read = reader
                .read_until(b'\n', &mut raw_line)
                .context("Failed to read the file")?;

            if bytes_read == 0 {
                break;
            }

            if total_lines >= offset && total_lines < offset + limit {
                if show_line_numbers {
                    write!(&mut content, "{}:", total_lines + 1)?;
                }
                content.push_str(&String::from_utf8_lossy(&raw_line));
            }

            total_lines += 1;
            raw_line.clear();
        }

        if total_lines == 0 {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "The file is empty".to_string(),
            )]));
        }

        if offset >= total_lines {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "No lines to show at the specified offset (the file has {} lines in total)",
                total_lines
            ))]));
        }

        let first_line = offset + 1;
        let last_line = (offset + limit).min(total_lines);

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Showing lines {} to {} (out of {} lines in total):\n{}",
            first_line, last_line, total_lines, content,
        ))]))
    }
}
