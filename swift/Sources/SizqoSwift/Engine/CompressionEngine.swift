import Foundation
import ImageIO
import CoreGraphics
import UniformTypeIdentifiers

enum CompressionEngine {
    /// 指定体积求解阶梯：与 Rust 线 `budget_ladder` **同一张表** —— 先只降质量
    /// （80/65/50），质量到底才动尺寸（2560/1920/1280 · 65/60/55），最多 6 次编码。
    /// `cap = nil` 的档位不强加尺寸，但用户在自定义里填的上限仍按 min 合并进去。
    static let targetLadder: [(cap: Int?, quality: Int)] = [
        (nil, 80), (nil, 65), (nil, 50), (2560, 65), (1920, 60), (1280, 55),
    ]

    /// 最多几次编码（与 Rust `BUDGET_ATTEMPT_LIMIT` 一致：搜索必须可中断、可预期）。
    static let budgetAttemptLimit = 6

    /// 阶梯档位与用户上限合并（与 Rust `merge_cap` 一致）：都有值时取更小的。
    static func mergeCap(user: Int?, rung: Int?) -> Int? {
        switch (user, rung) {
        case let (u?, r?): return min(u, r)
        case let (u?, nil): return u
        case let (nil, r?): return r
        case (nil, nil): return nil
        }
    }

    static func compress(file: String, options: CompressOptions) -> EngineResult {
        if options.processingMode == .system {
            return SystemImageConverter.convert(file: file, options: options)
        }
        if let budget = options.maxOutputBytes {
            return solveToBudget(file: file, options: options, budget: budget)
        }
        if options.maxDimensionPx != nil {
            return encodeWithDimensionCap(file: file, options: options) { f, o in
                encodeAdvanced(file: f, options: o)
            }
        }
        return encodeAdvanced(file: file, options: options)
    }

    /// 类型分发：与旧 `compress` 主体一致（不含系统转换 / 上限 / 预算处理）。
    private static func encodeAdvanced(file: String, options: CompressOptions) -> EngineResult {
        let imgType = detectImageType(path: file)
        if options.outputFormat != .original {
            return compressToFormat(file: file, target: options.outputFormat.rawValue, options: options)
        }
        switch imgType {
        case "png": return compressPNG(file: file, options: options)
        case "jpg": return compressJPG(file: file, options: options)
        case "gif": return compressGIF(file: file, options: options)
        case "webp": return compressToWebP(file: file, options: options)
        case "heic": return compressHEIC(file: file, options: options)
        default:
            let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
            return EngineResult(
                success: false, compressed: original, outType: imgType,
                algorithm: "none", error: "不支持的图片类型: \(imgType)",
                verdict: .rejected, reason: .unsupportedFormat
            )
        }
    }

    /// 智能模式：与 Tauri `compress_smart` 对齐——PNG 同时尝试 pngquant 与 WebP，取体积更小者。
    /// 候选集合由引擎决定，三轨一致；意图只给质量与约束，不许在某一轨偷偷多试一种编码。
    static func compressSmart(file: String, options: CompressOptions) -> EngineResult {
        if options.processingMode == .system {
            return SystemImageConverter.convert(file: file, options: options)
        }
        var opts = options
        if opts.quality <= 0 { opts.quality = analyzeQuality(file: file) }
        if opts.maxOutputBytes != nil {
            return solveToBudget(file: file, options: opts, budget: opts.maxOutputBytes!)
        }
        if opts.maxDimensionPx != nil {
            return encodeWithDimensionCap(file: file, options: opts) { f, o in
                smartEncode(file: f, options: o)
            }
        }
        return smartEncode(file: file, options: opts)
    }

    private static func smartEncode(file: String, options opts: CompressOptions) -> EngineResult {
        if opts.outputFormat != .original {
            return compressToFormat(file: file, target: opts.outputFormat.rawValue, options: opts)
        }
        let imgType = detectImageType(path: file)
        let candidates: [EngineResult]
        switch imgType {
        case "png": candidates = [compressPNG(file: file, options: opts), compressToWebP(file: file, options: opts)]
        case "jpg": candidates = [compressJPG(file: file, options: opts)]
        case "gif": candidates = [compressGIF(file: file, options: opts)]
        case "webp": candidates = [compressToWebP(file: file, options: opts)]
        case "heic": candidates = [compressHEIC(file: file, options: opts)]
        default: return compress(file: file, options: opts)
        }
        return candidates.min(by: { $0.compressed.count < $1.compressed.count })
            ?? compress(file: file, options: opts)
    }

