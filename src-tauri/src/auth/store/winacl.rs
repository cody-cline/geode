//! What `0o600` means on Windows, for the files in Geode only this account may
//! read: `accounts.json`, `config.json`, `hosts.json`, a server's share
//! password, and the sharing private key.
//!
//! Windows has no mode. What it has is a DACL — a list of entries saying who
//! may do what — and a new file's DACL is *inherited* from the folder it is
//! created in unless the list is marked protected. So the nearest honest
//! equivalent of 0600 is a protected DACL holding exactly three entries, all
//! of them full access:
//!
//! - the user this process is running as,
//! - `SY`, Local System,
//! - `BA`, the local Administrators group.
//!
//! The last two are not a weakening and there is no point excluding them: a
//! local administrator may take ownership of any file on the machine and
//! rewrite its DACL afterwards, so an entry refusing them stops nobody. What
//! is excluded is everything else — `Users`, `Authenticated Users`, every
//! other account on the machine, and the AppContainer capability SIDs that a
//! file under a packaged app's `%APPDATA%` inherits.
//!
//! Marking the list protected is half the work and the half that is easy to
//! leave out: without it, ours are three more entries beside whatever the
//! folder already granted.
//!
//! # What this does not promise
//!
//! A DACL needs a filesystem that has one. On a volume without them — a FAT or
//! exFAT stick named by `GEODE_HOME` — [`protect`] fails and the write fails
//! with it, which is the same thing a `chmod` on a `vfat` mount does to the
//! Unix path in [`super::write_private`]. A secret is not written in the clear
//! because its protection could not be applied.
//!
//! # None of this has run
//!
//! It compiles for `x86_64-pc-windows-msvc` and is written to the documented
//! contract of the calls it makes. Nobody has watched it protect a file:
//! Geode is developed on Linux, Windows has been run once by hand, and that
//! run predates this module. Treat the behaviour as unverified and the Unix
//! path beside it as the only one with a test that has ever executed.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_SUCCESS, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Create `path`, which must not already exist, under the owner-only list.
///
/// The descriptor is handed to `CreateFileW` as the file comes into being,
/// never applied afterwards. Windows decides access when a handle is opened
/// rather than when it is read, so a file that exists for even a moment at the
/// folder's own DACL can be opened by whoever that DACL admits, and the handle
/// they took goes on working through every later repair.
///
/// Fails with [`io::ErrorKind::AlreadyExists`] when the file is there, like
/// the `create_new(true)` it stands in for.
pub(crate) fn create_new(path: &Path) -> io::Result<File> {
    let descriptor = Descriptor::owner_only()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(path.as_os_str());
    // The sharing mode is the one `std::fs` uses, so this differs from an
    // ordinary create in its access list and in nothing else. Sharing is not
    // access control: whoever opens this file still has to be on the list.
    //
    // SAFETY: `name` is NUL-terminated and outlives the call, `attributes`
    // points at a descriptor `descriptor` keeps alive until after it, and the
    // result is either `INVALID_HANDLE_VALUE` or a handle nothing else owns.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle is open, ours alone, and not used again here.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Leave `path` existing and owner-only, whether this is the first save or the
/// hundredth.
///
/// A file that is not there yet is created under the list, so the caller's
/// bytes go into something nothing else can open. One that is already there —
/// written by a Geode older than this, or by this one before a crash — has its
/// list replaced *before* the caller writes, for the same reason. The two
/// cannot be one call: a descriptor passed to `CreateFileW` is applied only
/// when it creates the file, and ignored when it opens one.
pub(crate) async fn ensure(path: &Path) -> io::Result<()> {
    off_thread(path, |path| match create_new(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => set_dacl(path),
        Err(e) => Err(e),
    })
    .await
}

/// Replace the list on a file that already exists, for a caller that could not
/// have created it under one.
pub(crate) async fn protect(path: &Path) -> io::Result<()> {
    off_thread(path, set_dacl).await
}

/// Both of the above open a file or rewrite an access list, which is file I/O:
/// `tokio::fs` hands every one of its own calls to a blocking thread for that
/// reason, and a Geode whose `GEODE_HOME` is a network home directory is
/// exactly where it tells.
async fn off_thread(path: &Path, work: fn(&Path) -> io::Result<()>) -> io::Result<()> {
    let owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || work(&owned))
        .await
        .map_err(io::Error::other)?
}

