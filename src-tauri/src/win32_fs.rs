//! Windows 上缺的那几件文件系统原语：文件身份 `(volume, file-index)`、设置时间戳、
//! 卷剩余空间与卷号。
//!
//! 为什么单独开一个模块：所有「这份字节还是刚才那份吗」的判断都靠 `(dev,ino)` 这个
//! 凭证，而 std 在 Windows 上不暴露它。没有它的时候，占位清理、临时残骸回收、
//! 隔离改名后的复验在 Windows 上一律退化成"只比体积和时间戳"，
//! `file_identity()` 返回 `None` 又让空占位的凭证门永远开不了 —— CI 的
//! `windows-tests` 作业把这几处一次性全报了出来。
//!
//! 这里按 Win32 的原生语义补齐：`BY_HANDLE_FILE_INFORMATION` 里的
//! `dwVolumeSerialNumber + nFileIndexHigh/Low` 与 unix 的 `dev + ino` 是同一件事
//! （同一卷内唯一、重开文件号不变、被替换成新文件就变），因此**不新增依赖**、
//! 也不改变 unix 分支的任何行为。符号声明用 `#[link(name = "kernel32")]`，
//! std 本来就链着它。

#![cfg(windows)]

use std::fs;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[repr(C)]
#[allow(non_snake_case, dead_code)]
struct Filetime {
    dwLowDateTime: u32,
    dwHighDateTime: u32,
}

#[repr(C)]
#[allow(non_snake_case, dead_code)]
struct ByHandleFileInformation {
    dwFileAttributes: u32,
    ftCreationTime: Filetime,
    ftLastAccessTime: Filetime,
    ftLastWriteTime: Filetime,
    nNumberOfLinks: u32,
    dwVolumeSerialNumber: u32,
    nFileIndexHigh: u32,
    nFileIndexLow: u32,
    nFileSizeHigh: u32,
    nFileSizeLow: u32,
    dwReserved0: u32,
    dwReserved1: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(
        handle: *mut core::ffi::c_void,
        info: *mut ByHandleFileInformation,
    ) -> i32;
    fn SetFileTime(
        handle: *mut core::ffi::c_void,
        creation: *const Filetime,
        last_access: *const Filetime,
        last_write: *const Filetime,
    ) -> i32;
    fn GetDiskFreeSpaceExW(
        directory_path: *const u16,
        free_bytes_available_to_caller: *mut u64,
        total_number_of_bytes: *mut u64,
        total_number_of_free_bytes: *mut u64,
    ) -> i32;
}

/// `OpenOptions::custom_flags` 需要的目录句柄标志：没有它就打不开目录。
pub(crate) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// 从已打开的句柄上取 `(volume serial, file index)`。
pub fn identity_of_handle(handle: *mut core::ffi::c_void) -> Option<(u64, u64)> {
    let mut info = ByHandleFileInformation {
        dwFileAttributes: 0,
        ftCreationTime: Filetime {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        ftLastAccessTime: Filetime {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        ftLastWriteTime: Filetime {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        },
        nNumberOfLinks: 0,
        dwVolumeSerialNumber: 0,
        nFileIndexHigh: 0,
        nFileIndexLow: 0,
        nFileSizeHigh: 0,
        nFileSizeLow: 0,
        dwReserved0: 0,
        dwReserved1: 0,
    };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    if ok == 0 {
        return None;
    }
    let index = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    Some((info.dwVolumeSerialNumber as u64, index))
}

/// 路径版的文件身份：与 `output_transaction::file_identity` 的 unix 分支同语义。
///
/// 走 `File::open` 而不是 `FindFirstFile`：我们要的是**这个 inode** 的身份，
/// 打不开（没读权限、正被独占）就说明不了归属 —— 返回 `None`，调用方的凭证门
/// 一律按"证明不了就保留"处理，方向是安全的。
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    let file = fs::File::open(path).ok()?;
    identity_of_handle(file.as_raw_handle())
}

/// 1601-01-01 到 1970-01-01 的 100ns 间隔数。
const WIN32_EPOCH_OFFSET_100NS: u64 = 116_444_736_000_000_000;

fn to_filetime(time: SystemTime) -> Filetime {
    let since = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|error| {
            // 1970 年以前的时间戳：按 0 处理，Windows 也写不进负值。
            let _ = error;
            std::time::Duration::from_secs(0)
        });
    let hundred_ns = WIN32_EPOCH_OFFSET_100NS.saturating_add(since.as_nanos() as u64 / 100);
    Filetime {
        dwLowDateTime: hundred_ns as u32,
        dwHighDateTime: (hundred_ns >> 32) as u32,
    }
}

/// 设置访问时间 + 修改时间（`utimensat` 的 Windows 等价物）。
///
/// 只给 `SetFileTime` 传访问时间/修改时间，创建时间传 `NULL` 表示不动。
pub fn set_times(path: &Path, accessed: SystemTime, modified: SystemTime) -> Result<(), String> {
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let access = to_filetime(accessed);
    let write = to_filetime(modified);
    let ok = unsafe {
        SetFileTime(
            file.as_raw_handle(),
            std::ptr::null(),
            &access,
            &write,
        )
    };
    if ok == 0 {
        return Err(format!(
            "{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// 路径所在卷的序列号 —— `(volume, file-index)` 里那个 volume，用于回答
/// "输出目录和备份目录是不是同一块盘"（§5 要求同盘时把两份空间合并估算）。
///
/// 目录句柄必须带 `FILE_FLAG_BACKUP_SEMANTICS`；查不到就返回 `None`，
/// 由调用方按"无法区分就当成同一盘"的保守方向处理。
pub fn volume_id_of_path(path: &Path) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_ALL: u32 = 0x0000_0007;
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    let (volume, _) = identity_of_handle(handle.as_raw_handle())?;
    Some(volume)
}

/// 这个位置上还能写多少字节（`statvfs` 的 Windows 等价物）。
///
/// §5 要求编码前**分开**检查产物空间与持久备份空间。原来 Windows 上这个问题
/// 根本答不出来：`available_space` 直接 `Err`，于是视频轨在第一个检查点就整条失败。
pub fn available_space(path: &Path) -> Result<u64, String> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut free_bytes = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!(
            "{}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(free_bytes)
}