    // MARK: - 尺寸上限（只缩不放大）

    /// 缩一次到临时文件（保持源格式与 alpha），再走给定编码闭包；结束时删临时文件。
    private static func encodeWithDimensionCap(
        file: String, options: CompressOptions,
        encode: (String, CompressOptions) -> EngineResult
    ) -> EngineResult {
        guard let cap = options.maxDimensionPx else {
            return encode(file, options)
        }
        guard let temp = downscaledTempFile(file: file, cap: cap) else {
            var result = encode(file, options)
            if let reason = capSkipReason(file: file) { result.warnings.append(reason) }
            return result
        }
        defer { try? FileManager.default.removeItem(atPath: temp) }
        var rung = options
        rung.maxDimensionPx = nil // 已物理缩放，避免递归
        var result = encode(temp, rung)
        result.warnings.append("已按最长边上限 \(cap) px 缩放（只缩不放大）")
        return result
    }

    /// 不动尺寸的情形说明（GIF 动图 / 无法解码）；nil = 本可以缩放或无需缩放。
    private static func capSkipReason(file: String) -> String? {
        if detectImageType(path: file) == "gif" {
            return "动图不按尺寸上限缩放（单帧重编码会丢帧），已保持原尺寸"
        }
        if longestEdge(file: file) == nil {
            return "无法读取图像尺寸，尺寸上限未生效"
        }
        return nil
    }

