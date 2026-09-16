use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_fs_ext::{DirExt, SystemTimeSpec};
use cap_std::{
    fs::{Dir, DirEntry, File, FileType, OpenOptions, Permissions},
    time::SystemTime,
};

use anyhow::{Result, bail};

pub enum VfsDirBuilder {
    Root(Dir),
    Virtual(HashMap<OsString, VfsDirBuilder>),
}

impl VfsDirBuilder {
    pub fn new() -> Self {
        VfsDirBuilder::Virtual(HashMap::new())
    }

    pub fn from_root(dir: Dir) -> Self {
        VfsDirBuilder::Root(dir)
    }

    pub fn mount_dir<P>(&mut self, path: P, dir: Dir) -> Result<()>
    where
        P: AsRef<std::path::Path>,
    {
        let children = match self {
            VfsDirBuilder::Virtual(map) => map,
            _ => bail!("Cannot mount a directory on a root directory"),
        };

        let mut components = path.as_ref().components();

        match components.next() {
            Some(component) => {
                let remaining_path = components.as_path();

                if remaining_path.as_os_str().is_empty() {
                    if children.contains_key(component.as_os_str()) {
                        bail!("Path already exists in virtual directory");
                    }

                    children.insert(
                        component.as_os_str().to_os_string(),
                        VfsDirBuilder::Root(dir),
                    );
                } else {
                    children
                        .entry(component.as_os_str().to_os_string())
                        .or_insert_with(|| VfsDirBuilder::Virtual(HashMap::new()))
                        .mount_dir(remaining_path, dir)?;
                }
            }
            None => bail!("Cannot mount a directory with an empty path"),
        }

        Ok(())
    }

    pub fn build(self) -> VfsDir {
        match self {
            VfsDirBuilder::Root(dir) => VfsDir::Real(Arc::new(dir)),
            VfsDirBuilder::Virtual(children) => {
                let mut vfs_children = HashMap::new();

                for (name, entry) in children {
                    let vfs_dir = entry.build();
                    vfs_children.insert(name, vfs_dir);
                }

                VfsDir::Virtual(Arc::new(VirtualDir {
                    children: vfs_children,
                }))
            }
        }
    }
}

pub enum VfsMetadata {
    Real(cap_std::fs::Metadata),
    Virtual,
}

impl VfsMetadata {
    pub fn is_dir(&self) -> bool {
        match self {
            VfsMetadata::Real(metadata) => metadata.is_dir(),
            VfsMetadata::Virtual => true,
        }
    }

    pub fn is_file(&self) -> bool {
        match self {
            VfsMetadata::Real(metadata) => metadata.is_file(),
            VfsMetadata::Virtual => false,
        }
    }

    pub fn is_symlink(&self) -> bool {
        match self {
            VfsMetadata::Real(metadata) => metadata.is_symlink(),
            VfsMetadata::Virtual => false,
        }
    }

    pub fn permissions(&self) -> Option<Permissions> {
        match self {
            VfsMetadata::Real(metadata) => Some(metadata.permissions()),
            VfsMetadata::Virtual => None,
        }
    }

    pub fn modified(&self) -> Result<SystemTime> {
        Ok(match self {
            VfsMetadata::Real(metadata) => metadata.modified()?,
            VfsMetadata::Virtual => SystemTime::from_std(std::time::SystemTime::UNIX_EPOCH),
        })
    }

    pub fn file_type(&self) -> FileType {
        match self {
            VfsMetadata::Real(metadata) => metadata.file_type(),
            VfsMetadata::Virtual => FileType::dir(),
        }
    }
}

pub struct VirtualDir {
    pub children: HashMap<OsString, VfsDir>,
}

#[derive(Clone)]
pub enum VfsDir {
    Real(Arc<Dir>),
    Virtual(Arc<VirtualDir>),
}

