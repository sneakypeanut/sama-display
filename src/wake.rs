//! Best-effort DTR/RTS wake of the exact sleeping CDC bridge (1a86:ca21/CT21INCH).

use anyhow::Result;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::time::Duration;

pub const BRIDGE_VENDOR: &str = "1a86";
pub const BRIDGE_PRODUCT: &str = "ca21";
pub const BRIDGE_SERIAL: &str = "CT21INCH";

/// tty operations kept behind a seam so tests never need a real tty.
pub trait TtyOps {
    fn open(&self, path: &Path) -> io::Result<File>;
    fn assert_modem_lines(&self, file: &File) -> io::Result<()>;
}

pub struct RealTtyOps;

impl TtyOps for RealTtyOps {
    fn open(&self, path: &Path) -> io::Result<File> {
        use std::os::fd::FromRawFd;
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        // SAFETY: the C string lives for the call and the returned fd is owned
        // by the File immediately afterwards.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: open returned a fresh file descriptor owned by this function.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn assert_modem_lines(&self, file: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let lines: libc::c_int = libc::TIOCM_DTR | libc::TIOCM_RTS;
        // SAFETY: TIOCMBIS takes a pointer to an int with the modem lines to
        // set; the reference outlives the ioctl call.
        let result = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                libc::TIOCMBIS as libc::c_ulong,
                &lines as *const libc::c_int,
            )
        };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn identity_value(ancestor: &Path, key: &str) -> Option<String> {
    fs::read_to_string(ancestor.join(key))
        .ok()
        .map(|value| value.trim().to_string())
}

fn is_bridge(ancestor: &Path) -> bool {
    identity_value(ancestor, "idVendor").as_deref() == Some(BRIDGE_VENDOR)
        && identity_value(ancestor, "idProduct").as_deref() == Some(BRIDGE_PRODUCT)
        && identity_value(ancestor, "serial").as_deref() == Some(BRIDGE_SERIAL)
}

/// Opens the matched tty and asserts DTR/RTS without writing serial payload.
/// Returns `false` when no bridge with the exact identity is present.
pub fn wake_bridge(
    sysfs: &Path,
    devroot: &Path,
    ops: &dyn TtyOps,
    pause: &dyn Fn(Duration),
) -> Result<bool> {
    for entry in fs::read_dir(sysfs)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("tty") {
            continue;
        }
        let resolved = match fs::canonicalize(entry.path()) {
            Ok(resolved) => resolved,
            Err(_) => continue,
        };
        if !resolved.ancestors().any(is_bridge) {
            continue;
        }
        let file = ops.open(&devroot.join(name.as_ref()))?;
        ops.assert_modem_lines(&file)?;
        pause(Duration::from_millis(250));
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::symlink;

    struct FakeOps {
        opened: RefCell<Vec<std::path::PathBuf>>,
        asserted: RefCell<usize>,
        file: RefCell<Option<File>>,
    }

    impl FakeOps {
        fn new() -> Self {
            Self {
                opened: RefCell::new(Vec::new()),
                asserted: RefCell::new(0),
                file: RefCell::new(None),
            }
        }
    }

    impl TtyOps for FakeOps {
        fn open(&self, path: &Path) -> io::Result<File> {
            self.opened.borrow_mut().push(path.to_path_buf());
            let file = tempfile::tempfile()?;
            *self.file.borrow_mut() = Some(file.try_clone()?);
            Ok(file)
        }

        fn assert_modem_lines(&self, _file: &File) -> io::Result<()> {
            *self.asserted.borrow_mut() += 1;
            Ok(())
        }
    }

    #[test]
    fn bridge_exact_identity_and_dtr_only() {
        let root = tempfile::tempdir().expect("tempdir");
        let sysfs = root.path().join("sys");
        let devroot = root.path().join("dev");
        fs::create_dir_all(&sysfs).unwrap();
        fs::create_dir_all(&devroot).unwrap();
        let bridge = sysfs.join("usb-device");
        fs::create_dir_all(&bridge).unwrap();
        for (key, value) in [
            ("idVendor", "1a86"),
            ("idProduct", "ca21"),
            ("serial", "CT21INCH"),
        ] {
            fs::write(bridge.join(key), value).unwrap();
        }
        symlink(&bridge, sysfs.join("ttyACM0")).unwrap();

        let ops = FakeOps::new();
        let waited = std::cell::Cell::new(Duration::ZERO);
        assert!(
            wake_bridge(&sysfs, &devroot, &ops, &|duration| waited.set(duration)).expect("wake")
        );
        assert_eq!(ops.opened.borrow().as_slice(), [devroot.join("ttyACM0")]);
        assert_eq!(*ops.asserted.borrow(), 1);
        assert_eq!(waited.get(), Duration::from_millis(250));

        fs::write(bridge.join("serial"), "not-ours").unwrap();
        let ops = FakeOps::new();
        assert!(!wake_bridge(&sysfs, &devroot, &ops, &|_| {}).expect("wake without match"));
        assert!(ops.opened.borrow().is_empty());
    }
}