    private static func longestEdge(file: String) -> (edge: Int, width: Int, height: Int)? {
        let url = URL(fileURLWithPath: file)
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
              let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any],
              let w = props[kCGImagePropertyPixelWidth] as? Int,
              let h = props[kCGImagePropertyPixelHeight] as? Int
        else { return nil }
        return (max(w, h), w, h)
    }

    /// 只缩不放大：最长边超过上限时输出缩放后的同格式临时文件；无需缩放/不宜处理时返回 nil。
    /// ImageIO thumbnail 管线保持 alpha 通道不丢，重采样语义与 Rust 侧 Lanczos3 同一意图
    /// （尺寸收敛优先于逐像素等算法）——这是这里选它而不是自写 vImage 核的原因。
    private static func downscaledTempFile(file: String, cap: Int) -> String? {
        guard let (edge, _, _) = longestEdge(file: file), edge > cap else { return nil }
        let imgType = detectImageType(path: file)
        // 动图单帧缩放会丢帧，交给调用方按原尺寸处理（诚实跳过 + 警告）。
        guard imgType != "gif" else { return nil }
        let url = URL(fileURLWithPath: file)
        guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        let thumbOpts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: cap,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(src, 0, thumbOpts as CFDictionary) else { return nil }
        let uti: CFString
        let ext: String
        switch imgType {
        case "png": (uti, ext) = (UTType.png.identifier as CFString, "png")
        case "jpg": (uti, ext) = (UTType.jpeg.identifier as CFString, "jpg")
        case "webp": (uti, ext) = (UTType.webP.identifier as CFString, "webp")
        case "heic": (uti, ext) = (UTType.heic.identifier as CFString, "heic")
        default: return nil
        }
        let tmp = scratchFile(ext)
        guard let dest = CGImageDestinationCreateWithURL(
            URL(fileURLWithPath: tmp) as CFURL, uti, 1, nil
        ) else { return nil }
        // JPEG 临时帧用近无损质量，避免在正式编码前叠加第二次有损。
        let destOpts: [CFString: Any]? = imgType == "jpg"
            ? [kCGImageDestinationLossyCompressionQuality: 0.98] : nil
        CGImageDestinationAddImage(dest, image, destOpts as CFDictionary?)
        guard CGImageDestinationFinalize(dest),
              FileManager.default.contents(atPath: tmp) != nil
        else {
            try? FileManager.default.removeItem(atPath: tmp)
            return nil
        }
        return tmp
    }

    // MARK: - 指定体积求解（≤6 次编码）

    /// 三条硬规则（与 Rust `solve_to_budget` 一字不差）：**原图已在预算内就不重编码**；
    /// **命中即停**；**没命中时返回全程最小的有效候选 + targetMet=false + 把实际达到的体积
    /// 和预算都说出去**。比原图大的档位在规则 7 下不是候选，不能当成"压出来了"。
    private static func solveToBudget(file: String, options: CompressOptions, budget: Int64) -> EngineResult {
        let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
        let sourceType = detectImageType(path: file)
        let originalSize = Int64(original.count)
        if originalSize <= budget {
            var kept = noImprovement(original: original, outType: sourceType,
                                     algorithm: "指定体积", originalSize: original.count)
            // 这一支压根没进编码：原图已经在用户要的体积之内。它不是"压完没收益"。
            kept.reason = .alreadyUnderTarget
            kept.targetBytes = budget
            kept.targetMet = true
            kept.warnings.append("原图 \(CompressResult.formatBytesJS(originalSize)) 已在目标 \(CompressResult.formatBytesJS(budget)) 以内，未做无谓的重编码")
            return kept
        }
        var best: EngineResult?
        var attempts = 0
        for rung in targetLadder.prefix(budgetAttemptLimit) {
            attempts += 1
            var step = options
            step.quality = rung.quality
            step.maxOutputBytes = nil // 递归闸门：搜索内部只许走一次编码
            step.maxDimensionPx = mergeCap(user: options.maxDimensionPx, rung: rung.cap)
            let r = encodeWithDimensionCap(file: file, options: step) { f, o in
                encodeAdvanced(file: f, options: o)
            }
            guard r.success, !r.compressed.isEmpty else { continue }
            let size = Int64(r.compressed.count)
            if size >= originalSize { continue } // 规则 7：比原图大的输出不是赢
            if best == nil || r.compressed.count < best!.compressed.count { best = r }
            if size <= budget { break } // 命中即停，不做无谓的第二次编码
        }
        guard var closest = best else {
            var kept = noImprovement(original: original, outType: sourceType,
                                     algorithm: "指定体积", originalSize: original.count)
            kept.targetBytes = budget
            kept.targetMet = false
            kept.warnings.append("指定体积 \(CompressResult.formatBytesJS(budget)) 未达到：\(attempts) 次尝试里没有任何一档产出比原图 \(CompressResult.formatBytesJS(originalSize)) 更小的有效文件，已保留原图")
            return kept
        }
        let achieved = Int64(closest.compressed.count)
        closest.targetBytes = budget
        closest.targetMet = achieved <= budget
        if achieved > budget {
            closest.warnings.append("指定体积 \(CompressResult.formatBytesJS(budget)) 未达到：最激进的一档也只压到 \(CompressResult.formatBytesJS(achieved))（已给出这次真正产出的最小文件，不能当作达成目标）")
        }
        return closest
    }

    /// 按文件体积推测质量（仅当 quality 为 0 时使用，与 Tauri analyze_quality 一致）
    private static func analyzeQuality(file: String) -> Int {
        let size = (try? FileManager.default.attributesOfItem(atPath: file)[.size] as? Int64) ?? 0
        if size > 5 * 1024 * 1024 { return 60 }
        if size > 2 * 1024 * 1024 { return 65 }
        if size > 1 * 1024 * 1024 { return 70 }
        if size > 500 * 1024 { return 75 }
        return 80
    }

    static func compressToFormat(file: String, target: String, options: CompressOptions) -> EngineResult {
        if options.processingMode == .system {
            var opts = options
            opts.outputFormat = OutputFormat(rawValue: target) ?? .original
            return SystemImageConverter.convert(file: file, options: opts)
        }
        switch target {
        case "webp": return compressToWebP(file: file, options: options)
        case "avif": return compressToAVIF(file: file, options: options)
        case "jpg", "jpeg": return compressJPG(file: file, options: options)
        case "png": return compressPNG(file: file, options: options)
        case "heic", "heif": return compressHEIC(file: file, options: options)
        default:
            let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
            return EngineResult(
                success: false, compressed: original, outType: target,
                algorithm: "none", error: "不支持的目标格式: \(target)",
                verdict: .rejected, reason: .unsupportedFormat
            )
        }
    }

    // MARK: - PNG

    static func compressPNG(file: String, options: CompressOptions) -> EngineResult {
        let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
        let originalSize = original.count
        let quality = options.quality

        if CLIRunner.toolURL(for: "pngquant") != nil {
            let tmp = scratchFile("png")
            let qLow = max(quality - 10, 10)
            let qHigh = min(quality, 100)
            let data = CLIRunner.runToFile(
                tool: "pngquant",
                args: [
                    "--quality=\(qLow)-\(qHigh)", "--speed=3", "--strip",
                    "--output", tmp, "--", file
                ],
                outputPath: tmp
            )
            if let data, data.count < originalSize {
                try? FileManager.default.removeItem(atPath: tmp)
                return EngineResult(success: true, compressed: data, outType: "png", algorithm: "pngquant")
            }
            try? FileManager.default.removeItem(atPath: tmp)
        }

        if CLIRunner.toolURL(for: "oxipng") != nil {
            let tmp = scratchFile("png")
            try? FileManager.default.copyItem(atPath: file, toPath: tmp)
            let level = min(max(quality / 20, 1), 6)
            let _ = CLIRunner.runToFile(
                tool: "oxipng",
                args: ["-o\(level)", "--strip", "safe", tmp],
                outputPath: tmp
            )
            if let data = FileManager.default.contents(atPath: tmp), !data.isEmpty, data.count < originalSize {
                try? FileManager.default.removeItem(atPath: tmp)
                return EngineResult(success: true, compressed: data, outType: "png", algorithm: "oxipng")
            }
            try? FileManager.default.removeItem(atPath: tmp)
        }

        if let data = compressPNGImageIO(file: file), data.count < originalSize {
            return EngineResult(success: true, compressed: data, outType: "png", algorithm: "image-png")
        }
        return noImprovement(original: original, outType: "png", algorithm: "pngquant", originalSize: originalSize)
    }

    static func compressPNGImageIO(file: String) -> Data? {
        guard let data = FileManager.default.contents(atPath: file),
              let src = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(src, 0, nil)
        else { return nil }
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(
            out as CFMutableData, UTType.png.identifier as CFString, 1, nil
        ) else { return nil }
        CGImageDestinationAddImage(dest, image, nil)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }

    // MARK: - JPEG

    static func compressJPG(file: String, options: CompressOptions) -> EngineResult {
        let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
        let originalSize = original.count
        let quality = options.quality

        if CLIRunner.toolURL(for: "cjpeg") != nil,
           let ppmData = convertToPPM(file: file) {
            let data = CLIRunner.run(
                tool: "cjpeg",
                args: ["-quality", "\(quality)", "-optimize", "-progressive"],
                stdinData: ppmData
            )
            if let data, data.count < originalSize {
                return EngineResult(success: true, compressed: data, outType: "jpg", algorithm: "mozjpeg")
            }
        }

        if let data = compressJPGImageIO(file: file, quality: quality), data.count < originalSize {
            return EngineResult(success: true, compressed: data, outType: "jpg", algorithm: "image-jpeg")
        }
        return noImprovement(original: original, outType: "jpg", algorithm: "mozjpeg", originalSize: originalSize)
    }

    static func compressJPGImageIO(file: String, quality: Int) -> Data? {
        guard let data = FileManager.default.contents(atPath: file),
              let src = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(src, 0, nil)
        else { return nil }
        let out = NSMutableData()
        let destOpts: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: quality]
        guard let dest = CGImageDestinationCreateWithData(
            out as CFMutableData, UTType.jpeg.identifier as CFString, 1, nil
        ) else { return nil }
        CGImageDestinationAddImage(dest, image, destOpts as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }

    static func convertToPPM(file: String) -> Data? {
        guard let data = FileManager.default.contents(atPath: file),
              let src = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(src, 0, nil)
        else { return nil }

        let width = image.width
        let height = image.height
        // ⚠️ 24bpp（bytesPerRow = width*3 + kCGImageAlphaNone）的 CGContext 在 macOS 上
        // 初始化直接返回 nil——位图上下文只支持 32bpp 组合。这里必须用 32bpp 画完
        // 再在输出时剥掉 alpha 字节，否则 cjpeg 分支永远走不到，所有 JPEG 都被
        // 静默跳过（v2.5.31 起从没工作过，parity 静态测试钉不住这种运行期错误）。
        guard let context = CGContext(
            data: nil, width: width, height: height,
            bitsPerComponent: 8, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))

        guard let rawData = context.data else { return nil }
        let pixels = rawData.bindMemory(to: UInt8.self, capacity: width * height * 4)
        var ppm = Data(capacity: 15 + width * height * 3)
        ppm.append("P6\n\(width) \(height)\n255\n".data(using: .ascii)!)
        // RGBA -> RGB：剥掉每个像素的第 4 个字节
        for offset in 0..<(width * height * 4) where offset % 4 != 3 {
            ppm.append(pixels[offset])
        }
        return ppm
    }

    // MARK: - GIF

    static func compressGIF(file: String, options: CompressOptions) -> EngineResult {
        let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
        let originalSize = original.count
        let quality = options.quality

        if CLIRunner.toolURL(for: "gifsicle") != nil {
            let tmp = scratchFile("gif")
            let colors = max(Int(Double(quality) / 100.0 * 256.0), 32)
            if let data = CLIRunner.runToFile(
                tool: "gifsicle",
                args: [
                    "--optimize=3", "--colors=\(colors)", "--no-comments",
                    "--output", tmp, file
                ],
                outputPath: tmp
            ), data.count < originalSize {
                try? FileManager.default.removeItem(atPath: tmp)
                return EngineResult(success: true, compressed: data, outType: "gif", algorithm: "gifsicle")
            }
            try? FileManager.default.removeItem(atPath: tmp)
        }
        return noImprovement(original: original, outType: "gif", algorithm: "gifsicle", originalSize: originalSize)
    }

    // MARK: - WebP

    static func compressToWebP(file: String, options: CompressOptions) -> EngineResult {
        let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
        let originalSize = original.count
        let quality = options.quality

        if CLIRunner.toolURL(for: "cwebp") != nil {
            let tmp = scratchFile("webp")
            if let data = CLIRunner.runToFile(
                tool: "cwebp",
                args: [
                    "-q", "\(quality)", "-m", "6", "-pass", "10",
                    "-o", tmp, file
                ],
                outputPath: tmp
            ), data.count < originalSize {
                try? FileManager.default.removeItem(atPath: tmp)
                return EngineResult(success: true, compressed: data, outType: "webp", algorithm: "cwebp")
            }
            try? FileManager.default.removeItem(atPath: tmp)
        }

        if let data = compressWebPImageIO(file: file, quality: quality) {
            return EngineResult(success: true, compressed: data, outType: "webp", algorithm: "ImageIO")
        }
        return noImprovement(original: original, outType: "webp", algorithm: "cwebp", originalSize: originalSize)
    }

    static func compressWebPImageIO(file: String, quality: Int) -> Data? {
        guard let data = FileManager.default.contents(atPath: file),
              let src = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateImageAtIndex(src, 0, nil)
        else { return nil }
        let out = NSMutableData()
        let destOpts: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: quality]
        guard let dest = CGImageDestinationCreateWithData(
            out as CFMutableData, UTType.webP.identifier as CFString, 1, nil
        ) else { return nil }
        CGImageDestinationAddImage(dest, image, destOpts as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { return nil }
        return out as Data
    }

    // MARK: - AVIF

    static func compressToAVIF(file: String, options: CompressOptions) -> EngineResult {
        let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
        let quality = options.quality

        if CLIRunner.toolURL(for: "avifenc") != nil {
            let tmp = scratchFile("avif")
            if let data = CLIRunner.runToFile(
                tool: "avifenc",
                args: [
                    "--speed", "6", "--min", "0", "--max", "\(quality)",
                    "-o", tmp, file
                ],
                outputPath: tmp
            ) {
                try? FileManager.default.removeItem(atPath: tmp)
                return EngineResult(success: true, compressed: data, outType: "avif", algorithm: "avifenc")
            }
            try? FileManager.default.removeItem(atPath: tmp)
        }
        return noImprovement(original: original, outType: "avif", algorithm: "avifenc", originalSize: original.count)
    }

    // MARK: - HEIC

    /// 用 macOS 自带的 sips 直接写 HEIC（与 Rust 线 sips_heic_bytes 同一份契约）。
    ///
    /// 旧实现先把 HEIF 转成 JPEG 再"当作压缩结果"：磁盘上扩展名还是 .heic、字节
    /// 却是 JFIF —— replace 模式据此删掉真正的 .heic 原图，产物还打不开。sips 能写
    /// `public.heic`，这里只接受**字节真的是 HEIC** 的结果，JPEG 冒充一律不认。
    private static func sipsHEICBytes(file: String, quality: Int) -> Data? {
        let out = scratchFile("heic")
        defer { try? FileManager.default.removeItem(atPath: out) }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/sips")
        process.arguments = ["-s", "format", "heic", "-s", "formatOptions", String(quality), file, "--out", out]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            process.waitUntilExit()
            guard process.terminationStatus == 0,
                  let data = FileManager.default.contents(atPath: out),
                  !data.isEmpty,
                  detectImageType(of: data) == "heic" else { return nil }
            return data
        } catch { return nil }
    }

    static func compressHEIC(file: String, options: CompressOptions) -> EngineResult {
        let original = (try? Data(contentsOf: URL(fileURLWithPath: file))) ?? Data()
        let originalSize = original.count

        // 原格式 / 显式 HEIC：原地重编码，产物必须真的是 HEIC 字节，
        // 不许把"用户要 HEIC"悄悄兑现成一张 JPEG。
        if options.outputFormat == .original || options.outputFormat == .heic {
            // quality 0 是"由引擎推测"的旧约定；HEIC 编码器没有推测档，给一个稳妥的 75。
            let quality = options.quality == 0 ? 75 : min(options.quality, 100)
            if let data = sipsHEICBytes(file: file, quality: quality),
               data.count < originalSize {
                return EngineResult(
                    success: true, compressed: data, outType: "heic",
                    algorithm: "sips-heic", error: nil, verdict: .candidate
                )
            }
            return noImprovement(original: original, outType: "heic", algorithm: "sips-heic", originalSize: originalSize)
        }

        // 显式换格式（JPG/PNG/WebP/AVIF）走转换语义：JPEG 只是像素中转，产物格式由用户点选。
        let tmp = scratchFile("jpg")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/sips")
        process.arguments = ["-s", "format", "jpeg", file, "--out", tmp]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            process.waitUntilExit()
            if process.terminationStatus == 0,
               FileManager.default.fileExists(atPath: tmp) {
                let result: EngineResult
                switch options.outputFormat {
                case .webp: result = compressToWebP(file: tmp, options: options)
                case .avif: result = compressToAVIF(file: tmp, options: options)
                case .png: result = compressPNG(file: tmp, options: options)
                case .jpg: result = compressJPG(file: tmp, options: options)
                case .original, .heic:
                    // 上面已经分流处理；到这里说明枚举新增了未覆盖的值，按失败说清楚。
                    result = EngineResult(
                        success: false, compressed: original, outType: options.outputFormat.rawValue,
                        algorithm: "sips", error: "未支持的目标格式: \(options.outputFormat.rawValue)",
                        verdict: .rejected, reason: .unsupportedFormat)
                }
                if result.success {
                    try? FileManager.default.removeItem(atPath: tmp)
                    return EngineResult(
                        success: true, compressed: result.compressed,
                        outType: result.outType, algorithm: "sips+\(result.algorithm)",
                        error: result.error
                    )
                }
                // 回退：直接用 sips 转换后的 JPEG
                if let data = FileManager.default.contents(atPath: tmp), !data.isEmpty {
                    try? FileManager.default.removeItem(atPath: tmp)
                    return EngineResult(success: true, compressed: data, outType: "jpg", algorithm: "sips")
                }
                try? FileManager.default.removeItem(atPath: tmp)
            }
        } catch {}
        try? FileManager.default.removeItem(atPath: tmp)
        return noImprovement(original: original, outType: "heic", algorithm: "sips", originalSize: originalSize)
    }

    // MARK: - Helpers

    /// 在专用工作目录 `$TMPDIR/sizqo-work/` 下创建中间文件。
    /// 集中到子目录后，启动/退出时删整个目录即可清掉崩溃残留与正常残留。
    private static func scratchFile(_ ext: String) -> String {
        let dir = NSTemporaryDirectory() + "sizqo-work"
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        return dir + "/" + UUID().uuidString + "." + ext
    }

    static func noImprovement(original: Data, outType: String, algorithm: String, originalSize: Int) -> EngineResult {
        let msg: String
        // 交回的是原图字节本身，判据必须是 keptOriginal —— 说 success=true 就等于
        // 让调用方去猜「没收益」和「压成了」的区别，而这一支一个字节都没打算落盘。
        let verdict: EncodeVerdict = .keptOriginal
        if original.count > originalSize {
            msg = "压缩后 \(CompressResult.formatBytes(Int64(original.count))) > 原始 \(CompressResult.formatBytes(Int64(originalSize)))，原图已是最优压缩"
            return EngineResult(success: true, compressed: original, outType: outType, algorithm: algorithm,
                                error: msg, verdict: verdict, reason: .outputLargerThanSource)
        }
        msg = "压缩后体积未减小"
        return EngineResult(success: true, compressed: original, outType: outType, algorithm: algorithm,
                            error: msg, verdict: verdict, reason: .noSizeReduction)
    }
}