impl VfsDir {
    fn route_virtual<T, F>(&self, path: &Path, f: F) -> Result<T>
    where
        F: FnOnce(&VfsDir, &Path) -> Result<T>,
    {
        let VfsDir::Virtual(vdir) = self else {
            unreachable!()
        };

        let mut components = path.components();
        let first = components.next().unwrap();

        if let Some(child) = vdir.children.get(first.as_os_str()) {
            f(child, components.as_path())
        } else {
            bail!("Path not found in virtual directory");
        }
    }

    #[allow(unused)]
    pub fn exists<P: AsRef<Path>>(&self, path: P) -> bool {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return true; // The directory itself exists
        }
        match self {
            VfsDir::Real(dir) => dir.exists(path),
            VfsDir::Virtual(_) => self
                .route_virtual(path, |c, p| Ok(c.exists(p)))
                .unwrap_or(false),
        }
    }

    pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        from: P,
        to_dir: &Self,
        to: Q,
    ) -> Result<()> {
        let from = from.as_ref();
        let to = to.as_ref();

        if from.as_os_str().is_empty() {
            bail!("Cannot rename a virtual root or mount point itself");
        }

        if to.as_os_str().is_empty() {
            bail!("Cannot rename to a virtual root or mount point itself");
        }

        match (self, to_dir) {
            (VfsDir::Real(from_dir), VfsDir::Real(to_dir)) => {
                from_dir.rename(from, to_dir, to)?;
                Ok(())
            }
            (VfsDir::Virtual(_), _) => self.route_virtual(from, |c, p| c.rename(p, to_dir, to)),
            (VfsDir::Real(_), VfsDir::Virtual(_)) => {
                to_dir.route_virtual(to, |c, p| self.rename(from, c, p))
            }
        }
    }

    pub fn open<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot open a directory as a file");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.open(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.open(p)),
        }
    }

    pub fn open_with<P: AsRef<Path>>(&self, path: P, options: &OpenOptions) -> Result<File> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot open a directory as a file");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.open_with(path, options)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.open_with(p, options)),
        }
    }

    pub fn open_dir<P: AsRef<Path>>(&self, path: P) -> Result<VfsDir> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Ok(self.clone());
        }
        match self {
            VfsDir::Real(dir) => Ok(VfsDir::Real(Arc::new(dir.open_dir(path)?))),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.open_dir(p)),
        }
    }

    pub fn create_dir<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Directory already exists");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.create_dir(path)?),
            VfsDir::Virtual(_) => self
                .route_virtual(path, |c, p| c.create_dir(p))
                .map_err(|_| {
                    anyhow::anyhow!("Cannot create a directory inside a virtual directory")
                }),
        }
    }

    pub fn create_dir_all<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Ok(());
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.create_dir_all(path)?),
            VfsDir::Virtual(_) => self
                .route_virtual(path, |c, p| c.create_dir_all(p))
                .map_err(|_| {
                    anyhow::anyhow!("Cannot create a directory inside a virtual directory")
                }),
        }
    }

    #[allow(unused)]
    pub fn create<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot create a file without a filename");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.create(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.create(p)).map_err(|_| {
                anyhow::anyhow!("Cannot create a file directly in a virtual directory")
            }),
        }
    }

    pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(&self, path: P, data: C) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot write to a directory");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.write(path, data)?),
            VfsDir::Virtual(_) => self
                .route_virtual(path, |c, p| c.write(p, data.as_ref()))
                .map_err(|_| anyhow::anyhow!("Cannot write to a virtual directory")),
        }
    }

    #[allow(unused)]
    pub fn read_dir<P: AsRef<Path>>(&self, path: P) -> Result<Vec<Result<VfsDirEntry>>> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return self.entries();
        }
        match self {
            VfsDir::Real(dir) => {
                let entries = dir.read_dir(path)?;
                Ok(entries
                    .into_iter()
                    .map(|e| Ok(VfsDirEntry::Real(e?)))
                    .collect())
            }
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.read_dir(p)),
        }
    }

    pub fn remove_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot remove a directory as a file");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.remove_file(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.remove_file(p)),
        }
    }

    pub fn remove_dir<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot remove a virtual root or mount point itself");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.remove_dir(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.remove_dir(p)),
        }
    }

    pub fn remove_dir_all<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot remove a virtual root or mount point itself");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.remove_dir_all(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.remove_dir_all(p)),
        }
    }

    #[allow(unused)]
    pub fn read<P: AsRef<Path>>(&self, path: P) -> Result<Vec<u8>> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot read a directory as a file");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.read(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.read(p)),
        }
    }

    #[allow(unused)]
    pub fn read_to_string<P: AsRef<Path>>(&self, path: P) -> Result<String> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot read a directory as a file");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.read_to_string(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.read_to_string(p)),
        }
    }

    #[allow(unused)]
    pub fn set_mtime(&self, path: &Path, mtime: SystemTimeSpec) -> Result<()> {
        if path.as_os_str().is_empty() {
            bail!("Cannot set mtime on a virtual root or mount point itself");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.set_mtime(path, mtime)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.set_mtime(p, mtime)),
        }
    }

    pub fn entries(&self) -> Result<Vec<Result<VfsDirEntry>>> {
        match self {
            VfsDir::Real(dir) => {
                let entries = dir.entries()?;
                Ok(entries
                    .into_iter()
                    .map(|e| Ok(VfsDirEntry::Real(e?)))
                    .collect())
            }
            VfsDir::Virtual(vdir) => {
                let entries = vdir
                    .children
                    .iter()
                    .map(|(name, dir)| {
                        Ok(match dir {
                            VfsDir::Real(real_dir) => VfsDirEntry::Root(RootDirEntry {
                                name: name.clone(),
                                dir: real_dir.clone(),
                            }),
                            VfsDir::Virtual(virtual_dir) => VfsDirEntry::Virtual(VirtualDirEntry {
                                name: name.clone(),
                                dir: virtual_dir.clone(),
                            }),
                        })
                    })
                    .collect();
                Ok(entries)
            }
        }
    }

    pub fn symlink_metadata<P: AsRef<Path>>(&self, path: P) -> Result<VfsMetadata> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return match self {
                VfsDir::Real(dir) => Ok(VfsMetadata::Real(dir.dir_metadata()?)),
                VfsDir::Virtual(_) => Ok(VfsMetadata::Virtual),
            };
        }
        match self {
            VfsDir::Real(dir) => Ok(VfsMetadata::Real(dir.symlink_metadata(path)?)),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.symlink_metadata(p)),
        }
    }

    pub fn read_link_contents<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot read a directory as a symlink");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.read_link_contents(path)?),
            VfsDir::Virtual(_) => self.route_virtual(path, |c, p| c.read_link_contents(p)),
        }
    }

    pub fn set_permissions<P: AsRef<Path>>(&self, path: P, perm: Permissions) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("Cannot set permissions directly on a virtual root or mount point");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.set_permissions(path, perm)?),
            VfsDir::Virtual(_) => {
                self.route_virtual(path, |c, p| c.set_permissions(p, perm.clone()))
            }
        }
    }

    #[cfg(not(windows))]
    pub fn symlink_contents<P: AsRef<Path>, Q: AsRef<Path>>(&self, src: P, dst: Q) -> Result<()> {
        let dst = dst.as_ref();
        if dst.as_os_str().is_empty() {
            bail!("Cannot create a symlink without a filename");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.symlink_contents(src, dst)?),
            VfsDir::Virtual(_) => self
                .route_virtual(dst, |c, p| c.symlink_contents(src.as_ref(), p))
                .map_err(|_| anyhow::anyhow!("Cannot create a symlink in a virtual directory")),
        }
    }

    #[cfg(windows)]
    pub fn symlink_contents_dir<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        src: P,
        dst: Q,
    ) -> Result<()> {
        let dst = dst.as_ref();
        if dst.as_os_str().is_empty() {
            bail!("Cannot create a symlink without a filename");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.symlink_dir(src, dst)?),
            VfsDir::Virtual(_) => self
                .route_virtual(dst, |c, p| c.symlink_contents_dir(src.as_ref(), p))
                .map_err(|_| anyhow::anyhow!("Cannot create a symlink in a virtual directory")),
        }
    }

    #[cfg(windows)]
    pub fn symlink_contents_file<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        src: P,
        dst: Q,
    ) -> Result<()> {
        let dst = dst.as_ref();
        if dst.as_os_str().is_empty() {
            bail!("Cannot create a symlink without a filename");
        }
        match self {
            VfsDir::Real(dir) => Ok(dir.symlink_file(src, dst)?),
            VfsDir::Virtual(_) => self
                .route_virtual(dst, |c, p| c.symlink_contents_file(src.as_ref(), p))
                .map_err(|_| anyhow::anyhow!("Cannot create a symlink in a virtual directory")),
        }
    }
}

