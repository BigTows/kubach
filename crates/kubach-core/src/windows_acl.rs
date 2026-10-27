//! Windows counterpart of `chmod 0600`: replaces a file's DACL with a single
//! entry granting full access to the current user and stops inheriting the
//! parent folder's entries, so other accounts (and admins without taking
//! ownership) can't read it.

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GENERIC_ALL, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
    TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
    PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Closes the token handle when dropped.
struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcessToken` and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Frees the ACL allocated by `SetEntriesInAclW` when dropped.
struct LocalAcl(*mut ACL);

impl Drop for LocalAcl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the ACL was allocated with `LocalAlloc` by the system.
            unsafe { LocalFree(self.0.cast()) };
        }
    }
}

fn win32(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// `TOKEN_USER` of the current process, in a buffer aligned for it.
fn current_user() -> io::Result<Vec<u64>> {
    let mut handle: HANDLE = ptr::null_mut();
    // SAFETY: `handle` is a valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Token(handle);
    let mut len = 0u32;
    // SAFETY: a size query with a null buffer; fails with the needed size.
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: the buffer holds at least `len` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

/// Leaves only the current user in the DACL of `path`.
pub(crate) fn restrict_to_current_user(path: &Path) -> io::Result<()> {
    let user = current_user()?;
    // SAFETY: `current_user` filled the buffer with a `TOKEN_USER`, and the
    // buffer (holding the SID it points to) outlives every use below.
    let sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
            ..TRUSTEE_W::default()
        },
    };
    let mut acl = LocalAcl(ptr::null_mut());
    // SAFETY: one valid entry, no old ACL, `acl.0` is a valid out pointer.
    win32(unsafe { SetEntriesInAclW(1, &access, ptr::null(), &mut acl.0) })?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and `acl.0` a valid ACL.
    win32(unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl.0,
            ptr::null(),
        )
    })
}
