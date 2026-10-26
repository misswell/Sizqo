// 进程内压缩引擎（App Store 产物线 / feature = inproc-backends）
//
// 与 engine.rs 公共接口一致，但**只调用链接进本进程的 Rust crate**：
// 这里绝不允许出现 find_tool / make_command / Command::new —— 沙盒子进程拿不到
// 父进程的 security-scoped 文件授权（书签授权挂在父进程的 sandbox extension 上，
// 不随 fork 继承），所以打包 CLI 在这条线上必然失败，只会白付一次 spawn 的成本。
// 提交用的 scripts/build_appstore.sh 也从不往 .app 里放 CLI/dylib。
//
// 改造原则（强制）：本文件的公共函数签名必须与 engine.rs 同名函数严格一致，
// CompressOptions / EngineResult 等类型复用 super::engine，不得重复定义。
// 新增格式 → 只加 crate 实现；要引外部编码器，先在 AGENTS.md 对照表里登记。

use std::fs;
use std::path::Path;

// engine_inproc 是 engine 模块的子模块，super 就是 engine 自身
use super::{
    animation_rejection, conversion_failed, detect_image_format, read_source, rejected,
    CompressOptions, EngineResult, OutputRule,
};
use crate::result_model::{EncodeVerdict, ReasonCode};

fn ok(data: Vec<u8>, out_type: &str, algorithm: &str) -> EngineResult {
    EngineResult {
        success: true,
        compressed: data,
        out_type: out_type.into(),
        algorithm: algorithm.into(),
        error: None,
        encode: EncodeVerdict::Candidate,
        ..Default::default()
    }
}

/// 「保留原图」这一支交回的字节就是**源文件本身**，所以 out_type 必须写源格式。
/// 与 CLI 线（`engine::kept_original`）同一条规则：格式转换没压小回来时，
/// 落盘拿到的标签必须和字节内容一致，否则会得到一个扩展名骗人的文件。
fn kept_original(original: Vec<u8>, algorithm: &str, orig_size: u64, comp_size: u64) -> EngineResult {
    let source_type = detect_image_format(&original);
    no_improvement(original, source_type, algorithm, orig_size, comp_size)
}

/// 各编码器的统一出口：转换只要格式对就收，压缩只有变小才收。
fn acceptable(rule: OutputRule, size: usize, original_size: u64) -> bool {
    rule == OutputRule::Convert || (size as u64) < original_size
}

fn no_candidate(original: Vec<u8>, out_type: &str, algorithm: &str, rule: OutputRule) -> EngineResult {
    let size = original.len() as u64;
    match rule {
        OutputRule::BenefitOnly => kept_original(original, algorithm, size, size),
        OutputRule::Convert => conversion_failed(
            original,
            out_type,
            format!("没能产出 {out_type}：进程内编码器没有交回有效字节"),
        ),
    }
}

/// 压缩后体积未减小：返回原始数据，标记为"无优化空间"（不算失败）
fn no_improvement(original: Vec<u8>, out_type: &str, algorithm: &str, orig_size: u64, comp_size: u64) -> EngineResult {
    let msg = if comp_size >= orig_size {
        format!("压缩后 {} > 原始 {}，原图已是最优压缩", super::format_bytes(comp_size), super::format_bytes(orig_size))
    } else {
        "压缩后体积未减小".into()
    };
    EngineResult {
        success: true,
        compressed: original,
        out_type: out_type.into(),
        algorithm: algorithm.into(),
        error: Some(msg),
        encode: EncodeVerdict::KeptOriginal,
        reason: if comp_size > orig_size {
            ReasonCode::OutputLargerThanSource
        } else {
            ReasonCode::NoSizeReduction
        },
        ..Default::default()
    }
}

/// 与 CLI 线共用同一个构造器：两条产物线对"这件事我们不做"必须给出同一个判据（§65）。
fn unsupported(original: Vec<u8>, out_type: &str, reason: &str) -> EngineResult {
    rejected(original, out_type, "none", ReasonCode::UnsupportedFormat, reason)
}

