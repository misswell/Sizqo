import Foundation
import ImageIO

/// §9 R2：图片解码的内存账本（与 Rust `image_memory.rs` 同一职责）。
///
/// 并发只数"几个任务"是不够的：同时开工 6 张 100 MP 的 TIFF，光像素就要几 GB，
/// 再加上原始 Data、编码缓冲和 Smart 的多个候选。这里按 `宽 × 高 × 通道 × 安全系数`
/// 给每张图记一笔账，调度器据此决定还能不能再开下一个（小图正常并发、大图降并发、
/// 超大图独占）。
///
/// 估算只读文件头（ImageIO 的像素属性），绝不为了"量一下尺寸"把整张图解码进内存。
enum ImageMemory {
    /// 解码后的像素、缩放工作副本、编码器输入至少要同时存在一份。
    /// 3 倍是保守下限：宁可少开一个任务，也不能让一批大图把机器压进 swap。
    static let safetyFactor = 3

    /// 读不到物理内存时的兜底预算。512 MiB 足够让普通图片照常并发。
    static let fallbackBudgetBytes = 512 * 1024 * 1024

    /// 尺寸未知时按一张大图记账。未知不等于小：不估像素，就至少别让它无限铺开。
    static let unknownEstimateBytes = 512 * 1024 * 1024

    static let minBudgetBytes = 256 * 1024 * 1024
    static let maxBudgetBytes = 2 * 1024 * 1024 * 1024

    /// 解码预算取物理内存的四分之一，夹在 256 MiB 与 2 GiB 之间。
    /// 系统其余部分、界面和用户别的应用都要内存；压缩器没有理由把整机吃满。
    static func budgetBytes() -> Int {
        let physical = Int(clamping: ProcessInfo.processInfo.physicalMemory)
        let quarter = physical > 0 ? physical / 4 : fallbackBudgetBytes
        return min(max(quarter, minBudgetBytes), maxBudgetBytes)
    }

    /// 解码一帧需要多少字节。`channels` 是"最坏情况下"的每像素字节数：
    /// 带 alpha 的 4，不带的 3。JPEG 之外一律按 4 记，只会更保守。
    static func estimateBytes(width: Int, height: Int, channels: Int) -> Int {
        guard width > 0, height > 0 else { return unknownEstimateBytes }
        let (pixels, overflow) = width.multipliedReportingOverflow(by: height)
        guard !overflow else { return Int.max }
        let (withChannels, overflow2) = pixels.multipliedReportingOverflow(by: max(1, channels))
        guard !overflow2 else { return Int.max }
        let (withFactor, overflow3) = withChannels.multipliedReportingOverflow(by: safetyFactor)
        return overflow3 ? Int.max : max(1, withFactor)
    }

    static func channels(forExtension ext: String) -> Int {
        switch ext.lowercased() {
        case "jpg", "jpeg", "bmp": return 3
        // PNG / GIF / WebP / HEIC / AVIF / TIFF 都可能带 alpha。
        default: return 4
        }
    }

    /// 给一个文件记的内存账。尺寸读不出来时按"一张大图"兜底。
    static func estimate(path: String) -> Int {
        let ext = (path as NSString).pathExtension
        let channels = channels(forExtension: ext)
        guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil)
                as? [CFString: Any] else {
            return unknownEstimateBytes
        }
        let width = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue ?? 0
        let height = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue ?? 0
        return estimateBytes(width: width, height: height, channels: channels)
    }
}