pub struct VirtualDirEntry {
    name: OsString,
    dir: Arc<VirtualDir>,
}

#[derive(Clone)]
pub struct RootDirEntry {
    name: OsString,
    dir: Arc<Dir>,
}

pub enum VfsDirEntry {
    Root(RootDirEntry),
    Real(DirEntry),
    Virtual(VirtualDirEntry),
}

impl VfsDirEntry {
    pub fn open(&self) -> Result<File> {
        match self {
            VfsDirEntry::Real(entry) => Ok(entry.open()?),
            _ => bail!("Cannot open a directory as a file"),
        }
    }

    #[allow(unused)]
    pub fn open_with(&self, options: &OpenOptions) -> Result<File> {
        match self {
            VfsDirEntry::Real(entry) => Ok(entry.open_with(options)?),
            _ => bail!("Cannot open a directory as a file"),
        }
    }

    pub fn open_dir(&self) -> Result<VfsDir> {
        match self {
            VfsDirEntry::Root(entry) => Ok(VfsDir::Real(entry.dir.clone())),
            VfsDirEntry::Real(entry) => Ok(VfsDir::Real(Arc::new(entry.open_dir()?))),
            VfsDirEntry::Virtual(entry) => Ok(VfsDir::Virtual(entry.dir.clone())),
        }
    }

