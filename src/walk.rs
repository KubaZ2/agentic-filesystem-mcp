use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::Result;
use ignore::{
    gitignore::{Gitignore, GitignoreBuilder},
    overrides::Override,
};

use crate::fs::{VfsDir, VfsDirEntry};

pub enum RunEntry<'a> {
    Match(&'a VfsDirEntry, &'a Path),
    Error(anyhow::Error),
}

fn walk<F>(
    overrides: &[Override],
    current_dir: &VfsDir,
    current_path: &Path,
    gitignores: &[Gitignore],
    on_match: &mut F,
) -> Result<()>
where
    F: FnMut(RunEntry) -> Result<()>,
{
    let entries = match current_dir.entries() {
        Ok(entries) => entries,
        Err(err) => {
            on_match(RunEntry::Error(anyhow::anyhow!(
                "Failed to read directory {:?}: {}",
                current_path,
                err
            )))?;
            return Ok(());
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                on_match(RunEntry::Error(anyhow::anyhow!(
                    "Failed to read directory entry in {:?}: {}",
                    current_path,
                    err
                )))?;
                continue;
            }
        };

        let entry_name = entry.file_name();

        let entry_path = current_path.join(&entry_name);

        let is_dir = entry.file_type().is_ok_and(|ft| ft.is_dir());

        let mut is_ignored = false;
        let mut is_whitelisted = false;

        for r#override in overrides.iter().rev() {
            match r#override.matched(&entry_path, is_dir) {
                ignore::Match::Ignore(_) => {
                    is_ignored = true;
                    break;
                }
                ignore::Match::Whitelist(_) => {
                    is_whitelisted = true;
                    break;
                }
                ignore::Match::None => {}
            }
        }

        if !is_ignored && !is_whitelisted {
            for gitignore in gitignores.iter().rev() {
                match gitignore.matched(&entry_path, is_dir) {
                    ignore::Match::Ignore(_) => {
                        is_ignored = true;
                        break;
                    }
                    ignore::Match::Whitelist(_) => {
                        is_whitelisted = true;
                        break;
                    }
                    ignore::Match::None => {}
                }
            }
        }

        if !is_ignored && !is_whitelisted && is_entry_hidden(&entry) {
            is_ignored = true;
        }

        if is_ignored {
            continue;
        }

        if !is_dir || is_whitelisted || overrides.is_empty() {
            on_match(RunEntry::Match(&entry, &entry_path))?;
        }

        if is_dir && let Ok(new_sub_dir) = entry.open_dir() {
            if let Some(gitignore) = read_gitignore_safe(&new_sub_dir, &entry_path, on_match)? {
                let mut new_gitignores = gitignores.to_vec();
                new_gitignores.push(gitignore);

                walk(
                    overrides,
                    &new_sub_dir,
                    &entry_path,
                    &new_gitignores,
                    on_match,
                )?;
            } else {
                walk(overrides, &new_sub_dir, &entry_path, gitignores, on_match)?;
            }
        }
    }

    Ok(())
}

pub fn run<F>(
    overrides: &[Override],
    root_dir: &VfsDir,
    base_path: &Path,
    mut on_match: F,
) -> Result<()>
where
    F: FnMut(RunEntry) -> Result<()>,
{
    let mut base_gitignores = Vec::new();

    if let Some(gitignore) = read_gitignore_safe(root_dir, Path::new(""), &mut on_match)? {
        base_gitignores.push(gitignore);
    }

    let mut current_dir = root_dir.clone();
    let mut current_dir_path = PathBuf::new();

    for component in base_path.components() {
        if let std::path::Component::Normal(component) = component {
            match current_dir.open_dir(component) {
                Ok(new_sub_dir) => {
                    current_dir = new_sub_dir;
                    current_dir_path.push(component);

                    if let Some(gitignore) =
                        read_gitignore_safe(&current_dir, &current_dir_path, &mut on_match)?
                    {
                        base_gitignores.push(gitignore);
                    }
                }
                Err(err) => {
                    on_match(RunEntry::Error(anyhow::anyhow!(
                        "Failed to open directory {:?}: {}",
                        current_dir_path.join(component),
                        err
                    )))?;
                    return Ok(());
                }
            }
        }
    }

    walk(
        overrides,
        &current_dir,
        &current_dir_path,
        &base_gitignores,
        &mut on_match,
    )
}

fn read_gitignore_safe<F>(
    dir: &VfsDir,
    dir_path: &Path,
    on_match: &mut F,
) -> Result<Option<Gitignore>>
where
    F: FnMut(RunEntry) -> Result<()>,
{
    Ok(match read_gitignore(dir, dir_path, on_match) {
        Ok(gitignore) => gitignore,
        Err(err) => {
            on_match(RunEntry::Error(anyhow::anyhow!(
                "Failed to create gitignore for directory {:?}: {}",
                dir_path,
                err
            )))?;
            None
        }
    })
}

fn read_gitignore<F>(
    dir: &VfsDir,
    dir_path: &Path,
    on_match: &mut F,
) -> Result<Option<ignore::gitignore::Gitignore>>
where
    F: FnMut(RunEntry) -> Result<()>,
{
    if let Ok(file) = dir.open(".gitignore") {
        let mut gitignore_builder = GitignoreBuilder::new(dir_path);

        let reader = BufReader::new(file);

        let gitignore_path = dir_path.join(".gitignore");

        for line in reader.lines() {
            let line = line?;

            if let Err(err) = gitignore_builder.add_line(Some(gitignore_path.clone()), &line) {
                on_match(RunEntry::Error(anyhow::anyhow!(
                    "Failed to add line to gitignore builder in {:?}: {}",
                    gitignore_path,
                    err
                )))?;
            }
        }

        let gitignore = gitignore_builder.build()?;

        Ok(Some(gitignore))
    } else {
        Ok(None)
    }
}

fn is_entry_hidden(entry: &VfsDirEntry) -> bool {
    entry.file_name().as_encoded_bytes().starts_with(b".")
}
