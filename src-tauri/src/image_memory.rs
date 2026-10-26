//! §63 图片解码的内存预算。
//!
//! 并发度只数"几个任务"是不够的：8 张 200 MP 的图同时解码，光像素就要好几个 GB。
//! 这里按 `宽 × 高 × 通道 × 安全系数` 给每张图记一笔内存账，调度器据此决定
//! 还能不能再开下一个任务（小图正常并发、大图降并发、超大图独占）。
//!
//! 估算只读文件头，绝不为了"量一下尺寸"把整张图像素读进内存（§64）。

use std::path::Path;

/// 解码后的像素、缩放工作副本、编码器输入至少要同时存在一份。
/// 3 倍是保守下限：宁可少开一个任务，也不能让一批大图把机器压进 swap。
pub const SAFETY_FACTOR: usize = 3;

/// 读不到物理内存时的兜底预算。512 MiB 足够让普通图片照常并发。
pub const FALLBACK_BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// 尺寸未知（ImageIO 专属容器等）时按一张大图记账。
/// 未知不等于小：不估像素，就至少别让它无限铺开。
pub const UNKNOWN_ESTIMATE_BYTES: usize = 512 * 1024 * 1024;

const MIN_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const MAX_BUDGET_BYTES: usize = 2 * 1024 * 1024 * 1024;
/// `ispe` 盒通常在 `meta` 里、位于 `mdat` 之前；只读文件头，不碰像素。
const BMFF_HEADER_SCAN_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryEstimate {
    /// 预计同时占用的字节数（已乘安全系数）。
    pub bytes: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 尺寸是真读出来的，还是"未知按大图"的兜底。
    pub known: bool,
}

/// 解码预算取物理内存的四分之一，夹在 256 MiB 与 2 GiB 之间。
///
/// 系统其余部分、前端界面和用户别的应用都要内存；压缩器没有理由把整机吃满。
pub fn budget_bytes() -> usize {
    let physical = crate::system_info::physical_memory_bytes();
    let quarter = physical.map(|bytes| bytes / 4).unwrap_or(FALLBACK_BUDGET_BYTES);
    quarter.clamp(MIN_BUDGET_BYTES, MAX_BUDGET_BYTES)
}

/// 解码一帧需要多少字节。`channels` 是"最坏情况下"的每像素字节数：
/// 带 alpha 的 4，不带的 3。PNG 可能是调色板，但按 RGBA 记只会更保守。
pub fn estimate_bytes(width: u32, height: u32, channels: usize) -> usize {
    (width as u64)
        .saturating_mul(height as u64)
        .saturating_mul(channels.max(1) as u64)
        .saturating_mul(SAFETY_FACTOR as u64)
        .min(usize::MAX as u64) as usize
}

fn channels_for(source_type: &str) -> usize {
    match source_type {
        "jpg" | "jpeg" | "bmp" => 3,
        // PNG / GIF / WebP / HEIC / AVIF / JXL 都可能带 alpha。
        _ => 4,
    }
}

/// 给一个文件记的内存账。尺寸读不出来时按"一张大图"兜底并标记 `known = false`。
pub fn estimate(path: &Path) -> MemoryEstimate {
    let source_type = crate::engine::detect_image_type(path);
    let channels = channels_for(&source_type);
    let dimensions = match source_type.as_str() {
        // image crate 没有这两个容器的解码器，但它俩都是 ISO BMFF，
        // 尺寸就写在 `ispe` 属性盒里 —— 读头部即可，不必解码。
        "heic" | "heif" | "avif" => bmff_dimensions(path),
        _ => image::image_dimensions(path).ok(),
    };
    match dimensions {
        Some((width, height)) if width > 0 && height > 0 => MemoryEstimate {
            bytes: estimate_bytes(width, height, channels).max(1),
            width: Some(width),
            height: Some(height),
            known: true,
        },
        _ => MemoryEstimate {
            bytes: UNKNOWN_ESTIMATE_BYTES,
            width: None,
            height: None,
            known: false,
        },
    }
}