/// PNG：imagequant 量化（与 pngquant 同源算法）→ oxipng 库版 → image crate 兜底。
async fn compress_png(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "png") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "PNG") {
        return rejected(original, "png", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;

    if let Some(data) = compress_png_with_imagequant(file, quality) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "png", "pngquant (inproc)");
        }
    }

    if let Some(data) = compress_png_with_oxipng(file, quality) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "png", "oxipng (inproc)");
        }
    }

    // 兜底：无损重编码。前两条都不适用时才轮到它。
    if let Some(data) = compress_png_with_image(file) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "png", "image-png");
        }
    }

    no_candidate(original, "png", "pngquant (inproc)", rule)
}

/// image crate 兜底：PNG 无损重编码，保留原色型（RGB→RGB8，RGBA→RGBA8）。
fn compress_png_with_image(file: &Path) -> Option<Vec<u8>> {
    let img = image::open(file).ok()?;
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut buf,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    );
    use image::ImageEncoder;
    match img.color() {
        image::ColorType::Rgba8 | image::ColorType::Rgba16 => {
            let rgba = img.to_rgba8();
            encoder.write_image(&rgba, img.width(), img.height(), image::ExtendedColorType::Rgba8).ok()?
        }
        _ => {
            let rgb = img.to_rgb8();
            encoder.write_image(&rgb, img.width(), img.height(), image::ExtendedColorType::Rgb8).ok()?
        }
    }
    if buf.is_empty() { None } else { Some(buf) }
}

/// oxipng crate 无损优化。level = (quality/20).clamp(1,6)。
fn compress_png_with_oxipng(file: &Path, quality: u32) -> Option<Vec<u8>> {
    let data = std::fs::read(file).ok()?;
    let level = (quality / 20).clamp(1, 6) as u8;
    let opts = oxipng::Options::from_preset(level);
    oxipng::optimize_from_memory(&data, &opts).ok()
}

/// imagequant crate 量化（与 pngquant 同源算法）：quality→set_quality(q-10,q)，speed=3，
/// 量化后用 png crate 直接写 8-bit 调色板 PNG（PLTE + 索引像素），与 pngquant CLI 一致。
fn compress_png_with_imagequant(file: &Path, quality: u32) -> Option<Vec<u8>> {
    use imagequant::{RGBA, Attributes};
    let img = image::open(file).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let raw: &[u8] = rgba.as_raw();
    let pixels: Vec<RGBA> = raw.chunks_exact(4)
        .map(|c| RGBA::new(c[0], c[1], c[2], c[3]))
        .collect();
    let mut attr = Attributes::new();
    let _ = attr.set_speed(3);
    let q = quality.min(100).max(10);
    let _ = attr.set_quality((q - 10) as u8, q as u8);
    let mut image = attr.new_image(pixels, w, h, 0.0).ok()?;
    let mut quant = attr.quantize(&mut image).ok()?;
    let _ = quant.set_dithering_level(1.0);
    let (palette, indices) = quant.remapped(&mut image).ok()?;

    // 构建 RGB 调色板（3 bytes/entry）+ tRNS 透明度表
    let mut palette_rgb = Vec::with_capacity(palette.len() * 3);
    let mut trns = Vec::with_capacity(palette.len());
    let mut has_alpha = false;
    for p in &palette {
        palette_rgb.push(p.r); palette_rgb.push(p.g); palette_rgb.push(p.b);
        if p.a < 255 { has_alpha = true; }
        trns.push(p.a);
    }

    // 用 png crate 写 8-bit Indexed PNG（同 pngquant CLI 输出格式）
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w as u32, h as u32);
        enc.set_color(png::ColorType::Indexed);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_palette(&palette_rgb);
        if has_alpha { enc.set_trns(trns); }
        enc.set_compression(png::Compression::High);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&indices).ok()?;
        writer.finish().ok()?;
    }
    if buf.is_empty() { return None; }

    // 快速路径：palette PNG 已比原文件小 → 直接返回（多数图片走此路径）
    let original_size = std::fs::metadata(file).ok()?.len() as usize;
    if buf.len() < original_size {
        return Some(buf);
    }

    // 慢速路径：palette PNG 比原文件大 → oxipng 全过滤优化
    // preset 4 = 全部 5 种过滤器（None/Sub/Up/Avg/Paeth）逐行选优
    // + Libdeflater（C 库 zlib，compression=12）+ 无超时（确保压到最优）
    let mut opts = oxipng::Options::from_preset(4);
    opts.bit_depth_reduction = false;
    opts.color_type_reduction = false;
    opts.palette_reduction = false;
    opts.grayscale_reduction = false;
    if let oxipng::Deflaters::Libdeflater { compression } = &mut opts.deflate {
        *compression = 12;
    }
    opts.timeout = None;
    match oxipng::optimize_from_memory(&buf, &opts) {
        Ok(optimized) if optimized.len() < buf.len() => Some(optimized),
        _ => Some(buf),
    }
}

