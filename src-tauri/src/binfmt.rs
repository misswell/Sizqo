//! 「这个文件在不在」和「这台机器跑不跑得动」是两件事。
//!
//! `resources` 下 `bin` 目录里提交的是 macOS arm64 构建产物。按存在性把它交给
//! `Command` 之后，能力矩阵已经先一步把后端报告成"可用"，真正编码时才炸出
//! `Exec format error (os error 8)`。§12 要求不可用的原生后端绝不 spawn，
//! 所以格式判定只有这一份实现，图片线和视频线共用。
//!
//! 平台对上之后还剩一半问题：**同一台 mac 也分架构**。随包产物是 arm64 单切片
//! （`lipo -info` 实测 `ffmpeg`/`ffprobe`/7 个图片工具与全部 dylib 都是 arm64），
//! 而发布线同时出 `x86_64-apple-darwin` 与 universal —— Intel 上拿到 arm64 切片时
//! Rosetta 帮不上（它只反着帮），`posix_spawn` 直接 `EBADARCH`「Bad CPU type in
//! executable」。所以 Mach-O 这条路要多问一层切片架构，问不出就当作不可用。
use std::{fs, io::Read, path::{Path, PathBuf}};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Format {
    MachO,
    Elf,
    Pe,
}

/// `mach/machine.h`：CPU_TYPE_X86_64 / CPU_TYPE_ARM64（都带 `CPU_ARCH_ABI64`）。
const CPU_X86_64: u32 = 0x0100_0007;
const CPU_ARM64: u32 = 0x0100_000C;
/// fat 表最多读这么多条目：真正的 universal 不会超过这个数，多出来的一律当作
/// "读不出可信架构"，不去 exec。
const MAX_FAT_ARCHES: usize = 16;

fn host_cpu_type() -> Option<u32> {
    if cfg!(target_arch = "aarch64") {
        Some(CPU_ARM64)
    } else if cfg!(target_arch = "x86_64") {
        Some(CPU_X86_64)
    } else {
        None
    }
}

fn u32_at(bytes: &[u8], offset: usize, big_endian: bool) -> Option<u32> {
    let window = bytes.get(offset..offset + 4)?;
    let mut word = [0u8; 4];
    word.copy_from_slice(window);
    Some(if big_endian { u32::from_be_bytes(word) } else { u32::from_le_bytes(word) })
}

/// Mach-O 的切片是否包含本机 CPU。返回 `false` 的三种情况要分清：结构读不出、
/// 架构不是本机、本机架构根本不认识 —— 三者都不许 spawn。
fn macho_has_host_slice(path: &Path) -> bool {
    // 8 字节头 + 16 条 fat_arch（每条 20 字节）。
    let mut buffer = [0u8; 8 + 20 * MAX_FAT_ARCHES];
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let read = match file.read(&mut buffer) {
        Ok(read) => read,
        Err(_) => return false,
    };
    if read < 4 {
        return false;
    }
    let head = [buffer[0], buffer[1], buffer[2], buffer[3]];
    let host = match host_cpu_type() {
        Some(host) => host,
        None => return false,
    };
    match head {
        // thin：cputype 紧跟魔数；魔数决定字段字节序。
        [0xCF, 0xFA, 0xED, 0xFE] | [0xFE, 0xED, 0xFA, 0xCE] => {
            read >= 8 && u32_at(&buffer, 4, false) == Some(host)
        }
        [0xFE, 0xED, 0xFA, 0xCF] | [0xCE, 0xFA, 0xED, 0xFE] => {
            read >= 8 && u32_at(&buffer, 4, true) == Some(host)
        }
        // fat：条目从第 8 字节起，每条 20 字节，前 4 字节是 cputype。
        [0xCA, 0xFE, 0xBA, 0xBE] => fat_lists_host(&buffer, read, host, true),
        [0xBE, 0xBA, 0xFE, 0xCA] => fat_lists_host(&buffer, read, host, false),
        _ => false,
    }
}

