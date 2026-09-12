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

        let file = dir.dir.open(rel_path)?;

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

#[cfg(test)]
mod tests {
    use crate::tools::test_utils::setup_test_fs;

    use super::*;

    use anyhow::Result;
    use base64::Engine;

    fn test_read_text(
        limit: Option<usize>,
        offset: Option<usize>,
        show_line_numbers: Option<bool>,
        file_content: &str,
        expected_output: &str,
    ) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dirs[0].dir.write("test.txt", file_content)?;

        let params = Parameters(ReadParams {
            path: "test.txt".to_string(),
            limit,
            offset,
            show_line_numbers,
        });

        let result = Filesystem::try_read(data.clone(), params)?;

        assert_eq!(
            result,
            CallToolResult::success(vec![ContentBlock::text(expected_output.to_string())])
        );

        Ok(())
    }

    fn test_read_text_with_none_params_and_offset_null(offset: Option<usize>) -> Result<()> {
        assert!(offset.unwrap_or(0) == 0);

        test_read_text(
            None,
            offset,
            None,
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5",
            "Showing lines 1 to 5 (out of 5 lines in total):\n1:Line 1\n2:Line 2\n3:Line 3\n4:Line 4\n5:Line 5",
        )
    }

    #[test]
    fn test_read_text_none_params() -> Result<()> {
        test_read_text_with_none_params_and_offset_null(None)
    }

    #[test]
    fn test_read_text_with_offset_offset_zero() -> Result<()> {
        test_read_text_with_none_params_and_offset_null(Some(0))
    }

    #[test]
    fn test_read_text_with_limit() -> Result<()> {
        test_read_text(
            Some(3),
            None,
            None,
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5",
            "Showing lines 1 to 3 (out of 5 lines in total):\n1:Line 1\n2:Line 2\n3:Line 3\n",
        )
    }

    fn test_read_text_with_offset(limit: Option<usize>) -> Result<()> {
        assert!(
            limit.is_none() || limit.unwrap() >= 3,
            "Limit must be at least 3 for this test"
        );

        test_read_text(
            limit,
            Some(2),
            None,
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5",
            "Showing lines 3 to 5 (out of 5 lines in total):\n3:Line 3\n4:Line 4\n5:Line 5",
        )
    }

    #[test]
    fn test_read_text_with_offset_limit_none() -> Result<()> {
        test_read_text_with_offset(None)
    }

    #[test]
    fn test_read_text_with_offset_limit_exact() -> Result<()> {
        test_read_text_with_offset(Some(3))
    }

    #[test]
    fn test_read_text_with_offset_limit_huge() -> Result<()> {
        test_read_text_with_offset(Some(10000))
    }

    #[test]
    fn test_read_text_with_limit_and_offset() -> Result<()> {
        test_read_text(
            Some(2),
            Some(1),
            None,
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5",
            "Showing lines 2 to 3 (out of 5 lines in total):\n2:Line 2\n3:Line 3\n",
        )
    }

    #[test]
    fn test_read_text_with_show_line_numbers_false() -> Result<()> {
        test_read_text(
            Some(3),
            Some(1),
            Some(false),
            "Line 1\nLine 2\nLine 3\nLine 4\nLine 5",
            "Showing lines 2 to 4 (out of 5 lines in total):\nLine 2\nLine 3\nLine 4\n",
        )
    }

    fn test_read_image_file(extension: &str, expected_mime_type: &str) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let image_data = vec![0u8, 1, 2, 3, 4, 5];

        let file_path = format!("test.{}", extension);

        data.dirs[0].dir.write(&file_path, &image_data)?;

        let params = Parameters(ReadParams {
            path: file_path,
            limit: None,
            offset: None,
            show_line_numbers: None,
        });

        let result = Filesystem::try_read(data.clone(), params)?;

        let expected_base64 = base64::engine::general_purpose::STANDARD.encode(&image_data);

        assert_eq!(
            result,
            CallToolResult::success(vec![ContentBlock::image(
                &expected_base64,
                expected_mime_type
            )])
        );

        Ok(())
    }

    #[test]
    fn test_read_image_file_png() -> Result<()> {
        test_read_image_file("png", "image/png")
    }

    #[test]
    fn test_read_image_file_jpg() -> Result<()> {
        test_read_image_file("jpg", "image/jpeg")
    }

    #[test]
    fn test_read_image_file_jpeg() -> Result<()> {
        test_read_image_file("jpeg", "image/jpeg")
    }

    #[test]
    fn test_read_image_file_gif() -> Result<()> {
        test_read_image_file("gif", "image/gif")
    }

    #[test]
    fn test_read_image_file_webp() -> Result<()> {
        test_read_image_file("webp", "image/webp")
    }

    #[test]
    fn test_read_image_file_bmp() -> Result<()> {
        test_read_image_file("bmp", "image/bmp")
    }

    #[test]
    fn test_read_image_file_svg() -> Result<()> {
        test_read_image_file("svg", "image/svg+xml")
    }

    fn test_read_audio_file(extension: &str, expected_mime_type: &str) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        let audio_data = vec![0u8, 1, 2, 3, 4, 5];

        let file_path = format!("test.{}", extension);

        data.dirs[0].dir.write(&file_path, &audio_data)?;

        let params = Parameters(ReadParams {
            path: file_path,
            limit: None,
            offset: None,
            show_line_numbers: None,
        });

        let result = Filesystem::try_read(data.clone(), params)?;

        let expected_base64 = base64::engine::general_purpose::STANDARD.encode(&audio_data);

        assert_eq!(
            result,
            CallToolResult::success(vec![ContentBlock::audio(
                &expected_base64,
                expected_mime_type
            )])
        );

        Ok(())
    }

    #[test]
    fn test_read_audio_file_mp3() -> Result<()> {
        test_read_audio_file("mp3", "audio/mpeg")
    }

    #[test]
    fn test_read_audio_file_wav() -> Result<()> {
        test_read_audio_file("wav", "audio/wav")
    }

    #[test]
    fn test_read_audio_file_ogg() -> Result<()> {
        test_read_audio_file("ogg", "audio/ogg")
    }

    #[test]
    fn test_read_audio_file_opus() -> Result<()> {
        test_read_audio_file("opus", "audio/ogg")
    }

    #[test]
    fn test_read_audio_file_flac() -> Result<()> {
        test_read_audio_file("flac", "audio/flac")
    }
}
