use std::path::Path;

use cap_std::fs::{Dir, DirEntry, Metadata};

use anyhow::Result;

pub fn copy_recursive(
    src_dir: &Dir,
    src_path: &Path,
    dst_dir: &Dir,
    dst_path: &Path,
) -> Result<()> {
    let metadata = src_dir.symlink_metadata(src_path)?;

    copy_recursive_unknown(src_dir, src_path, dst_dir, dst_path, metadata)
}

fn copy_recursive_unknown(
    ancestor_src_dir: &Dir,
    src_path: &Path,
    ancestor_dst_dir: &Dir,
    dst_path: &Path,
    metadata: Metadata,
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
            // TODO: cap std doesn't seem to support absolute symlinks on Windows

            use cap_std::fs::FileTypeExt;

            if file_type.is_symlink_dir() {
                ancestor_dst_dir.symlink_dir(symlink_target_path, dst_path)?;
            } else if file_type.is_symlink_file() {
                ancestor_dst_dir.symlink_file(symlink_target_path, dst_path)?;
            }
        }
    } else if file_type.is_file() {
        ancestor_src_dir.copy(src_path, ancestor_dst_dir, dst_path)?;
    } else if file_type.is_dir() {
        let current_src_dir = ancestor_src_dir.open_dir(src_path)?;

        let entries = current_src_dir
            .entries()?
            .collect::<Result<Vec<DirEntry>, _>>()?;

        ancestor_dst_dir.create_dir(dst_path)?;

        let current_dst_dir = ancestor_dst_dir.open_dir(dst_path)?;

        copy_recursive_internal(entries, current_src_dir, current_dst_dir)?;

        ancestor_dst_dir.set_permissions(dst_path, metadata.permissions())?;
    }

    Ok(())
}

fn copy_recursive_internal(entries: Vec<DirEntry>, src_dir: Dir, dst_dir: Dir) -> Result<()> {
    for entry in entries {
        let file_name = entry.file_name();

        let file_name = Path::new(&file_name);

        let metadata = entry.metadata()?;

        copy_recursive_unknown(&src_dir, file_name, &dst_dir, file_name, metadata)?;
    }

    Ok(())
}