    pub fn metadata(&self) -> Result<VfsMetadata> {
        match self {
            VfsDirEntry::Root(entry) => Ok(VfsMetadata::Real(entry.dir.dir_metadata()?)),
            VfsDirEntry::Real(entry) => Ok(VfsMetadata::Real(entry.metadata()?)),
            VfsDirEntry::Virtual(_) => Ok(VfsMetadata::Virtual),
        }
    }

    pub fn file_type(&self) -> Result<FileType> {
        match self {
            VfsDirEntry::Root(entry) => Ok(entry.dir.dir_metadata()?.file_type()),
            VfsDirEntry::Real(entry) => Ok(entry.file_type()?),
            VfsDirEntry::Virtual(_) => Ok(FileType::dir()),
        }
    }

    pub fn file_name(&self) -> OsString {
        match self {
            VfsDirEntry::Root(entry) => entry.name.clone(),
            VfsDirEntry::Real(entry) => entry.file_name(),
            VfsDirEntry::Virtual(entry) => entry.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use cap_std::ambient_authority;

    use super::*;

    #[test]
    fn test_vfs_builder_mounts_dir() -> Result<()> {
        let mut builder = VfsDirBuilder::new();

        let temp_dir = tempfile::tempdir()?;

        let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())?;
        builder.mount_dir("test", dir)?;

        let vfs = builder.build();

        let mut entries = vfs.entries()?;

        assert_eq!(entries.len(), 1);

        let entry = entries.remove(0)?;

        assert_eq!(entry.file_name(), "test");

        assert!(matches!(entry, VfsDirEntry::Root(_)));

        Ok(())
    }

    #[test]
    fn test_vfs_builder_mounts_nested_dir() -> Result<()> {
        let mut builder = VfsDirBuilder::new();

        let temp_dir = tempfile::tempdir()?;

        let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())?;
        builder.mount_dir("nested/test", dir)?;

        let vfs = builder.build();

        let mut entries = vfs.entries()?;

        assert_eq!(entries.len(), 1);

        let entry = entries.remove(0)?;

        assert_eq!(entry.file_name(), "nested");

        assert!(matches!(entry, VfsDirEntry::Virtual(_)));

        let mut nested_entries = entry.open_dir()?.entries()?;

        assert_eq!(nested_entries.len(), 1);

        let nested_entry = nested_entries.remove(0)?;

        assert_eq!(nested_entry.file_name(), "test");

        assert!(matches!(nested_entry, VfsDirEntry::Root(_)));

        Ok(())
    }

