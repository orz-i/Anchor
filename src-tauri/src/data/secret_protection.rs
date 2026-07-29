#[cfg(windows)]
pub(super) fn protect(data: &[u8]) -> Result<(&'static str, Vec<u8>), String> {
    use std::ffi::c_void;

    use windows::core::w;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input_len = u32::try_from(data.len()).map_err(|_| "secret payload is too large")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_len,
        pbData: data.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            w!("Anchor secrets"),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|error| format!("Windows DPAPI encryption failed: {error}"))?;
    }
    let protected = unsafe {
        std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast::<c_void>())));
    }
    Ok(("windows-dpapi-current-user-v1", protected))
}

#[cfg(windows)]
pub(super) fn unprotect(protection: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    if protection != "windows-dpapi-current-user-v1" {
        return Err(format!("unsupported Windows secret protection: {protection}"));
    }
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
    let plaintext = unsafe {
        std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec()
    };
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
}
