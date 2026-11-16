//! Storage capability probes: does this directory's filesystem clone blocks?
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-call probe counter. The probe file names must be unique per call, not
/// just per process: two `pgboxdb` operations in one process — or the test suite's
/// parallel tests — can probe the same directory at the same time, and sharing
/// a name makes `clone_file` race (EEXIST/ENOENT) and report a false negative.
static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Probes block cloning by actually cloning a small file inside `dir`
/// (`clonefile` on macOS, `FICLONE` on Linux). True means `file_copy_method = clone`
/// shares blocks between a template and its clones on this filesystem.
pub fn supports_block_clone(dir: &Path) -> bool {
    let uniq = format!(
        "{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let src = dir.join(format!(".pgboxdb-clone-probe-{uniq}.src"));
    let dst = dir.join(format!(".pgboxdb-clone-probe-{uniq}.dst"));
    let _ = std::fs::remove_file(&dst);
    if std::fs::write(&src, b"pgboxdb block clone probe").is_err() {
        return false;
    }
    let ok = clone_file(&src, &dst);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dst);
    ok
}

#[cfg(target_os = "macos")]
fn clone_file(src: &Path, dst: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let s = std::ffi::CString::new(src.as_os_str().as_bytes()).unwrap();
    let d = std::ffi::CString::new(dst.as_os_str().as_bytes()).unwrap();
    unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) == 0 }
}

#[cfg(target_os = "linux")]
fn clone_file(src: &Path, dst: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(s) = std::fs::File::open(src) else {
        return false;
    };
    let Ok(d) = std::fs::File::create(dst) else {
        return false;
    };
    unsafe { libc::ioctl(d.as_raw_fd(), libc::FICLONE as _, s.as_raw_fd()) == 0 }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clone_file(_src: &Path, _dst: &Path) -> bool {
    false
}

/// Filesystem type name of the directory, for `pgboxdb doctor` (best effort).
pub fn fs_type_name(dir: &Path) -> String {
    let out = std::process::Command::new("df")
        .args(["-P", "-T"])
        .arg(dir)
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            if let Some(line) = String::from_utf8_lossy(&o.stdout).lines().nth(1) {
                if let Some(t) = line.split_whitespace().nth(1) {
                    return t.to_string();
                }
            }
        }
    }
    // macOS df has no -T; use mount output.
    if let Ok(o) = std::process::Command::new("mount").output() {
        let dir_s = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let mut best: Option<(usize, String)> = None;
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            // "/dev/disk3s1 on / (apfs, local, journaled)"
            if let Some((_, rest)) = line.split_once(" on ") {
                if let Some((mp, kind)) = rest.split_once(" (") {
                    if dir_s.starts_with(mp)
                        && best.as_ref().map(|(l, _)| mp.len() > *l).unwrap_or(true)
                    {
                        best = Some((
                            mp.len(),
                            kind.split([',', ')']).next().unwrap_or("").to_string(),
                        ));
                    }
                }
            }
        }
        if let Some((_, k)) = best {
            return k;
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private, unique subdirectory of the system temp dir. It lives on the
    /// same filesystem as the temp dir (so clone-capability is identical) but
    /// isolates each test's probe files from the other parallel tests.
    fn isolated_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pgboxdb-fsdetect-{tag}-{}-{}",
            std::process::id(),
            PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn probe_cleans_up() {
        let dir = isolated_dir("cleanup");
        let _ = supports_block_clone(&dir);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".pgboxdb-clone-probe")
            })
            .collect();
        let empty = leftovers.is_empty();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(empty, "{leftovers:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fsdetect_apfs_is_clone_capable() {
        // The macOS system temp dir is APFS; a private subdirectory of it shares
        // that filesystem, so this still proves the temp dir is clone-capable
        // while isolating the probe from other parallel tests.
        let dir = isolated_dir("apfs");
        let fs = fs_type_name(&dir);
        let ok = supports_block_clone(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(fs, "apfs");
        assert!(ok);
    }
}
