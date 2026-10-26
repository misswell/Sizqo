//! macOS Finder-style image conversion implemented entirely with ImageIO.

use std::fs;
use std::path::Path;

use objc2_core_foundation::{
    CFBoolean, CFData, CFDictionary, CFMutableData, CFMutableDictionary, CFNumber, CFRetained,
    CFString, CFType,
};
use objc2_core_graphics::CGImage;
use objc2_image_io::{
    kCGImagePropertyOrientation, kCGImageSourceCreateThumbnailFromImageAlways,
    kCGImageSourceCreateThumbnailWithTransform, kCGImageSourceThumbnailMaxPixelSize,
    CGImageDestination, CGImageSource,
};

use crate::engine::{CompressOptions, EngineResult};
use crate::result_model::{EncodeVerdict, ReasonCode};

const LARGE_MAX_PIXEL: isize = 1280;
const MEDIUM_MAX_PIXEL: isize = 640;
const SMALL_MAX_PIXEL: isize = 320;

/// `verdict` 由调用点决定，不是由文案猜：ImageIO 明确不接这个格式是 `Rejected`，
/// 解码到一半坏了 / 编不出来是 `Failed`。两者对用户都是"没有产物"，但原因不同，
/// 而 UI 要说的正是原因（§59）。
fn fail(
    original: Vec<u8>,
    out_type: &str,
    verdict: EncodeVerdict,
    reason: ReasonCode,
    message: impl Into<String>,
) -> EngineResult {
    EngineResult {
        success: false,
        compressed: original,
        out_type: out_type.into(),
        algorithm: "macOS ImageIO".into(),
        error: Some(message.into()),
        encode: verdict,
        reason,
        ..Default::default()
    }
}

fn target_type(target: &str) -> Option<(&'static str, &'static str)> {
    match target {
        "jpg" | "jpeg" => Some(("public.jpeg", "jpg")),
        "png" => Some(("public.png", "png")),
        "heic" | "heif" => Some(("public.heic", "heic")),
        // "original" 由 convert() 根据输入图片类型动态决定
        "original" => None,
        _ => None,
    }
}

fn max_pixel_for(size: &str, actual_max_pixel: isize) -> isize {
    match size {
        "large" => LARGE_MAX_PIXEL.min(actual_max_pixel),
        "medium" => MEDIUM_MAX_PIXEL.min(actual_max_pixel),
        "small" => SMALL_MAX_PIXEL.min(actual_max_pixel),
        _ => actual_max_pixel,
    }
}

/// 把用户要求的 `maxDimensionPx` 夹进 ImageIO 的最长边。
///
/// 系统转换自带 1280/640/320 尺寸档，这两个约束必须**取更严的那个**而不是互相覆盖：
/// 上限比档位更宽松时不动原行为，比档位更严格时由 ImageIO 真正兑现（不是"看起来生效"）。
/// 上限为 0 / 缺省时不改任何东西。
fn apply_dimension_cap(longest_edge: isize, cap: Option<u32>) -> isize {
    match cap {
        Some(edge) if edge > 0 => longest_edge.min(edge as isize),
        _ => longest_edge,
    }
}

fn thumbnail_options(max_pixel: isize) -> CFRetained<CFDictionary<CFType, CFType>> {
    let size = CFNumber::new_isize(max_pixel.max(1));
    CFDictionary::<CFType, CFType>::from_slices(
        &[
            unsafe { kCGImageSourceCreateThumbnailFromImageAlways }.as_ref(),
            unsafe { kCGImageSourceCreateThumbnailWithTransform }.as_ref(),
            unsafe { kCGImageSourceThumbnailMaxPixelSize }.as_ref(),
        ],
        &[
            CFBoolean::new(true).as_ref(),
            CFBoolean::new(true).as_ref(),
            size.as_ref(),
        ],
    )
}