fn set_dacl(path: &Path) -> io::Result<()> {
    let descriptor = Descriptor::owner_only()?;
    let dacl = descriptor.dacl()?;
    let name = wide(path.as_os_str());
    // `PROTECTED_DACL_SECURITY_INFORMATION` is the half that switches
    // inheritance off. `DACL_SECURITY_INFORMATION` on its own would set these
    // three entries and leave the folder's beside them.
    //
    // SAFETY: `name` is NUL-terminated and outlives the call, and `dacl`
    // points inside a descriptor alive for the same span. The owner, group and
    // SACL pointers are null because none of them is being set.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// A security descriptor from `LocalAlloc`ed memory, freed when it goes out of
/// scope.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    /// The three entries, built by Windows from SDDL rather than by hand from
    /// `InitializeAcl` and three `AddAccessAllowedAce` calls: it is the same
    /// list either way, and this way it is one line somebody can read.
    ///
    /// `D:P` is the DACL, protected. `(A;;FA;;;X)` allows X everything, with
    /// no inheritance flags — these are files, and nothing inherits from them.
    fn owner_only() -> io::Result<Self> {
        let sddl = format!(
            "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{})",
            current_user_sid()?
        );
        let wide = wide(OsStr::new(&sddl));
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `wide` is NUL-terminated and outlives the call; on success
        // `sd` is a descriptor this type frees, and on failure it is untouched.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sd))
    }

    /// The DACL inside it, which is the half `SetNamedSecurityInfoW` takes.
    fn dacl(&self) -> io::Result<*mut ACL> {
        let mut present = 0;
        let mut dacl: *mut ACL = null_mut();
        let mut defaulted = 0;
        // SAFETY: `self.0` is a descriptor built above and still alive.
        let ok =
            unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            // A descriptor with no DACL is not one with an empty DACL: no list
            // at all means everyone is allowed everything, so this must never
            // be passed on as if it were the list that was asked for.
            return Err(io::Error::other(
                "the owner-only access list came back from Windows without a list in it",
            ));
        }
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: `LocalAlloc`ed by the call that produced it, and not freed
        // anywhere else.
        unsafe { LocalFree(self.0) };
    }
}

/// The SID of the user this process is running as, as text for the SDDL above.
fn current_user_sid() -> io::Result<String> {
    let mut raw: HANDLE = null_mut();
    // SAFETY: the pseudo-handle from `GetCurrentProcess` needs no closing, and
    // `raw` is written only on success.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Token(raw);

    // A `TOKEN_USER` is a header pointing at a SID that follows it, so the
    // size is asked for rather than guessed. The buffer is `u64` to be aligned
    // for the pointer inside that header; a `Vec<u8>` would not be.
    let mut needed = 0u32;
    // SAFETY: a null buffer with length zero is how this call is asked how
    // much it needs; it fails with ERROR_INSUFFICIENT_BUFFER and writes
    // `needed`.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    // SAFETY: the buffer is at least `needed` bytes and aligned for the struct
    // written into it.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut text: PWSTR = null_mut();
    // SAFETY: the buffer holds the `TOKEN_USER` the call above wrote, whose
    // `Sid` points inside that same buffer, which outlives this call.
    let ok = unsafe {
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        ConvertSidToStringSidW(user.User.Sid, &mut text)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalText(text).read())
}

/// An access token handle, closed when it goes out of scope.
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: opened above and not closed anywhere else.
        unsafe { CloseHandle(self.0) };
    }
}

/// A `LocalAlloc`ed wide string from Windows, freed when it goes out of scope.
struct LocalText(PWSTR);