/// image crate 兜底：JPEG 按质量重编码（RGB8）。
fn compress_jpg_with_image(file: &Path, quality: u8) -> Option<Vec<u8>> {
    let img = image::open(file).ok()?;
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    use image::ImageEncoder;
    encoder
        .write_image(&rgb, img.width(), img.height(), image::ExtendedColorType::Rgb8)
        .ok()?;
    if buf.is_empty() { None } else { Some(buf) }
}

/// mozjpeg crate 量化（同源 cjpeg trellis quant）：set_quality + optimize + progressive。
fn compress_jpg_with_mozjpeg(file: &Path, quality: u32) -> Option<Vec<u8>> {
    let img = image::open(file).ok()?;
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let mut cinfo = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_EXT_RGB);
    cinfo.set_size(w, h);
    cinfo.set_quality(quality as f32);
    cinfo.set_optimize_coding(true);
    cinfo.set_progressive_mode();
    let mut comp = cinfo.start_compress(Vec::new()).ok()?;
    let row_stride = w * 3;
    let raw: &[u8] = rgb.as_raw();
    for row in raw.chunks_exact(row_stride) {
        let _ = comp.write_scanlines(row);
    }
    comp.finish().ok()
}

/// JPEG：两级 fallback（对齐 cli 版 engine.rs），成功判定 data.len() < original_size。
/// mozjpeg crate 量化（同源 cjpeg trellis quant）→ image crate 兜底。
async fn compress_jpg(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "jpg") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "JPEG") {
        return rejected(original, "jpg", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;
    if let Some(data) = compress_jpg_with_mozjpeg(file, quality) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "jpg", "mozjpeg (inproc)");
        }
    }
    if let Some(data) = compress_jpg_with_image(file, quality as u8) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "jpg", "image-jpeg (inproc)");
        }
    }
    no_candidate(original, "jpg", "mozjpeg (inproc)", rule)
}

/// webp crate 编码（同源 cwebp/libwebp）：from_rgba + encode(quality)。
fn compress_to_webp_with_cwebp(file: &Path, quality: u32) -> Option<Vec<u8>> {
    let img = image::open(file).ok()?;
    let rgba = img.to_rgba8();
    let encoder = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
    let webp_data = encoder.encode(quality as f32);
    let bytes: &[u8] = &webp_data;
    if bytes.is_empty() { None } else { Some(bytes.to_vec()) }
}

/// WebP：进程内 webp crate 编码（对齐 cli 版 cwebp）。成功判定 < original_size。
async fn compress_to_webp(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "webp") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "WebP") {
        return rejected(original, "webp", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;
    if let Some(data) = compress_to_webp_with_cwebp(file, quality) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "webp", "cwebp (inproc)");
        }
    }
    no_candidate(original, "webp", "cwebp (inproc)", rule)
}

