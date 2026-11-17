//! Keys and tokens are stored encrypted with Windows DPAPI (tied to this Windows account).

use base64::Engine;

const PREFIX: &str = "dpapi1:";

#[cfg(windows)]
fn crypt(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = if protect {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out)
        };
        if ok == 0 || out.pbData.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Some(v)
    }
}

#[cfg(not(windows))]
fn crypt(data: &[u8], _protect: bool) -> Option<Vec<u8>> {
    Some(data.to_vec())
}

/// Encrypt for storage. Falls back to plain text only if the OS refuses.
pub fn seal(plain: &str) -> String {
    match crypt(plain.as_bytes(), true) {
        Some(blob) => format!("{PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(blob)),
        None => plain.to_string(),
    }
}

/// Decrypt a stored value; old plain-text values pass through unchanged.
pub fn open(stored: &str) -> Option<String> {
    let Some(b64) = stored.strip_prefix(PREFIX) else { return Some(stored.to_string()) };
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    String::from_utf8(crypt(&blob, false)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_legacy() {
        let sealed = seal("{\"google_api_key\":\"abc\"}");
        assert!(sealed.starts_with(PREFIX) || !cfg!(windows));
        assert_eq!(open(&sealed).as_deref(), Some("{\"google_api_key\":\"abc\"}"));
        assert_eq!(open("plain old value").as_deref(), Some("plain old value"));
    }
}