impl LocalText {
    fn read(&self) -> String {
        let mut len = 0;
        // SAFETY: Windows wrote a NUL-terminated string here.
        while unsafe { *self.0.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` units precede the NUL just found.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(self.0, len) })
    }
}

impl Drop for LocalText {
    fn drop(&mut self) {
        // SAFETY: `LocalAlloc`ed by the call that produced it, and not freed
        // anywhere else.
        unsafe { LocalFree(self.0.cast()) };
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// Fail unless `path` is readable by this account, Local System and the
/// Administrators group, and by nothing else.
///
/// Shared with the modules that write those files so each one can assert about
/// its own file, the way each asserts its own mode on Unix. The list is read
/// back as SDDL — Windows' own text for a descriptor — because that is one
/// string to assert against instead of a walk over `ACE_HEADER`s, and because
/// it names the well-known accounts the same way the request did.
///
/// Never run. Like everything else here it exists for whoever first runs
/// `cargo test` on Windows.
#[cfg(test)]
pub(crate) fn assert_owner_only(path: &Path) {
    let sddl = dacl_sddl(path).expect("the file's access list reads back");

    // `D:P`, possibly with flags after the P. Without the P this file is
    // whatever its folder grants, plus three entries.
    assert!(
        sddl.starts_with("D:P"),
        "inheritance is still switched on for this file: {sddl}"
    );
    assert_eq!(
        sddl.matches('(').count(),
        3,
        "there should be three entries and nothing else: {sddl}"
    );
    assert_eq!(
        sddl.matches("(A;;FA;;;").count(),
        3,
        "all three entries should allow full access with no inheritance flags: {sddl}"
    );
    // Everyone, Authenticated Users, the local Users group, Interactive,
    // Anonymous, Restricted — and `S-1-15-` for the AppContainer and
    // capability SIDs a packaged app's files inherit, which have no
    // abbreviation.
    for stranger in [";WD)", ";AU)", ";BU)", ";IU)", ";AN)", ";RC)", "S-1-15-"] {
        assert!(
            !sddl.contains(stranger),
            "{stranger} is on the list for this file: {sddl}"
        );
    }
    // The other half of owner-only, and the half an SDDL string cannot show:
    // this account must still be able to read its own file.
    std::fs::read(path).expect("this account can read the file it just wrote");
}

/// `path`'s DACL as SDDL.
#[cfg(test)]
fn dacl_sddl(path: &Path) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };

    let name = wide(path.as_os_str());
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `name` is NUL-terminated and outlives the call. The four null
    // pointers are the parts of the descriptor not being asked for.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Descriptor(sd);

    let mut text: PWSTR = null_mut();
    let mut len = 0u32;
    // SAFETY: `descriptor` is alive for the call, and `text` is written only
    // on success.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.0,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalText(text).read())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A file created here is owner-only from the instant it exists, which is
    /// the whole reason `create_new` is not an open followed by a repair.
    #[test]
    fn a_new_file_is_owner_only_as_it_is_created() {
        let scratch = crate::paths::Scratch::new("winacl-new");
        std::fs::create_dir_all(&scratch.paths.root).unwrap();
        let path = scratch.paths.root.join("secret.json");

        let mut file = create_new(&path).expect("the file is created");
        file.write_all(b"a-real-looking-token").unwrap();
        drop(file);

        assert_owner_only(&path);
        assert_eq!(
            create_new(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists,
            "it must refuse a file that is already there, like create_new(true)"
        );
    }

    /// The other case: a file written by a Geode from before any of this, left
    /// at whatever its folder granted. The next save repairs it.
    #[tokio::test]
    async fn a_file_already_there_has_its_list_replaced() {
        let scratch = crate::paths::Scratch::new("winacl-repair");
        std::fs::create_dir_all(&scratch.paths.root).unwrap();
        let path = scratch.paths.root.join("inherited.json");
        std::fs::write(&path, b"written the old way").unwrap();

        ensure(&path).await.expect("an existing file is repaired");

        assert_owner_only(&path);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"written the old way",
            "repairing the list must not touch the contents"
        );
    }
}
