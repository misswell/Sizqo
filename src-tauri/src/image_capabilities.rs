//! §22 图片能力矩阵：图片能做哪些事，只有这一张表说了算。
//!
//! 前端不再自己维护「允许的输出格式」列表 —— 那份列表和产物线的实际能力一旦分叉，
//! 下拉框里就会出现点了不生效的选项（Direct 没有 cwebp、App Store 没有 libjxl）。
//!
//! 字段分两层：
//! - `readable / writable / compressible / convertible / resize_supported ...`
//!   是**实现**层面的静态事实；
//! - `available / reason` 是**这一刻这台机器**能否真的用（CLI 是否随包、平台是否有 ImageIO）。
//!
//! `readable` 的含义是「引擎里存在一条能把它解码的路径」，不等于「可以原地压缩」：
//! AVIF 在两条线上都能写、却没有解码器，所以 readable=false、compressible=false。

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageFormatCapability {
    pub id: &'static str,
    pub label: &'static str,
    pub extensions: Vec<&'static str>,
    pub readable: bool,
    pub writable: bool,
    pub compressible: bool,
    pub convertible: bool,
    pub resize_supported: bool,
    pub animated_supported: bool,
    pub metadata_supported: bool,
    pub system_convertible: bool,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageCapabilityMatrix {
    /// `cli`（Direct，外部编码器）或 `inproc`（App Store，链接进进程的 Rust 库）。
    pub backend: &'static str,
    pub formats: Vec<ImageFormatCapability>,
    /// 高级路径可选的输出目标：`convertible && available`。
    pub output_targets: Vec<&'static str>,
    /// 系统转换（ImageIO）可写的容器；非 macOS 为空。
    pub system_output_targets: Vec<&'static str>,
    /// 动画输入可以无损转入的目标。不在这张表里的目标遇到动图必须拒绝，不许只留第一帧。
    pub animation_preserving_targets: Vec<&'static str>,
}

#[allow(clippy::too_many_arguments)]
fn format(
    id: &'static str,
    label: &'static str,
    extensions: &[&'static str],
    readable: bool,
    writable: bool,
    compressible: bool,
    convertible: bool,
    resize_supported: bool,
    animated_supported: bool,
    metadata_supported: bool,
    system_convertible: bool,
    available: bool,
    reason: Option<String>,
) -> ImageFormatCapability {
    ImageFormatCapability {
        id,
        label,
        extensions: extensions.to_vec(),
        readable,
        writable,
        compressible,
        convertible,
        resize_supported,
        animated_supported,
        metadata_supported,
        system_convertible,
        available,
        reason,
    }
}

fn backend_id() -> &'static str {
    // 两个 feature 同时打开（回归构建）时，encode_source 实际走 inproc，矩阵必须跟着走。
    if cfg!(feature = "inproc-backends") {
        "inproc"
    } else {
        "cli"
    }
}

pub fn capabilities() -> ImageCapabilityMatrix {
    let inproc = cfg!(feature = "inproc-backends");
    let macos = cfg!(target_os = "macos");

    let (webp_ready, gif_ready, avif_ready, jxl_ready) = if inproc {
        // App Store 线：cwebp/gifsicle/ravif 都链接在进程里；libjxl 没有链接。
        (true, true, true, false)
    } else {
        (
            crate::engine::tool_available("cwebp"),
            crate::engine::tool_available("gifsicle"),
            crate::engine::tool_available("avifenc"),
            crate::engine::tool_available("cjxl"),
        )
    };
    let heif_ready = macos;

    let missing = |tool: &str| Some(format!("没有找到 {tool}，这台机器上无法写出该格式"));
    let formats = vec![
        format(
            "jpg", "JPEG", &["jpg", "jpeg"],
            true, true, true, true, true, false, false, macos,
            true, None,
        ),
        format(
            "png", "PNG", &["png", "apng"],
            true, true, true, true, true, false, false, macos,
            true, None,
        ),
        format(
            "webp", "WebP", &["webp"],
            true, true, true, true, true, false, false, false,
            webp_ready,
            if webp_ready { None } else { missing("cwebp") },
        ),
        format(
            "avif", "AVIF", &["avif"],
            // 两条产物线都没有 AVIF 解码器：可以写，不能读，也不能原地压缩。
            false, true, false, true, false, false, false, false,
            avif_ready,
            if avif_ready { None } else { missing("avifenc") },
        ),
        format(
            "heif", "HEIF", &["heic", "heif"],
            // HEIC 的读写都靠 ImageIO/sips；高级路径的缩放解码器读不动它。
            true, true, true, true, false, false, true, macos,
            heif_ready,
            if heif_ready { None } else { Some("HEIC 输出需要 macOS 的 ImageIO/sips".into()) },
        ),
        format(
            "gif", "GIF", &["gif"],
            // Direct 的 GIF 写出必须靠 gifsicle，进程内重编码会丢帧，因此没有可用的替代路径。
            true, true, true, true, false, true, false, false,
            gif_ready,
            if gif_ready { None } else { missing("gifsicle") },
        ),
        format(
            "jxl", "JPEG XL", &["jxl"],
            false, !inproc, false, !inproc, false, false, false, false,
            jxl_ready,
            if jxl_ready {
                None
            } else if inproc {
                Some("App Store 版没有链接 libjxl，无法读写 JPEG XL".into())
            } else {
                missing("cjxl")
            },
        ),
        format(
            "bmp", "BMP", &["bmp"],
            // BMP 只作为输入：能读、能缩、能转出，但没有 BMP 输出编码器。
            true, false, false, false, true, false, false, false,
            true, None,
        ),
        format(
            "tiff", "TIFF", &["tif", "tiff"],
            false, false, false, false, false, false, false, false,
            false,
            Some("引擎没有链接 TIFF 解码器，无法读取 TIFF".into()),
        ),
    ];

    let output_targets = formats
        .iter()
        .filter(|format| format.convertible && format.available)
        .map(|format| format.id)
        .collect();
    let system_output_targets = if macos { vec!["jpg", "png", "heif"] } else { Vec::new() };
    let animation_preserving_targets = formats
        .iter()
        .filter(|format| format.animated_supported && format.available)
        .map(|format| format.id)
        .collect();

    ImageCapabilityMatrix {
        backend: backend_id(),
        formats,
        output_targets,
        system_output_targets,
        animation_preserving_targets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_id<'a>(matrix: &'a ImageCapabilityMatrix, id: &str) -> &'a ImageFormatCapability {
        matrix
            .formats
            .iter()
            .find(|format| format.id == id)
            .unwrap_or_else(|| panic!("能力表里没有 {id}"))
    }

    #[test]
    fn every_output_target_is_convertible_and_available() {
        let matrix = capabilities();
        for id in &matrix.output_targets {
            let format = by_id(&matrix, id);
            assert!(format.convertible, "{id} 出现在输出目标里却不可转换");
            assert!(format.available, "{id} 出现在输出目标里却不可用");
            assert!(format.writable, "{id} 出现在输出目标里却没有编码器");
            assert!(format.reason.is_none(), "{id} 可用却带着不可用理由");
        }
        assert!(!matrix.output_targets.is_empty(), "至少要有一种可写格式");
    }

    #[test]
    fn a_format_without_a_decoder_is_never_claimed_compressible() {
        let matrix = capabilities();
        for format in &matrix.formats {
            if !format.readable {
                assert!(
                    !format.compressible,
                    "{} 没有解码器，却被标成可原地压缩",
                    format.id
                );
            }
        }
    }

    #[test]
    fn only_gif_promises_to_carry_animation() {
        let matrix = capabilities();
        for format in &matrix.formats {
            if format.id != "gif" {
                assert!(
                    !format.animated_supported,
                    "{} 不该承诺保留动画：单帧编码器会吃帧",
                    format.id
                );
            }
        }
        if by_id(&matrix, "gif").available {
            assert_eq!(matrix.animation_preserving_targets, vec!["gif"]);
        }
    }

    /// 「这一刻用不了」必须自己说清楚：有理由、且不混进输出目标。
    /// 这一条不分平台，也不分 feature —— Windows 上缺 cwebp、Linux 上缺 HEIC、
    /// App Store 里没链接 libjxl，走的都是同一句实话。
    #[test]
    fn every_unavailable_format_explains_itself_and_stays_out_of_the_targets() {
        let matrix = capabilities();
        let mut reported_unavailable = 0;
        for format in &matrix.formats {
            if format.available {
                continue;
            }
            reported_unavailable += 1;
            assert!(format.reason.is_some(), "{} 不可用却没给用户理由", format.id);
            assert!(
                !matrix.output_targets.contains(&format.id),
                "{} 不可用却出现在输出目标里",
                format.id
            );
        }
        assert!(
            reported_unavailable > 0,
            "这台机器上没有任何不可用格式，这条门是空跑的"
        );
    }

    #[test]
    fn system_targets_are_exactly_the_imageio_containers() {
        let matrix = capabilities();
        if cfg!(target_os = "macos") {
            assert_eq!(matrix.system_output_targets, vec!["jpg", "png", "heif"]);
            for id in &matrix.system_output_targets {
                assert!(by_id(&matrix, id).system_convertible, "{id} 不是系统转换目标");
            }
        } else {
            assert!(matrix.system_output_targets.is_empty());
        }
    }

    #[cfg(feature = "inproc-backends")]
    #[test]
    fn inproc_never_pretends_to_write_jxl() {
        let matrix = capabilities();
        assert_eq!(matrix.backend, "inproc");
        let jxl = by_id(&matrix, "jxl");
        assert!(!jxl.available && !jxl.convertible && !jxl.writable);
        assert!(jxl.reason.is_some(), "不可用必须给用户一个理由");
        assert!(!matrix.output_targets.contains(&"jxl"));
    }

    #[cfg(not(feature = "inproc-backends"))]
    #[test]
    fn direct_reports_the_tools_it_actually_bundles() {
        let matrix = capabilities();
        assert_eq!(matrix.backend, "cli");
        // jpg/png 有进程内兜底编码器：任何平台都真写得出来，这一条不许按平台放宽。
        for id in ["jpg", "png"] {
            assert!(matrix.output_targets.contains(&id), "Direct 缺少 {id} 输出");
        }
        if cfg!(target_os = "macos") {
            assert!(matrix.output_targets.contains(&"heif"));
        }
        // webp 这类只有外部 CLI 一条路：矩阵必须跟着"这台机器上到底有没有这个编码器"走，
        // 凭空许诺就等于下拉框里出现一个点了不生效的目标。
        let webp = by_id(&matrix, "webp");
        assert_eq!(
            webp.available,
            crate::engine::tool_available("cwebp"),
            "webp 的可用性必须等于 cwebp 的在场与否"
        );
        if !webp.available {
            assert!(
                !matrix.output_targets.contains(&"webp"),
                "编码器不在场，webp 不许进输出目标"
            );
            let reason = webp.reason.as_deref().expect("不可用必须给用户一个理由");
            assert!(reason.contains("cwebp"), "理由没点名缺哪个编码器：{reason}");
        }
        // Windows Direct 的随包事实：resources/bin 里全是 macOS 的 Mach-O 二进制，
        // 一个 .exe 都没有 —— 那边此刻真的写不出 webp。将来真带上 cwebp.exe 时，
        // 该改的是打包，不是这条断言（它会自己翻成"应当可用"）。
        if cfg!(target_os = "windows") {
            assert!(!webp.available, "Windows Direct 没随包 cwebp.exe，矩阵不许声称可用");
        }
    }
}