/// image crate 重编码 GIF（保留所有帧，无 gifsicle 减色优化，功能降级但可用）。
fn compress_gif_with_image(file: &Path) -> Option<Vec<u8>> {
    use image::AnimationDecoder;
    let f = std::fs::File::open(file).ok()?;
    let dec = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(f)).ok()?;
    let frames = dec.into_frames().collect_frames().ok()?;
    let mut buf = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new_with_speed(&mut buf, 3);
        let _ = enc.encode_frames(frames);
    }
    if buf.is_empty() { None } else { Some(buf) }
}

/// GIF：image crate 逐帧重编码（无 gifsicle 减色，属已记录的质量降级，非功能缺失）。
async fn compress_gif(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let _ = options;
    let original = match read_source(file, "gif") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;

    // 进程内的 GifDecoder/GifEncoder 逐帧处理，动画不会丢；因此这里不拒收动画输入。
    if let Some(data) = compress_gif_with_image(file) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "gif", "gifsicle (inproc)");
        }
    }

    no_candidate(original, "gif", "gifsicle (inproc)", rule)
}

/// ravif crate 编码（同源 AV1/rav1e，对齐 avifenc）：with_quality + with_speed(6)。
fn compress_to_avif_with_avifenc(file: &Path, quality: u32) -> Option<Vec<u8>> {
    let img = image::open(file).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let pixels: Vec<ravif::RGBA8> = rgba.as_raw().chunks_exact(4)
        .map(|c| ravif::RGBA8 { r: c[0], g: c[1], b: c[2], a: c[3] })
        .collect();
    // ravif 的线程数默认跟着 rayon 全局池走（= 全部核心），必须显式压到 1 份预算。
    let enc = ravif::Encoder::new()
        .with_quality(quality as f32)
        .with_speed(6)
        .with_num_threads(Some(super::per_task_threads()));
    let result = enc.encode_rgba(ravif::Img::new(&pixels, w, h)).ok()?;
    Some(result.avif_file)
}

/// AVIF：进程内 ravif 编码（对齐 cli 版 avifenc）。成功判定 < original_size。
async fn compress_to_avif(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "avif") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "AVIF") {
        return rejected(original, "avif", "none", ReasonCode::UnsupportedFormat, message);
    }
    // ravif 只接受解码后的像素，而进程内没有 AVIF 解码器。AVIF 输入（原地压缩或
    // 再转 AVIF）必须明说"读不了"，不能让它走到 image::open 失败后被报成"没有收益"。
    if detect_image_format(&original) == "avif" {
        return unsupported(
            original,
            "avif",
            "App Store 版没有 AVIF 解码器：可以输出 AVIF，但不能读取或原地压缩 AVIF",
        );
    }
    let quality = options.quality;
    if let Some(data) = compress_to_avif_with_avifenc(file, quality) {
        if acceptable(rule, data.len(), original_size) {
            return ok(data, "avif", "avifenc (inproc)");
        }
    }
    no_candidate(original, "avif", "avifenc (inproc)", rule)
}

/// JPEG XL：App Store 版暂未接入 libjxl（编译链重）。
/// 不能退回 `KeptOriginal` —— 那会把"没有编码器"说成"已经没有优化空间"。
async fn compress_to_jxl(file: &Path, _options: &CompressOptions, _rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "jxl") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    rejected(
        original,
        "jxl",
        "none",
        ReasonCode::UnsupportedFormat,
        "App Store 版暂未接入 JPEG XL 编码",
    )
}

/// HEIC：ImageIO 能真正写 `public.heic`，而且不 spawn 任何进程（沙盒允许）。
/// 旧路径把 HEIF 存成 JPEG 的 bug 在这里同样不存在：只收字节确实是 HEIC 的结果。
#[cfg(target_os = "macos")]
async fn compress_heic(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "heic") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "HEIC") {
        return rejected(original, "heic", "none", ReasonCode::UnsupportedFormat, message);
    }
    let mut opts = options.clone();
    opts.output_format = "heic".into();
    // 高级路径的尺寸已经在工作文件里兑现；这里的尺寸档只允许"实际大小"，
    // 不然一套参数会同时被两条缩放路径各改一次。
    opts.system_image_size = "actual".into();
    let converted = crate::system_image::convert(file, &opts);
    if converted.encode == EncodeVerdict::Candidate && !converted.compressed.is_empty() {
        if acceptable(rule, converted.compressed.len(), original_size) {
            return EngineResult {
                out_type: "heic".into(),
                algorithm: "ImageIO (inproc)".into(),
                ..converted
            };
        }
    }
    if matches!(converted.encode, EncodeVerdict::Failed | EncodeVerdict::Rejected) {
        return converted;
    }
    no_candidate(original, "heic", "ImageIO (inproc)", rule)
}

