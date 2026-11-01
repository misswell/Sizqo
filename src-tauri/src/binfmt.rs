//! 「这个文件在不在」和「这台机器跑不跑得动」是两件事。
//!
//! `resources` 下 `bin` 目录里提交的是 macOS arm64 构建产物。按存在性把它交给
//! `Command` 之后，能力矩阵已经先一步把后端报告成"可用"，真正编码时才炸出
//! `Exec format error (os error 8)`。§12 要求不可用的原生后端绝不 spawn，
//! 所以格式判定只有这一份实现，图片线和视频线共用。
use std::{fs, io::Read, path::Path};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Format {
    MachO,
    Elf,
    Pe,
}

/// 认不出的头部（脚本、数据文件、空文件）一律返回 `None`，也就是"不是本机后端"。
/// 宁可让解析回退到系统 PATH，也不去 exec 一个必然失败的候选。
pub(crate) fn format_of(path: &Path) -> Option<Format> {
    let mut head = [0u8; 4];
    let mut file = fs::File::open(path).ok()?;
    if file.read_exact(&mut head).is_err() {
        return None;
    }
    // Mach-O thin/fat 的字节序两种都可能出现，认全 6 个魔数而不是赌一个。
    const MACHO: [[u8; 4]; 6] = [
        [0xFE, 0xED, 0xFA, 0xCE],
        [0xCE, 0xFA, 0xED, 0xFE],
        [0xFE, 0xED, 0xFA, 0xCF],
        [0xCF, 0xFA, 0xED, 0xFE],
        [0xCA, 0xFE, 0xBA, 0xBE],
        [0xBE, 0xBA, 0xFE, 0xCA],
    ];
    if MACHO.contains(&head) {
        return Some(Format::MachO);
    }
    if head == [0x7F, b'E', b'L', b'F'] {
        return Some(Format::Elf);
    }
    if &head[..2] == b"MZ" {
        return Some(Format::Pe);
    }
    None
}

/// 只有格式和本机平台对得上，才允许把它当作可用的原生后端。
pub(crate) fn runnable_here(path: &Path) -> bool {
    match format_of(path) {
        Some(Format::MachO) => cfg!(target_vendor = "apple"),
        Some(Format::Elf) => cfg!(target_os = "linux"),
        Some(Format::Pe) => cfg!(windows),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn magic(dir: &Path, name: &str, head: [u8; 4]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, head).unwrap();
        path
    }

    /// 三种原生格式各自只在本机成立。这条门在 CI 的三台机器上跑的是同一份断言，
    /// 所以"Linux 把 macOS 产物当后端"不可能再靠平台差异躲过去。
    #[test]
    fn a_native_format_is_runnable_only_on_its_own_platform() {
        let dir = tempfile::tempdir().unwrap();
        let macho = magic(dir.path(), "macho", [0xCF, 0xFA, 0xED, 0xFE]);
        let elf = magic(dir.path(), "elf", [0x7F, b'E', b'L', b'F']);
        let pe = magic(dir.path(), "pe", [b'M', b'Z', 0x90, 0x00]);
        assert_eq!(format_of(&macho), Some(Format::MachO));
        assert_eq!(format_of(&elf), Some(Format::Elf));
        assert_eq!(format_of(&pe), Some(Format::Pe));
        assert_eq!(runnable_here(&macho), cfg!(target_vendor = "apple"));
        assert_eq!(runnable_here(&elf), cfg!(target_os = "linux"));
        assert_eq!(runnable_here(&pe), cfg!(windows));
    }

    /// 认不出格式的文件不构成"可用后端"：脚本包装、半截下载、空文件都走这一支。
    #[test]
    fn an_unrecognized_header_is_never_a_backend() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("wrapper");
        fs::write(&script, b"#!/bin/sh\nexec ffmpeg \"$@\"\n").unwrap();
        assert_eq!(format_of(&script), None);
        assert!(!runnable_here(&script));
        let empty = dir.path().join("empty");
        fs::write(&empty, b"").unwrap();
        assert!(!runnable_here(&empty));
        // 目录不是可执行文件，也不许因为"存在"被端出去。
        assert!(!runnable_here(dir.path()));
    }
}
