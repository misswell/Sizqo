import Foundation
import ImageIO
import CoreGraphics

struct EngineResult {
    var success: Bool
    var compressed: Data
    var outType: String
    var algorithm: String
    var error: String?
    /// 指定体积求解：预算（字节），仅 targetSize 路径有值
    var targetBytes: Int64? = nil
    /// 达成与否照实写：没打中预算绝不报成达成
    var targetMet: Bool? = nil
    /// 缩放 / 未达预算 / 元数据等如实说明
    var warnings: [String] = []
    /// 这一轮交回的**字节属于哪一种事实**（见 ResultContract.swift）。
    /// 默认 candidate = 「这是编码产物」；交回原图字节的每一支都必须自己明说 keptOriginal，
    /// 否则 evaluate 会把忘了标判据的失败一律降级成失败，而不是冒充成功。
    var verdict: EncodeVerdict = .candidate
    /// 引擎自己知道的那个原因（不支持 / 解码失败 / 没收益…）。没标就是 unknown。
    var reason: ReasonCode = .unknown
}

struct CompressResult: Identifiable {
    let id = UUID()
    var file: String
    var originalSize: Int64
    var compressedSize: Int64
    var savings: Double
    var outType: String
    var algorithm: String
    var error: String?
    var outputPath: String?
    var backupPath: String?
    var outputMode: String?
    /// 本批次使用的压缩参数快照（复制日志用，与 Tauri 的 result.compressOptions 对应）
    var options: CompressOptions?
    /// 以下与契约「CompressResult gains optional fields」对齐，均为可选、缺省即不显示。
    var elapsedMs: Int?
    var sourceWidth: Int?
    var sourceHeight: Int?
    var outputWidth: Int?
    var outputHeight: Int?
    var targetBytes: Int64?
    var targetMet: Bool?
    var warnings: [String] = []
    /// 这次任务对用户的最终结论（见 ResultContract.swift）。
    /// 它由落盘之后那一处写入（Publish），落盘之前谁都不许预先承诺；nil 只出现在
    /// 这一版之前留下的载荷上，界面按「没有结论」兜底，绝不按「完成」兜底。
    var outcome: ResultOutcome? = nil
    /// 结论的理由。UI 选句子只读它，不读 error 文案。
    var reasonCode: ReasonCode? = nil

    var originalSizeFormatted: String { Self.formatBytes(originalSize) }
    var compressedSizeFormatted: String { Self.formatBytes(compressedSize) }
    var savingsFormatted: String { String(format: "%.1f%%", savings) }
    /// 与 Tauri 前端一致：节省 >= 0 显示 "-N.N%"，否则 "+N.N%"
    var savingsSignedText: String {
        "\(savings >= 0 ? "-" : "+")\(String(format: "%.1f", abs(savings)))%"
    }

    /// 与 Rust `engine::format_bytes` 对齐（用于 originalSizeFormatted / compressedSizeFormatted）
    static func formatBytes(_ bytes: Int64) -> String {
        if bytes < 1024 { return String(format: "%.1fB", Double(bytes)) }
        if bytes < 1024 * 1024 { return String(format: "%.1fKB", Double(bytes) / 1024.0) }
        return String(format: "%.1fMB", Double(bytes) / (1024.0 * 1024.0))
    }

    /// 与前端 JS `formatBytes` 对齐（用于统计条、队列行大小）：0 显示为 "0B"
    static func formatBytesJS(_ bytes: Int64) -> String {
        if bytes == 0 { return "0B" }
        if bytes < 1024 { return String(format: "%.1fB", Double(bytes)) }
        if bytes < 1024 * 1024 { return String(format: "%.1fKB", Double(bytes) / 1024.0) }
        return String(format: "%.1fMB", Double(bytes) / (1024.0 * 1024.0))
    }

    init(engine: EngineResult, file: String, originalSize: Int64, options: CompressOptions? = nil) {
        self.file = file
        self.originalSize = originalSize
        self.compressedSize = Int64(engine.compressed.count)
        self.savings = Self.savings(originalSize: originalSize, compressedSize: self.compressedSize)
        self.outType = engine.outType
        self.algorithm = engine.algorithm
        self.error = engine.error
        self.options = options
        self.targetBytes = engine.targetBytes
        self.targetMet = engine.targetMet
        self.warnings = engine.warnings
    }

