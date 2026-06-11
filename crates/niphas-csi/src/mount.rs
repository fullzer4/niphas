use std::ffi::CString;
use std::io;
use std::path::Path;

/// Mount flags for bind mount.
const MS_BIND: libc::c_ulong = 4096;
const MS_RDONLY: libc::c_ulong = 1;
const MS_NOSUID: libc::c_ulong = 2;
const MS_NODEV: libc::c_ulong = 4;
const MS_REMOUNT: libc::c_ulong = 32;

/// Trait abstracting filesystem mount operations.
///
/// Enables testing `NodeService` without root privileges or real mounts.
pub trait MountOps: Send + Sync {
    fn is_mountpoint(&self, path: &str) -> bool;
    fn setup_target_dir(&self, path: &str) -> io::Result<()>;
    fn bind_mount_readonly(&self, source: &str, target: &str) -> io::Result<()>;
    fn unmount(&self, target: &str) -> io::Result<()>;
    fn cleanup_target(&self, target: &str) -> io::Result<()>;
}

/// Real Linux mount operations using libc syscalls.
pub struct RealMountOps;

impl MountOps for RealMountOps {
    fn is_mountpoint(&self, path: &str) -> bool {
        let Ok(content) = std::fs::read_to_string("/proc/self/mountinfo") else {
            return false;
        };

        let canonical = match std::fs::canonicalize(path) {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => return false,
        };

        for line in content.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 5 && fields[4] == canonical {
                return true;
            }
        }
        false
    }

    fn setup_target_dir(&self, path: &str) -> io::Result<()> {
        let p = Path::new(path);
        if !p.exists() {
            std::fs::create_dir_all(p)?;
        }
        Ok(())
    }

    fn bind_mount_readonly(&self, source: &str, target: &str) -> io::Result<()> {
        let c_source =
            CString::new(source).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let c_target =
            CString::new(target).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // SAFETY: libc::mount with valid CString pointers and standard flags.
        let ret = unsafe {
            libc::mount(
                c_source.as_ptr(),
                c_target.as_ptr(),
                c"none".as_ptr(),
                MS_BIND,
                std::ptr::null(),
            )
        };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: libc::mount remount on already-bound target.
        let ret = unsafe {
            libc::mount(
                std::ptr::null(),
                c_target.as_ptr(),
                std::ptr::null(),
                MS_BIND | MS_REMOUNT | MS_RDONLY | MS_NOSUID | MS_NODEV,
                std::ptr::null(),
            )
        };
        if ret != 0 {
            let _ = self.unmount(target);
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    fn unmount(&self, target: &str) -> io::Result<()> {
        let c_target =
            CString::new(target).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // SAFETY: libc::umount2 with valid CString pointer and MNT_DETACH flag.
        let ret = unsafe { libc::umount2(c_target.as_ptr(), libc::MNT_DETACH) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn cleanup_target(&self, target: &str) -> io::Result<()> {
        let path = Path::new(target);
        if path.exists() {
            std::fs::remove_dir(path)?;
        }
        Ok(())
    }
}
