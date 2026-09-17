use anyhow::{Result, bail};
use std::path::{Component, Path};

pub fn sanitize_path<P>(path: &P) -> Result<&Path>
where
    P: AsRef<Path> + ?Sized,
{
    let path = path.as_ref();

    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::RootDir => {
                bail!("Path cannot contain root components");
            }
            Component::Prefix(_) => {
                bail!("Path cannot contain prefix components");
            }
            Component::CurDir => {
                bail!("Path cannot contain leading '.' components");
            }
            Component::ParentDir => {
                bail!("Path cannot contain '..' components");
            }
        }
    }

    Ok(path)
}

pub fn sanitize_path_option<P>(path: Option<&P>) -> Result<&Path>
where
    P: AsRef<Path> + ?Sized,
{
    match path {
        Some(p) => Ok(sanitize_path(p)?),
        None => Ok(Path::new("")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_path_valid() -> Result<()> {
        assert_eq!(sanitize_path("valid/path")?, Path::new("valid/path"));

        Ok(())
    }

    #[test]
    fn test_sanitize_path_invalid() -> Result<()> {
        let result = sanitize_path("invalid/../path");
        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn test_sanitize_path_current_dir_inside_valid() -> Result<()> {
        // Path itself seems to perform some normalization
        assert_eq!(sanitize_path("invalid/./path")?, Path::new("invalid/path"));

        Ok(())
    }

    #[test]
    fn test_sanitize_path_current_dir_prefix_invalid() -> Result<()> {
        let result = sanitize_path("./invalid/path");
        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn test_sanitize_path_root_dir_invalid() -> Result<()> {
        let result = sanitize_path("/invalid/path");
        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn test_sanitize_path_parent_dir_invalid() -> Result<()> {
        let result = sanitize_path("invalid/path/..");

        assert!(result.is_err());

        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn test_sanitize_path_prefix_invalid() -> Result<()> {
        let result = sanitize_path("C:\\valid\\path");

        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn test_sanitize_path_option_some() -> Result<()> {
        assert_eq!(
            sanitize_path_option(Some("valid/path"))?,
            Path::new("valid/path")
        );

        Ok(())
    }

    #[test]
    fn test_sanitize_path_option_some_invalid() -> Result<()> {
        let result = sanitize_path_option(Some("invalid/../path"));
        assert!(result.is_err());

        Ok(())
    }

    #[test]
    fn test_sanitize_path_option_none() -> Result<()> {
        assert_eq!(sanitize_path_option::<Path>(None)?, Path::new(""));

        Ok(())
    }
}
