import Foundation

enum ProcessingMode: String, CaseIterable {
    case system = "system"
    case advanced = "advanced"

    var label: String {
        switch self {
        case .system: return "系统转换"
        case .advanced: return "高级压缩"
        }
    }
}

enum OutputFormat: String, CaseIterable {
    case original = "original"
    case jpg = "jpg"
    case png = "png"
    case heic = "heic"
    case webp = "webp"
    case avif = "avif"

    var label: String {
        switch self {
        case .original: return "原格式"
        case .jpg: return "JPEG"
        case .png: return "PNG"
        case .heic: return "HEIF"
        case .webp: return "WebP"
        case .avif: return "AVIF"
        }
    }
}

enum OutputMode: String, CaseIterable {
    case replace = "replace"
    case suffix = "suffix"
    case folder = "folder"

    var label: String {
        switch self {
        case .replace: return "覆盖原文件"
        case .suffix: return "添加自定义后缀"
        case .folder: return "输出到指定文件夹"
        }
    }
}

enum SystemImageSize: String, CaseIterable {
    case actual = "actual"
    case large = "large"
    case medium = "medium"
    case small = "small"

    var label: String {
        switch self {
        case .actual: return "实际大小"
        case .large: return "大（最长边 1280 px）"
        case .medium: return "中（最长边 640 px）"
        case .small: return "小（最长边 320 px）"
        }
    }

    /// 与 Tauri 一致的算法后缀标签
    var shortLabel: String {
        switch self {
        case .actual: return "实际大小"
        case .large: return "大"
        case .medium: return "中"
        case .small: return "小"
        }
    }
}

struct CompressOptions {
    var processingMode: ProcessingMode = .advanced
    var systemImageSize: SystemImageSize = .actual
    var preserveMetadata: Bool = true
    /// 均衡档（默认意图）的质量；与 Rust 线 `quality: 75` 一致。
    var quality: Int = 75
    var outputFormat: OutputFormat = .original
    var outputMode: OutputMode = .replace
    var outputSuffix: String = "_compressed"
    var outputDir: String?

    /// 最长边上限（px，只缩不放大）；nil = 不限制。与 Rust 线 `max_dimension_px` 对齐。
    var maxDimensionPx: Int?
    /// 字节预算：触发有界求解器（先降质量 80/65/50，再上尺寸上限 2560/1920/1280，最多 6 次编码）。
    /// nil = 不设预算。与 Rust 线 `max_output_bytes` 对齐。
    var maxOutputBytes: Int64?
}
