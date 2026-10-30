//! §5.1 视频输入能力分类：位深 / Alpha / 浮点 / HDR 按白名单与传递函数判定。
//!
//! 为什么不继续用 `pixel_format.contains("10")`：子串匹配认不出 32bit 浮点
//! （`grayf32le`/`gbrpf32le` 不含数字字样），认不出 packed alpha（`bgra`/`argb`
//! 不含 "rgba"），还会把 `yuv410p9le`（9-bit）当成"含 10 判 10"。漏掉的格式会被
//! 管线里硬编码的 `-pix_fmt yuv420p` 静默降级 —— 丢位深、丢 alpha、丢精度，
//! 违反"先拒绝或说明，绝不无声转换"。
//!
//! 初期策略是保守白名单：只有验证过的 8-bit SDR 格式允许进管线；高位深、
//! Alpha、浮点、HDR 一律拒绝并说明理由。后续支持必须单独建立保真编码路径。

/// 一次输入分类的完整结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFormatCapability {
    /// 每分量位深（白名单内精确已知；表外格式给 0 并拒绝）。
    pub bit_depth: u32,
    pub has_alpha: bool,
    pub is_float: bool,
    /// Ok = 可以进 8-bit SDR 管线；Err = 拒绝原因（照实说明，绝不静默转换）。
    pub support_level: Result<(), String>,
}

/// 经测试的 8-bit SDR 像素格式白名单：(格式, 位深)。
/// 输出管线承诺的是 8-bit yuv420p：422/444 → 420 是 scale 滤镜显式声明的
/// 色度重采样，不属于静默降级；位深与 alpha 则是承诺的一部分。
const VERIFIED_SDR_8BIT: &[(&str, u32)] = &[
    ("yuv420p", 8),
    ("yuvj420p", 8),
    ("yuv422p", 8),
    ("yuvj422p", 8),
    ("yuv444p", 8),
    ("yuvj444p", 8),
    ("nv12", 8),
    ("nv21", 8),
    ("gray", 8),
    ("gray8", 8),
];

/// 已知的高位深格式 → 位深。用于给出**准确**的拒绝理由，而不是猜。
const KNOWN_HIGH_BIT_DEPTH: &[(&str, u32)] = &[
    ("yuv420p9le", 9), ("yuv422p9le", 9), ("yuv444p9le", 9), ("yuv410p9le", 9), ("yuv411p9le", 9),
    ("yuv420p10le", 10), ("yuv422p10le", 10), ("yuv444p10le", 10), ("yuv410p10le", 10),
    ("yuv420p12le", 12), ("yuv422p12le", 12), ("yuv444p12le", 12),
    ("p010le", 10), ("p016le", 16),
    ("gray9le", 9), ("gray10le", 10), ("gray12le", 12), ("gray14le", 14), ("gray16le", 16),
    ("gbrp9le", 9), ("gbrp10le", 10), ("gbrp12le", 12), ("gbrp14le", 14), ("gbrp16le", 16),
    ("rgb48le", 48), ("rgb48be", 48), ("bgr48le", 48), ("bgr48be", 48),
    ("yuv420p16le", 16), ("yuv422p16le", 16), ("yuv444p16le", 16),
];

/// 已知带 Alpha 的格式。塞进 yuv420p 就是把透明通道整个丢掉。
const KNOWN_ALPHA: &[&str] = &[
    "yuva420p", "yuva422p", "yuva444p",
    "yuva420p10le", "yuva422p10le", "yuva444p10le",
    "yuva420p12le", "yuva422p12le", "yuva444p12le",
    "yuva420p16le", "yuva422p16le", "yuva444p16le",
    "rgba", "bgra", "argb", "abgr", "rgb0", "bgr0", "0rgb", "0bgr",
    "gbrap", "gbrap10le", "gbrap12le", "gbrap16le",
    "bgra64le", "bgra64be", "rgba64le", "rgba64be",
];

/// 已知的浮点格式。浮点进整数管线是精度静默截断。
const KNOWN_FLOAT: &[&str] = &[
    "grayf16le", "grayf32le", "grayf32be",
    "gbrpf16le", "gbrpf32le", "gbrpf32be",
    "yuv444pf16le", "yuv444pf32le", "yuv444pf32be",
    "yuv422pf16le", "yuv422pf32le", "yuv420pf16le", "yuv420pf32le",
];

/// 传递函数判定 HDR（§5.2）。返回 Some(种类) = 这是 HDR 输入。
///
/// 规则：PQ、HLG、Dolby Vision、HDR10+ 是 HDR；**BT.2020 原色只表示色彩
/// 原色，不能单独认定 HDR** —— BT.2020 SDR 有自己的色彩空间要单独处理。
pub fn hdr_by_transfer(color_transfer: &str, hdr_kind: &str) -> Option<&'static str> {
    let transfer = color_transfer.to_ascii_lowercase();
    match hdr_kind {
        "dolbyVision" => return Some("dolbyVision"),
        "hdr10Plus" => return Some("hdr10Plus"),
        // probe 从 mastering-display / side data 聚合出的通用 HDR 信号同样算数。
        "hdr" => return Some("hdr"),
        _ => {}
    }
    if transfer.contains("smpte2084") || transfer.contains("pq") {
        return Some("hdr10");
    }
    if transfer.contains("arib-std-b67") || transfer.contains("hlg") {
        return Some("hlg");
    }
    None
}