fn fat_lists_host(buffer: &[u8], read: usize, host: u32, big_endian: bool) -> bool {
    let count = match u32_at(buffer, 4, big_endian) {
        Some(count) => usize::try_from(count).unwrap_or(usize::MAX).min(MAX_FAT_ARCHES),
        None => return false,
    };
    (0..count).any(|index| {
        let offset = 8 + 20 * index;
        read >= offset + 4 && u32_at(buffer, offset, big_endian) == Some(host)
    })
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

/// 只有格式、本机平台以及（Mach-O 的）切片架构都对得上，才允许把它当作可用的原生后端。
///
/// 一条已知保守：只带 x86_64 切片的引擎在 Apple Silicon 上也返回 `false`，尽管装了
/// Rosetta 2 就能跑。这里选择"不假设 Rosetta 存在"，理由是同一条门要拦的正是
/// "以为能跑而 spawn"；真要走这条路，得由打包投出 universal 引擎，而不是靠猜。
pub(crate) fn runnable_here(path: &Path) -> bool {
    match format_of(path) {
        Some(Format::MachO) => cfg!(target_vendor = "apple") && macho_has_host_slice(path),
        Some(Format::Elf) => cfg!(target_os = "linux"),
        Some(Format::Pe) => cfg!(windows),
        None => false,
    }
}

/// 系统里（PATH）的候选只能用这一条，不能用 `runnable_here` 那种正向闸门。
///
/// 差别只在"认不出格式"这一支：随包产物认不出就该拒（那是我们自己投的，必须是本机
/// 原生二进制）；用户装的认不出就该放行 —— flatpak/snap 的 shim、shell 包装都是真能
/// exec 的东西，用正向闸门会把它们一起报成"引擎不可用"，而这台机器其实跑得好。
/// 认得出格式但格式不是本机的那一种，照旧拒绝：那正是 `Exec format error` 的来源，
/// 只不过候选来自 PATH 而不是 `resources`。引擎到底行不行由能力探测真跑一次定论。
pub(crate) fn plausible_system_backend(path: &Path) -> bool {
    format_of(path).is_none() || runnable_here(path)
}

/// PATH 候选的扫描只留一份实现：图片线和视频线走同一条门，避免以后又是一处补、一处漏。
pub(crate) fn system_tool_in<P: AsRef<Path>, I: IntoIterator<Item = P>>(
    dirs: I,
    file_name: &str,
) -> Option<PathBuf> {
    dirs.into_iter()
        .map(|dir| dir.as_ref().join(file_name))
        .find(|path| path.is_file() && plausible_system_backend(path))
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

    /// thin Mach-O：魔数 + cputype（按魔数暗示的字节序写）。
    fn thin(dir: &Path, name: &str, big_endian: bool, cpu: u32) -> PathBuf {
        let magic: [u8; 4] =
            if big_endian { [0xFE, 0xED, 0xFA, 0xCF] } else { [0xCF, 0xFA, 0xED, 0xFE] };
        let cpu_bytes = if big_endian { cpu.to_be_bytes() } else { cpu.to_le_bytes() };
        let path = dir.join(name);
        let mut bytes = magic.to_vec();
        bytes.extend_from_slice(&cpu_bytes);
        bytes.extend_from_slice(&[0u8; 24]);
        fs::write(&path, bytes).unwrap();
        path
    }

    /// fat/universal：魔数 + 条目数 + 每条 20 字节（cputype 在条首）。
    fn fat(dir: &Path, name: &str, cpus: &[u32]) -> PathBuf {
        let path = dir.join(name);
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        bytes.extend_from_slice(&(cpus.len() as u32).to_be_bytes());
        for (index, cpu) in cpus.iter().enumerate() {
            bytes.extend_from_slice(&cpu.to_be_bytes());
            bytes.extend_from_slice(&(16u32 << index).to_be_bytes()); // cpusubtype（无关）
            bytes.extend_from_slice(&0u32.to_be_bytes()); // offset
            bytes.extend_from_slice(&64u32.to_be_bytes()); // size
            bytes.extend_from_slice(&4u32.to_be_bytes()); // align
        }
        fs::write(&path, bytes).unwrap();
        path
    }

    /// 三种原生格式各自只在本机成立。这条门在 CI 的三台机器上跑的是同一份断言，
    /// 所以"Linux 把 macOS 产物当后端"不可能再靠平台差异躲过去。
    #[test]
    fn a_native_format_is_runnable_only_on_its_own_platform() {
        let dir = tempfile::tempdir().unwrap();
        let macho = thin(dir.path(), "macho", false, host_cpu_type().unwrap_or(CPU_ARM64));
        let elf = magic(dir.path(), "elf", [0x7F, b'E', b'L', b'F']);
        let pe = magic(dir.path(), "pe", [b'M', b'Z', 0x90, 0x00]);
        assert_eq!(format_of(&macho), Some(Format::MachO));
        assert_eq!(format_of(&elf), Some(Format::Elf));
        assert_eq!(format_of(&pe), Some(Format::Pe));
        assert_eq!(runnable_here(&macho), cfg!(target_vendor = "apple"));
        assert_eq!(runnable_here(&elf), cfg!(target_os = "linux"));
        assert_eq!(runnable_here(&pe), cfg!(windows));
    }

    /// 平台对上不等于跑得动：随包引擎只有 arm64 切片时，Intel mac 上 exec 会炸
    /// `EBADARCH`「Bad CPU type in executable」。架构门必须在这一层把住，
    /// 而 universal（同时列两个切片）在两种 mac 上都必须放行 —— 否则闸门会把
    /// 自家产物一起拒掉。断言写成"与本机架构比较"，因此在三台 CI 机器上都成立。
    #[test]
    fn a_macho_slice_the_host_cpu_cannot_run_is_never_a_backend() {
        let dir = tempfile::tempdir().unwrap();
        let arm = thin(dir.path(), "arm64", false, CPU_ARM64);
        let intel = thin(dir.path(), "x86_64", false, CPU_X86_64);
        let both = fat(dir.path(), "universal", &[CPU_X86_64, CPU_ARM64]);
        let only_intel_arch = fat(dir.path(), "fat-intel", &[CPU_X86_64]);
        // 大端 thin 走另一条魔数分支，结论必须一致。
        let arm_be = thin(dir.path(), "arm64-be", true, CPU_ARM64);
        assert_eq!(runnable_here(&arm), cfg!(all(target_vendor = "apple", target_arch = "aarch64")));
        assert_eq!(runnable_here(&arm_be), cfg!(all(target_vendor = "apple", target_arch = "aarch64")));
        assert_eq!(runnable_here(&intel), cfg!(all(target_vendor = "apple", target_arch = "x86_64")));
        assert_eq!(runnable_here(&only_intel_arch), cfg!(all(target_vendor = "apple", target_arch = "x86_64")));
        assert_eq!(runnable_here(&both), cfg!(target_vendor = "apple"), "universal 不许被架构门误杀");
        // 半截头（只有魔数，读不出 cputype）不构成后端：证明不了就不 exec。
        let truncated = magic(dir.path(), "truncated", [0xCF, 0xFA, 0xED, 0xFE]);
        assert_eq!(format_of(&truncated), Some(Format::MachO));
        assert!(!runnable_here(&truncated));
        // fat 表里一个本机架构都没有，同样不许 exec。
        let foreign = fat(dir.path(), "fat-power", &[0x0000_0012]);
        assert!(!runnable_here(&foreign), "只列了本机没有的切片的 fat 不是后端");
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

    /// PATH 那条门和随包那条门在"认不出格式"上必须分家：用户装的 shell 包装 /
    /// flatpak shim 是真能 exec 的，正向闸门会把跑得动的机器报成"引擎不可用"；
    /// 而"认得出格式却不是本机那一种"照样要拒 —— 那和随包产物是同一个
    /// `Exec format error`，只是候选来自 PATH。
    #[test]
    fn the_path_gate_rejects_foreign_formats_but_not_wrappers() {
        let dir = tempfile::tempdir().unwrap();
        let native = thin(dir.path(), "native", false, host_cpu_type().unwrap_or(CPU_ARM64));
        let elf = magic(dir.path(), "linux-engine", [0x7F, b'E', b'L', b'F']);
        let macho = thin(dir.path(), "mac-engine", false, CPU_ARM64);
        let pe = magic(dir.path(), "win-engine.exe", [b'M', b'Z', 0x90, 0x00]);
        let wrapper = dir.path().join("shim");
        fs::write(&wrapper, b"#!/bin/sh\nexec ffmpeg \"$@\"\n").unwrap();

        assert_eq!(plausible_system_backend(&native), cfg!(target_vendor = "apple"));
        assert_eq!(plausible_system_backend(&elf), cfg!(target_os = "linux"));
        assert_eq!(
            plausible_system_backend(&macho),
            cfg!(all(target_vendor = "apple", target_arch = "aarch64"))
        );
        assert_eq!(plausible_system_backend(&pe), cfg!(windows));
        // 只有这一支和 runnable_here 不同：认不出格式就交给真跑一次的能力探测定论。
        assert!(plausible_system_backend(&wrapper), "脚本包装不该被格式门误杀");
        assert!(!runnable_here(&wrapper), "同一条文件在随包那一步仍然不算后端");
    }

    /// 扫描要真按门跳：前面的目录躺着本机跑不动的引擎时，必须继续找到后面那个，
    /// 而不是把第一个"存在"的东西交出去。
    #[test]
    fn the_path_scan_steps_over_an_unexecuable_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad");
        let good = dir.path().join("good");
        fs::create_dir(&bad).unwrap();
        fs::create_dir(&good).unwrap();
        let foreign: [u8; 4] = if cfg!(target_os = "macos") {
            [0x7F, b'E', b'L', b'F']
        } else {
            [0xFE, 0xED, 0xFA, 0xCF]
        };
        magic(&bad, "ffmpeg", foreign);
        magic(&good, "ffmpeg", [b'#', b'!', b'/', b'i']);

        assert_eq!(
            system_tool_in([bad.as_path()], "ffmpeg"),
            None,
            "本机跑不动的 PATH 候选不许被当成后端"
        );
        assert_eq!(
            system_tool_in([bad.as_path(), good.as_path()], "ffmpeg"),
            Some(good.join("ffmpeg")),
            "跳过跑不动的候选之后要找得到下一个"
        );
        assert_eq!(system_tool_in([bad.as_path()], "ffprobe"), None, "找不到就是 None");
    }
}