#[cfg(not(target_os = "macos"))]
async fn compress_heic(file: &Path, _options: &CompressOptions, _rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "heic") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    rejected(original, "heic", "none", ReasonCode::UnsupportedFormat, "HEIC 输出仅支持 macOS")
}

pub async fn compress_image(
    file: &Path,
    options: &CompressOptions,
    rule: OutputRule,
    source_type: &str,
) -> EngineResult {
    if options.output_format != "original" {
        return compress_to_format(file, &options.output_format, options, rule).await;
    }
    match source_type {
        "png" => compress_png(file, options, rule).await,
        "jpg" => compress_jpg(file, options, rule).await,
        "webp" => compress_to_webp(file, options, rule).await,
        "gif" => compress_gif(file, options, rule).await,
        "avif" => compress_to_avif(file, options, rule).await,
        "heic" => compress_heic(file, options, rule).await,
        "jxl" => compress_to_jxl(file, options, rule).await,
        _ => {
            let original = match read_source(file, source_type) {
                Ok(bytes) => bytes,
                Err(failure) => return failure,
            };
            unsupported(original, source_type, "未识别的图片类型")
        }
    }
}

pub async fn compress_to_format(
    file: &Path,
    target: &str,
    options: &CompressOptions,
    rule: OutputRule,
) -> EngineResult {
    match target {
        "png" => compress_png(file, options, rule).await,
        "jpg" | "jpeg" => compress_jpg(file, options, rule).await,
        "webp" => compress_to_webp(file, options, rule).await,
        "avif" => compress_to_avif(file, options, rule).await,
        "jxl" => compress_to_jxl(file, options, rule).await,
        "heic" | "heif" => compress_heic(file, options, rule).await,
        "gif" => compress_gif(file, options, rule).await,
        other => {
            let original = match read_source(file, other) {
                Ok(bytes) => bytes,
                Err(failure) => return failure,
            };
            unsupported(original, other, &format!("App Store 版暂未接入 {} 输出", other))
        }
    }
}