/// 输入分类入口。`hdr_kind` 是 probe 从 side data / 标签聚合出的种类，
/// `color_transfer` / `color_primaries` 是视频流的色彩字段。
pub fn classify(
    pixel_format: &str,
    color_transfer: &str,
    color_primaries: &str,
    hdr_kind: &str,
    field_order: &str,
) -> VideoFormatCapability {
    // ① HDR 按传递函数判定；BT.2020 原色是"色彩空间未验证"，单独拒绝并说明。
    if let Some(kind) = hdr_by_transfer(color_transfer, hdr_kind) {
        return VideoFormatCapability {
            bit_depth: 0,
            has_alpha: false,
            is_float: false,
            support_level: Err(format!(
                "{kind} input skipped: verified HDR / dynamic metadata preservation is unavailable"
            )),
        };
    }
    let primaries = color_primaries.to_ascii_lowercase();
    if primaries.contains("bt2020") {
        return VideoFormatCapability {
            bit_depth: 0,
            has_alpha: false,
            is_float: false,
            support_level: Err(
                "BT.2020 SDR input requires a verified color-management path; \
                 converting it as plain BT.709 would silently repaint the colors"
                    .into(),
            ),
        };
    }
    // ② 像素格式按白名单精确判定。
    let format = pixel_format.to_ascii_lowercase();
    if let Some((_, depth)) = VERIFIED_SDR_8BIT.iter().find(|(name, _)| *name == format) {
        let mut capability = VideoFormatCapability {
            bit_depth: *depth,
            has_alpha: false,
            is_float: false,
            support_level: Ok(()),
        };
        if !["", "unknown", "progressive"].contains(&field_order) {
            capability.support_level =
                Err("Interlaced input needs an explicitly validated deinterlace plan".into());
        }
        return capability;
    }
    let reason = if KNOWN_HIGH_BIT_DEPTH.iter().any(|(name, _)| *name == format) {
        "High bit-depth input requires a verified preservation path".to_string()
    } else if KNOWN_ALPHA.iter().any(|name| *name == format) {
        "Alpha video preservation is unavailable".to_string()
    } else if KNOWN_FLOAT.iter().any(|name| *name == format) {
        "Floating-point pixel formats are unverified".to_string()
    } else {
        format!("Unknown pixel format {pixel_format} is not on the verified whitelist")
    };
    VideoFormatCapability { bit_depth: 0, has_alpha: false, is_float: false, support_level: Err(reason) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_ok(format: &str) -> VideoFormatCapability {
        classify(format, "", "", "sdr", "progressive")
    }

    #[test]
    fn verified_8bit_sdr_formats_pass() {
        for (format, depth) in VERIFIED_SDR_8BIT {
            let capability = classify_ok(format);
            assert_eq!(capability.bit_depth, *depth, "{format} 位深必须精确标注");
            assert!(capability.support_level.is_ok(), "{format} 应当放行");
            assert!(!capability.has_alpha && !capability.is_float);
        }
    }

    #[test]
    fn high_bit_depth_is_rejected_by_whitelist_not_substring() {
        // 子串匹配抓不住的 9-bit 格式：不含 "10"/"12"/"16"/"f32"。
        let capability = classify_ok("yuv410p9le");
        assert!(capability.support_level.is_err(), "9-bit 必须拒绝");
        for format in ["yuv420p10le", "yuv420p12le", "yuv420p16le", "rgb48le", "gray16le"] {
            assert!(classify_ok(format).support_level.is_err(), "{format} 必须拒绝");
        }
    }

    #[test]
    fn float_and_alpha_are_rejected_even_without_numeric_tells() {
        // grayf32le 不含数字字样，bgra 不含 "rgba" —— 子串法全都漏。
        assert!(classify_ok("grayf32le").support_level.is_err());
        assert!(classify_ok("gbrpf32le").support_level.is_err());
        assert!(classify_ok("bgra").support_level.is_err());
        assert!(classify_ok("yuva420p").support_level.is_err());
        assert!(classify_ok("gbrap").support_level.is_err());
    }

    #[test]
    fn unknown_pixel_format_is_rejected_not_guessed() {
        let capability = classify_ok("zzz-mystery-fmt");
        assert!(capability.support_level.is_err());
    }

    #[test]
    fn hdr_is_decided_by_transfer_function_not_primaries() {
        // PQ / HLG / DOVI / HDR10+ 都是 HDR。
        assert!(classify("yuv420p", "smpte2084", "bt709", "sdr", "progressive")
            .support_level
            .is_err());
        assert!(classify("yuv420p", "arib-std-b67", "bt709", "sdr", "progressive")
            .support_level
            .is_err());
        assert!(classify("yuv420p", "bt709", "bt709", "dolbyVision", "progressive")
            .support_level
            .is_err());
        assert!(classify("yuv420p", "bt709", "bt709", "hdr10Plus", "progressive")
            .support_level
            .is_err());
        // BT.2020 原色 ≠ HDR，但也不能当普通 BT.709 转。
        let bt2020 = classify("yuv420p", "bt709", "bt2020", "sdr", "progressive");
        assert!(bt2020.support_level.is_err());
        assert!(bt2020
            .support_level
            .unwrap_err()
            .contains("color-management"), "拒绝理由必须说明是色彩空间问题");
    }

    #[test]
    fn interlaced_input_is_rejected_at_classification() {
        let capability = classify("yuv420p", "", "", "sdr", "tt");
        assert!(capability.support_level.is_err());
        assert!(classify("yuv420p", "", "", "sdr", "unknown").support_level.is_ok());
    }
}
