use std::env;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use ntapi::ntioapi::{NtLoadDriver, NtUnloadDriver};
use ntapi::ntrtl::{RtlInitUnicodeString, RtlNtStatusToDosError};
use winapi::shared::ntdef::UNICODE_STRING;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES,
    TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};

const SERVICES_KEY: &str = r"System\CurrentControlSet\Services";
const SERVICE_KERNEL_DRIVER: u32 = 1;
const ERROR_NOT_ALL_ASSIGNED: i32 = 1300;

#[allow(dead_code)]
pub fn current_dir() -> Option<String> {
    env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
}

pub fn default_driver_path() -> String {
    default_current_dir_path("noumen_loader.sys")
}

pub fn default_image_path() -> String {
    default_current_dir_path("noumen")
}

pub fn default_log_path() -> String {
    default_current_dir_path("noumen.log")
}

fn default_current_dir_path(name: &str) -> String {
    match env::current_dir() {
        Ok(dir) => dir.join(name).to_string_lossy().into_owned(),
        Err(_) => String::from(name),
    }
}

pub fn service_name_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("noumen_loader"))
}

pub fn pick_driver_file() -> Option<PathBuf> {
    let filter: Vec<u16> = "驱动文件 (*.sys)\0*.sys\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title = wide("选择驱动文件");

    let mut buffer = [0u16; 1024];

    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    dialog.lpstrFilter = filter.as_ptr();
    dialog.lpstrFile = buffer.as_mut_ptr();
    dialog.nMaxFile = buffer.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | OFN_HIDEREADONLY;

    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        return None;
    }

    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn normalize_image_path(drv_path: &str) -> Result<String, String> {
    let path = Path::new(drv_path);
    let absolute: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|e| format!("获取当前目录失败: {e}"))?
            .join(path)
    };

    let canonical = std::fs::canonicalize(&absolute)
        .map_err(|e| format!("驱动文件不可访问 {}: {e}", absolute.display()))?;

    let text = canonical.to_string_lossy();
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!(r"\\{rest}"))
        .or_else(|| text.strip_prefix(r"\\?\").map(str::to_owned))
        .unwrap_or_else(|| text.into_owned());

    Ok(format!(r"\??\{text}"))
}

fn enable_load_driver_privilege() -> Result<(), String> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Err(format!(
                "OpenProcessToken 失败: {}",
                io::Error::last_os_error()
            ));
        }

        let name = wide("SeLoadDriverPrivilege");
        let mut luid = std::mem::zeroed();
        if LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &mut luid) == 0 {
            let err = io::Error::last_os_error();
            CloseHandle(token);
            return Err(format!("LookupPrivilegeValueW 失败: {err}"));
        }

        let mut privileges: TOKEN_PRIVILEGES = std::mem::zeroed();
        privileges.PrivilegeCount = 1;
        privileges.Privileges[0].Luid = luid;
        privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;

        let ok = AdjustTokenPrivileges(token, 0, &privileges, 0, ptr::null_mut(), ptr::null_mut());
        let err = io::Error::last_os_error();
        CloseHandle(token);

        if ok == 0 {
            return Err(format!("AdjustTokenPrivileges 失败: {err}"));
        }
        if err.raw_os_error() == Some(ERROR_NOT_ALL_ASSIGNED) {
            return Err(String::from(
                "当前进程没有 SeLoadDriverPrivilege，请以管理员身份运行",
            ));
        }
        Ok(())
    }
}

fn write_service_key(drv_name: &str, image_path: &str) -> Result<(), String> {
    let subkey = wide(&format!(r"{SERVICES_KEY}\{drv_name}"));

    unsafe {
        let mut key: HKEY = ptr::null_mut();
        let mut disposition = 0u32;
        let rc = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            ptr::null(),
            &mut key,
            &mut disposition,
        );
        if rc != 0 {
            return Err(format!(
                "创建注册表键 HKLM\\{SERVICES_KEY}\\{drv_name} 失败: {}",
                io::Error::from_raw_os_error(rc as i32)
            ));
        }

        let value_name = wide("ImagePath");
        let image = wide(image_path);
        let rc = RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_SZ,
            image.as_ptr().cast(),
            (image.len() * std::mem::size_of::<u16>()) as u32,
        );
        if rc != 0 {
            RegCloseKey(key);
            return Err(format!(
                "写入 ImagePath 失败: {}",
                io::Error::from_raw_os_error(rc as i32)
            ));
        }

        let value_name = wide("Type");
        let driver_type = SERVICE_KERNEL_DRIVER;
        let rc = RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_DWORD,
            (&driver_type as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        RegCloseKey(key);
        if rc != 0 {
            return Err(format!(
                "写入 Type 失败: {}",
                io::Error::from_raw_os_error(rc as i32)
            ));
        }

        Ok(())
    }
}

