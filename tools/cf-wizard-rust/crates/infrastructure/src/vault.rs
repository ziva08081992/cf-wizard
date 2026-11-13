//! Windows Credential Manager, compatible with 0.7.3 credential names.
use crate::{canonical::key, git::canonical_server};
use cf_domain::{Error, Provider, Result};
use zeroize::Zeroizing;
pub fn target(provider: Provider, server: &str) -> Result<String> {
    let server = canonical_server(server, provider)?;
    Ok(format!(
        "CF-Wizard/{}/{}",
        if provider == Provider::Github {
            "GitHub"
        } else {
            "Gitea"
        },
        key(&[&server])
    ))
}
#[cfg(windows)]
mod win {
    use super::*;
    use std::{ffi::c_void, ptr};
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target: *mut u16,
        comment: *mut u16,
        time: FileTime,
        size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        alias: *mut u16,
        user: *mut u16,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredReadW(target: *const u16, kind: u32, flags: u32, out: *mut *mut Credential) -> i32;
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    pub fn load(name: &str) -> Result<Option<Zeroizing<String>>> {
        let name = wide(name);
        let mut value = ptr::null_mut();
        unsafe {
            if CredReadW(name.as_ptr(), 1, 0, &mut value) == 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(1168) {
                    return Ok(None);
                }
                return Err(Error::External(
                    "Не удалось прочитать Диспетчер учётных данных Windows".into(),
                ));
            }
            if value.is_null() {
                return Err(Error::External("Пустой ответ хранилища".into()));
            }
            let credential = &*value;
            let result = if credential.size % 2 != 0
                || credential.size > 65536
                || credential.blob.is_null()
            {
                Err(Error::Invalid("Повреждён сохранённый токен".into()))
            } else {
                let mut units = Zeroizing::new(Vec::with_capacity(credential.size as usize / 2));
                for i in 0..credential.size as usize / 2 {
                    units.push(ptr::read_unaligned(
                        credential.blob.add(i * 2).cast::<u16>(),
                    ));
                }
                String::from_utf16(&units)
                    .map(Zeroizing::new)
                    .map(Some)
                    .map_err(|_| Error::Invalid("Повреждён сохранённый токен".into()))
            };
            if !credential.blob.is_null() {
                ptr::write_bytes(credential.blob, 0, credential.size as usize);
            }
            CredFree(value.cast());
            result
        }
    }
    pub fn save(name: &str, secret: &str) -> Result<()> {
        let mut target = wide(name);
        let mut user = wide("CF Wizard Git");
        let mut units = Zeroizing::new(secret.encode_utf16().collect::<Vec<_>>());
        if units.is_empty() || units.len() * 2 > 2560 {
            return Err(Error::Invalid("Пустой или слишком длинный токен".into()));
        }
        let credential = Credential {
            flags: 0,
            kind: 1,
            target: target.as_mut_ptr(),
            comment: ptr::null_mut(),
            time: FileTime { low: 0, high: 0 },
            size: (units.len() * 2) as u32,
            blob: units.as_mut_ptr().cast(),
            persist: 2,
            attribute_count: 0,
            attributes: ptr::null_mut(),
            alias: ptr::null_mut(),
            user: user.as_mut_ptr(),
        };
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(Error::External(
                "Не удалось сохранить токен в Windows".into(),
            ));
        }
        Ok(())
    }
    pub fn delete(name: &str) -> Result<()> {
        if unsafe { CredDeleteW(wide(name).as_ptr(), 1, 0) } == 0
            && std::io::Error::last_os_error().raw_os_error() != Some(1168)
        {
            return Err(Error::External(
                "Не удалось удалить сохранённый токен".into(),
            ));
        }
        Ok(())
    }
}
#[cfg(windows)]
pub use win::{delete, load, save};
#[cfg(not(windows))]
pub fn load(_: &str) -> Result<Option<Zeroizing<String>>> {
    Ok(None)
}
#[cfg(not(windows))]
pub fn save(_: &str, _: &str) -> Result<()> {
    Err(Error::Invalid(
        "Сохранение токенов в этой версии доступно в Windows".into(),
    ))
}
#[cfg(not(windows))]
pub fn delete(_: &str) -> Result<()> {
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn windows_vault_roundtrip_uses_own_temporary_credential() {
        let name = format!("CF-Wizard/Rust-QA/{}", uuid::Uuid::new_v4());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = delete(&self.0);
            }
        }
        let _cleanup = Cleanup(name.clone());
        save(&name, "dummy-token-тест-😀").unwrap();
        let value = load(&name).unwrap().unwrap();
        assert_eq!(value.as_str(), "dummy-token-тест-😀");
        delete(&name).unwrap();
        assert!(load(&name).unwrap().is_none());
    }
    #[test]
    fn legacy_github_name() {
        assert_eq!(
            target(Provider::Github, "https://api.github.com/").unwrap(),
            "CF-Wizard/GitHub/996e1f714b08e971ec79e3be"
        );
    }
}
