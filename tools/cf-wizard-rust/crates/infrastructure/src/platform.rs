//! Validate an explicitly installed, pinned platform. No DLL search overrides.
use crate::source::{no_links, normal_path};
use cf_domain::{BuildMode, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Platform {
    pub path: PathBuf,
    pub version: String,
    pub backend: String,
}
pub fn validate(path: &Path, mode: BuildMode) -> Result<Platform> {
    no_links(path)?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let backend = match mode {
        BuildMode::Auto | BuildMode::Designer => "designer",
        BuildMode::Ibcmd => "ibcmd",
    };
    if name
        != if backend == "designer" {
            "1cv8.exe"
        } else {
            "ibcmd.exe"
        }
    {
        return Err(Error::Invalid(format!(
            "Выберите {} из bin установленной платформы",
            if backend == "designer" {
                "1cv8.exe"
            } else {
                "ibcmd.exe"
            }
        )));
    }
    let mut file = fs::File::open(path).map_err(crate::external)?;
    let mut data = [0u8; 64];
    file.read_exact(&mut data).map_err(crate::external)?;
    let offset = u32::from_le_bytes(data[0x3c..0x40].try_into().unwrap()) as u64;
    let mut pe = [0u8; 6];
    if offset > 16 * 1024 * 1024 {
        return Err(Error::Invalid("Неверный PE заголовок".into()));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(crate::external)?;
    file.read_exact(&mut pe).map_err(crate::external)?;
    if data.get(..2) != Some(b"MZ") || pe != [b'P', b'E', 0, 0, 0x64, 0x86] {
        return Err(Error::Invalid(
            "Нужна 64-разрядная платформа Windows".into(),
        ));
    }
    let version = version(path)?;
    Ok(Platform {
        path: path.to_path_buf(),
        version,
        backend: backend.into(),
    })
}
pub fn discover(mode: BuildMode) -> Vec<Platform> {
    let mut rows = vec![];
    for variable in ["ProgramW6432", "ProgramFiles"] {
        let Some(base) = std::env::var_os(variable) else {
            continue;
        };
        let Ok(items) = fs::read_dir(Path::new(&base).join("1cv8")) else {
            continue;
        };
        for item in items.flatten() {
            let file = item.path().join("bin").join(if mode == BuildMode::Ibcmd {
                "ibcmd.exe"
            } else {
                "1cv8.exe"
            });
            if let Ok(value) = validate(&file, mode)
                && !rows
                    .iter()
                    .any(|p: &Platform| normal_path(&p.path) == normal_path(&value.path))
            {
                rows.push(value);
            }
        }
    }
    rows.sort_by_key(|p| {
        std::cmp::Reverse(
            p.version
                .split('.')
                .map(|v| v.parse::<u32>().unwrap_or(0))
                .collect::<Vec<_>>(),
        )
    });
    rows
}
#[cfg(windows)]
pub fn version(path: &Path) -> Result<String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    #[link(name = "version")]
    unsafe extern "system" {
        fn GetFileVersionInfoSizeW(file: *const u16, handle: *mut u32) -> u32;
        fn GetFileVersionInfoW(file: *const u16, handle: u32, size: u32, data: *mut c_void) -> i32;
        fn VerQueryValueW(
            data: *const c_void,
            block: *const u16,
            value: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
    }
    let name: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut handle = 0;
    unsafe {
        let size = GetFileVersionInfoSizeW(name.as_ptr(), &mut handle);
        if size == 0 || size > 16 * 1024 * 1024 {
            return Err(Error::Invalid(
                "Не удалось определить версию платформы".into(),
            ));
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(name.as_ptr(), 0, size, data.as_mut_ptr().cast()) == 0 {
            return Err(crate::external(std::io::Error::last_os_error()));
        }
        let mut value = std::ptr::null_mut();
        let mut len = 0;
        if VerQueryValueW(
            data.as_ptr().cast(),
            [92u16, 0].as_ptr(),
            &mut value,
            &mut len,
        ) == 0
            || len < 52
            || value.is_null()
        {
            return Err(Error::Invalid("Нет версии платформы".into()));
        }
        // VS_FIXEDFILEINFO may be unaligned in a byte buffer.
        let word =
            |offset| std::ptr::read_unaligned((value as *const u8).add(offset).cast::<u32>());
        if word(0) != 0xfeef04bd {
            return Err(Error::Invalid("Неверный блок версии".into()));
        }
        let (a, b) = (word(8), word(12));
        Ok(format!(
            "{}.{}.{}.{}",
            a >> 16,
            a & 65535,
            b >> 16,
            b & 65535
        ))
    }
}
#[cfg(not(windows))]
pub fn version(_: &Path) -> Result<String> {
    Err(Error::Invalid(
        "Сборка CF в этой версии доступна в Windows".into(),
    ))
}
