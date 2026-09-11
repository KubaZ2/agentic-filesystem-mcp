use std::{io::Write, sync::Arc};

use aho_corasick::AhoCorasick;
use anyhow::{Context, Result};
use cap_tempfile::TempFile;
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct EditParams {
    #[schemars(description = "The path to the file to edit.")]
    path: String,

    #[schemars(
        description = "The exact text to replace.\n\nIMPORTANT: This must match the file contents exactly, including all indentation, newlines, and whitespace. If you previously read the file with line numbers, you must strip them before matching."
    )]
    old_string: String,

    #[schemars(
        description = "The text to replace it with. This will be inserted exactly as provided."
    )]
    new_string: String,

    #[schemars(
        description = "Whether to replace all occurrences. Set to `true` to replace every instance instead.\n\nIMPORTANT: If `false` or omitted, the edit will fail if `old_string` appears more than once in the file.\n\nDefaults to `false` if not specified."
    )]
    replace_all: Option<bool>,
}

#[tool_router(router = tool_router_edit, vis = "pub")]
impl Filesystem {
    #[tool(
        description = "Performs exact string replacement in a file. Useful for making partial changes to an existing file."
    )]
    async fn edit(&self, parameters: Parameters<EditParams>) -> CallToolResult {
        let data = self.data.clone();
        Self::run_simple("edit", move || Self::try_edit(data, parameters)).await
    }

    fn try_edit(
        data: Arc<FilesystemData>,
        Parameters(EditParams {
            path,
            old_string,
            new_string,
            replace_all,
        }): Parameters<EditParams>,
    ) -> Result<String> {
        let (dir, rel_path) = data.get_dir(&path)?;

        let mut file = dir.dir.open(&rel_path)?;

        let file_permissions = file
            .metadata()
            .context("Failed to get file metadata")?
            .permissions();

        let ac =
            AhoCorasick::new([&old_string]).context("Failed to create Aho-Corasick automaton")?;

        let tempfile = TempFile::new(&dir.dir).context("Failed to create a temporary file")?;

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
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Too many matches found for single replacement",
                    ));
                }
                replacements += 1;
                writer.write_all(new_string.as_bytes())
            })
        }
        .context("Failed to perform string replacement")?;

        if replacements == 0 {
            return Ok("No matches found for the specified string".to_string());
        }

        let tempfile = writer
            .into_inner()
            .map_err(|e| e.into_error())
            .context("Failed to flush the temporary file")?;

        tempfile
            .as_file()
            .set_permissions(file_permissions)
            .context("Failed to set permissions on the temporary file")?;

        drop(file);

        tempfile
            .replace(&rel_path)
            .context("Failed to replace the original file with the edited file")?;

        Ok(format!(
            "Successfully edited the file ({} replacement(s) made)",
            replacements
        ))
    }
}
