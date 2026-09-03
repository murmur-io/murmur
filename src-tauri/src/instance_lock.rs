//! Process-wide recording-store ownership.
//!
//! SQL lease expiry is not proof that a capture process died: a temporarily stalled spool writer
//! can miss renewal while CoreAudio keeps filling its bounded ring. A second Murmur must therefore
//! never run startup recovery (which may truncate to the last durable checkpoint) or start another
//! mic-only capture concurrently. This kernel advisory lock is global across release and dev data
//! profiles because both processes share the physical microphone.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

const LOCK_EX: i32 = 0x02;
const LOCK_NB: i32 = 0x04;

#[cfg(target_os = "macos")]
const NOFOLLOW_FLAG: i32 = 0x0000_0100;
#[cfg(not(target_os = "macos"))]
const NOFOLLOW_FLAG: i32 = 0;

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[must_use = "dropping the guard releases Murmur's process-wide instance lock"]
pub(crate) struct InstanceGuard {
    _file: File,
}

pub(crate) enum AcquireResult {
    Acquired(InstanceGuard),
    AlreadyRunning,
}

#[derive(Clone, Copy)]
pub(crate) enum StartupRefusal {
    AlreadyRunning,
    GuardUnavailable,
}

/// The single-instance lock file, scoped to the dev/release split.
///
/// Dev and release keep deliberately separate databases and app-support directories
/// (`state::app_dir_name`), but this lock used ONE fixed filename for both — so a debug build and
/// an installed release refused to run at the same time, each reporting the other as "already
/// running", even though they share no state at all.
///
/// The DIRECTORY stays `com.meetnotes.app` and the RELEASE FILENAME is unchanged. That is
/// deliberate: putting the discriminator in a subdirectory would move the release path too, and
/// during an upgrade an already-running old instance would hold the old path while the new binary
/// took the new one — briefly admitting two live instances, which is the single thing this file
/// exists to prevent. Only the dev build gets a different name.
fn lock_path() -> Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        AppError::Unavailable("could not resolve the single-instance coordination directory".into())
    })?;
    let name = if crate::state::app_dir_name() == "MeetNotes" {
        "murmur.instance.lock"
    } else {
        "murmur-dev.instance.lock"
    };
    Ok(base.join("com.meetnotes.app").join(name))
}

pub(crate) fn acquire() -> Result<AcquireResult> {
    acquire_at(&lock_path()?)
}

fn acquire_at(path: &Path) -> Result<AcquireResult> {
    let parent = path.parent().ok_or_else(|| {
        AppError::Unavailable("single-instance lock has no parent directory".into())
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        AppError::Unavailable(format!(
            "could not create the single-instance coordination directory: {error}"
        ))
    })?;
    let parent_meta = std::fs::symlink_metadata(parent).map_err(|error| {
        AppError::Unavailable(format!(
            "could not inspect the single-instance coordination directory: {error}"
        ))
    })?;
    if parent_meta.file_type().is_symlink() || !parent_meta.is_dir() {
        return Err(AppError::Unavailable(
            "single-instance coordination path is not a directory".into(),
        ));
    }
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        AppError::Unavailable(format!(
            "could not secure the single-instance coordination directory: {error}"
        ))
    })?;
    if std::fs::symlink_metadata(parent)
        .map_err(|error| {
            AppError::Unavailable(format!(
                "could not revalidate the single-instance coordination directory: {error}"
            ))
        })?
        .permissions()
        .mode()
        & 0o777
        != 0o700
    {
        return Err(AppError::Unavailable(
            "single-instance coordination directory is not private".into(),
        ));
    }

    // Persistent inode, never truncated and never unlinked. Kernel ownership—not PID text or file
    // age—is authoritative. Rust opens descriptors close-on-exec, so helpers cannot retain it.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(NOFOLLOW_FLAG)
        .open(path)
        .map_err(|error| {
            AppError::Unavailable(format!("could not open the single-instance lock: {error}"))
        })?;
    // Inspect identity BEFORE chmod: an attacker-controlled hard link must never let startup change
    // permissions on an unrelated file. The open descriptor is authoritative only after regular +
    // single-link and open-vs-name identity checks pass.
    let mut opened = file.metadata().map_err(|error| {
        AppError::Unavailable(format!(
            "could not inspect the single-instance lock: {error}"
        ))
    })?;
    if !opened.is_file() || opened.nlink() != 1 {
        return Err(AppError::Unavailable(
            "single-instance lock is not a private regular file".into(),
        ));
    }

    loop {
        // SAFETY: fd belongs to the live `File`; BSD flock reports failures through errno and does
        // not cross an Objective-C exception boundary.
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        match error.kind() {
            ErrorKind::Interrupted => continue,
            ErrorKind::WouldBlock => return Ok(AcquireResult::AlreadyRunning),
            _ => {
                return Err(AppError::Unavailable(format!(
                    "could not establish exclusive Murmur ownership: {error}"
                )))
            }
        }
    }

    // Detect an open-vs-replace race. We intentionally keep the named inode on disk after exit;
    // close/OOM/SIGKILL releases only its kernel lock.
    let named = std::fs::symlink_metadata(path).map_err(|error| {
        AppError::Unavailable(format!(
            "could not revalidate the single-instance lock: {error}"
        ))
    })?;
    if named.file_type().is_symlink()
        || !named.is_file()
        || named.dev() != opened.dev()
        || named.ino() != opened.ino()
        || named.nlink() != 1
    {
        return Err(AppError::Unavailable(
            "single-instance lock identity changed during acquisition".into(),
        ));
    }

    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            AppError::Unavailable(format!(
                "could not secure the single-instance lock: {error}"
            ))
        })?;
    opened = file.metadata().map_err(|error| {
        AppError::Unavailable(format!(
            "could not revalidate the secured single-instance lock: {error}"
        ))
    })?;
    if !opened.is_file() || opened.nlink() != 1 || opened.permissions().mode() & 0o777 != 0o600 {
        return Err(AppError::Unavailable(
            "single-instance lock could not be secured as a private regular file".into(),
        ));
    }
    let secured_name = std::fs::symlink_metadata(path).map_err(|error| {
        AppError::Unavailable(format!(
            "could not revalidate the secured single-instance lock name: {error}"
        ))
    })?;
    if secured_name.file_type().is_symlink()
        || !secured_name.is_file()
        || secured_name.dev() != opened.dev()
        || secured_name.ino() != opened.ino()
        || secured_name.nlink() != 1
    {
        return Err(AppError::Unavailable(
            "single-instance lock identity changed while it was secured".into(),
        ));
    }

    Ok(AcquireResult::Acquired(InstanceGuard { _file: file }))
}

