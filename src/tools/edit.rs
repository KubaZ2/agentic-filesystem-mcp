use std::{io::Write, sync::Arc};

use aho_corasick::AhoCorasick;
use anyhow::{Context, Result, bail};
use cap_tempfile::TempFile;
use rmcp::{
    handler::server::wrapper::Parameters, model::CallToolResult, schemars, tool, tool_router,
};

use crate::{Filesystem, FilesystemData, fs::VfsDir, path_sanitizer::sanitize_path};

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
        let path = sanitize_path(&path)?;

        let mut file = data.dir.open(path)?;

        let file_permissions = file
            .metadata()
            .context("Failed to get file metadata")?
            .permissions();

        let ac =
            AhoCorasick::new([&old_string]).context("Failed to create Aho-Corasick automaton")?;

        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid file path: {}", path.display()))?;

        let dir = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                let dir = data
                    .dir
                    .open_dir(parent)
                    .context("Failed to open parent directory")?;

                match dir {
                    VfsDir::Real(ref real_dir) => real_dir.clone(),
                    VfsDir::Virtual(_) => {
                        bail!("Cannot open parent directory of virtual file");
                    }
                }
            }
            _ => match data.dir {
                VfsDir::Real(ref real_dir) => real_dir.clone(),
                VfsDir::Virtual(_) => {
                    bail!("Cannot open parent directory of virtual file");
                }
            },
        };

        let tempfile = TempFile::new(&dir).context("Failed to create a temporary file")?;

        let mut writer = std::io::BufWriter::new(tempfile);

        let mut replacements: usize = 0;

        let replace_all = replace_all.unwrap_or(false);

        if replace_all {
            ac.try_stream_replace_all_with(&mut file, &mut writer, |_, _, writer| {
                replacements += 1;
                writer.write_all(new_string.as_bytes())
            })
            .context("Failed to perform string replacement")?;
        } else {
            ac.try_stream_replace_all_with(&mut file, &mut writer, |_, _, writer| {
                if replacements >= 1 {
                    return Err(std::io::Error::other(
                        "Too many matches found for single replacement",
                    ));
                }
                replacements += 1;
                writer.write_all(new_string.as_bytes())
            })
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::Other if replacements >= 1 => anyhow::anyhow!(err),
                _ => anyhow::Error::from(err).context("Failed to perform string replacement"),
            })?;
        }

        if replacements == 0 {
            return Err(anyhow::anyhow!(
                "No matches found for the specified string".to_string()
            ));
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
            .replace(file_name)
            .context("Failed to replace the original file with the edited file")?;

        Ok(format!(
            "Successfully edited the file ({} replacement(s) made)",
            replacements
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use anyhow::Result;
    use cap_tempfile::TempDir;

    use crate::tools::test_utils::{setup_test_fs, setup_virtual_fs};

    fn test_edit_single(replace_all: Option<bool>) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.write("test.txt", "Hello, World!")?;

        let params = Parameters(EditParams {
            path: "test.txt".to_string(),
            old_string: "World".to_string(),
            new_string: "Rust".to_string(),
            replace_all,
        });

        let result = Filesystem::try_edit(data.clone(), params)?;

        assert_eq!(
            result,
            "Successfully edited the file (1 replacement(s) made)"
        );

        Ok(())
    }

    #[test]
    fn test_edit_single_replace_all_none() -> Result<()> {
        test_edit_single(Some(false))
    }

    #[test]
    fn test_edit_single_replace_all_true() -> Result<()> {
        test_edit_single(Some(true))
    }

    #[test]
    fn test_edit_single_replace_all_false() -> Result<()> {
        test_edit_single(Some(false))
    }

    fn test_edit_multiple(
        replace_all: Option<bool>,
    ) -> Result<(TempDir, Arc<FilesystemData>, Result<String>)> {
        let (tempdir, data) = setup_test_fs()?;

        data.dir.write("test.txt", "Hello, World! Hello, World!")?;

        let params = Parameters(EditParams {
            path: "test.txt".to_string(),
            old_string: "World".to_string(),
            new_string: "Rust".to_string(),
            replace_all,
        });

        Ok((
            tempdir,
            data.clone(),
            Filesystem::try_edit(data.clone(), params),
        ))
    }

    fn test_edit_multiple_fails_due_to_replace_all(replace_all: Option<bool>) -> Result<()> {
        assert_ne!(
            replace_all,
            Some(true),
            "This test is only for cases where replace_all is None or false"
        );

        let (_tempdir, _, result) = test_edit_multiple(replace_all)?;

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("Too many matches found for single replacement".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_edit_multiple_replace_all_none() -> Result<()> {
        test_edit_multiple_fails_due_to_replace_all(None)
    }

    #[test]
    fn test_edit_multiple_replace_all_true() -> Result<()> {
        let (_tempdir, data, result) = test_edit_multiple(Some(true))?;

        let result = result?;

        assert_eq!(
            result,
            "Successfully edited the file (2 replacement(s) made)"
        );

        let content = data.dir.read_to_string("test.txt")?;

        assert_eq!(content, "Hello, Rust! Hello, Rust!");

        Ok(())
    }

    #[test]
    fn test_edit_multiple_replace_all_false() -> Result<()> {
        test_edit_multiple_fails_due_to_replace_all(Some(false))
    }

    fn test_edit_no_matches(replace_all: Option<bool>) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.write("test.txt", "Hello, World!")?;

        let params = Parameters(EditParams {
            path: "test.txt".to_string(),
            old_string: "C#".to_string(),
            new_string: "Rust".to_string(),
            replace_all,
        });

        let result = Filesystem::try_edit(data.clone(), params);

        assert_eq!(
            result.err().map(|e| e.to_string()),
            Some("No matches found for the specified string".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_edit_no_matches_replace_all_none() -> Result<()> {
        test_edit_no_matches(None)
    }

    #[test]
    fn test_edit_no_matches_replace_all_true() -> Result<()> {
        test_edit_no_matches(Some(true))
    }

    #[test]
    fn test_edit_no_matches_replace_all_false() -> Result<()> {
        test_edit_no_matches(Some(false))
    }

    fn test_edit_multiline(replace_all: Option<bool>) -> Result<()> {
        let (_tempdir, data) = setup_test_fs()?;

        data.dir.write(
            "test.txt",
            "int a = 0;
int b = 1;
while (a < 50) {
    Console.WriteLine(b);

    (a, b) = (b, a + b);
}
",
        )?;

        let params = Parameters(EditParams {
            path: "test.txt".to_string(),
            old_string: "Console.WriteLine(b);

    (a, b) = (b, a + b);"
                .to_string(),
            new_string: "(a, b) = (b, a + b);

    Console.WriteLine(b);"
                .to_string(),
            replace_all,
        });

        let result = Filesystem::try_edit(data.clone(), params)?;

        assert_eq!(
            result,
            "Successfully edited the file (1 replacement(s) made)"
        );

        let content = data.dir.read_to_string("test.txt")?;

        assert_eq!(
            content,
            "int a = 0;
int b = 1;
while (a < 50) {
    (a, b) = (b, a + b);

    Console.WriteLine(b);
}
"
        );
        Ok(())
    }

    #[test]
    fn test_edit_multiline_replace_all_none() -> Result<()> {
        test_edit_multiline(None)
    }

    #[test]
    fn test_edit_multiline_replace_all_true() -> Result<()> {
        test_edit_multiline(Some(true))
    }

    #[test]
    fn test_edit_multiline_replace_all_false() -> Result<()> {
        test_edit_multiline(Some(false))
    }

    #[test]
    fn test_edit_virtual_fs() -> Result<()> {
        let (_tempdirs, data) = setup_virtual_fs(&[OsStr::new("dir_a")])?;

        let virtual_file_path = "dir_a/test.txt";

        data.dir.write(virtual_file_path, "Hello, World!")?;

        let params = Parameters(EditParams {
            path: virtual_file_path.to_string(),
            old_string: "World".to_string(),
            new_string: "Rust".to_string(),
            replace_all: None,
        });

        let result = Filesystem::try_edit(data.clone(), params)?;

        assert_eq!(
            result,
            "Successfully edited the file (1 replacement(s) made)"
        );

        let content = data.dir.read_to_string(virtual_file_path)?;

        assert_eq!(content, "Hello, Rust!");

        Ok(())
    }
}