fn remove_service_key(drv_name: &str) {
    let subkey = wide(&format!(r"{SERVICES_KEY}\{drv_name}"));
    unsafe {
        RegDeleteTreeW(HKEY_LOCAL_MACHINE, subkey.as_ptr());
    }
}

fn nt_error(operation: &str, status: i32) -> String {
    let win32 = unsafe { RtlNtStatusToDosError(status) };
    let message = io::Error::from_raw_os_error(win32 as i32);
    format!("{operation} 失败: NTSTATUS 0x{status:08X} ({message})")
}

pub fn nt_load_driver(drv_name: &str, drv_path: &str) -> Result<(), String> {
    if drv_name.is_empty() {
        return Err(String::from("服务名不能为空"));
    }

    let image_path = normalize_image_path(drv_path)?;
    enable_load_driver_privilege()?;
    write_service_key(drv_name, &image_path)?;

    let registry_path = wide(&format!(r"\Registry\Machine\{SERVICES_KEY}\{drv_name}"));
    let mut service_name: UNICODE_STRING = unsafe { std::mem::zeroed() };
    unsafe {
        RtlInitUnicodeString(&mut service_name, registry_path.as_ptr());
    }

    let status = unsafe { NtLoadDriver(&mut service_name) };
    if status != 0 {
        remove_service_key(drv_name);
        return Err(nt_error("NtLoadDriver", status));
    }

    Ok(())
}

pub fn nt_unload_driver(drv_name: &str) -> Result<(), String> {
    if drv_name.is_empty() {
        return Err(String::from("服务名不能为空"));
    }

    enable_load_driver_privilege()?;

    let registry_path = wide(&format!(r"\Registry\Machine\{SERVICES_KEY}\{drv_name}"));
    let mut service_name: UNICODE_STRING = unsafe { std::mem::zeroed() };
    unsafe {
        RtlInitUnicodeString(&mut service_name, registry_path.as_ptr());
    }

    let status = unsafe { NtUnloadDriver(&mut service_name) };
    remove_service_key(drv_name);

    if status != 0 {
        return Err(nt_error("NtUnloadDriver", status));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_path_is_absolute_and_prefixed() {
        // build.rs 在 crate 根目录里一定存在
        let image = normalize_image_path("build.rs").unwrap();
        assert!(image.starts_with(r"\??\C:\"), "{image}");
        assert!(image.ends_with(r"build.rs"), "{image}");
        assert!(!image.contains(r"\\?\"), "{image}");
    }

    #[test]
    fn missing_file_is_reported_before_touching_the_registry() {
        let err = nt_load_driver("noumen_test", r"C:\definitely\missing\noumen.sys").unwrap_err();
        assert!(err.contains("驱动文件不可访问"), "{err}");
    }

    #[test]
    fn service_name_comes_from_the_file_stem() {
        assert_eq!(
            service_name_from_path(Path::new(r"C:\x\noumen_loader.sys")),
            "noumen_loader"
        );
        assert_eq!(
            service_name_from_path(Path::new(r"D:\drv\Test Drv.sys")),
            "Test Drv"
        );
        assert_eq!(service_name_from_path(Path::new("")), "noumen_loader");
    }

    #[test]
    fn default_paths_live_under_the_current_directory() {
        let cwd = env::current_dir().unwrap();

        assert_eq!(
            Path::new(&default_image_path()),
            cwd.join("noumen").as_path()
        );
        assert_eq!(
            Path::new(&default_log_path()),
            cwd.join("noumen.log").as_path()
        );
        assert_eq!(
            Path::new(&default_driver_path()),
            cwd.join("noumen_loader.sys").as_path()
        );
    }
}