    /// 历史页要复用对比窗口/复制日志那一套，但记录里只有大小、没有字节本身。
    init(historyEntry entry: HistoryEntry) {
        self.file = entry.sourcePath
        self.originalSize = entry.originalSize
        self.compressedSize = entry.compressedSize
        self.savings = entry.savings
        self.outType = entry.outType
        self.algorithm = entry.algorithm
        self.error = nil
        self.outputPath = entry.outputPath
        self.backupPath = entry.backupPath
        self.outputMode = entry.outputMode
        // 历史只登记真正落过盘的结果；路径都没记的那条（异常数据）不许被读成「有产物」。
        // 已恢复 / 重建条目同样不是产物：前者的 outputPath 指向写回后的原图，后者没有明细。
        let isProduct = entry.status == .compressed || entry.status == .missing
        self.outcome = isProduct && entry.outputPath != nil ? .committed : nil
        self.options = nil
    }

    static func savings(originalSize: Int64, compressedSize: Int64) -> Double {
        guard originalSize > 0 else { return 0 }
        let raw = (Double(originalSize) - Double(min(compressedSize, originalSize)))
            / Double(originalSize) * 100.0
        return (raw * 10).rounded() / 10
    }
}

/// 队列项的持久状态 —— **只有这六种**。
///
/// 暂停 / 停止是**这一轮执行**的状态（`CompressionPhase`），不是文件的状态：
/// 被停止挡在门外的文件照旧是 `pending`，下一轮「继续压缩」自然还会带上它。
/// 老实现用 `cancelled` 同时表达"用户把它移出队列"和"停止时这一轮没轮到它"，
/// 于是停止之后剩下的图全都变成"已跳过"，再也压不动 —— 这两件事必须分开。
enum QueueStatus: String {
    /// 还需要压缩：第一次没开始、暂停中等待、停止后留待下一轮、新导入，都是它。
    case pending = "pending"
    /// 真的在压。
    case running = "running"
    case done = "done"
    /// 压完了但失败。只有显式「重试」才会回到 pending，普通「继续压缩」不碰它。
    case failed = "failed"
    /// 用户把它移出了队列。
    case removed = "removed"
    /// 用户把这次压缩撤销了（原图回来了）。要压得重新点。
    case restored = "restored"
}

struct QueueItem: Identifiable {
    let id = UUID()
    var path: String
    var fileName: String
    var fileSize: Int64
    /// §39 §40：来源根与相对位置在入队时定死，重试 / 续跑 / 导出都读这两个字段，
    /// 不再按"这一批选了哪些目录"重新推一遍。
    var sourceRoot: String = ""
    var relativePath: String = ""
    /// §41：只在真正开工（pending → running）时加一。重试沿用同一个队列项 / id，
    /// 绝不删掉旧项再建新项 —— 否则历史、进度和事件会对不上号。
    var attemptCount: Int = 0
    /// §42：至少完整处理过一次的闩锁，一旦置上永不复位。总体进度的分子读它，
    /// 所以重试把状态放回 pending 时，进度不会从 10/10 掉成 9/10。
    var everProcessed: Bool = false
    var status: QueueStatus = .pending
    var result: CompressResult?
}

/// 队列进度 —— **队列的派生值**，不是某一轮的计数器。
///
/// 主界面显示的永远是它（与 Tauri 前端 `getQueueProgress` 同一套判据）。
/// 停止之后重新开始一轮时，那一轮的目标会变小（只含 pending），
/// 但这里的 total 始终是整个队列，所以进度不会从 40/100 掉回 0/60。
struct QueueProgress {
    var total = 0
    var done = 0
    var failed = 0
    var running = 0
    var pending = 0
    /// §42 §43：至少完整处理过一次的任务数。失败也算（它确实跑完了一次，
    /// 只是没成）；重试只改当前状态，不改这个分子 —— 按 `done + failed`
    /// 的瞬时值算，10/10 会在重试时掉成 9/10。
    var processed = 0
    /// completed 的两种结果：有没有落下产物（判据仍只走 CompressResult.hasProduct）。
    var committed = 0
    var skipped = 0
    /// 处理过一次、此刻又在 running / pending 的任务数（§42 的「正在重试 N 项」）。
    var retrying = 0

    /// §43 的拆分读法：completed 是走完的生命周期，committed + skipped 解释它；
    /// active 是此刻真正在跑的。done / running 保留为旧读法的别名。
    var completed: Int { done }
    var active: Int { running }
    var fraction: Double { total > 0 ? Double(processed) / Double(total) : 0 }

    /// 摘要那一行（为 0 的那几段不写）。
    var summaryText: String {
        var text = "\(processed) / \(total) 已处理"
        if failed > 0 { text += " · 失败 \(failed)" }
        if retrying > 0 { text += " · 正在重试 \(retrying) 项" }
        return text
    }

