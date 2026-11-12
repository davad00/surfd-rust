#[cfg(windows)]
pub(super) use windows::{clear, read, write};

#[cfg(not(windows))]
pub(super) fn read(_origin: &str) -> std::io::Result<Option<String>> {
    Ok(None)
}

#[cfg(not(windows))]
pub(super) fn write(_origin: &str, _token: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn clear(_origin: &str, _token: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
mod windows {
    use std::{ffi::c_void, io, ptr, slice, str};

    use sha2::{Digest, Sha256};

    const GENERIC: u32 = 1;
    const PERSIST_LOCAL_MACHINE: u32 = 2;
    const NOT_FOUND: i32 = 1168;
    const MAX_BEARER_SIZE: usize = 256;

    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target: *mut u16,
        comment: *mut u16,
        last_written: [u32; 2],
        blob_size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredReadW(target: *const u16, kind: u32, flags: u32, out: *mut *mut Credential) -> i32;
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }

    struct CredentialBuffer(*mut Credential);

    impl Drop for CredentialBuffer {
        fn drop(&mut self) {
            // CredReadW gives the caller ownership of this buffer.
            unsafe { CredFree(self.0.cast()) };
        }
    }

    fn target(origin: &str) -> Vec<u16> {
        let digest = Sha256::digest(origin.as_bytes());
        format!("Surfd Rust/records/{digest:x}")
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }

    fn valid_token(token: &str) -> bool {
        !token.is_empty()
            && token.len() <= MAX_BEARER_SIZE
            && token.bytes().all(|byte| byte.is_ascii_graphic())
    }

    pub(in crate::online) fn read(origin: &str) -> io::Result<Option<String>> {
        let target = target(origin);
        let mut credential = ptr::null_mut();
        // The target is NUL-terminated and the out pointer lives through the call.
        if unsafe { CredReadW(target.as_ptr(), GENERIC, 0, &mut credential) } == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(NOT_FOUND) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if credential.is_null() {
            return Ok(None);
        }
        let buffer = CredentialBuffer(credential);
        // A successful read returns an initialized credential and its blob in the same allocation.
        let credential = unsafe { &*buffer.0 };
        let len = credential.blob_size as usize;
        if credential.blob.is_null() || len == 0 || len > MAX_BEARER_SIZE {
            return Ok(None);
        }
        let bytes = unsafe { slice::from_raw_parts(credential.blob, len) };
        Ok(str::from_utf8(bytes)
            .ok()
            .filter(|token| valid_token(token))
            .map(str::to_owned))
    }

    pub(in crate::online) fn write(origin: &str, token: &str) -> io::Result<()> {
        if !valid_token(token) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid session token",
            ));
        }
        let mut target = target(origin);
        let mut blob = token.as_bytes().to_vec();
        let credential = Credential {
            flags: 0,
            kind: GENERIC,
            target: target.as_mut_ptr(),
            comment: ptr::null_mut(),
            last_written: [0; 2],
            blob_size: blob.len() as u32,
            blob: blob.as_mut_ptr(),
            persist: PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: ptr::null_mut(),
            target_alias: ptr::null_mut(),
            user_name: ptr::null_mut(),
        };
        // Both buffers remain valid while Windows copies the credential.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(in crate::online) fn clear(origin: &str, token: &str) -> io::Result<()> {
        if read(origin)?.as_deref() != Some(token) {
            return Ok(());
        }
        let target = target(origin);
        if unsafe { CredDeleteW(target.as_ptr(), GENERIC, 0) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(NOT_FOUND) {
                return Err(error);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    #[test]
    fn stores_one_origin_and_clears_only_the_matching_token() {
        let origin = format!(
            "http://saved-session-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        write(&origin, "synthetic-token").unwrap();
        assert_eq!(read(&origin).unwrap().as_deref(), Some("synthetic-token"));
        assert_eq!(read(&format!("{origin}-other")).unwrap(), None);
        clear(&origin, "older-token").unwrap();
        assert_eq!(read(&origin).unwrap().as_deref(), Some("synthetic-token"));
        clear(&origin, "synthetic-token").unwrap();
        assert_eq!(read(&origin).unwrap(), None);
    }
}
