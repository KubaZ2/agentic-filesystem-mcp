use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use cap_std::fs::Permissions;

use crate::fs::{VfsDir, VfsMetadata};

/// src_path and dst_path have to be sanitized
pub fn copy_recursive<P, Q>(dir: &VfsDir, src_path: P, dst_path: Q) -> Result<()>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let initial_dst_path = dst_path.as_ref();

    enum Task {
        Copy {
            src_dir: VfsDir,
            src_path: Arc<Path>,
            dst_dir: VfsDir,
            dst_path: Arc<Path>,
            full_src_path: PathBuf,
        },
        ApplyDirPermissions {
            ancestor_dir: VfsDir,
            path: Arc<Path>,
            permissions: Permissions,
        },
    }

    let mut tasks = vec![Task::Copy {
        src_dir: dir.clone(),
        src_path: Arc::from(src_path.as_ref()),
        dst_dir: dir.clone(),
        dst_path: Arc::from(dst_path.as_ref()),
        full_src_path: PathBuf::from(src_path.as_ref()),
    }];

    while let Some(task) = tasks.pop() {
        match task {
            Task::Copy {
                src_dir,
                src_path,
                dst_dir,
                dst_path,
                full_src_path,
            } => {
                let metadata = src_dir.symlink_metadata(&src_path)?;

                let file_type = metadata.file_type();

                if file_type.is_symlink() {
                    let symlink_target_path = src_dir.read_link_contents(&src_path)?;

                    #[cfg(not(windows))]
                    {
                        dst_dir.symlink_contents(symlink_target_path, &dst_path)?;
                    }

                    #[cfg(windows)]
                    {
                        // TODO: the code below follows the symlink to check the type,
                        // because cap std hides Windows-specific extensions behind
                        // a feature flag

                        let is_dir = src_dir
                            .metadata(&src_path)
                            .map(|m| m.is_dir())
                            .unwrap_or(false);

                        if is_dir {
                            dst_dir.symlink_contents_dir(symlink_target_path, &dst_path)?;
                        } else {
                            dst_dir.symlink_contents_file(symlink_target_path, &dst_path)?;
                        }
                    }
                } else if file_type.is_file() {
                    copy_file_with_metadata(&src_dir, &src_path, &metadata, &dst_dir, &dst_path)?;
                } else if file_type.is_dir() {
                    let current_src_dir = src_dir.open_dir(&src_path)?;

                    let entries = current_src_dir.entries()?;

                    dst_dir.create_dir(&dst_path)?;

                    let current_dst_dir = dst_dir.open_dir(&dst_path)?;

                    if let Some(permissions) = metadata.permissions() {
                        tasks.push(Task::ApplyDirPermissions {
                            ancestor_dir: dst_dir.clone(),
                            path: dst_path.clone(),
                            permissions,
                        });
                    }

                    for entry in entries {
                        let entry = entry?;

                        let entry_name = entry.file_name();

                        let entry_name = PathBuf::from(entry_name);

                        let full_entry_src_path = full_src_path.join(&entry_name);

                        if full_entry_src_path == initial_dst_path {
                            continue;
                        }

                        let entry_name: Arc<Path> = Arc::from(entry_name);

                        tasks.push(Task::Copy {
                            src_dir: current_src_dir.clone(),
                            src_path: entry_name.clone(),
                            dst_dir: current_dst_dir.clone(),
                            dst_path: entry_name,
                            full_src_path: full_entry_src_path,
                        });
                    }
                }
            }
            Task::ApplyDirPermissions {
                ancestor_dir,
                path,
                permissions,
            } => {
                ancestor_dir.set_permissions(&path, permissions)?;
            }
        }
    }

    Ok(())
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