/// 从 HEIC/AVIF 的 `ispe` 属性盒里取最大的一对宽高（主图通常比缩略图大）。
fn bmff_dimensions(path: &Path) -> Option<(u32, u32)> {
    let mut head = Vec::new();
    fs_take(path, &mut head, BMFF_HEADER_SCAN_BYTES)?;
    let mut best: Option<(u32, u32)> = None;
    for index in 0..head.len().saturating_sub(15) {
        if &head[index..index + 4] != b"ispe" {
            continue;
        }
        // ispe 是 FullBox：类型后 4 字节版本/标志，再往后才是宽、高。
        let width = u32::from_be_bytes(head[index + 8..index + 12].try_into().ok()?);
        let height = u32::from_be_bytes(head[index + 12..index + 16].try_into().ok()?);
        if width == 0 || height == 0 || width > 100_000 || height > 100_000 {
            continue;
        }
        let area = width as u64 * height as u64;
        if best.map_or(true, |(w, h)| (w as u64) * (h as u64) < area) {
            best = Some((width, height));
        }
    }
    best
}

fn fs_take(path: &Path, buffer: &mut Vec<u8>, limit: u64) -> Option<()> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    file.take(limit).read_to_end(buffer).ok()?;
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &Path, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        image::RgbImage::new(width, height).save(&path).unwrap();
        path
    }

    #[test]
    fn the_safety_factor_and_channels_are_both_in_the_bill() {
        assert_eq!(estimate_bytes(100, 100, 4), 100 * 100 * 4 * SAFETY_FACTOR);
        assert_eq!(estimate_bytes(100, 100, 3), 100 * 100 * 3 * SAFETY_FACTOR);
        // 裁剪过的通道数不能让估算变成 0。
        assert_eq!(estimate_bytes(10, 10, 0), 10 * 10 * 1 * SAFETY_FACTOR);
        // 天文尺寸不能溢出回一个小数字。
        assert!(estimate_bytes(u32::MAX, u32::MAX, 4) > 0);
    }

    #[test]
    fn a_normal_png_is_estimated_from_its_header_without_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_png(dir.path(), "normal.png", 120, 80);
        let estimate = estimate(&file);
        assert!(estimate.known);
        assert_eq!(estimate.width, Some(120));
        assert_eq!(estimate.height, Some(80));
        assert_eq!(estimate.bytes, estimate_bytes(120, 80, 4));
    }

    #[test]
    fn a_broken_file_is_billed_as_a_large_unknown_instead_of_free() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("broken.png");
        std::fs::write(&file, b"\x89PNG\r\n\x1a\n definitely not complete").unwrap();
        let estimate = estimate(&file);
        assert!(!estimate.known);
        assert_eq!(estimate.bytes, UNKNOWN_ESTIMATE_BYTES);
        assert!(estimate.bytes > 0);
    }

    #[test]
    fn heic_dimensions_come_from_the_ispe_box() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.heic");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x00\x00\x00\x18ftypheic");
        bytes.extend_from_slice(b"\x00\x00\x00\x00heicmif1heic");
        // 一个缩略图 ispe + 一个主图 ispe：必须取大的那个。
        bytes.extend_from_slice(&ispe_box(320, 240));
        bytes.extend_from_slice(&ispe_box(4032, 3024));
        std::fs::write(&file, bytes).unwrap();

        let estimate = estimate(&file);
        assert!(estimate.known, "HEIC 的尺寸就在 ispe 里，不该当成未知");
        assert_eq!(estimate.width, Some(4032));
        assert_eq!(estimate.height, Some(3024));
        assert_eq!(estimate.bytes, estimate_bytes(4032, 3024, 4));
    }

    #[test]
    fn an_impossible_ispe_never_becomes_the_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("weird.heic");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x00\x00\x00\x18ftypheic");
        bytes.extend_from_slice(&ispe_box(0, 999_999));
        std::fs::write(&file, bytes).unwrap();
        let estimate = estimate(&file);
        assert!(!estimate.known);
        assert_eq!(estimate.bytes, UNKNOWN_ESTIMATE_BYTES);
    }

    #[test]
    fn the_budget_is_bounded_even_without_physical_memory_info() {
        let budget = budget_bytes();
        assert!(budget >= MIN_BUDGET_BYTES);
        assert!(budget <= MAX_BUDGET_BYTES);
    }

    fn ispe_box(width: u32, height: u32) -> Vec<u8> {
        let mut boxed = Vec::new();
        boxed.extend_from_slice(&20u32.to_be_bytes());
        boxed.extend_from_slice(b"ispe");
        boxed.extend_from_slice(&[0, 0, 0, 0]);
        boxed.extend_from_slice(&width.to_be_bytes());
        boxed.extend_from_slice(&height.to_be_bytes());
        boxed
    }
}