    init(items: [QueueItem]) {
        for item in items {
            // removed / restored 已经离开了这套账：一个被移出队列，一个被用户撤销了。
            switch item.status {
            case .removed, .restored: continue
            case .done: done += 1
            case .failed: failed += 1
            case .running: running += 1
            case .pending: pending += 1
            }
            total += 1
            // 瞬时终态兜底：分子永远不小于终态数，且重试不减少。
            if item.everProcessed || item.status == .done || item.status == .failed {
                processed += 1
            }
            if item.everProcessed && (item.status == .running || item.status == .pending) {
                retrying += 1
            }
            if item.status == .done {
                if item.result?.hasProduct == true { committed += 1 } else { skipped += 1 }
            }
        }
    }
}

func detectImageType(path: String) -> String {
    // 嗅探只需要 12 个字节的魔数：为它们读整个文件（200MB 的 TIFF 也整份进内存）
    // 与"大文件不许整份进内存"的纪律冲突。多 worker 并行时放大最明显。
    guard let handle = FileHandle(forReadingAtPath: path) else { return "unknown" }
    defer { try? handle.close() }
    guard let header = try? handle.read(upToCount: 12) else { return "unknown" }
    return detectImageType(of: header)
}

func detectImageType(of data: Data) -> String {
    let bytes = [UInt8](data.prefix(12))
    if bytes.count >= 4 && bytes[0] == 0x89 && bytes[1] == 0x50 && bytes[2] == 0x4E && bytes[3] == 0x47 {
        return "png"
    }
    if bytes.count >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return "jpg"
    }
    if bytes.count >= 3 && bytes[0] == 0x47 && bytes[1] == 0x49 && bytes[2] == 0x46 {
        return "gif"
    }
    if bytes.count >= 12 && bytes[0] == 0x52 && bytes[1] == 0x49 && bytes[2] == 0x46 && bytes[3] == 0x46 {
        return "webp"
    }
    if bytes.count >= 2 && bytes[0] == 0x42 && bytes[1] == 0x4D {
        return "bmp"
    }
    if bytes.count >= 12 && bytes[4] == 0x66 && bytes[5] == 0x74 && bytes[6] == 0x79 && bytes[7] == 0x70 {
        let brand = String(bytes: bytes[8..<12], encoding: .utf8) ?? ""
        if ["heic", "heix", "mif1", "hevc"].contains(brand) {
            return "heic"
        }
    }
    return "unknown"
}

// MARK: - 尺寸读取（详情栏的源/输出像素尺寸；读不出来就如实缺省）

func imagePixelDimensions(file: String) -> (width: Int, height: Int)? {
    let url = URL(fileURLWithPath: file)
    guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
          let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any],
          let w = props[kCGImagePropertyPixelWidth] as? Int,
          let h = props[kCGImagePropertyPixelHeight] as? Int
    else { return nil }
    return (w, h)
}

func imagePixelDimensions(data: Data) -> (width: Int, height: Int)? {
    guard let src = CGImageSourceCreateWithData(data as CFData, nil),
          let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any],
          let w = props[kCGImagePropertyPixelWidth] as? Int,
          let h = props[kCGImagePropertyPixelHeight] as? Int
    else { return nil }
    return (w, h)
}

/// 与 Tauri 后端 collect_image_files 的支持扩展名保持一致
let supportedExtensions: Set<String> = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp",
    "avif", "jxl", "heic", "heif", "tif", "tiff"
]

/// 规范化路径（解析符号链接，如 /tmp → /private/tmp），与 Tauri `canonicalize` 对齐。
/// 去重必须基于规范化路径，否则同一文件以两种写法出现时会重复入队。
func canonicalPath(_ path: String) -> String {
    URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
}

func expandImageFiles(paths: [String]) -> [String] {
    var result: [String] = []
    let fm = FileManager.default
    for path in paths {
        var isDir: ObjCBool = false
        guard fm.fileExists(atPath: path, isDirectory: &isDir) else { continue }
        if isDir.boolValue {
            var dirFiles: [String] = []
            if let enumerator = fm.enumerator(atPath: path) {
                while let item = enumerator.nextObject() as? String {
                    let full = (path as NSString).appendingPathComponent(item)
                    var itemIsDir: ObjCBool = false
                    guard fm.fileExists(atPath: full, isDirectory: &itemIsDir), !itemIsDir.boolValue else { continue }
                    let ext = (full as NSString).pathExtension.lowercased()
                    if supportedExtensions.contains(ext) {
                        dirFiles.append(canonicalPath(full))
                    }
                }
            }
            result.append(contentsOf: dirFiles.sorted())
        } else {
            let ext = (path as NSString).pathExtension.lowercased()
            if supportedExtensions.contains(ext) {
                result.append(canonicalPath(path))
            }
        }
    }
    // 去重但保持顺序（目录内已排序 + 文件选择顺序）
    var seen = Set<String>()
    return result.filter { seen.insert($0).inserted }
}
