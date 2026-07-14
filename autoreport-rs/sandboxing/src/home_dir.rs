use autoreport_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::path::PathBuf;

/// Returns the path to the AutoReport configuration directory, which can be
/// specified by the `AUTOREPORT_HOME` environment variable. If not set, defaults to
/// `~/.autoreport`.
///
/// - If `AUTOREPORT_HOME` is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If `AUTOREPORT_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_autoreport_home() -> std::io::Result<AbsolutePathBuf> {
    let autoreport_home_env = std::env::var("AUTOREPORT_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    find_autoreport_home_from_env(autoreport_home_env.as_deref())
}

fn find_autoreport_home_from_env(autoreport_home_env: Option<&str>) -> std::io::Result<AbsolutePathBuf> {
    // Honor the `AUTOREPORT_HOME` environment variable when it is set to allow users
    // (and tests) to override the default location.
    match autoreport_home_env {
        Some(val) => {
            let path = PathBuf::from(val);
            let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("AUTOREPORT_HOME points to {val:?}, but that path does not exist"),
                ),
                _ => std::io::Error::new(
                    err.kind(),
                    format!("failed to read AUTOREPORT_HOME {val:?}: {err}"),
                ),
            })?;

            if !metadata.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("AUTOREPORT_HOME points to {val:?}, but that path is not a directory"),
                ))
            } else {
                let canonical = path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize AUTOREPORT_HOME {val:?}: {err}"),
                    )
                })?;
                AbsolutePathBuf::from_absolute_path(canonical)
            }
        }
        None => {
            let mut p = home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(".autoreport");
            AbsolutePathBuf::from_absolute_path(p)
        }
    }
}
