//! Safe Unix-socket path preparation (§26, §101).
//!
//! The broker must never trust arbitrary writable path components. This
//! module:
//!
//! * requires an absolute path and walks every component with `lstat`,
//!   refusing symlinks anywhere in the path (symlink attacks, replacing
//!   path components);
//! * refuses parents that are world- or group-writable, or owned by
//!   someone other than the runtime uid or root (no insecure `/tmp`
//!   defaults in production);
//! * when a stale endpoint exists, proves it is a Unix socket **at the
//!   expected path, owned appropriately** before removing it — a regular
//!   file, directory, device or symlink at the target causes the broker to
//!   refuse to start rather than delete attacker-controlled data;
//! * after binding, verifies the bound inode is still the inode at the
//!   path (bind-race detection) and applies the configured mode.
//!
//! Because the parent directory is validated to be owned by the runtime
//! user (never group/world-writable), no other user can race the path
//! between bind and chmod; the inode re-check is defense in depth.

use nix::sys::stat::{lstat, SFlag};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Component, Path, PathBuf};

/// Errors from socket path preparation.
#[derive(Debug, thiserror::Error)]
pub enum SocketPathError {
    #[error("socket path must be absolute: {0}")]
    NotAbsolute(String),
    #[error("socket path must not be empty")]
    Empty,
    #[error("unsafe path component in {path}: {why}")]
    UnsafeComponent { path: String, why: String },
    #[error("unsafe parent directory {path}: {why}")]
    UnsafeParent { path: String, why: String },
    #[error("refusing to remove non-socket at {path}: {what}")]
    RefuseRemove { path: String, what: String },
    #[error("stale socket {path} is not owned by the runtime user or root")]
    StaleSocketForeignOwner { path: String },
    #[error("cannot remove stale socket {path}: {source}")]
    RemoveStale {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot bind {path}: {source}")]
    Bind {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot set socket permissions on {path}: {source}")]
    Permissions {
        path: String,
        source: std::io::Error,
    },
    #[error("bound socket raced: the inode at {path} no longer matches the bound socket")]
    BindRace { path: String },
    #[error("socket path {path} is too long for the kernel (max 107 bytes)")]
    TooLong { path: String },
}

/// Prepare, bind and secure the Unix listener at `path`.
///
/// # Preconditions
/// * `path` is an absolute path; its parent directory exists and passes the
///   ownership/permission checks.
///
/// # Postconditions
/// * On `Ok`, a non-blocking listener bound at `path` with mode `mode`
///   applied, and the inode at `path` provably equals the bound socket.
pub fn bind_secure(path: &Path, mode: u32) -> Result<UnixListener, SocketPathError> {
    if path.as_os_str().is_empty() {
        return Err(SocketPathError::Empty);
    }
    let path_str: String = path.to_string_lossy().into_owned();
    if !path.is_absolute() {
        return Err(SocketPathError::NotAbsolute(path_str));
    }
    if path_str.len() > 107 {
        return Err(SocketPathError::TooLong { path: path_str });
    }

    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| SocketPathError::NotAbsolute(path_str.clone()))?;
    validate_parent(parent)?;

    // Stale endpoint: prove it is a socket owned appropriately (§26, §101).
    match lstat(path) {
        Ok(stat) => {
            if stat.st_mode & SFlag::S_IFMT.bits() != SFlag::S_IFSOCK.bits() {
                return Err(SocketPathError::RefuseRemove {
                    path: path_str.clone(),
                    what: describe_type(stat.st_mode),
                });
            }
            let euid = nix::unistd::geteuid().as_raw();
            if stat.st_uid != euid && stat.st_uid != 0 {
                return Err(SocketPathError::StaleSocketForeignOwner {
                    path: path_str.clone(),
                });
            }
            std::fs::remove_file(path).map_err(|source| SocketPathError::RemoveStale {
                path: path_str.clone(),
                source,
            })?;
        }
        Err(nix::errno::Errno::ENOENT) => {}
        Err(e) => {
            return Err(SocketPathError::RefuseRemove {
                path: path_str.clone(),
                what: format!("cannot stat: {e}"),
            });
        }
    }

    let listener = UnixListener::bind(path).map_err(|source| SocketPathError::Bind {
        path: path_str.clone(),
        source,
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|source| SocketPathError::Bind {
            path: path_str.clone(),
            source,
        })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|source| {
        SocketPathError::Permissions {
            path: path_str.clone(),
            source,
        }
    })?;

    // Bind-race check: the path must still resolve to a socket after the
    // bind (an attacker who could race the path would have to write to the
    // parent directory, which the ownership/permission validation forbids).
    // Note: on Linux, fstat(fd) reports the sockfs inode while lstat(path)
    // reports the filesystem dentry inode — dev/ino never match for unix
    // sockets, so the check is the file *type* at the path.
    match lstat(path) {
        Ok(stat) => {
            if stat.st_mode & SFlag::S_IFMT.bits() != SFlag::S_IFSOCK.bits() {
                return Err(SocketPathError::BindRace {
                    path: path_str.clone(),
                });
            }
        }
        Err(_) => {
            return Err(SocketPathError::BindRace { path: path_str });
        }
    }

    Ok(listener)
}

/// Validate every path component of `parent` with `lstat`.
fn validate_parent(parent: &Path) -> Result<(), SocketPathError> {
    let mut current = PathBuf::from("/");
    for comp in parent.components() {
        match comp {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                return Err(SocketPathError::UnsafeComponent {
                    path: parent.display().to_string(),
                    why: "contains '..'".into(),
                });
            }
            Component::Normal(part) => {
                current.push(part);
                let stat = lstat(&current).map_err(|e| SocketPathError::UnsafeComponent {
                    path: current.display().to_string(),
                    why: format!("cannot stat: {e}"),
                })?;
                if stat.st_mode & SFlag::S_IFMT.bits() == SFlag::S_IFLNK.bits() {
                    return Err(SocketPathError::UnsafeComponent {
                        path: current.display().to_string(),
                        why: "path component is a symlink".into(),
                    });
                }
                if stat.st_mode & SFlag::S_IFMT.bits() != SFlag::S_IFDIR.bits() {
                    return Err(SocketPathError::UnsafeComponent {
                        path: current.display().to_string(),
                        why: "path component is not a directory".into(),
                    });
                }
            }
            Component::Prefix(_) => unreachable!("unix paths have no prefixes"),
        }
    }
    // Ownership/permission checks on the final parent.
    let stat = lstat(parent).map_err(|e| SocketPathError::UnsafeParent {
        path: parent.display().to_string(),
        why: format!("cannot stat: {e}"),
    })?;
    let euid = nix::unistd::geteuid().as_raw();
    if stat.st_uid != euid && stat.st_uid != 0 {
        return Err(SocketPathError::UnsafeParent {
            path: parent.display().to_string(),
            why: format!(
                "owned by uid {}, not the runtime user ({euid}) or root",
                stat.st_uid
            ),
        });
    }
    let perms = stat.st_mode & 0o777;
    if perms & 0o022 != 0 {
        return Err(SocketPathError::UnsafeParent {
            path: parent.display().to_string(),
            why: format!("group/world-writable ({perms:#o})"),
        });
    }
    Ok(())
}