/// Native pre-Tauri refusal dialog. This is a CoreFoundation C function (no Objective-C message
/// send/exception boundary), and it runs before any library, log, recovery, window, or helper is
/// opened by the rejected process.
pub(crate) fn show_startup_refusal(reason: StartupRefusal) {
    #[cfg(target_os = "macos")]
    {
        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        let (title, body, level) = match reason {
            StartupRefusal::AlreadyRunning => (
                "Murmur is already running",
                "The existing Murmur session was left untouched. This launch was stopped so an active recording cannot be opened or recovered by two processes.",
                1usize,
            ),
            StartupRefusal::GuardUnavailable => (
                "Murmur can’t start safely",
                "Murmur couldn’t secure exclusive access to its recording store. Nothing was opened or changed. Please try again.",
                2usize,
            ),
        };
        let title = CFString::new(title);
        let body = CFString::new(body);
        let ok = CFString::new("OK");
        let mut response = 0usize;
        // SAFETY: all CF references remain alive for the synchronous call; nullable URL/alternate
        // arguments are null as required by the CoreFoundation API.
        let status = unsafe {
            CFUserNotificationDisplayAlert(
                0.0,
                level,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                title.as_CFTypeRef(),
                body.as_CFTypeRef(),
                ok.as_CFTypeRef(),
                std::ptr::null(),
                std::ptr::null(),
                &mut response,
            )
        };
        if status != 0 {
            eprintln!("Murmur could not display the startup refusal dialog");
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = reason;
        eprintln!("Murmur cannot start because exclusive recording ownership is unavailable");
    }
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFUserNotificationDisplayAlert(
        timeout: f64,
        flags: usize,
        icon_url: *const core::ffi::c_void,
        sound_url: *const core::ffi::c_void,
        localization_url: *const core::ffi::c_void,
        header: *const core::ffi::c_void,
        message: *const core::ffi::c_void,
        default_button: *const core::ffi::c_void,
        alternate_button: *const core::ffi::c_void,
        other_button: *const core::ffi::c_void,
        response_flags: *mut usize,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "murmur-instance-{label}-{}-{}/murmur.instance.lock",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn second_open_description_is_refused_until_guard_drops() {
        let path = temp_lock_path("exclusive");
        let first = match acquire_at(&path).unwrap() {
            AcquireResult::Acquired(guard) => guard,
            AcquireResult::AlreadyRunning => panic!("fresh lock unexpectedly busy"),
        };
        assert!(matches!(
            acquire_at(&path).unwrap(),
            AcquireResult::AlreadyRunning
        ));
        drop(first);
        assert!(matches!(
            acquire_at(&path).unwrap(),
            AcquireResult::Acquired(_)
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn final_component_symlink_is_never_followed() {
        use std::os::unix::fs::symlink;

        let path = temp_lock_path("symlink");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = path.parent().unwrap().join("target");
        std::fs::write(&target, b"do not touch").unwrap();
        symlink(&target, &path).unwrap();
        assert!(acquire_at(&path).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"do not touch");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn hardlinked_lock_is_rejected_without_chmodding_the_other_name() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_lock_path("hardlink");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = path.parent().unwrap().join("unrelated");
        std::fs::write(&target, b"do not chmod").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::hard_link(&target, &path).unwrap();

        assert!(acquire_at(&path).is_err());
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640,
            "rejecting a planted hard link must not change the unrelated inode's mode"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"do not chmod");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
