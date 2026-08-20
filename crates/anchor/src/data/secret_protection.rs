#[cfg(windows)]
pub(super) fn protect(data: &[u8]) -> Result<(&'static str, Vec<u8>), String> {
    protect_windows(data, false)
}

#[cfg(windows)]
pub(super) fn protect_for_service(data: &[u8]) -> Result<(&'static str, Vec<u8>), String> {
    protect_windows(data, true)
}

#[cfg(windows)]
fn protect_windows(data: &[u8], local_machine: bool) -> Result<(&'static str, Vec<u8>), String> {
    use std::ffi::c_void;

    use windows::core::w;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input_len = u32::try_from(data.len()).map_err(|_| "secret payload is too large")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let flags = if local_machine {
        CRYPTPROTECT_UI_FORBIDDEN | CRYPTPROTECT_LOCAL_MACHINE
    } else {
        CRYPTPROTECT_UI_FORBIDDEN
    };
    unsafe {
        CryptProtectData(
            &input,
            w!("Anchor secrets"),
            None,
            None,
            None,
            flags,
            &mut output,
        )
        .map_err(|error| format!("Windows DPAPI encryption failed: {error}"))?;
    }
    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast::<c_void>())));
    }
    Ok((
        if local_machine {
            "windows-dpapi-local-machine-v1"
        } else {
            "windows-dpapi-current-user-v1"
        },
        protected,
    ))
}

#[cfg(windows)]
pub(super) fn unprotect(protection: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    if protection != "windows-dpapi-current-user-v1" {
        return Err(format!(
            "unsupported Windows secret protection: {protection}"
        ));
    }
    unprotect_windows(data)
}

#[cfg(windows)]
pub(super) fn unprotect_for_service(protection: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    if protection != "windows-dpapi-local-machine-v1" {
        return Err(format!(
            "unsupported Windows service secret protection: {protection}"
        ));
    }
    unprotect_windows(data)
}

#[cfg(windows)]
fn unprotect_windows(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input_len = u32::try_from(data.len()).map_err(|_| "secret payload is too large")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| format!("Windows DPAPI decryption failed: {error}"))?;
    }
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast::<c_void>())));
    }
    Ok(plaintext)
}

#[cfg(not(windows))]
pub(super) fn protect(data: &[u8]) -> Result<(&'static str, Vec<u8>), String> {
    Ok(("private-file-permissions-v1", data.to_vec()))
}

#[cfg(not(windows))]
pub(super) fn unprotect(protection: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    if protection != "private-file-permissions-v1" {
        return Err(format!("unsupported secret protection: {protection}"));
    }
    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    #[test]
    fn protected_payload_round_trips() {
        let secret = br#"{"token":"do-not-log"}"#;
        let (protection, protected) = super::protect(secret).expect("protect");
        let plaintext = super::unprotect(protection, &protected).expect("unprotect");
        assert_eq!(plaintext, secret);
        #[cfg(windows)]
        assert_ne!(protected, secret);
    }

    #[cfg(windows)]
    #[test]
    fn service_protected_payload_round_trips() {
        let secret = br#"{\"token\":\"service-secret\"}"#;
        let (protection, protected) =
            super::protect_for_service(secret).expect("protect for service");
        let plaintext =
            super::unprotect_for_service(protection, &protected).expect("service unprotect");
        assert_eq!(plaintext, secret);
        assert_eq!(protection, "windows-dpapi-local-machine-v1");
        assert_ne!(protected, secret);
    }
}