fn normalized_properties(
    source: &CGImageSource,
    preserve_metadata: bool,
) -> Option<CFRetained<CFMutableDictionary>> {
    if !preserve_metadata {
        return None;
    }

    let properties = unsafe { source.properties_at_index(0, None) }?;
    let properties = unsafe { CFMutableDictionary::new_copy(None, 0, Some(&properties)) }?;
    let orientation = CFNumber::new_i32(1);
    unsafe {
        CFMutableDictionary::set_value(
            Some(&properties),
            (kCGImagePropertyOrientation as *const CFString).cast(),
            (orientation.as_ref() as *const CFNumber).cast(),
        );
    }
    Some(properties)
}

fn source_read_failure(error: &std::io::Error, out_type: &str) -> EngineResult {
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => ReasonCode::SourceNotFound,
        std::io::ErrorKind::PermissionDenied => ReasonCode::PermissionDenied,
        _ => ReasonCode::SourceReadFailed,
    };
    fail(
        Vec::new(),
        out_type,
        EncodeVerdict::Failed,
        reason,
        error.to_string(),
    )
}

pub fn convert(file: &Path, options: &CompressOptions) -> EngineResult {
    let original = match fs::read(file) {
        Ok(data) => data,
        Err(error) => return source_read_failure(&error, &options.output_format),
    };
    let source_format = crate::engine::detect_image_format(&original);

    let input = CFData::from_bytes(&original);
    let Some(source) = (unsafe { CGImageSource::with_data(&input, None) }) else {
        return fail(original, source_format, EncodeVerdict::Failed, ReasonCode::DecodeFailed, "macOS 无法读取此图像");
    };

    // §23：ImageIO 这条路径只取 index 0。多帧输入必须明确拒绝，不能静默只留第一帧。
    let frame_count = unsafe { source.count() };
    if frame_count > 1 {
        return fail(
            original,
            source_format,
            EncodeVerdict::Rejected,
            ReasonCode::UnsupportedFormat,
            format!(
                "系统转换不支持动图（{frame_count} 帧）：只会保留第一帧。已拒绝这次降级，原文件未改动"
            ),
        );
    }

    // 原格式：目标容器由**源文件真实 UTI** 决定，不再按扩展名猜，也绝不回退成 JPEG。
    // 输出标签取字节头判定，保证落盘扩展名与字节说的是同一件事（§19 §20）。
    let (target_uti, out_type): (CFRetained<CFString>, &'static str) =
        if options.output_format == "original" {
            if source_format == "unknown" {
                return fail(
                    original,
                    "original",
                    EncodeVerdict::Rejected,
                    ReasonCode::UnsupportedFormat,
                    "无法从字节识别源图格式，系统转换不会替你换一个格式",
                );
            }
            let Some(uti) = (unsafe { source.r#type() }) else {
                return fail(
                    original,
                    source_format,
                    EncodeVerdict::Rejected,
                    ReasonCode::UnsupportedFormat,
                    "macOS 没有给出源图的类型标识，无法按原格式输出",
                );
            };
            (uti, source_format)
        } else {
            match target_type(&options.output_format) {
                Some((uti, out_type)) => (CFString::from_str(uti), out_type),
                None => {
                    return fail(
                        original,
                        &options.output_format,
                        EncodeVerdict::Rejected,
                        ReasonCode::UnsupportedFormat,
                        "系统转换仅支持 JPEG、PNG 和 HEIF",
                    );
                }
            }
        };

    let Some(full_image) = (unsafe { source.image_at_index(0, None) }) else {
        return fail(original, out_type, EncodeVerdict::Failed, ReasonCode::DecodeFailed, "macOS 无法解码此图像");
    };

    let longest_edge =
        CGImage::width(Some(&full_image)).max(CGImage::height(Some(&full_image))) as isize;
    // maxDimensionPx 在系统转换里同样要兑现（不是只给引擎线用），取尺寸档与上限的更严者。
    let actual_max = apply_dimension_cap(longest_edge, options.max_dimension_px);
    let thumbnail_options =
        thumbnail_options(max_pixel_for(&options.system_image_size, actual_max));
    let typed_thumbnail_options: &CFDictionary<CFType, CFType> = &thumbnail_options;
    let thumbnail_options: &CFDictionary = typed_thumbnail_options.as_ref();
    let Some(image) = (unsafe { source.thumbnail_at_index(0, Some(thumbnail_options)) }) else {
        return fail(original, out_type, EncodeVerdict::Failed, ReasonCode::EncodeFailed, "macOS 无法调整图像尺寸");
    };

    let Some(output) = CFMutableData::new(None, 0) else {
        return fail(original, out_type, EncodeVerdict::Failed, ReasonCode::EncodeFailed, "无法创建系统转换缓冲区");
    };
    let Some(destination) =
        (unsafe { CGImageDestination::with_data(&output, &target_uti, 1, None) })
    else {
        return fail(
            original,
            out_type,
            EncodeVerdict::Rejected,
            ReasonCode::UnsupportedFormat,
            format!("此 macOS 版本不支持 {} 输出", out_type),
        );
    };

    let properties = normalized_properties(&source, options.preserve_metadata);
    unsafe {
        destination.add_image(&image, properties.as_deref().map(AsRef::as_ref));
    }
    if !unsafe { destination.finalize() } {
        return fail(original, out_type, EncodeVerdict::Failed, ReasonCode::EncodeFailed, "macOS 系统转换失败");
    }

    // §19 §20：ImageIO 说"写完了"不等于写出来的是我们要的容器。
    // 最终字节说了算；对不上就当作没有产物，绝不按 out_type 换个扩展名落盘。
    let bytes = output.to_vec();
    let actual = crate::engine::detect_image_format(&bytes);
    if !crate::engine::same_kind(actual, out_type) {
        return fail(
            original,
            out_type,
            EncodeVerdict::Failed,
            ReasonCode::OutputValidationFailed,
            format!("系统转换请求 {out_type}，实际产出的是 {actual}；已拒绝落盘"),
        );
    }

    EngineResult {
        success: true,
        compressed: bytes,
        out_type: out_type.into(),
        encode: EncodeVerdict::Candidate,
        algorithm: format!(
            "macOS ImageIO · {}",
            match options.system_image_size.as_str() {
                "large" => "大",
                "medium" => "中",
                "small" => "小",
                _ => "实际大小",
            }
        ),
        error: None,
        // 系统转换是唯一**保留**元数据的路径（properties 原样带进 ImageIO），
        // 所以这里不产生"元数据已丢失"的告警；warnings 的其余来源由 engine 汇总。
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_size_presets_match_expected_pixels() {
        assert_eq!(max_pixel_for("actual", 4000), 4000);
        assert_eq!(max_pixel_for("large", 4000), 1280);
        assert_eq!(max_pixel_for("medium", 4000), 640);
        assert_eq!(max_pixel_for("small", 4000), 320);
        assert_eq!(max_pixel_for("large", 800), 800);
    }

    /// 尺寸档与 maxDimensionPx 必须取**更严的那个**：上限不能静默覆盖尺寸档，
    /// 尺寸档也不能把用户点名的上限放宽回去。
    #[test]
    fn the_dimension_cap_never_loosens_the_finder_size_preset() {
        assert_eq!(apply_dimension_cap(4000, Some(800)), 800);
        assert_eq!(apply_dimension_cap(4000, Some(8000)), 4000); // 只缩不放
        assert_eq!(apply_dimension_cap(4000, None), 4000);
        assert_eq!(apply_dimension_cap(4000, Some(0)), 4000);
        assert_eq!(max_pixel_for("large", apply_dimension_cap(4000, Some(800))), 800);
        assert_eq!(max_pixel_for("small", apply_dimension_cap(4000, Some(800))), 320);
    }

    #[tokio::test]
    async fn the_system_conversion_path_honors_the_requested_longest_edge_cap() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.png");
        image::RgbImage::from_pixel(900, 600, image::Rgb([10, 200, 30]))
            .save(&input)
            .unwrap();

        let mut options = CompressOptions::default();
        options.processing_mode = "system".into();
        options.system_image_size = "actual".into(); // 尺寸档本不动尺寸，靠上限缩
        options.output_format = "png".into();
        options.max_dimension_px = Some(300);

        let result = crate::engine::compress_image(&input, &options).await;
        assert!(result.success, "{:?}", result.error);
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&result.compressed))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(width.max(height), 300);
        assert_eq!(width, 300, "最长边必须是水平边");
        assert_eq!(height, 200, "宽高比必须原样保持");
    }

    #[test]
    fn finder_formats_are_limited_to_native_choices() {
        assert_eq!(target_type("jpg"), Some(("public.jpeg", "jpg")));
        assert_eq!(target_type("png"), Some(("public.png", "png")));
        assert_eq!(target_type("heif"), Some(("public.heic", "heic")));
        assert_eq!(target_type("webp"), None);
    }

    #[test]
    fn image_io_converts_and_resizes_without_external_tools() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.png");
        let image = image::RgbImage::from_pixel(900, 600, image::Rgb([30, 120, 220]));
        image.save(&input).unwrap();

        let mut options = CompressOptions::default();
        options.processing_mode = "system".into();
        options.system_image_size = "small".into();
        options.output_format = "jpg".into();
        options.preserve_metadata = false;

        let result = convert(&input, &options);
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.out_type, "jpg");
        let decoded = image::load_from_memory(&result.compressed).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (320, 213));

        options.system_image_size = "actual".into();
        options.output_format = "heif".into();
        let heif = convert(&input, &options);
        assert!(heif.success, "{:?}", heif.error);
        assert_eq!(heif.out_type, "heic");
        assert!(!heif.compressed.is_empty());
    }

    #[test]
    fn a_missing_source_is_reported_as_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let result = convert(
            &temp.path().join("gone.png"),
            &CompressOptions::default(),
        );
        assert_eq!(result.encode, EncodeVerdict::Failed);
        assert_eq!(result.reason, ReasonCode::SourceNotFound);
        assert!(result.compressed.is_empty(), "读不到源文件时不许交回任何字节");
    }

    /// §19：系统转换的「原格式」过去按扩展名猜，webp/gif/avif 一律回退成 JPEG，
    /// 于是 photo.webp 会被写成一份 JPEG 字节。现在目标容器由源 UTI 决定，
    /// 写不出来就明确拒绝 —— 任何情况下都不许把 JPEG 冒充成原格式。
    #[test]
    fn original_format_never_falls_back_to_jpeg() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.gif");
        image::RgbImage::from_pixel(120, 90, image::Rgb([40, 160, 90]))
            .save(&input)
            .unwrap();

        let mut options = CompressOptions::default();
        options.processing_mode = "system".into();
        options.output_format = "original".into();
        let result = convert(&input, &options);

        if result.success {
            assert_eq!(
                result.out_type, "gif",
                "原格式是 GIF，标签不能变成别的：{:?}",
                result.error
            );
            assert_eq!(
                crate::engine::detect_image_format(&result.compressed),
                "gif",
                "交回的字节必须是源格式，不能偷偷换成 JPEG"
            );
        } else {
            assert_ne!(
                result.out_type, "jpg",
                "写不出来时要拒绝，不能把 JPEG 冒充成原格式：{:?}",
                result.error
            );
            assert_eq!(result.encode, EncodeVerdict::Rejected);
        }
    }

    /// §23：ImageIO 只取 index 0。多帧输入必须明确拒绝，不许静默只留第一帧。
    #[test]
    fn an_animated_gif_is_rejected_instead_of_losing_its_frames() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("animated.gif");
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            let frame = |tint: u8| image::Frame::new({
                let mut img = image::RgbaImage::from_pixel(32, 32, image::Rgba([tint, 80, 200, 255]));
                img.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
                img
            });
            encoder
                .encode_frames(vec![frame(10), frame(200), frame(120)])
                .unwrap();
        }
        fs::write(&input, &bytes).unwrap();

        let mut options = CompressOptions::default();
        options.processing_mode = "system".into();
        options.output_format = "jpg".into();
        let result = convert(&input, &options);

        assert_eq!(result.encode, EncodeVerdict::Rejected, "{:?}", result.error);
        assert_eq!(result.reason, ReasonCode::UnsupportedFormat);
        assert_eq!(result.compressed, bytes, "拒绝时交回的必须是原图本身");
        assert!(
            result.error.as_deref().unwrap_or("").contains("帧"),
            "必须说清是帧数问题：{:?}",
            result.error
        );
    }
}
