import Foundation

/// §22：Swift 轨的图片能力表。
///
/// 与 Rust `image_capabilities.rs` 说同一批事实：哪些格式能读、能写、能原地压缩、
/// 能缩放、能保留动画与元数据，以及这台机器此刻是否真的具备对应编码器。
/// 界面从这里取选项，不再在 Picker 里手写第二份格式列表。
enum ImageCapabilities {
    struct Format: Identifiable {
        let id: String
        let label: String
        let extensions: [String]
        let readable: Bool
        let writable: Bool
        let compressible: Bool
        let convertible: Bool
        let resizeSupported: Bool
        let animatedSupported: Bool
        let metadataSupported: Bool
        let systemConvertible: Bool
        let available: Bool
        let reason: String?

        /// 界面枚举；没有输出编码器的格式（BMP/TIFF/JXL）返回 nil。
        var outputFormat: OutputFormat? {
            switch id {
            case "jpg": return .jpg
            case "png": return .png
            case "webp": return .webp
            case "avif": return .avif
            case "heif", "heic": return .heic
            default: return nil
            }
        }
    }

    struct Matrix {
        let backend: String
        let formats: [Format]

        /// 高级路径可选的输出目标：`convertible && available`。
        var outputTargets: [Format] {
            formats.filter { $0.convertible && $0.available }
        }

        /// 系统转换（ImageIO）可写的容器。
        var systemOutputTargets: [Format] {
            formats.filter { $0.systemConvertible && $0.available }
        }

        /// 动画输入可以无损转入的目标；其余目标遇到动图必须拒绝。
        var animationPreservingTargets: [String] {
            formats.filter { $0.animatedSupported && $0.available }.map(\.id)
        }

        func format(_ id: String) -> Format? {
            formats.first { $0.id == id }
        }
    }

    private static let missingTool = "缺少对应的命令行编码器，这台机器上无法写出该格式"

    static func matrix() -> Matrix {
        let webpReady = CLIRunner.toolURL(for: "cwebp") != nil
        let avifReady = CLIRunner.toolURL(for: "avifenc") != nil
        let gifReady = CLIRunner.toolURL(for: "gifsicle") != nil

        let formats: [Format] = [
            Format(
                id: "jpg", label: "JPEG", extensions: ["jpg", "jpeg"],
                readable: true, writable: true, compressible: true, convertible: true,
                resizeSupported: true, animatedSupported: false, metadataSupported: false,
                systemConvertible: true, available: true, reason: nil
            ),
            Format(
                id: "png", label: "PNG", extensions: ["png", "apng"],
                readable: true, writable: true, compressible: true, convertible: true,
                resizeSupported: true, animatedSupported: false, metadataSupported: false,
                systemConvertible: true, available: true, reason: nil
            ),
            Format(
                id: "webp", label: "WebP", extensions: ["webp"],
                readable: true, writable: true, compressible: true, convertible: true,
                resizeSupported: true, animatedSupported: false, metadataSupported: false,
                systemConvertible: false, available: webpReady,
                reason: webpReady ? nil : missingTool
            ),
            Format(
                id: "avif", label: "AVIF", extensions: ["avif"],
                // 没有 AVIF 解码器：能写，不能读，也不能原地压缩。
                readable: false, writable: true, compressible: false, convertible: true,
                resizeSupported: false, animatedSupported: false, metadataSupported: false,
                systemConvertible: false, available: avifReady,
                reason: avifReady ? nil : missingTool
            ),
            Format(
                id: "heif", label: "HEIF", extensions: ["heic", "heif"],
                readable: true, writable: true, compressible: true, convertible: true,
                resizeSupported: false, animatedSupported: false, metadataSupported: true,
                systemConvertible: true, available: true, reason: nil
            ),
            Format(
                id: "gif", label: "GIF", extensions: ["gif"],
                // GIF 写出必须靠 gifsicle；ImageIO 重编码会丢帧，不能作为替代。
                readable: true, writable: true, compressible: true, convertible: false,
                resizeSupported: false, animatedSupported: true, metadataSupported: false,
                systemConvertible: false, available: gifReady,
                reason: gifReady ? nil : missingTool
            ),
            Format(
                id: "jxl", label: "JPEG XL", extensions: ["jxl"],
                readable: false, writable: false, compressible: false, convertible: false,
                resizeSupported: false, animatedSupported: false, metadataSupported: false,
                systemConvertible: false, available: false,
                reason: "Swift 版没有接入 JPEG XL 编解码器"
            ),
            Format(
                id: "bmp", label: "BMP", extensions: ["bmp"],
                readable: true, writable: false, compressible: false, convertible: false,
                resizeSupported: true, animatedSupported: false, metadataSupported: false,
                systemConvertible: false, available: true, reason: nil
            ),
            Format(
                id: "tiff", label: "TIFF", extensions: ["tif", "tiff"],
                readable: false, writable: false, compressible: false, convertible: false,
                resizeSupported: false, animatedSupported: false, metadataSupported: false,
                systemConvertible: false, available: false,
                reason: "Swift 版没有接入 TIFF 解码器"
            ),
        ]

        return Matrix(backend: "swift", formats: formats)
    }
}

extension OutputFormat {
    /// 高级路径下拉可选的格式（来自能力表，不在这里重写格式字面量）。
    static var advancedTargets: [OutputFormat] {
        ImageCapabilities.matrix().outputTargets.compactMap(\.outputFormat)
    }

    /// 系统转换下拉可选的格式。
    static var systemTargets: [OutputFormat] {
        ImageCapabilities.matrix().systemOutputTargets.compactMap(\.outputFormat)
    }
}