    #[test]
    fn test_vfs_builder_mounts_multiple_dirs() -> Result<()> {
        let mut builder = VfsDirBuilder::new();

        let temp_dir1 = tempfile::tempdir()?;
        let temp_dir2 = tempfile::tempdir()?;

        let dir1 = Dir::open_ambient_dir(temp_dir1.path(), ambient_authority())?;
        let dir2 = Dir::open_ambient_dir(temp_dir2.path(), ambient_authority())?;

        builder.mount_dir("dir1", dir1)?;
        builder.mount_dir("dir2", dir2)?;

        let vfs = builder.build();

        let mut entries = vfs.entries()?;

        assert_eq!(entries.len(), 2);

        let entry1 = entries.remove(0)?;

        let entry2 = entries.remove(0)?;

        assert!(entry1.file_name() == "dir1" || entry1.file_name() == "dir2");

        assert!(entry2.file_name() == "dir1" || entry2.file_name() == "dir2");

        assert!(entry1.file_name() != entry2.file_name());

        assert!(matches!(entry1, VfsDirEntry::Root(_)));

        assert!(matches!(entry2, VfsDirEntry::Root(_)));

        Ok(())
    }

    #[test]
    fn test_vfs_open_file_direct() -> Result<()> {
        let mut builder = VfsDirBuilder::new();

        let temp_dir = tempfile::tempdir()?;

        let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())?;
        builder.mount_dir("test", dir)?;

        let vfs = builder.build();

        let mut file =
            vfs.open_with("test/file.txt", OpenOptions::new().create(true).write(true))?;

        file.write_all(b"Hello, World!")?;

        drop(file);

        let mut file = vfs.open("test/file.txt")?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        assert_eq!(contents, "Hello, World!");

        Ok(())
    }

    #[test]
    fn test_vfs_open_file_via_parent_dir() -> Result<()> {
        let mut builder = VfsDirBuilder::new();

        let temp_dir = tempfile::tempdir()?;

        let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())?;
        builder.mount_dir("test", dir)?;

        let vfs = builder.build();

        let dir = vfs.open_dir("test")?;

        let mut file = dir.open_with("test.txt", OpenOptions::new().create(true).write(true))?;

        file.write_all(b"Hello, World!")?;

        drop(file);

        let mut file = dir.open("test.txt")?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        assert_eq!(contents, "Hello, World!");

        Ok(())
    }

    #[test]
    fn test_vfs_open_file_via_parent_entry() -> Result<()> {
        let mut builder = VfsDirBuilder::new();

        let temp_dir = tempfile::tempdir()?;

        let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())?;
        builder.mount_dir("test", dir)?;

        let vfs = builder.build();

        let mut entries = vfs.entries()?;

        let entry = entries.remove(0)?;

        let dir = entry.open_dir()?;

        let mut file = dir.open_with("test.txt", OpenOptions::new().create(true).write(true))?;

        file.write_all(b"Hello, World!")?;

        drop(file);

        let mut file = dir.open("test.txt")?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)?;

        assert_eq!(contents, "Hello, World!");

        Ok(())
    }
}
