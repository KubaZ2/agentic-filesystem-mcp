use std::io::Write;

use anyhow::{Context, Result};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};
use std::fmt::Write as _;
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
};

use crate::{Filesystem, MimeType};

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
        match self.try_read(parameters).await {
            Ok(result) => result,
            Err(err) => {
                Self::log_tool_error("read", &err);

                CallToolResult::error(vec![ContentBlock::text(err.to_string())])
            }
        }
    }

    async fn try_read(&self, parameters: Parameters<ReadParams>) -> Result<CallToolResult> {
        let abs_path = self.get_abs_path(&parameters.0.path)?;

        let file = File::open(&abs_path)
            .await
            .context("Failed to open the file")?;

        if let Some(extension) = abs_path.extension()
            && let Some(extension) = extension.to_str()
            && let Some(media_mime_type) =
                self.media_mime_types.get(extension.to_lowercase().as_str())
        {
            return self.try_read_media(media_mime_type, file).await;
        }

        self.try_read_text(file, parameters).await
    }

    async fn try_read_media(&self, mime_type: &MimeType, mut file: File) -> Result<CallToolResult> {
        let data = Vec::new();

        let mut encoder =
            base64::write::EncoderWriter::new(data, &base64::engine::general_purpose::STANDARD);

        let mut buf = [0u8; 8192];

        loop {
            let bytes_read = file
                .read(&mut buf)
                .await
                .context("Failed to read the media file")?;

            if bytes_read == 0 {
                break;
            }

            encoder
                .write_all(&buf[..bytes_read])
                .context("Failed to encode the media file")?;
        }

        let data = encoder
            .finish()
            .context("Failed to finish encoding the media file")?;

        let data = String::from_utf8(data)?;

        Ok(CallToolResult::success(vec![match mime_type {
            MimeType::Image(mime) => ContentBlock::image(&data, *mime),
            MimeType::Audio(mime) => ContentBlock::audio(&data, *mime),
        }]))
    }

    async fn try_read_text(
        &self,
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
                .await
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
