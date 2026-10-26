import Foundation
import ImageIO
import CoreGraphics
import UniformTypeIdentifiers

enum SystemImageConverter {
    /// 源文件真实 UTI → 输出标签。认不出就返回 nil：调用方必须拒绝，不许换格式。
    private static func label(forSourceUTI uti: String) -> String? {
        switch uti {
        case "public.jpeg": return "jpg"
        case "public.png": return "png"
        case "public.heic", "public.heif": return "heic"
        case "com.compuserve.gif": return "gif"
        case "public.tiff": return "tiff"
        case "com.microsoft.bmp": return "bmp"
        case "org.webmproject.webp": return "webp"
        default: return nil
        }
    }

    static func convert(
        file: String,
        options: CompressOptions
    ) -> EngineResult {
        guard let original = FileManager.default.contents(atPath: file) else {
            return EngineResult(
                success: false, compressed: Data(), outType: options.outputFormat.rawValue,
                algorithm: "macOS ImageIO", error: "macOS 无法读取此图像",
                verdict: .rejected, reason: .sourceReadFailed
            )
        }
        guard let source = CGImageSourceCreateWithData(original as CFData, nil) else {
            return EngineResult(
                success: false, compressed: original, outType: options.outputFormat.rawValue,
                algorithm: "macOS ImageIO", error: "macOS 无法读取此图像",
                verdict: .rejected, reason: .decodeFailed
            )
        }

        // §23 多帧输入明确拒绝：这条路径只取第一帧，静默丢帧就是毁数据。
        let frameCount = CGImageSourceGetCount(source)
        if frameCount > 1 {
            return EngineResult(
                success: false, compressed: original, outType: options.outputFormat.rawValue,
                algorithm: "macOS ImageIO",
                error: "系统转换不支持动图（\(frameCount) 帧）：只会保留第一帧。已拒绝这次降级，原文件未改动",
                verdict: .rejected, reason: .unsupportedFormat
            )
        }

        let (uti, outType): (String, String)
        if options.outputFormat == .original {
            // 原格式：目标容器由**源文件真实 UTI** 决定，不按扩展名猜，
            // 也绝不回退成 JPEG —— GIF 保持 GIF；认不出就拒绝，不换格式。
            guard let sourceType = CGImageSourceGetType(source) as String?,
                  let label = label(forSourceUTI: sourceType) else {
                return EngineResult(
                    success: false, compressed: original, outType: "original",
                    algorithm: "macOS ImageIO",
                    error: "无法从字节识别源图格式，系统转换不会替你换一个格式",
                    verdict: .rejected, reason: .unsupportedFormat
                )
            }
            (uti, outType) = (sourceType, label)
        } else {
            switch options.outputFormat {
            case .jpg: (uti, outType) = ("public.jpeg", "jpg")
            case .png: (uti, outType) = ("public.png", "png")
            case .heic: (uti, outType) = ("public.heic", "heic")
            default:
                return EngineResult(
                    success: false, compressed: original, outType: options.outputFormat.rawValue,
                    algorithm: "macOS ImageIO", error: "系统转换仅支持 JPEG、PNG 和 HEIF",
                    verdict: .rejected, reason: .unsupportedFormat
                )
            }
        }

        guard let fullImage = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
            return EngineResult(
                success: false, compressed: original, outType: outType,
                algorithm: "macOS ImageIO", error: "macOS 无法解码此图像",
                verdict: .rejected, reason: .decodeFailed
            )
        }

        let actualMax = max(fullImage.width, fullImage.height)
        let maxPixel: Int
        switch options.systemImageSize {
        case .large: maxPixel = min(1280, actualMax)
        case .medium: maxPixel = min(640, actualMax)
        case .small: maxPixel = min(320, actualMax)
        case .actual: maxPixel = actualMax
        }

        let thumbOpts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixel
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, thumbOpts as CFDictionary) else {
            return EngineResult(
                success: false, compressed: original, outType: outType,
                algorithm: "macOS ImageIO", error: "macOS 无法调整图像尺寸",
                verdict: .failed, reason: .encodeFailed
            )
        }

        let destData = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(
            destData as CFMutableData, uti as CFString, 1, nil
        ) else {
            return EngineResult(
                success: false, compressed: original, outType: outType,
                algorithm: "macOS ImageIO", error: "无法创建系统转换缓冲区",
                verdict: .failed, reason: .encodeFailed
            )
        }

        var props: [CFString: Any] = [:]
        if options.preserveMetadata, let srcProps = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] {
            props = srcProps
            props[kCGImagePropertyOrientation] = 1
        }
        CGImageDestinationAddImage(dest, image, props as CFDictionary)
        guard CGImageDestinationFinalize(dest) else {
            return EngineResult(
                success: false, compressed: original, outType: outType,
                algorithm: "macOS ImageIO", error: "macOS 系统转换失败",
                verdict: .failed, reason: .encodeFailed
            )
        }

        let compressed = destData as Data
        return EngineResult(
            success: true, compressed: compressed, outType: outType,
            algorithm: "macOS ImageIO · \(options.systemImageSize.shortLabel)", error: nil,
            verdict: .candidate
        )
    }
}
