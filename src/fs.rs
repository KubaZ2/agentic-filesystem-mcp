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
        let mut current_node = self;
        let mut components = path.as_ref().components().peekable();

        if components.peek().is_none() {
            bail!("Cannot mount a directory with an empty path");
        }

        while let Some(component) = components.next() {
            let is_last = components.peek().is_none();

            let children = match current_node {
                VfsDirBuilder::Virtual(map) => map,
                _ => bail!("Cannot mount a directory on a root directory"),
            };

            if is_last {
                if children.contains_key(component.as_os_str()) {
                    bail!("Path already exists in virtual directory");
                }

                children.insert(
                    component.as_os_str().to_os_string(),
                    VfsDirBuilder::Root(dir),
                );

                break;
            } else {
                current_node = children
                    .entry(component.as_os_str().to_os_string())
                    .or_insert_with(|| VfsDirBuilder::Virtual(HashMap::new()));
            }
        }

        Ok(())
    }

    pub fn build(self) -> VfsDir {
        enum Task {
            Process(VfsDirBuilder),
            BuildVirtual(usize),
        }

        let mut tasks = vec![Task::Process(self)];
        let mut results = vec![];
        let mut names = vec![];

        while let Some(task) = tasks.pop() {
            match task {
                Task::Process(VfsDirBuilder::Root(dir)) => {
                    results.push(VfsDir::Real(Arc::new(dir)));
                }
                Task::Process(VfsDirBuilder::Virtual(children)) => {
                    tasks.push(Task::BuildVirtual(children.len()));

                    for (name, child) in children {
                        names.push(name);
                        tasks.push(Task::Process(child));
                    }
                }
                Task::BuildVirtual(num_children) => {
                    let mut vfs_children = HashMap::with_capacity(num_children);

                    for _ in 0..num_children {
                        let child_vfs = results.pop().unwrap();
                        let name = names.pop().unwrap();
                        vfs_children.insert(name, child_vfs);
                    }

                    results.push(VfsDir::Virtual(Arc::new(VirtualDir {
                        children: vfs_children,
                    })));
                }
            }
        }

        results.pop().unwrap()
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

#[allow(unused)]
impl VfsDir {
    fn resolve<'a>(&'a self, path: &'a Path) -> Result<(&'a VfsDir, &'a Path)> {
        let mut current_dir = self;
        let mut current_path = path;

        while let VfsDir::Virtual(vdir) = current_dir {
            let mut components = current_path.components();

            let Some(first) = components.next() else {
                break;
            };

            if let Some(child) = vdir.children.get(first.as_os_str()) {
                current_dir = child;
                current_path = components.as_path();
            } else {
                bail!("No such file or directory");
            }
        }

        Ok((current_dir, current_path))
    }

    pub fn exists<P: AsRef<Path>>(&self, path: P) -> bool {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return false;
        }

        match self.resolve(path) {
            Ok((dir, remaining)) => {
                if remaining.as_os_str().is_empty() {
                    true
                } else {
                    match dir {
                        VfsDir::Real(r) => r.exists(remaining),
                        VfsDir::Virtual(_) => unreachable!(),
                    }
                }
            }
            Err(_) => false,
        }
    }

    pub fn open_dir<P: AsRef<Path>>(&self, path: P) -> Result<VfsDir> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            return Ok(dir.clone());
        }

        match dir {
            VfsDir::Real(r) => Ok(VfsDir::Real(Arc::new(r.open_dir(remaining)?))),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn read_dir<P: AsRef<Path>>(&self, path: P) -> Result<Vec<Result<VfsDirEntry>>> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            return dir.entries();
        }

        match dir {
            VfsDir::Real(r) => {
                let entries = r.read_dir(remaining)?;
                Ok(entries
                    .into_iter()
                    .map(|e| Ok(VfsDirEntry::Real(e?)))
                    .collect())
            }
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn metadata<P: AsRef<Path>>(&self, path: P) -> Result<VfsMetadata> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            return match dir {
                VfsDir::Real(r) => Ok(VfsMetadata::Real(r.dir_metadata()?)),
                VfsDir::Virtual(_) => Ok(VfsMetadata::Virtual),
            };
        }

        match dir {
            VfsDir::Real(r) => Ok(VfsMetadata::Real(r.metadata(remaining)?)),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn symlink_metadata<P: AsRef<Path>>(&self, path: P) -> Result<VfsMetadata> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            return match dir {
                VfsDir::Real(r) => Ok(VfsMetadata::Real(r.dir_metadata()?)),
                VfsDir::Virtual(_) => Ok(VfsMetadata::Virtual),
            };
        }

        match dir {
            VfsDir::Real(r) => Ok(VfsMetadata::Real(r.symlink_metadata(remaining)?)),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn open<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.open(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn open_with<P: AsRef<Path>>(&self, path: P, options: &OpenOptions) -> Result<File> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.open_with(remaining, options)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn read<P: AsRef<Path>>(&self, path: P) -> Result<Vec<u8>> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.read(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn read_to_string<P: AsRef<Path>>(&self, path: P) -> Result<String> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.read_to_string(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(&self, path: P, data: C) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.write(remaining, data.as_ref())?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn create<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.create(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn read_link_contents<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Invalid argument");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.read_link_contents(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn create_dir<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("File exists");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.create_dir(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn create_dir_all<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            return Ok(());
        }

        match dir {
            VfsDir::Real(r) => Ok(r.create_dir_all(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn remove_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.remove_file(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn remove_dir<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Device or resource busy");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.remove_dir(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn remove_dir_all<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Device or resource busy");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.remove_dir_all(remaining)?),
            VfsDir::Virtual(_) => unreachable!(),
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
        if from.as_os_str().is_empty() || to.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (r_from_dir, rem_from) = self.resolve(from)?;
        let (r_to_dir, rem_to) = to_dir.resolve(to)?;

        if rem_from.as_os_str().is_empty() || rem_to.as_os_str().is_empty() {
            bail!("Device or resource busy");
        }

        match (r_from_dir, r_to_dir) {
            (VfsDir::Real(real_from), VfsDir::Real(real_to)) => {
                real_from.rename(rem_from, real_to, rem_to)?;
                Ok(())
            }
            _ => unreachable!(),
        }
    }

    pub fn set_mtime<P: AsRef<Path>>(&self, path: P, mtime: SystemTimeSpec) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Operation not permitted");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.set_mtime(remaining, mtime)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    pub fn set_permissions<P: AsRef<Path>>(&self, path: P, perm: Permissions) -> Result<()> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, remaining) = self.resolve(path)?;
        if remaining.as_os_str().is_empty() {
            bail!("Operation not permitted");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.set_permissions(remaining, perm)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    #[cfg(not(windows))]
    pub fn symlink_contents<P: AsRef<Path>, Q: AsRef<Path>>(&self, src: P, dst: Q) -> Result<()> {
        let src = src.as_ref();
        let dst = dst.as_ref();
        if src.as_os_str().is_empty() || dst.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, rem_dst) = self.resolve(dst)?;
        if src.as_os_str().is_empty() || rem_dst.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.symlink_contents(src, rem_dst)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    #[cfg(windows)]
    pub fn symlink_contents_dir<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        src: P,
        dst: Q,
    ) -> Result<()> {
        let src = src.as_ref();
        let dst = dst.as_ref();
        if src.as_os_str().is_empty() || dst.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, rem_dst) = self.resolve(dst)?;
        if src.as_os_str().is_empty() || rem_dst.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.symlink_dir(src, rem_dst)?),
            VfsDir::Virtual(_) => unreachable!(),
        }
    }

    #[cfg(windows)]
    pub fn symlink_contents_file<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        src: P,
        dst: Q,
    ) -> Result<()> {
        let src = src.as_ref();
        let dst = dst.as_ref();
        if src.as_os_str().is_empty() || dst.as_os_str().is_empty() {
            bail!("No such file or directory");
        }

        let (dir, rem_dst) = self.resolve(dst)?;
        if src.as_os_str().is_empty() || rem_dst.as_os_str().is_empty() {
            bail!("Is a directory");
        }

        match dir {
            VfsDir::Real(r) => Ok(r.symlink_file(src, rem_dst)?),
            VfsDir::Virtual(_) => unreachable!(),
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

#[allow(unused)]
impl VfsDirEntry {
    pub fn open(&self) -> Result<File> {
        match self {
            VfsDirEntry::Real(entry) => Ok(entry.open()?),
            _ => bail!("Cannot open a directory as a file"),
        }
    }

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
    use std::ffi::OsString;
    use std::io::{Read, Write};

    use cap_std::ambient_authority;

    use super::*;

    fn setup_vfs() -> Result<(tempfile::TempDir, VfsDir)> {
        let mut builder = VfsDirBuilder::new();
        let temp_dir = tempfile::tempdir()?;
        let dir = Dir::open_ambient_dir(temp_dir.path(), ambient_authority())?;
        builder.mount_dir("mnt", dir)?;
        Ok((temp_dir, builder.build()))
    }

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

    #[test]
    fn test_vfs_exists() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;
        assert!(vfs.exists("mnt"));
        assert!(!vfs.exists("mnt/non_existent.txt"));
        assert!(!vfs.exists("non_existent_mount"));

        vfs.write("mnt/exists.txt", b"data")?;
        assert!(vfs.exists("mnt/exists.txt"));

        assert!(!vfs.exists(""));
        Ok(())
    }

    #[test]
    fn test_vfs_file_io_helpers() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;

        vfs.write("mnt/hello.txt", b"hello string")?;
        assert_eq!(vfs.read_to_string("mnt/hello.txt")?, "hello string");

        let mut file = vfs.create("mnt/bin.dat")?;
        file.write_all(&[1, 2, 3, 4])?;
        drop(file);

        assert_eq!(vfs.read("mnt/bin.dat")?, vec![1, 2, 3, 4]);

        Ok(())
    }

    #[test]
    fn test_vfs_dir_management() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;

        vfs.create_dir("mnt/dir1")?;
        assert!(vfs.exists("mnt/dir1"));
        assert!(vfs.metadata("mnt/dir1")?.is_dir());

        vfs.remove_dir("mnt/dir1")?;
        assert!(!vfs.exists("mnt/dir1"));

        vfs.create_dir_all("mnt/parent/child/grandchild")?;
        assert!(vfs.exists("mnt/parent/child/grandchild"));

        vfs.remove_dir_all("mnt/parent")?;
        assert!(!vfs.exists("mnt/parent"));

        Ok(())
    }

    #[test]
    fn test_vfs_remove_file() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;
        vfs.write("mnt/del.txt", b"")?;
        assert!(vfs.exists("mnt/del.txt"));

        vfs.remove_file("mnt/del.txt")?;
        assert!(!vfs.exists("mnt/del.txt"));

        Ok(())
    }

    #[test]
    fn test_vfs_rename() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;
        vfs.write("mnt/old_name.txt", b"rename content")?;

        vfs.rename("mnt/old_name.txt", &vfs, "mnt/new_name.txt")?;

        assert!(!vfs.exists("mnt/old_name.txt"));
        assert!(vfs.exists("mnt/new_name.txt"));
        assert_eq!(vfs.read_to_string("mnt/new_name.txt")?, "rename content");

        Ok(())
    }

    #[test]
    fn test_vfs_read_dir() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;
        vfs.write("mnt/a.txt", b"")?;
        vfs.write("mnt/b.txt", b"")?;

        let entries = vfs.read_dir("mnt")?;
        assert_eq!(entries.len(), 2);

        let mut names: Vec<OsString> = entries
            .into_iter()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();

        assert_eq!(
            names,
            vec![OsString::from("a.txt"), OsString::from("b.txt")]
        );

        Ok(())
    }

    #[test]
    fn test_vfs_metadata() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;
        vfs.write("mnt/file.txt", b"")?;

        let file_meta = vfs.metadata("mnt/file.txt")?;
        assert!(file_meta.is_file());
        assert!(!file_meta.is_dir());
        assert!(!file_meta.is_symlink());
        assert!(file_meta.modified().is_ok());
        assert!(file_meta.permissions().is_some());

        let dir_meta = vfs.metadata("mnt")?;
        assert!(dir_meta.is_dir());

        Ok(())
    }

    #[test]
    fn test_vfs_attributes_mtime_permissions() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;
        vfs.write("mnt/attr.txt", b"")?;

        vfs.set_mtime("mnt/attr.txt", SystemTimeSpec::SymbolicNow)?;

        let metadata = vfs.metadata("mnt/attr.txt")?;
        if let Some(mut perms) = metadata.permissions() {
            perms.set_readonly(true);
            vfs.set_permissions("mnt/attr.txt", perms)?;
        }

        Ok(())
    }

    #[test]
    #[cfg(not(windows))]
    fn test_vfs_symlinks_unix() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;
        vfs.write("mnt/target.txt", b"symlink target data")?;

        vfs.symlink_contents("target.txt", "mnt/link.txt")?;
        assert!(vfs.exists("mnt/link.txt"));

        assert_eq!(vfs.read_to_string("mnt/link.txt")?, "symlink target data");

        let link_contents = vfs.read_link_contents("mnt/link.txt")?;
        assert_eq!(link_contents.to_str().unwrap(), "target.txt");

        let sym_meta = vfs.symlink_metadata("mnt/link.txt")?;
        assert!(sym_meta.is_symlink());

        Ok(())
    }

    #[test]
    #[cfg(windows)]
    fn test_vfs_symlinks_windows() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;

        vfs.write("mnt/target_file.txt", b"symlink file target")?;
        vfs.symlink_contents_file("target_file.txt", "mnt/link_file.txt")?;
        assert!(vfs.exists("mnt/link_file.txt"));

        vfs.create_dir("mnt/target_dir")?;
        vfs.symlink_contents_dir("target_dir", "mnt/link_dir")?;
        assert!(vfs.exists("mnt/link_dir"));
        assert!(vfs.metadata("mnt/link_dir")?.is_dir());

        Ok(())
    }

    #[test]
    fn test_empty_paths_are_prevented() -> Result<()> {
        let (_td, vfs) = setup_vfs()?;

        assert!(vfs.open_dir("").is_err());
        assert!(vfs.read_dir("").is_err());
        assert!(vfs.metadata("").is_err());
        assert!(vfs.symlink_metadata("").is_err());
        assert!(vfs.open("").is_err());
        assert!(vfs.open_with("", &OpenOptions::new()).is_err());
        assert!(vfs.read("").is_err());
        assert!(vfs.read_to_string("").is_err());
        assert!(vfs.write("", b"").is_err());
        assert!(vfs.create("").is_err());
        assert!(vfs.read_link_contents("").is_err());
        assert!(vfs.create_dir("").is_err());
        assert!(vfs.create_dir_all("").is_err());
        assert!(vfs.remove_file("").is_err());
        assert!(vfs.remove_dir("").is_err());
        assert!(vfs.remove_dir_all("").is_err());
        assert!(vfs.rename("", &vfs, "mnt/new").is_err());
        assert!(vfs.set_mtime("", SystemTimeSpec::SymbolicNow).is_err());

        #[cfg(not(windows))]
        assert!(vfs.symlink_contents("", "mnt/link.txt").is_err());

        #[cfg(windows)]
        assert!(vfs.symlink_contents_file("", "mnt/link.txt").is_err());

        Ok(())
    }
}
