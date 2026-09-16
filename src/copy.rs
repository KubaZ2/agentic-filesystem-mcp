use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::fs::{VfsDir, VfsDirEntry, VfsMetadata};

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component),
        };
    }

    result
}

pub fn copy_recursive<P, Q>(dir: &VfsDir, src_path: &P, dst_path: &Q) -> Result<()>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let metadata = dir.symlink_metadata(src_path)?;

    copy_recursive_unknown(
        dir,
        src_path.as_ref(),
        dir,
        dst_path.as_ref(),
        metadata,
        &normalize_path(src_path.as_ref()),
        &normalize_path(dst_path.as_ref()),
    )
}

pub fn copy_file<P, Q>(src_dir: &VfsDir, src_path: &P, dst_dir: &VfsDir, dst_path: &Q) -> Result<()>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let mut src_file = src_dir.open(src_path)?;

    let mut dst_file = dst_dir.open_with(
        dst_path,
        cap_std::fs::OpenOptions::new().create_new(true).write(true),
    )?;

    std::io::copy(&mut src_file, &mut dst_file)?;

    dst_file.set_permissions(src_file.metadata()?.permissions())?;

    Ok(())
}

fn copy_file_with_metadata(
    src_dir: &VfsDir,
    src_path: &Path,
    src_metadata: &VfsMetadata,
    dst_dir: &VfsDir,
    dst_path: &Path,
) -> Result<()> {
    let mut src_file = src_dir.open(src_path)?;

    let mut dst_file = dst_dir.open_with(
        dst_path,
        cap_std::fs::OpenOptions::new().create_new(true).write(true),
    )?;

    std::io::copy(&mut src_file, &mut dst_file)?;

    if let Some(permissions) = src_metadata.permissions() {
        dst_file.set_permissions(permissions)?;
    }

    Ok(())
}

fn copy_recursive_unknown(
    ancestor_src_dir: &VfsDir,
    src_path: &Path,
    ancestor_dst_dir: &VfsDir,
    dst_path: &Path,
    metadata: VfsMetadata,
    current_src_path: &Path,
    initial_dst_path: &Path,
) -> Result<()> {
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        let symlink_target_path = ancestor_src_dir.read_link_contents(src_path)?;

        #[cfg(not(windows))]
        {
            ancestor_dst_dir.symlink_contents(symlink_target_path, dst_path)?;
        }

        #[cfg(windows)]
        {
            // TODO: the code below follows the symlink to check the type,
            // because cap std hides Windows-specific extensions behind
            // a feature flag

            let is_dir = ancestor_src_dir
                .metadata(src_path)
                .map(|m| m.is_dir())
                .unwrap_or(false);

            if is_dir {
                ancestor_dst_dir.symlink_contents_dir(symlink_target_path, dst_path)?;
            } else {
                ancestor_dst_dir.symlink_contents_file(symlink_target_path, dst_path)?;
            }
        }
    } else if file_type.is_file() {
        copy_file_with_metadata(
            ancestor_src_dir,
            src_path,
            &metadata,
            ancestor_dst_dir,
            dst_path,
        )?;
    } else if file_type.is_dir() {
        let current_src_dir = ancestor_src_dir.open_dir(src_path)?;

        let entries = current_src_dir
            .entries()?
            .into_iter()
            .collect::<Result<Vec<VfsDirEntry>>>()?;

        ancestor_dst_dir.create_dir(dst_path)?;

        let current_dst_dir = ancestor_dst_dir.open_dir(dst_path)?;

        copy_recursive_internal(
            entries,
            current_src_dir,
            current_dst_dir,
            current_src_path,
            initial_dst_path,
        )?;

        if let Some(permissions) = metadata.permissions() {
            ancestor_dst_dir.set_permissions(dst_path, permissions)?;
        }
    }

    Ok(())
}

fn copy_recursive_internal(
    entries: Vec<VfsDirEntry>,
    src_dir: VfsDir,
    dst_dir: VfsDir,
    current_src_path: &Path,
    initial_dst_path: &Path,
) -> Result<()> {
    for entry in entries {
        let current_src_path = current_src_path.join(entry.file_name());

        if current_src_path == initial_dst_path {
            continue;
        }

        let file_name = entry.file_name();

        let file_name = Path::new(&file_name);

        let metadata = entry.metadata()?;

        copy_recursive_unknown(
            &src_dir,
            file_name,
            &dst_dir,
            file_name,
            metadata,
            &current_src_path,
            initial_dst_path,
        )?;
    }

    Ok(())
}