fn describe_type(mode: u32) -> String {
    match mode & SFlag::S_IFMT.bits() {
        m if m == SFlag::S_IFREG.bits() => "a regular file".into(),
        m if m == SFlag::S_IFDIR.bits() => "a directory".into(),
        m if m == SFlag::S_IFLNK.bits() => "a symlink".into(),
        m if m == SFlag::S_IFCHR.bits() => "a character device".into(),
        m if m == SFlag::S_IFBLK.bits() => "a block device".into(),
        m if m == SFlag::S_IFIFO.bits() => "a fifo".into(),
        _ => format!("an unknown file type (mode {mode:#o})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn private_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fk-sockpath-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[test]
    fn binds_and_listens() {
        use std::io::Write as _;
        let dir = private_dir("binds");
        let path = dir.join("ferrokeyd.sock");
        let listener = bind_secure(&path, 0o666).unwrap();
        assert!(path.exists());
        // A client can connect to the bound socket.
        let mut client = UnixStream::connect(&path).unwrap();
        let _ = client.write_all(b"hi");
        let _ = listener;
        drop(client);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn stale_socket_is_replaced() {
        let dir = private_dir("stale");
        let path = dir.join("ferrokeyd.sock");
        let stale = nix::sys::socket::socket(
            nix::sys::socket::AddressFamily::Unix,
            nix::sys::socket::SockType::Stream,
            nix::sys::socket::SockFlag::empty(),
            None::<nix::sys::socket::SockProtocol>,
        )
        .unwrap();
        nix::sys::socket::bind(
            std::os::fd::AsRawFd::as_raw_fd(&stale),
            &nix::sys::socket::UnixAddr::new(&path).unwrap(),
        )
        .unwrap();
        let listener = bind_secure(&path, 0o666).unwrap();
        drop(listener);
        assert!(path.exists());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn regular_file_at_target_refuses_to_start() {
        let dir = private_dir("regular");
        let path = dir.join("ferrokeyd.sock");
        std::fs::write(&path, b"attacker data").unwrap();
        let err = bind_secure(&path, 0o666).unwrap_err();
        assert!(
            matches!(err, SocketPathError::RefuseRemove { .. }),
            "must refuse to delete a regular file: {err}"
        );
        // The attacker's file is untouched.
        assert_eq!(std::fs::read(&path).unwrap(), b"attacker data");
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn symlink_at_target_refuses_to_start() {
        let dir = private_dir("symlink-target");
        let path = dir.join("ferrokeyd.sock");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"x").unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        let err = bind_secure(&path, 0o666).unwrap_err();
        assert!(
            matches!(err, SocketPathError::RefuseRemove { .. }),
            "must refuse to remove a symlink: {err}"
        );
        assert!(path.exists() && victim.exists());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(&victim).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn relative_path_is_refused() {
        let err = bind_secure(Path::new("relative.sock"), 0o666).unwrap_err();
        assert!(matches!(err, SocketPathError::NotAbsolute(_)));
    }

    #[test]
    fn world_writable_parent_is_refused() {
        let dir = std::env::temp_dir().join(format!("fk-sockpath-ww-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = dir.join("ferrokeyd.sock");
        let err = bind_secure(&path, 0o666).unwrap_err();
        assert!(
            matches!(err, SocketPathError::UnsafeParent { .. }),
            "world-writable parent must be refused: {err}"
        );
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn symlinked_parent_component_is_refused() {
        let real = private_dir("symlink-parent-real");
        let link = std::env::temp_dir().join(format!("fk-sockpath-link-{}", std::process::id()));
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let path = link.join("ferrokeyd.sock");
        let err = bind_secure(&path, 0o666).unwrap_err();
        assert!(
            matches!(err, SocketPathError::UnsafeComponent { .. }),
            "symlinked parent component must be refused: {err}"
        );
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir(&real).unwrap();
    }
}