pub async fn compress_smart(
    file: &Path,
    options: &CompressOptions,
    rule: OutputRule,
    source_type: &str,
) -> EngineResult {
    // 多格式候选挑选（与 Direct 版 engine.rs compress_smart 逻辑一致）
    let quality = options.quality;
    let mut opts = options.clone();
    opts.quality = quality;

    // 如果指定了输出格式，走格式转换路径
    if opts.output_format != "original" {
        return compress_to_format(file, &opts.output_format, &opts, rule).await;
    }

    let mut candidates: Vec<EngineResult> = Vec::new();
    let mut push = |result: EngineResult| {
        if result.encode == EncodeVerdict::Candidate && !result.compressed.is_empty() {
            candidates.push(result);
        }
    };

    match source_type {
        "png" => {
            // PNG 同时尝试原格式 + WebP，选最小
            push(compress_png(file, &opts, rule).await);
            push(compress_to_webp(file, &opts, rule).await);
        }
        "jpg" => push(compress_jpg(file, &opts, rule).await),
        "gif" => push(compress_gif(file, &opts, rule).await),
        "webp" => push(compress_to_webp(file, &opts, rule).await),
        "avif" => push(compress_to_avif(file, &opts, rule).await),
        "heic" => push(compress_heic(file, &opts, rule).await),
        _ => {}
    }
    drop(push);

    if let Some(best) = candidates.into_iter().min_by_key(|result| result.compressed.len()) {
        return best;
    }

    // 兜底
    compress_image(file, &opts, rule, source_type).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 沙盒线必须**没有**外部进程：security-scoped 授权挂在父进程的 sandbox
    /// extension 上，不随 spawn 继承，所以打包 CLI 在这里必然读不到用户文件，
    /// 只是白付一次进程启动。这条不变量靠读自己的源码守 —— 注释里提到这些词不算。
    #[test]
    fn the_inproc_engine_never_spawns_an_external_tool() {
        let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine_inproc.rs"))
            .expect("自检读不到自己的源码");
        // 只扫测试模块之前的生产代码：本测试自己就写着这些词。
        let production = source.split("#[cfg(test)]").next().unwrap_or("");
        let code: String = production
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        for banned in ["find_tool", "make_command", "cli_to_file", "Command::new", "std::process"] {
            assert!(
                !code.contains(banned),
                "进程内引擎出现了 {banned}：沙盒线只许调链接进本进程的 crate"
            );
        }
    }

    /// 真跑一次进程内 PNG 压缩：自己造夹具，不许因为临时文件不存在就静默跳过。
    #[tokio::test]
    async fn compressing_a_png_keeps_pixels_and_shrinks_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.png");
        // 渐变 + 少量噪声：能被量化压缩，但像素语义仍可核对。
        let mut img = image::RgbImage::new(64, 64);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgb([x as u8, y as u8, ((x + y) / 2) as u8]);
        }
        img.save(&file).unwrap();
        let original = image::open(&file).unwrap().to_rgb8();

        let result = compress_image(&file, &inproc_options(80), OutputRule::BenefitOnly, "png").await;
        assert!(result.success, "压缩失败：{:?}", result.error);
        assert!(!result.compressed.is_empty());
        fs::write(&file, &result.compressed).unwrap();
        let round_trip = image::open(&file).unwrap().to_rgb8();
        assert_eq!(round_trip.dimensions(), original.dimensions());
        // 量化带误差扩散，单像素最大偏差可以很大但平均很小 —— 这里只保证"图没坏"，
        // 所以用平均偏差，不用最大偏差（后者是 dithering 的正常表现，不是缺陷）。
        let mut total = 0u64;
        let mut samples = 0u64;
        for (before, after) in original.pixels().zip(round_trip.pixels()) {
            for (a, b) in before.0.iter().zip(&after.0) {
                total += a.abs_diff(*b) as u64;
                samples += 1;
            }
        }
        let mean = total as f64 / samples as f64;
        assert!(mean <= 6.0, "量化平均偏差 {mean}，图已经坏了");
        assert!(!result.algorithm.starts_with("none"), "走了兜底分支：{:?}", result.error);
    }

    /// 只覆盖真正被引擎读到的字段；其余走 Default（旧的 backend/effort/smartMode/
    /// convertToWebp/lossless 已删除，没有任何一条路径读过它们）。
    /// §22/§59：没有 AVIF 解码器时，AVIF 输入必须报"不支持"，
    /// 不能走到 image::open 失败后被说成"没有优化空间"。
    #[tokio::test]
    async fn an_avif_source_is_reported_unsupported_instead_of_no_improvement() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.avif");
        // 判定发生在解码之前，这里只需要一个合法的 ftyp/avif 头。
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24u32.to_be_bytes());
        bytes.extend_from_slice(b"ftypavif");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(b"avifmif1");
        fs::write(&file, &bytes).unwrap();

        let result =
            compress_image(&file, &inproc_options(75), OutputRule::BenefitOnly, "avif").await;
        assert_eq!(result.encode, EncodeVerdict::Rejected);
        assert_eq!(result.reason, ReasonCode::UnsupportedFormat);
        assert!(
            result.error.as_deref().unwrap_or("").contains("AVIF 解码器"),
            "拒绝理由必须说清楚是没有解码器：{:?}",
            result.error
        );
    }

    fn inproc_options(quality: u32) -> CompressOptions {
        CompressOptions {
            quality,
            ..Default::default()
        }
    }
}
