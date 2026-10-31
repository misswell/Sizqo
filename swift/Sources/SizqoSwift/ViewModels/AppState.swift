import Foundation
import AppKit
import SwiftUI
import UniformTypeIdentifiers

// MARK: - Cancel tracking (thread-safe, nonisolated)

final class CancelBox: @unchecked Sendable {
    private var paths: Set<String> = []
    private let lock = NSLock()
    func insert(_ p: String) { lock.lock(); paths.insert(p); lock.unlock() }
    func contains(_ p: String) -> Bool { lock.lock(); defer { lock.unlock() }; return paths.contains(p) }
    func removeAll() { lock.lock(); paths.removeAll(); lock.unlock() }
}

// MARK: - Theme

enum AppTheme: String, CaseIterable {
    case auto = "auto", light = "light", dark = "dark"
    var label: String {
        switch self {
        case .auto: return "自动（跟随系统）"
        case .light: return "亮色模式"
        case .dark: return "暗黑模式"
        }
    }
    var shortLabel: String {
        switch self {
        case .auto: return "自动"
        case .light: return "亮色"
        case .dark: return "暗黑"
        }
    }
    /// SizqoIcon glyph 名字（与前端 icon-theme-auto / icon-sun / icon-moon 同 id）。
    var iconName: String {
        switch self {
        case .auto: return SizqoIconName.themeAuto
        case .light: return SizqoIconName.sun
        case .dark: return SizqoIconName.moon
        }
    }
    func resolved() -> Bool { // true = dark
        switch self {
        case .dark: return true
        case .light: return false
        case .auto: return NSApp.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        }
    }
    func next() -> AppTheme {
        let all = AppTheme.allCases
        let idx = all.firstIndex(of: self)!
        return all[(idx + 1) % all.count]
    }
}

// MARK: - Sort Key

enum SortKey: String, CaseIterable {
    case importOrder = "import"
    case name = "name"
    case originalSize = "original"
    case compressedSize = "compressed"
    case ratio = "ratio"
    case status = "status"

    var label: String {
        switch self {
        case .importOrder: return "导入顺序"
        case .name: return "名称"
        case .originalSize: return "原始大小"
        case .compressedSize: return "压缩后大小"
        case .ratio: return "压缩比"
        case .status: return "状态"
        }
    }
}

// MARK: - App Page（rail 目的地：本轮 / 历史 / 设置；切换不销毁队列，也不打断压缩）

enum AppPage: String, CaseIterable {
    case main, history, settings

    var railLabel: String {
        switch self {
        case .main: return "本轮"
        case .history: return "历史"
        case .settings: return "设置"
        }
    }
}

// MARK: - AppState

@MainActor
final class AppState: ObservableObject {
    /// 全进程唯一的实例（init 时登记）。退出清理（AppDelegate）用它核对
    /// 批次/事务闸门 —— 另建实例会看不到同一个闸门上的状态。
    private(set) static var current: AppState?

    /// 退出清理是否允许执行：批次仍在跑或事务未收口时必须跳过（与 clearHistory
    /// 同一道判据），否则刚建好、还没写进历史的备份会被 sweep 当孤儿删掉。
    static func exitCleanupPermitted() -> Bool {
        guard let state = current else { return true }
        return !state.scheduler.isBatchActive && !state.transactions.hasPending()
    }

    // 队列
    @Published var items: [QueueItem] = []
    @Published var options = CompressOptions()
    @Published private(set) var isCompressing = false
    /// §3.4 人工恢复现场：启动恢复收不了口（外部修改/老日志无凭证/凭证不符）。
    @Published var manualRecoveries: [OutputTransactionStore.PendingRecoveryItem] = []
    /// 队列进度：**派生值**，不是某一轮的计数器。
    /// 停止之后重新开始一轮时，这一轮的目标变小了（只含 pending），
    /// 但用户看到的总进度不该从 40/100 掉回 0/60 —— 所以它属于队列，不属于批次。
    var compressProgress: Double { queueProgress.fraction }
    /// 当前这个按钮上该写什么（开始 / 继续 / 完成 / 处理完成·N 失败）。
    @Published private(set) var compressButtonText = ""

    // 意图栏 UI
    /// 当前图片意图；改动任何原始参数会切到 `.custom`（契约：intent-model.md）。
    @Published private(set) var intent: ImageIntent = .balanced
    /// 原始参数折叠区（默认折叠；HistoryStoreCheck [30] 钉住初值与"只由视图点击切换"）。
    @Published var settingsExpanded = false
    /// 已选中队列行的路径；详情栏据此画单件详情，nil 时画本轮汇总。
    @Published var selectedPath: String?
    /// 已选中历史记录 id（历史页同样是列表 + 详情两栏）。
    @Published var selectedHistoryID: String?
    /// 是否已导入过文件：空队列时工作列显示拖拽区
    @Published var hasImported = false
    @Published var autoCompress = false
    @Published var sortKey: SortKey = .importOrder
    @Published var sortAscending = true
    @Published var onlyShowFailed = false

    // 主题（入口在设置页「外观」小节）
    @Published var theme: AppTheme = .auto

    // Toast
    @Published var toastMessage: String?
    @Published var toastVisible = false

    // 版本
    @Published var appVersion: String = ""

    // 页面导航（主窗口内部视图，切换不销毁队列、不影响压缩）
    @Published var page: AppPage = .main

    // 批次阶段（idle / running / paused / stopping）：UI 按钮与文案的唯一依据。
    // 下面 isCompressing / compressionPaused 是它的派生镜像，**只有 setCompressionPhase 能写**，
    // 别处一律只读 —— 两个真相并存就是"暂停了但界面还在转"的来源。
    @Published private(set) var compressionPhase: CompressionPhase = .idle
    /// 清空队列后递增；旧 worker 的回调不能写入重新导入的同名文件。
    private var queueRevision = 0
    private var activeSessionID: UUID?
    var compressionPaused: Bool { compressionPhase == .paused }
    var compressionStopping: Bool { compressionPhase == .stopping }

    // 历史记录页 / 设置页
    @Published var historyEntries: [HistoryEntry] = []
    @Published var retentionDays = Retention.defaultDays
    // CPU 使用上限：nil = 自动。生效值还要按本机并行能力 clamp。
    @Published var cpuThreadLimit: Int? = nil

    // 持久层：压缩历史 + 原图备份（App Support，去留由保留档位决定）
    // 全进程共用一份实例：退出清理必须看得见启动时立起来的损坏锁死标记。
    let history = HistoryStore.shared
    /// replace 覆盖的记账凭证：崩溃后凭它把"覆盖了但历史没记下"的现场回滚成原图。
    let transactions = OutputTransactionStore(root: HistoryStore.appDataRoot())
    let settingsStore = SettingsStore()
    let cpuInfo = CpuInfo.detect()
    // 暂停和 CPU 上限是同一套闸门，不分两层锁。用 let 而不是 lazy var：
    // 压缩 worker 在别的线程上读它，lazy 的隐式初始化判定不是线程安全的。
    let scheduler: CompressionScheduler

    /// 本机真正生效的并行份数（换到核更少的机器会自动收敛）。
    var effectiveCpuThreadLimit: Int {
        CPULimit.effective(configured: cpuThreadLimit, detected: cpuInfo.budgetCeiling)
    }

    /// 队列摘要里的「CPU 4/10」。自动档不假装知道具体数字。
    var cpuSummaryText: String {
        CPUStatusText.summary(info: cpuInfo,
                              configured: cpuThreadLimit,
                              effective: effectiveCpuThreadLimit)
    }

    init() {
        let settings = settingsStore.load()
        retentionDays = settings.originalRetentionDays
        cpuThreadLimit = settings.cpuThreadLimit
        scheduler = CompressionScheduler(maxParallelism: CPULimit.effective(
            configured: settings.cpuThreadLimit, detected: cpuInfo.budgetCeiling))
        // 供 AppDelegate 在退出时核对批次/事务状态：全进程只有一个 AppState 实例，
        // 退出清理必须看得见这同一个实例上的闸门（在主线程读写）。
        AppState.current = self
        // 第一步先结清上次没走完的覆盖事务，**必须早于任何清理**：回滚要用的那份备份
        // 如果被清理当成孤儿扫掉，原图就真没了。
        let recovered = transactions.recover(history)
        if recovered.committed > 0 || recovered.rolledBack > 0 {
            NSLog("Sizqo 启动恢复: 补记事务 %d 次，自动恢复原图 %d 个文件",
                  recovered.committed, recovered.rolledBack)
        }
        for warning in recovered.warnings { NSLog("Sizqo 启动恢复未完成: %@", warning) }
        manualRecoveries = transactions.pendingRecoveryItems(history: history)
        // `history.json` 读不出来时：改名留档 + 按备份重建恢复入口 + 本次禁止清理。
        if let report = history.takeStartupReport() {
            if let path = report.quarantinedTo {
                NSLog("Sizqo 历史记录文件已损坏，现场留档在 %@", path)
            }
            if report.recoveredEntries > 0 {
                NSLog("Sizqo 从原图备份重建了 %d 条可恢复记录", report.recoveredEntries)
            }
        }
        // 启动不清理：清理只有一个时机，就是正常退出（见 AppDelegate）。崩溃现场那份
        // 备份可能是原图唯一的副本，"下次启动就删"恰好会在最需要它的时候动手。
        historyEntries = history.list()
    }

    // 自动压缩续队列
    private var pendingAutoCompress = false
    private let cancelBox = CancelBox()

    // MARK: - 页面导航

    func showPage(_ page: AppPage) {
        self.page = page
        if page == .history { refreshHistory() }
    }

    // MARK: - 历史记录页

    func refreshHistory() {
        historyEntries = history.list()
    }

    /// 历史页的「恢复原图」：路径全部从存储里读，前端不自己拼。
    func restoreHistoryEntry(id: String) {
        guard let entry = history.find(id) else {
            showToast("找不到这条历史记录")
            refreshHistory()
            return
        }
        restore(entry: entry)
    }

    /// 另存为：历史里的压缩结果就是磁盘上的一个文件，复制一份到用户挑的位置。
    func saveHistoryOutput(_ entry: HistoryEntry) {
        guard let outPath = entry.outputPath, fileExists(at: outPath) else {
            showToast("压缩结果已不存在")
            refreshHistory()
            return
        }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = (outPath as NSString).lastPathComponent
        guard panel.runModal() == .OK, let dest = panel.url else { return }
        showToast(atomicCopyFile(from: outPath, to: dest.path)
            ? "已保存到: \(dest.lastPathComponent)"
            : "保存失败，请重试")
    }

    /// 对比查看：备份还在就比备份（覆盖模式的真正原图），否则比源文件本身。
    func compareHistoryEntry(_ entry: HistoryEntry) {
        guard entry.status == .compressed || entry.status == .missing,
              let outPath = entry.outputPath, fileExists(at: outPath) else {
            showToast("压缩结果已不存在")
            refreshHistory()
            return
        }
        let result = CompressResult(historyEntry: entry)
        let original = (entry.backupPath.flatMap { fileExists(at: $0) ? $0 : nil }) ?? entry.sourcePath
        CompareWindowController.show(
            appState: self,
            originalPath: original,
            result: result,
            allResults: [result]
        )
    }

    /// 后缀 / 目录模式的对等「反悔」= 删掉这次生成的压缩结果。
    /// 删的是用户目录里的真实文件，所以必须二次确认；原图从头到尾没动过。
    func deleteHistoryOutput(_ entry: HistoryEntry) {
        guard !restoreBlockedBySession else {
            showToast(restoreBlockedText)
            return
        }
        guard let output = entry.outputPath else { return }
        let alert = NSAlert()
        alert.messageText = "删除这次压缩结果？"
        alert.informativeText = "将删除 \(output) 并移除这条历史记录。原图未被覆盖，不受影响。"
        alert.addButton(withTitle: "删除")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        restore(entry: entry)
    }

    /// 历史行的复制日志：记录里没有本批次的压缩参数快照，只报历史上记下来的那些事实。
    func copyHistoryLog(_ entry: HistoryEntry) {
        var lines: [String] = []
        lines.append("版本: Swift")
        lines.append("=== Sizqo 压缩历史 ===")
        lines.append("")
        lines.append("文件: \(entry.sourcePath)")
        lines.append("时间: \(historyTimeText(entry.createdAt))")
        lines.append("状态: \(historyStatusText(entry))")
        lines.append("")
        lines.append("--- 压缩结果 ---")
        lines.append("输出: \(entry.outputPath ?? entry.sourcePath)")
        lines.append("输出方式: \(entry.outputMode)")
        if entry.status == .recoveryAvailable {
            lines.append("明细: 已丢失（这条记录是按原图备份重建出来的）")
        } else {
            lines.append("原始大小: \(CompressResult.formatBytesJS(entry.originalSize)) (\(entry.originalSize) bytes)")
            lines.append("压缩后大小: \(CompressResult.formatBytesJS(entry.compressedSize)) (\(entry.compressedSize) bytes)")
            lines.append("压缩率: \(entry.savings >= 0 ? "-" : "+")\(String(format: "%.1f", abs(entry.savings)))%")
            lines.append("输出格式: \(entry.outType.isEmpty ? "(未知)" : entry.outType)")
            lines.append("算法: \(entry.algorithm.isEmpty ? "(未知)" : entry.algorithm)")
        }
        lines.append("")
        lines.append("--- 原图备份 ---")
        lines.append(entry.backupPath ?? "(无)")
        let text = lines.joined(separator: "\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        showToast("压缩日志已复制到剪贴板")
    }

    /// 清空历史 = 连原图备份一起删，是全 App 破坏性最大的操作：判据必须在后端，
    /// 不能指望前端把按钮禁用住。
    func clearHistory() {
        guard !historyEntries.isEmpty else { return }
        guard !scheduler.isBatchActive else {
            showToast("压缩进行中，无法清空历史记录")
            return
        }
        // 批次可能刚好结束、事务还挂着（比如 worker 被取消后迟到的写入）：
        // 这时清历史会连正在跑那批的备份一起删掉，比 batch active 更严格的判据。
        guard !transactions.hasPending() else {
            showToast("仍有文件事务正在处理，暂时无法清空历史记录")
            return
        }
        let alert = NSAlert()
        alert.messageText = "确定要清空 \(historyEntries.count) 条历史记录吗？"
        // 清空就是手动到期：备份立刻删掉，不用等保留期。说清楚删的是 Sizqo 的副本，
        // 以及这一次到底会带走几份 —— 「不会删除你的图片」必须是真的。
        let pendingBackups = historyEntries.filter { $0.backupExists }.count
        alert.informativeText = pendingBackups > 0
            ? "同时立即删除 Sizqo 保存的 \(pendingBackups) 份原图备份，不必等保留期到期。不会删除你的任何图片文件。"
            : "Sizqo 目前没有保存原图备份，这次只清记录。不会删除你的任何图片文件。"
        alert.addButton(withTitle: "清空")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        // §4.3 维护租约在弹窗确认**之后**抢：runModal 的嵌套 runloop 里可能已经
        // 开跑新一轮，抢不到租约就一个文件都不动（与 Rust clear_history 同一契约）。
        guard let lease = scheduler.beginMaintenance() else {
            showToast("压缩进行中，无法清空历史记录")
            return
        }
        defer { lease.release() }
        guard !transactions.hasPending() else {
            showToast("仍有文件事务正在处理，暂时无法清空历史记录")
            return
        }
        let report = history.clear()
        refreshHistory()
        var message = "已清空历史记录"
        if report.removedBackups > 0 { message += "，原图备份 \(report.removedBackups) 份已删除" }
        showToast(message)
        for warning in report.warnings { showToast(warning) }
    }

    func setRetentionDays(_ days: Int) {
        let clamped = Retention.clamp(days)
        retentionDays = clamped
        settingsStore.setRetentionDays(clamped)
        showToast(clamped == Retention.noRetain
            ? "原图备份改为不保留，关闭应用时清理记录和备份"
            : "原图备份保留 \(clamped) 天，关闭应用时清理过期项")
    }

    /// 改 CPU 上限：正在跑的任务不抢回来，只影响之后启动的新任务（与暂停同语义）。
    func setCpuThreadLimit(_ limit: Int?) {
        cpuThreadLimit = limit
        settingsStore.setCpuThreadLimit(limit)
        scheduler.setMaxParallelism(effectiveCpuThreadLimit)
        showToast(limit == nil
            ? "CPU 上限改为自动（\(effectiveCpuThreadLimit)）"
            : "CPU 上限改为 \(effectiveCpuThreadLimit) / \(cpuInfo.budgetCeiling)")
    }

    // MARK: - Display items (filter + sort)

    var displayItems: [QueueItem] {
        var list = items
        if onlyShowFailed {
            list = list.filter { $0.status == .failed }
        }
        list.sort { a, b in
            let result: Bool
            switch sortKey {
            case .importOrder:
                let ai = items.firstIndex(where: { $0.id == a.id }) ?? 0
                let bi = items.firstIndex(where: { $0.id == b.id }) ?? 0
                return sortAscending ? ai < bi : ai > bi
            case .name:
                result = a.fileName.localizedCompare(b.fileName) == .orderedAscending
            case .originalSize:
                let av = a.result?.originalSize ?? a.fileSize
                let bv = b.result?.originalSize ?? b.fileSize
                result = av < bv
            case .compressedSize:
                let av = a.result?.hasProduct == true ? a.result!.compressedSize : nil
                let bv = b.result?.hasProduct == true ? b.result!.compressedSize : nil
                if av == nil { return false }
                if bv == nil { return true }
                result = av! < bv!
            case .ratio:
                let av = a.result?.hasProduct == true ? a.result!.savings : nil
                let bv = b.result?.hasProduct == true ? b.result!.savings : nil
                if av == nil { return false }
                if bv == nil { return true }
                result = av! < bv!
            case .status:
                let order: [QueueStatus: Int] = [.failed: 0, .running: 1, .pending: 2, .done: 3, .restored: 4, .removed: 5]
                result = (order[a.status] ?? 9) < (order[b.status] ?? 9)
            }
            return sortAscending ? result : !result
        }
        return list
    }

    // MARK: - Stats

    /// 体积账只数**盘上真有产物**的那些项（§6.1）。"压成功但一个字节没写"的那种
    /// 以前会被记进账里，于是摘要说「省 0B · 2 个产物」而其中一张图根本没变过。
    var totalOriginal: Int64 {
        items.reduce(0) { sum, item in
            guard let r = item.result, r.hasProduct else { return sum }
            return sum + r.originalSize
        }
    }
    var totalCompressed: Int64 {
        items.reduce(0) { sum, item in
            guard let r = item.result, r.hasProduct else { return sum }
            return sum + r.compressedSize
        }
    }
    var totalSaved: Int64 { totalOriginal - totalCompressed }
    var totalSavingsPct: Double {
        guard totalOriginal > 0 else { return 0 }
        return Double(totalSaved) / Double(totalOriginal) * 100.0
    }
    var doneCount: Int { items.filter { $0.status == .done }.count }
    var failedCount: Int { items.filter { $0.status == .failed }.count }
    /// 只要有产物即可恢复（与 Tauri 展示「恢复全部原图」的条件一致）
    var hasRestorable: Bool { items.contains { $0.result?.hasProduct == true } }
    /// 还要压的文件数。失败的不算 —— 它们得用户显式点「重试」。
    var pendingCount: Int { items.filter { $0.status == .pending }.count }

    /// 队列进度的唯一算处（与 Tauri 前端 `getQueueProgress` 逐条对齐）。
    var queueProgress: QueueProgress { QueueProgress(items: items) }
    /// 队列规模：removed / restored 已经离开了这套账（一个被移出，一个被撤销）。
    var queueTotal: Int { queueProgress.total }
    /// 已处理 = 成功 + 失败。失败也算处理过，否则 98 成功 + 2 失败的队列
    /// 会永远停在 98%，那条进度条就成了假的。
    var processedCount: Int { queueProgress.processed }
    /// 摘要里的「42 / 100 已处理 · 失败 2」（失败为 0 时不写那一段）。
    var queueSummaryText: String { queueProgress.summaryText }

    /// 对比窗口要打开两个文件：没落盘的那些年"结果"不许进来（§68 与前端同一判据）。
    var comparableResults: [CompressResult] {
        items.compactMap(\.result).filter { $0.hasProduct }
    }

    // MARK: - Intent / 请求编译

    /// 意图编译后的实际发送请求（非 custom 意图把常量写回原始参数，屏幕所见即所发）。
    var compiledRequest: CompressOptions { intent.apply(to: options) }

    /// 智能路由：真实引擎行为（compressSmart 的候选搜索），由意图决定何时启用。
    var usesSmartRouting: Bool {
        options.processingMode == .advanced && intent.usesSmartRouting
    }

    /// 选中意图的屏幕摘要（详情栏空态与意图栏都用它）。
    var activeIntentSummary: String { intent.requestSummary(base: options) }

    /// 选择意图：把编译常量写回原始参数并持久化。
    func selectIntent(_ next: ImageIntent) {
        intent = next
        options = next.apply(to: options)
        saveSettings()
    }

    // 原始参数编辑：改一处即切 custom（契约：Editing any raw parameter switches intent to custom）。
    func setQuality(_ value: Int) {
        options.quality = min(max(value, 1), 100)
        markCustom()
    }

    func setOutputFormat(_ value: OutputFormat) {
        options.outputFormat = value
        markCustom()
    }

    func setMaxDimension(_ value: Int?) {
        options.maxDimensionPx = value.map { max($0, 1) }
        markCustom()
    }

    /// 预算是 targetSize 意图的输入而非原始参数：该意图下编辑保持意图不变。
    func setBudget(_ value: Int64?) {
        options.maxOutputBytes = value.map { max($0, 1) }
        if intent != .targetSize { markCustom() } else { saveSettings() }
    }

    private func markCustom() {
        intent = .custom
        saveSettings()
    }

    // MARK: - Version

    func detectVersion() {
        appVersion = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.0.0"
        loadSettings()
        loadTheme()
    }

    // MARK: - Theme

    /// 把主题落到 AppKit 外观上。**auto 必须是 nil**：nil 表示"跟随系统"，
    /// AppKit 会自动实时响应系统外观切换；若按当前系统解析成具体的
    /// aqua / darkAqua 再赋给 NSApp.appearance，就等于把自动固化成一张
    /// 启动时的快照 —— 系统换主题后应用不跟随（Tauri 线的 matchMedia
    /// 是实时跟随的，三线不一致）。
    private func applyThemeAppearance() {
        switch theme {
        case .auto: NSApp.appearance = nil
        case .light: NSApp.appearance = NSAppearance(named: .aqua)
        case .dark: NSApp.appearance = NSAppearance(named: .darkAqua)
        }
    }

    /// 主题设置入口在设置页「外观」小节（标题栏不再放切换按钮）。
    func setTheme(_ next: AppTheme) {
        guard next != theme else { return }
        theme = next
        saveTheme()
        applyThemeAppearance()
        showToast("主题: \(theme.shortLabel)")
    }

    private func loadTheme() {
        let raw = UserDefaults.standard.string(forKey: "sizqo.theme") ?? "auto"
        theme = AppTheme(rawValue: raw) ?? .auto
        applyThemeAppearance()
    }

    private func saveTheme() {
        UserDefaults.standard.set(theme.rawValue, forKey: "sizqo.theme")
    }

    // MARK: - Settings persistence

    func saveSettings() {
        let d = UserDefaults.standard
        d.set(options.processingMode.rawValue, forKey: "sizqo.setting-processingMode")
        d.set(options.systemImageSize.rawValue, forKey: "sizqo.setting-systemImageSize")
        d.set(options.preserveMetadata, forKey: "sizqo.setting-preserveMetadata")
        d.set(options.quality, forKey: "sizqo.setting-quality")
        d.set(options.outputFormat.rawValue, forKey: "sizqo.setting-outputFormat")
        d.set(options.outputMode.rawValue, forKey: "sizqo.setting-outputMode")
        d.set(options.outputSuffix, forKey: "sizqo.setting-outputSuffix")
        d.set(autoCompress, forKey: "sizqo.setting-autoCompress")
        d.set(intent.rawValue, forKey: "sizqo.setting-intent")
        // 0 = 不限制（UserDefaults 没有"缺省 nil"的标量写法）。
        d.set(options.maxDimensionPx ?? 0, forKey: "sizqo.setting-maxDimensionPx")
        d.set(options.maxOutputBytes ?? 0, forKey: "sizqo.setting-maxOutputBytes")
    }

    private func loadSettings() {
        let d = UserDefaults.standard
        if let raw = d.string(forKey: "sizqo.setting-processingMode"), let v = ProcessingMode(rawValue: raw) { options.processingMode = v }
        if let raw = d.string(forKey: "sizqo.setting-systemImageSize"), let v = SystemImageSize(rawValue: raw) { options.systemImageSize = v }
        if d.object(forKey: "sizqo.setting-preserveMetadata") != nil { options.preserveMetadata = d.bool(forKey: "sizqo.setting-preserveMetadata") }
        if d.object(forKey: "sizqo.setting-quality") != nil { options.quality = d.integer(forKey: "sizqo.setting-quality") }
        if let raw = d.string(forKey: "sizqo.setting-outputFormat"), let v = OutputFormat(rawValue: raw) {
            // §22：存回来的格式若这台机器已经写不出来（缺少对应编码器），退回原格式，
            // 而不是留一个菜单里选不中的值继续驱动请求。
            let allowed = options.processingMode == .system ? OutputFormat.systemTargets : OutputFormat.advancedTargets
            options.outputFormat = allowed.contains(v) ? v : .original
        }
        if let raw = d.string(forKey: "sizqo.setting-outputMode"), let v = OutputMode(rawValue: raw) { options.outputMode = v }
        if let raw = d.string(forKey: "sizqo.setting-outputSuffix"), !raw.isEmpty { options.outputSuffix = raw }
        autoCompress = d.bool(forKey: "sizqo.setting-autoCompress")
        if let raw = d.string(forKey: "sizqo.setting-intent"), let v = ImageIntent(rawValue: raw) { intent = v }
        // 尺寸/预算上限按持久化值恢复；意图非 custom/targetSize 时，请求里的常量以编译为准。
        if d.integer(forKey: "sizqo.setting-maxDimensionPx") > 0 { options.maxDimensionPx = d.integer(forKey: "sizqo.setting-maxDimensionPx") }
        if d.integer(forKey: "sizqo.setting-maxOutputBytes") > 0 { options.maxOutputBytes = Int64(d.integer(forKey: "sizqo.setting-maxOutputBytes")) }
        if intent == .targetSize, options.maxOutputBytes == nil {
            options.maxOutputBytes = ImageIntent.defaultBudgetBytes
        }
    }

    func resetSettings() {
        let keys = ["sizqo.setting-processingMode", "sizqo.setting-systemImageSize", "sizqo.setting-preserveMetadata",
                    "sizqo.setting-quality", "sizqo.setting-outputFormat", "sizqo.setting-outputMode",
                    "sizqo.setting-outputSuffix", "sizqo.setting-autoCompress", "sizqo.setting-intent",
                    "sizqo.setting-maxDimensionPx", "sizqo.setting-maxOutputBytes",
                    // 已删除的假控件残留：引擎从不读它们，重置时顺手从磁盘清掉。
                    "sizqo.setting-backend", "sizqo.setting-effort",
                    "sizqo.setting-convertToWebp", "sizqo.setting-smartMode",
                    "sizqo.setting-smartMode-direct-default-v1"]
        for key in keys { UserDefaults.standard.removeObject(forKey: key) }
        options = CompressOptions()
        autoCompress = false
        intent = .balanced
        saveSettings()
        showToast("已还原默认设置")
    }

    // MARK: - Toast

    func showToast(_ message: String) {
        toastMessage = message
        toastVisible = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) { [self] in
            toastVisible = false
        }
    }

    // MARK: - Import

    func addFiles(paths: [String]) {
        let incoming = uniqueFilePaths(paths)
        guard !incoming.isEmpty else { return }
        let expanded = expandImageFiles(paths: incoming)
        guard !expanded.isEmpty else {
            showToast("文件夹中没有找到可压缩的图片")
            return
        }
        let existing = Set(items.filter { $0.status != .removed && $0.status != .restored }.map(\.path))
        // §39 §40：先把这次导入的来源根收好，再逐项把「根 + 相对位置」写进队列项。
        var importRoots: [String] = []
        for root in incoming {
            var isDir: ObjCBool = false
            if FileManager.default.fileExists(atPath: root, isDirectory: &isDir), isDir.boolValue {
                let canonical = canonicalPath(root)
                if !importRoots.contains(canonical) { importRoots.append(canonical) }
            }
        }
        var added = 0
        for path in expanded where !existing.contains(path) {
            // 已移除 / 已恢复的行只是历史展示，重新加入时创建全新的 pending 行
            if let idx = items.firstIndex(where: { $0.path == path && ($0.status == .removed || $0.status == .restored) }) {
                items.remove(at: idx)
            }
            let size = (try? FileManager.default.attributesOfItem(atPath: path)[.size] as? Int64) ?? 0
            let parent = (path as NSString).deletingLastPathComponent
            // 目录导入 → 命中的那个来源根；单文件导入 → 它自己的父目录。
            let root = Self.deepestRoot(file: path, roots: importRoots) ?? parent
            let relative = Self.relativePathFromRoots(file: path, roots: [root])
                ?? (path as NSString).lastPathComponent
            items.append(QueueItem(
                path: path,
                fileName: (path as NSString).lastPathComponent,
                fileSize: size,
                sourceRoot: root,
                relativePath: relative
            ))
            added += 1
        }
        if added == 0 {
            hasImported = true
            showToast("所选图片已在队列中")
            return
        }
        hasImported = true
        // 自动压缩
        if autoCompress {
            if isCompressing {
                pendingAutoCompress = true
            } else {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [self] in
                    if autoCompress && !isCompressing { startCompress() }
                }
            }
        }
    }

    func addFilesPanel() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.allowedContentTypes = supportedExtensions.compactMap { UTType(filenameExtension: $0) }
        guard panel.runModal() == .OK else { return }
        addFiles(paths: panel.urls.map(\.path))
    }

    func addFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        guard panel.runModal() == .OK else { return }
        addFiles(paths: panel.urls.map(\.path))
    }

    func pickOutputDir() -> Bool {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            options.outputDir = url.path
            saveSettings()
            return true
        }
        return false
    }

    func clearQueue() {
        guard !items.isEmpty || isCompressing else { return }
        let alert = NSAlert()
        alert.messageText = "确定要清空全部 \(items.count) 个文件吗？"
        alert.addButton(withTitle: "清空")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        queueRevision += 1
        // 压缩中清空时先取消当前批次（与 Tauri clearAllFiles 语义一致）
        if isCompressing {
            // 只叫醒等待者，让它们看见"自己已被取消"；暂停状态不动，闸门不偷偷开。
            for item in items { cancelBox.insert(item.path) }
            scheduler.wakeWaiters()
        }
        items.removeAll()
        pendingAutoCompress = false
    }

    /// 移除队列中的一行（等待中/压缩中的用「移除」，已完成的不再显示移除按钮）
    func removeItem(path: String) {
        cancelBox.insert(path)
        // 堵在闸门上的 worker 得被叫醒才会发现"自己要处理的文件已经不在队列里"。
        scheduler.wakeWaiters()
        if let idx = items.firstIndex(where: { $0.path == path }) {
            guard items[idx].status == .pending else {
                // §7.3 运行中禁止移除：压缩引擎仍会执行完并落盘，"从队列隐藏"
                // 会把 UI 状态和真实执行状态撕开。前端同款语义（pending 才显示移除）。
                if items[idx].status == .running {
                    showToast("当前文件正在压缩，无法移除")
                }
                return
            }
            items[idx].status = .removed
        }
    }

    // MARK: - Batch compress

    /// 「开始压缩」/「继续压缩」—— 同一个入口。
    ///
    /// **只处理 pending**：已经压完的绝不重压，失败的要用户显式点「重试」，
    /// 否则一个坏文件会被「继续压缩 → 自动续队列 → 再失败」反复重试。
    /// 停止之后剩下的文件仍然是 pending，所以这个入口天然就是「继续压缩」。
    func startCompress() {
        guard !isCompressing else { return }
        if options.outputMode == .folder && options.outputDir == nil {
            showToast("请先选择输出目录")
            return
        }
        let pending = items.filter { $0.status == .pending }
        guard !pending.isEmpty else { return }
        runBatch(paths: pending.map(\.path))
    }

    /// 重试单个文件（与 Tauri compressOneFile 对齐：只处理该文件）。
    /// 失败 / 已恢复的行靠它回到 pending，普通「继续压缩」不碰这两种。
    ///
    /// §41：重试不换身份 —— 原地改同一个 QueueItem（id 不变），attemptCount 由
    /// 开工那一刻加一。§42：everProcessed 不清，所以「已处理」的分子不会倒退。
    func retryFile(path: String) {
        guard !isCompressing else { return }
        guard options.outputMode != .folder || options.outputDir != nil else {
            showToast("请先选择输出目录")
            return
        }
        guard let idx = items.firstIndex(where: { $0.path == path && ($0.status == .failed || $0.status == .restored) }) else { return }
        items[idx].status = .pending
        items[idx].result = nil
        runBatch(paths: [path])
    }

    /// 摘要里跟着阶段走的那一小段（与 Tauri 的 compressionStateSuffix 逐字一致）。
    var compressionPhaseSummary: String {
        guard isCompressing else { return "" }
        switch compressionPhase {
        case .paused: return " · 已暂停"
        case .stopping: return " · 正在停止"
        default: return ""
        }
    }

    /// 批次阶段的唯一写入口：镜像（isCompressing / compressionPaused）都从这里派生。
    /// 别处直接写 isCompressing 就会又出现"两个真相"。
    private func setCompressionPhase(_ next: CompressionPhase) {
        compressionPhase = next
        isCompressing = next != .idle
        updateCompressButtonText()
    }

    /// 跑一轮执行会话：**只处理传进来的这些文件**（调用方只提交 pending）。
    ///
    /// 这里绝不重置任何进度：总体进度是队列的派生值（`compressProgress`），
    /// 停止 / 继续都不会把它清零 —— 老实现每次进这里都写一遍
    /// `compressTotal = paths.count; compressCurrent = 0`，于是"停止后继续"
    /// 会把 40/100 变成 0/60，那是用户最直接能看到的那种错。
    private func runBatch(paths: [String]) {
        guard !paths.isEmpty else { return }

        let revision = queueRevision
        let targets = paths.compactMap { path -> (path: String, itemID: UUID, relativePath: String)? in
            guard let item = items.first(where: { $0.path == path && $0.status == .pending }) else { return nil }
            return (path, item.id, item.relativePath)
        }
        guard !targets.isEmpty else { return }

        // §37 §38：一轮 session 只有一个入口，抢不到就说明已经有一轮在跑。
        // 抢失败时**什么都不写**（会话号、按钮、取消账本一个都不动）—— 并跑第二轮的话，
        // 先到那轮的收尾会把后一轮的暂停 / 停止 / 取消全清零，那正是"暂停失效、进度乱跳"。
        guard scheduler.tryBeginBatch(maxParallelism: effectiveCpuThreadLimit) else {
            showToast("已有一轮压缩正在运行，请等它结束或先停止")
            return
        }
        let sessionID = UUID()
        activeSessionID = sessionID

        setCompressionPhase(.running)
        compressButtonText = ""
        cancelBox.removeAll()

        // 会话快照：意图编译后的请求。运行中改意图只影响下一轮（契约：running session
        // keeps the snapshot it captured）。质量/尺寸/预算都来自意图编译，没有第二处真相。
        let opts = compiledRequest
        let useSmart = usesSmartRouting

        let group = DispatchGroup()
        let queue = DispatchQueue(label: "sizqo.compress", attributes: .concurrent)
        let gate = scheduler
        let store = history
        let journal = transactions
        let retention = retentionDays

        for (path, itemID, relativePath) in targets {
            group.enter()
            queue.async { [self] in
                // 每条出口都必须 leave 一次，否则被取消的文件会把整批吊住。
                defer { group.leave() }
                // 暂停中或 CPU 名额已满就堵在这里；拿到名额的一刻闸门一定是开着的。
                // 已经在跑的文件会正常完成（不 kill、不 SIGSTOP）。
                // 被取消的文件在闸门**之前**就自己退出，不需要谁替它开门。
                switch gate.acquireOutcome(cancelled: { [self] in self.isCancelled(path) }) {
                case .cancelled:
                    // 用户明确把它移出了队列：它本轮到此为止，不回到 pending。
                    DispatchQueue.main.async { self.updateStatus(path, itemID: itemID, revision: revision, sessionID: sessionID, .removed) }
                    return
                case .stopped:
                    // 整批被停止，这一轮没轮到它：**什么都不做**，队列里它仍然是 pending，
                    // 下一次「继续压缩」自然带上。绝不写成 .cancelled（那就是"再也压不动"）。
                    return
                case .acquired(let permit):
                    defer { permit.release() }
                    // 过闸之后、开工之前还有一个窗口：用户在此刻把它移出队列的话，
                    // 不许照样压完落盘 —— 磁盘与历史都不该记下一份用户已经不要的结果。
                    if self.isCancelled(path) {
                        DispatchQueue.main.async { self.updateStatus(path, itemID: itemID, revision: revision, sessionID: sessionID, .removed) }
                        return
                    }
                    // 「压缩中」只在真正开工这一刻标记，等待中的行保持「等待」。
                    DispatchQueue.main.async {
                        if self.queueRevision == revision, self.activeSessionID == sessionID,
                           let idx = self.items.firstIndex(where: { $0.id == itemID && $0.path == path }),
                           self.items[idx].status == .pending {
                            self.items[idx].status = .running
                            // §41：attemptCount 只在真正开工时加一；重试沿用同一个项 / id。
                            self.items[idx].attemptCount += 1
                        }
                    }
                    let result = Self.compressOneStatic(
                        path: path, relativePath: relativePath, options: opts, useSmart: useSmart,
                        history: store, transactions: journal, retentionDays: retention
                    )
                    DispatchQueue.main.async { self.applyResult(path, itemID: itemID, revision: revision, sessionID: sessionID, result: result) }
                }
            }
        }

        group.notify(queue: .main) { [self] in
            gate.endBatch()
            if activeSessionID == sessionID { activeSessionID = nil }
            // 用户按过停止：这一轮就地结束，绝不许被"自动压缩"再拉起来跑下一轮。
            let stoppedByUser = compressionStopping
            setCompressionPhase(.idle)
            // 按钮写什么看**队列**：还剩 pending 就是「继续压缩」，
            // 全压完才是完成文案。老实现一律写「压缩完成」，停止之后明明还剩
            // 58 张也照写不误 —— 那是把"这一轮停了"说成"队列干完了"。
            updateCompressButtonText()
            cancelBox.removeAll()
            refreshHistory()
            // 自动续队列：只续 pending（失败的重试要用户自己点）。
            let stillPending = items.contains { $0.status == .pending }
            if pendingAutoCompress && stillPending && !stoppedByUser {
                pendingAutoCompress = false
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { [self] in
                    self.startCompress()
                }
            } else {
                pendingAutoCompress = false
            }
        }
    }

    /// 主按钮的文案 —— 与 Tauri 前端 `renderStartButton` 同一套判据：
    /// 会话在跑就写阶段；跑完了则看队列还剩什么。
    func updateCompressButtonText() {
        if isCompressing {
            switch compressionPhase {
            case .paused: compressButtonText = options.processingMode == .system ? "暂停中…" : "暂停中…"
            case .stopping: compressButtonText = "正在停止…"
            default: compressButtonText = options.processingMode == .system ? "转换中…" : "压缩中…"
            }
            return
        }
        if pendingCount > 0 && processedCount > 0 {
            compressButtonText = options.processingMode == .system ? "继续转换" : "继续压缩"
            return
        }
        if pendingCount > 0 {
            compressButtonText = options.processingMode == .system ? "开始转换" : "开始压缩"
            return
        }
        if queueTotal > 0 && failedCount > 0 {
            compressButtonText = "处理完成 · \(failedCount) 个失败"
            return
        }
        compressButtonText = queueTotal > 0
            ? (options.processingMode == .system ? "转换完成" : "压缩完成")
            : (options.processingMode == .system ? "开始转换" : "开始压缩")
    }

    /// 暂停 / 继续：闸门只决定「要不要再启动新文件」，绝不打断正在跑的压缩。
    /// 停止在收尾途中不许再切换暂停 —— 闸门已经焊死，按下去只会让文案和真相分叉。
    func togglePause() {
        guard isCompressing, !compressionStopping else { return }
        if compressionPaused {
            scheduler.resume()
            setCompressionPhase(.running)
            showToast("继续压缩")
        } else {
            scheduler.pause()
            setCompressionPhase(.paused)
            showToast("已暂停，正在压缩的文件会先完成")
        }
    }

    /// 停止整批：等待中的文件全部作废，已经在压的几个跑完各自收尾。
    ///
    /// 与「取消全部」的区别是层次：`cancelAll` 是文件级的（逐条记账），
    /// 停止是批次级的 —— 闸门对还没开始的文件永久关闭，后来者自己就退了，
    /// 不需要谁替它记账。绝不 kill 正在跑的进程。
    func stopBatch() {
        guard isCompressing, !compressionStopping else { return }
        // 用户刚说"停下"，再被"自动压缩"拉起来就是没听他说话。
        pendingAutoCompress = false
        setCompressionPhase(.stopping)
        scheduler.stop()
        scheduler.wakeWaiters()
        showToast("正在停止：正在处理的文件会先完成，其余文件保留在队列中")
    }

    /// 取消全部：把还在等 / 正在跑的行都移出队列（与 Tauri 的 removed 一致）。
    /// 这是**文件级**的破坏性操作，与「停止」不同 —— 停止是保留剩余任务的。
    func cancelAll() {
        for i in items.indices {
            if items[i].status == .pending || items[i].status == .running {
                cancelBox.insert(items[i].path)
                items[i].status = .removed
            }
        }
        // 先记账再叫醒等待者；暂停状态不动 —— 取消不是「继续」。
        scheduler.wakeWaiters()
    }

    func cancelFile(path: String) {
        cancelBox.insert(path)
        if let idx = items.firstIndex(where: { $0.path == path }),
           items[idx].status == .running || items[idx].status == .pending {
            items[idx].status = .removed
        }
        scheduler.wakeWaiters()
    }

    nonisolated private func isCancelled(_ path: String) -> Bool {
        cancelBox.contains(path)
    }

    private func updateStatus(_ path: String, itemID: UUID, revision: Int, sessionID: UUID, _ status: QueueStatus) {
        guard queueRevision == revision, activeSessionID == sessionID else { return }
        if let idx = items.firstIndex(where: { $0.id == itemID && $0.path == path && $0.status == .pending }) {
            items[idx].status = status
        }
    }

    private func applyResult(_ path: String, itemID: UUID, revision: Int, sessionID: UUID, result: CompressResult) {
        guard queueRevision == revision, activeSessionID == sessionID,
              let idx = items.firstIndex(where: { $0.id == itemID && $0.path == path && $0.status == .running }) else { return }
        items[idx].result = result
        // §2 §67 生命周期由结论派生：跳过、取消、未达目标都是"这一轮处理完了"（done），
        // 只有 outcome=failed 才是 Failed。
        items[idx].status = result.contractQueueStatus
        // §42：生命周期走完就落闩锁，之后的「重试」只改当前状态，不让进度倒退。
        // 没收到开工事件时也补一个计数，保证「尝试过」至少是 1。
        items[idx].everProcessed = true
        if items[idx].attemptCount == 0 { items[idx].attemptCount = 1 }
    }

    // MARK: - Single compress

    nonisolated private static func compressOneStatic(
        path: String, relativePath: String, options: CompressOptions, useSmart: Bool,
        history: HistoryStore, transactions: OutputTransactionStore, retentionDays: Int
    ) -> CompressResult {
        let originalSize = (try? FileManager.default.attributesOfItem(atPath: path)[.size] as? Int64) ?? 0
        // §24 §25 覆盖模式的原图快照必须在**编码之前**落好：编码要跑几秒到几十秒，
        // 期间用户完全可能又改了这张图。到 Commit 才备份，备份到的就是改完的那份，
        // 「恢复原图」从此恢复不出用户认得出的东西。非覆盖模式一次磁盘写都不做。
        let baseline = captureReplaceBaseline(path: path, options: options, history: history)
        let started = Date()
        let engineResult = useSmart
            ? CompressionEngine.compressSmart(file: path, options: options)
            : CompressionEngine.compress(file: path, options: options)
        let elapsedMs = Int(-started.timeIntervalSinceNow * 1000)
        var result = CompressResult(engine: engineResult, file: path, originalSize: originalSize, options: options)
        result.elapsedMs = elapsedMs
        // 源/输出像素尺寸：读得出就报，读不出如实缺省，不猜。
        if let dims = imagePixelDimensions(file: path) {
            result.sourceWidth = dims.width; result.sourceHeight = dims.height
        }
        if let dims = imagePixelDimensions(data: engineResult.compressed) {
            result.outputWidth = dims.width; result.outputHeight = dims.height
        }
        // 诚实规则 3：重编像素的意图照实说明 EXIF/GPS 不带入；系统转换才是保留元数据的路径。
        if options.processingMode == .advanced, engineResult.success,
           engineResult.compressed.count != Int(originalSize) {
            result.warnings.append("EXIF / GPS 等元数据不会带入输出（需保留请用系统转换）")
        }

        // §10 §11 「这些字节值不值得落盘」只在这里回答一次， writer 不再自己猜。
        let evaluation = Evaluation.evaluate(
            verdict: engineResult.verdict,
            engineReason: engineResult.reason,
            engineSuccess: engineResult.success,
            encodedBytes: engineResult.compressed.count,
            originalSize: Int(originalSize),
            formatConversion: options.outputFormat != .original,
            targetMet: engineResult.targetMet
        )
        let beforeWrite = evaluation.preWriteOutcome
        if let outcome = beforeWrite.outcome {
            // 不提交的一支：一个字节都不许写、历史一条都不许记。
            result.outcome = outcome
            result.reasonCode = beforeWrite.reason
            result.outputPath = nil
            result.compressedSize = originalSize
            result.savings = 0
            return publish(result, mediaKind: "image")
        }

        if let failure = writeOutputStatic(
            &result, compressed: engineResult.compressed, relativePath: relativePath, options: options,
            history: history, transactions: transactions, retentionDays: retentionDays,
            baseline: baseline
        ) {
            // 落盘没成就是没成：UI 绝不许显示「压缩完成」。
            result.outputPath = nil
            result.backupPath = nil
            result.error = failure.userMessage
            result.outcome = .failed
            result.reasonCode = failure.reasonCode
            return publish(result, mediaKind: "image")
        }
        let published = evaluation.committedOutcome
        result.outcome = published.outcome
        result.reasonCode = published.reason
        return publish(result, mediaKind: "image")
    }

    /// §141 发布前的最后一道核对：结论与字段必须自洽。
    /// 违反它的是后端 bug，不是措辞 —— 宁可降级成 failed 把问题说出来，也不许让一个
    /// 「说自己有产物却指向空气」的结果进到界面（那正是这次重构要根除的那类 bug）。
    ///
    /// 它只读参数本身，不碰 `AppState` 的任何状态，所以能留在 worker 线程上跑。
    nonisolated private static func publish(_ result: CompressResult, mediaKind: String) -> CompressResult {
        guard let problem = invariantViolation(
            mediaKind: mediaKind, file: result.file,
            outcome: result.outcome, outputPath: result.outputPath
        ) else { return result }
        var degraded = result
        degraded.outputPath = nil
        degraded.outcome = .failed
        degraded.reasonCode = .unknown
        // 原图到底动没动，走到这一步我们并不知道，绝不许替它保证。
        degraded.error = "\(problem)；这次结果已按失败处理"
        return degraded
    }

    /// 落盘 + 记历史，与 Tauri 的 commit_output 同一套顺序：
    ///
    /// ```text
    /// ① 完整复核源文件（stat + 内容哈希）  ② 独占目标名 + 独占临时名  ③ 写 transaction
    /// ④ 写满候选 → flush + fsync  ⑤ 改名前再核一次指纹 + 确认备份字节对得上
    /// ⑥ rename 覆盖目标  ⑦ history.add  ⑧ 销账
    /// ```
    ///
    /// 原图快照**不在这里做**（§24）：它已经在编码前由 `captureReplaceBaseline` 备好，
    /// 这里只负责复核它。⑦ 失败才回滚；①②③④⑤⑥ 失败只撤回我们自己的临时文件和占位名
    /// —— 那些时刻用户的文件还完好在原地，没动过的东西不许假装恢复（§34 同一条道理）。
    ///
    /// 返回 nil = 一切正常，否则返回要报给用户的失败原因。
    ///
    /// 「这些字节值不值得落盘」不属于这里 —— 那由 `Evaluation` 在调用之前就答完了，
    /// 走到这里的只有 Commit 一支。以前它自己再看一遍体积、顺手写一句「原图已是最优」
    /// 然后返回 nil，于是 success 还是 true、产物却一个没有。
    nonisolated private static func writeOutputStatic(
        _ result: inout CompressResult, compressed: Data, relativePath: String, options: CompressOptions,
        history: HistoryStore, transactions: OutputTransactionStore, retentionDays: Int,
        baseline: ReplaceBaseline
    ) -> OutputWriteError? {
        if compressed.isEmpty {
            return .outputWriteFailed("要写出的字节是空的")
        }
        let fm = FileManager.default
        let path = result.file
        // 目标位置的扩展名以**引擎交回的字节格式**为准（§10）。
        let outType = result.outType
        var backupFile: String?
        // 只有覆盖模式才有"要复核的对象"；后缀/目录模式的原图从头到尾没被碰过。
        var replaceIdentity: SourceIdentity?
        let target: String
        let placement: OutputPlacement

        // §29 先定落盘模式，再由这一个决定派生"要不要备份""名字怎么来""事务算哪一类"。
        // 拿到手的目标名已经是独占申请到的（同格式覆盖除外：它的目标本来就是用户那个文件）。
        switch options.outputMode {
        case .replace:
            // §25 覆盖的依据必须在场：读不到源文件指纹、或编码前的快照没写成，
            // 就一个字节都不许碰用户文件。原因由 Publish 统一发布成对应结论。
            let identity: SourceIdentity
            let backup: String
            switch baseline {
            case .ready(let value, let snapshot): identity = value; backup = snapshot
            case .unavailable(let error): return error
            case .notReplace:
                return .backupFailed("覆盖模式没有编码前留下的原图快照")
            }
            // ①复核放在占名之前：判定一旦变成"文件已经不是你压缩的那份了"，
            //   用户目录里连一个多余文件都不该出现过（§25「删除 candidate」）。
            //   这里必须是完整 `verify`：改名前那道快速门看不出"同大小、时间戳没动"
            //   的原地改动，而那种改动恰恰是最需要拦的。
            if case .changed(let detail) = identity.verify(at: path) {
                return .sourceChanged(detail)
            }
            backupFile = backup
            result.backupPath = backup
            replaceIdentity = identity
            // 字节属于哪种格式，文件就得叫什么扩展名：把 JPEG 字节塞进 photo.gif
            // 得到的是一个两边都打不开的"压缩成功"产物。判据只看字节格式，不看用户的
            // 意图 —— 系统模式在 outputFormat=原格式 时也会把 GIF/WebP 转成 JPEG。
            if sameOutputFormat(path: path, outType: outType) {
                target = path
                placement = .replaceExisting(crossFormat: false)
            } else {
                // 跨格式转换的产物是新文件，但名字照样要独占申请：photo.webp 可能是
                // 邻居的成品，"转换"把它盖掉就不是转换而是毁掉别人的东西（§28）。
                guard let claimed = OutputNameAllocator.claim(
                    in: outputDirectory(of: path),
                    .converted(stem: fileStem(of: path), extension: outType)
                ) else {
                    return .collision(outputDirectory(of: path))
                }
                target = claimed.path
                placement = .replaceExisting(crossFormat: true)
            }

        case .suffix:
            guard let claimed = OutputNameAllocator.claim(
                in: outputDirectory(of: path),
                .withSuffix(stem: fileStem(of: path),
                            suffix: normalizedOutputSuffix(options.outputSuffix),
                            extension: outType)
            ) else {
                return .collision(outputDirectory(of: path))
            }
            target = claimed.path
            placement = .createNew

        case .folder:
            // 评估说"该落盘"，输出方式却给不出目标位置（folder 没选目录）。这不是跳过，
            // 是配置没兑现：以前这里 `return nil`，用户看到「压缩完成」而磁盘上什么都没有。
            guard let outDir = options.outputDir, !outDir.isEmpty else {
                return .invalidDestination("请先选择输出目录")
            }
            // §39 §40：产物在输出目录里的层级只读队列项入队时定死的相对路径。
            let rel = relativePath.isEmpty ? (path as NSString).lastPathComponent : relativePath
            let relDir = (rel as NSString).deletingLastPathComponent
            let dir = relDir.isEmpty ? outDir : (outDir as NSString).appendingPathComponent(relDir)
            // 子目录必须先存在：否则独占申请报出来的是"找不到目录"，
            // 和用户能理解的毛病（输出目录还没建）对不上。
            do {
                try fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
            } catch {
                return .outputWriteFailed("\(dir): \(error.localizedDescription)")
            }
            let stem = ((rel as NSString).deletingPathExtension as NSString).lastPathComponent
            guard let claimed = OutputNameAllocator.claim(
                in: dir, .withSuffix(stem: stem, suffix: "", extension: outType)
            ) else {
                return .collision(dir)
            }
            target = claimed.path
            placement = .createNew
        }
        let crossFormat = placement.crossFormat
        // §3.1 占位凭证：独占申请来的 0 字节占位的 (dev,ino)。崩溃恢复时
        // "这个空文件是我们的占位"全靠它证明，没有凭证的空文件一律不删。
        let placeholderIdentity: (device: Int64, inode: UInt64)?
        if placement.crossFormat || placement.transactionMode == .createNew {
            placeholderIdentity = OutputTransactionStore.fileIdentity(target)
        } else {
            placeholderIdentity = nil
        }

        // ① 事务 id 必须在动手之前定下来：历史落盘成功与否就靠它和 transaction 对账。
        //    两种模式都要记账 —— 新建模式那份日志负责"证明不了提交就删掉自己生成的文件"（§32）。
        let historyId = HistoryEntry.newId(forSource: path)

        // ② 临时名先独占、再记账：日志必须在一个字节落盘之前就知道它叫什么。反过来做的话，
        //    崩在"写完临时文件"与"改名"之间时，那个 .tmp 是唯一知道自己在场的记录还没写。
        guard let staged = StagedWrite.claim(target: target) else {
            releaseOutputPlaceholder(target, source: path)
            return .outputWriteFailed("无法在 \(target) 旁边独占一个临时文件名")
        }
        let txn = ReplaceTransaction(
            id: historyId,
            historyId: historyId,
            sourcePath: canonicalPath(path),
            outputPath: target,
            backupPath: backupFile ?? "",
            tempOutputPath: staged.temp,
            originalSize: result.originalSize,
            expectedOutputSize: Int64(compressed.count),
            // §2.5：事务必须带着身份证据落账。original 来自编码前钉下的源指纹，
            // output 来自还没落盘的产物字节 —— 启动恢复靠这两个哈希证明归属，
            // 证明不了就不会拿备份去盖一个被外部改过的文件。
            originalSha256: replaceIdentity?.sha256,
            outputSha256: sha256OfData(compressed),
            // §3.1 备份凭证：来历记录里有就抄，老备份没有记录就现场算一份。
            // 覆盖事务缺这份凭证会在 prepare 处被拒绝落账。
            backupSha256: backupFile.flatMap { history.recordedBackupSha256(of: $0) },
            placeholderIdentity: placeholderIdentity,
            createdAt: SizqoClock.nowMillis,
            crossFormat: crossFormat,
            mode: placement.transactionMode,
            // §2.3 可信度随事务走：LegacyUnverified 的备份绝不能被自动回滚
            // 当成可信原图 —— 标记进日志，恢复侧据此转人工。
            backupLegacyUnverified: backupFile.map { history.backupVerification($0) == .legacyUnverified } ?? false
        )
        // ③ 记账先于动手：写不进日志就还不许碰用户文件，占好的名字也得收回去。
        if let detail = placement.journal(transactions, txn) {
            staged.discard()
            releaseOutputPlaceholder(target, source: path)
            return .transactionFailed(detail)
        }

        // 改名**之前**失败都只做一件事：把这次自己造出来的东西收回去。用户的文件一个字
        // 没动，所以绝不 rollback —— 把快照写回去反而会把用户在此期间的新改动盖掉（§25）。
        func withdraw(_ error: OutputWriteError) -> OutputWriteError {
            // 先删临时文件、后销账：反过来的话，两步之间崩溃会在用户图片目录里留下
            // 一个没有任何 journal 知道它在场的 .tmp 孤儿。反过来最坏只是 journal
            // 多留一拍 —— 下次启动的恢复按幂等语义收掉，磁盘不会多东西。
            staged.discard()
            transactions.finish(txn.id)
            releaseOutputPlaceholder(target, source: path)
            return error
        }

        // ④ 候选写满 → flush + fsync。到这一步结束，目标文件依然没被动过一个字节。
        if !staged.writeSynced(compressed) {
            return withdraw(.outputWriteFailed("无法写入 \(target)"))
        }

        // ⑤ 改名前的最后两道门（§26「最终 Commit 前必须确认当前 source == 本次压缩所
        //    基于的 source」+ §31 的 backup check）。快速门只 stat 不读内容：改名之后
        //    就是用户的文件了，来不及反悔，所以这一道必须便宜到任何时候都跑得动。
        //    备份则按 §27 验字节 —— 覆盖之后它是原图唯一的副本。
        if let identity = replaceIdentity, let backup = backupFile {
            // §26：终检用完整 verify —— 快速门看不出"同大小、同修改时间"的原地改动，
            // 宁可多读一遍源文件，也不能把用户刚改过的内容盖掉（与 Rust 线同一标准）。
            if case .changed(let detail) = identity.verify(at: path) {
                return withdraw(.sourceChanged(detail))
            }
            if let detail = history.backupIsFaithful(backup) {
                return withdraw(.backupFailed(detail))
            }
        }

        // ⑥ 原子发布。占位凭证在场时走 swap 验证：先交换、再确认被换出来的
        // 是我们认领的那个 inode —— 外部在提交前一刻放进目标路径的内容绝不被盖。
        if !staged.publishIfPlaceholderMatches(to: target, placeholderIdentity: placeholderIdentity) {
            return withdraw(.outputWriteFailed("无法改名到 \(target)（占位所有权验证未通过，目标内容已保留）"))
        }

        if crossFormat {
            do { try fm.removeItem(atPath: path) }
            catch {
                // 删不掉（权限/被占用）时不许静默：磁盘上会同时留着原图和新格式产物。
                // 产物已提交、历史已记，不能反过来撤销一次成功的压缩 —— 留警告给用户。
                NSLog("Sizqo 跨格式替换：原文件删除失败（产物已保留）: %@", error.localizedDescription)
                result.warnings.append("原格式文件删除失败，已保留: \(path)")
            }
        }
        result.outputPath = target
        result.outputMode = options.outputMode.rawValue

        // 输出确认落盘之后才记历史：历史里绝不出现没写成的文件。
        let entry = HistoryEntry.record(
            withId: historyId, source: path, result: result, output: target,
            backup: backupFile, retentionDays: retentionDays
        )
        if history.add(entry) {
            // ⑦ 历史已经落盘 = 这次覆盖有据可查，⑧ 销账。留着下次启动会误判成中断事务。
            transactions.finish(txn.id)
            return nil
        }
        // ⑦ 失败 = 这次落盘不能算数。覆盖模式用备份把真正原图写回去；新建模式从没动过
        //    原图，它删掉自己生成的那个文件（§32「磁盘没有 orphan output」）。
        if let rollbackError = OutputTransactionStore.rollback(txn) {
            // 回滚也没成：事务日志必须留着，下次启动的 recovery 再试一次。
            return .rollbackFailed(rollbackError)
        }
        transactions.finish(txn.id)
        return .historyWriteFailed
    }

    // MARK: - Restore（统一服务：主队列 / 历史页 / 对比窗口 / 恢复全部 都走 HistoryStore.restore）

    /// §44 压缩会话与历史管理是两条生命周期：这一轮没结束前恢复会改写正在被压缩的
    /// 源文件，进度还会从「5 / 10」倒成「4 / 9」。所有恢复入口共用这一道闸。
    var restoreBlockedBySession: Bool { isCompressing }
    var restoreBlockedText: String { "压缩进行中，暂不能恢复；请先停止或等这一轮结束" }

    /// 历史行里依赖「当前没有压缩会话」的动作：恢复与删除结果都会改写源文件或产物。
    func historyActionBlocked(_ action: HistoryRowAction) -> Bool {
        restoreBlockedBySession && (action == .restore || action == .deleteOutput)
    }

    // MARK: §3.4 人工恢复现场

    /// 刷新人工恢复现场清单（恢复处置后调用）。
    func refreshManualRecoveries() {
        manualRecoveries = transactions.pendingRecoveryItems(history: history)
    }

    /// 人工恢复处置：restore=false = 暂不处理（现场原样保留）；
    /// restore=true 必须由界面先展示双方状态并拿到 confirmOverwrite 确认。
    @discardableResult
    func resolveManualRecovery(_ item: OutputTransactionStore.PendingRecoveryItem,
                               restore: Bool, confirmOverwrite: Bool) -> String? {
        guard let lease = scheduler.beginMaintenance() else {
            return "压缩进行中，暂不能处理恢复现场"
        }
        defer { lease.release() }
        guard let txn = transactions.list().first(where: { $0.historyId == item.historyId }) else {
            refreshManualRecoveries()
            return "这条恢复现场不存在（可能已被处理）"
        }
        if !restore {
            refreshManualRecoveries()
            return nil
        }
        if let error = transactions.manualRestore(txn, confirmOverwrite: confirmOverwrite) {
            refreshManualRecoveries()
            return error
        }
        transactions.finish(txn.id)
        refreshManualRecoveries()
        return nil
    }

    /// 恢复一条历史记录。压缩之后又被外部改过时先问用户，坚持才 force 覆盖；
    /// 备份来自旧版本（§2.3）时单独确认，确认也不绕过备份字节验证。
    /// 所有路径都由历史记录提供，调用方不参与拼路径。
    @discardableResult
    func restore(entry: HistoryEntry) -> Bool {
        guard !restoreBlockedBySession else {
            showToast(restoreBlockedText)
            return false
        }
        // §4.3 维护租约：与会话启动在同一把条件锁上仲裁，恢复与压缩不可能各拿
        // 一半检查再各走各的。租约持有到本次恢复结束（RAII），弹窗期间也在场。
        guard let lease = scheduler.beginMaintenance() else {
            showToast(restoreBlockedText)
            return false
        }
        defer { lease.release() }
        var outcome = history.restoreOutcome(entry: entry, force: false)
        if outcome.conflict {
            let alert = NSAlert()
            alert.messageText = "这个文件在压缩后又被修改过。"
            // 覆盖模式恢复 = 写回原图（覆盖改动）；非覆盖模式撤销 = 删产物（丢弃改动）。
            // 两种动作丢的东西不一样，提示必须照实说。
            alert.informativeText = entry.outputMode == "replace"
                ? "恢复原图会覆盖当前版本。"
                : "删除这次压缩结果会丢弃你在产物上做的修改。"
            alert.addButton(withTitle: "仍然继续")
            alert.addButton(withTitle: "取消")
            guard alert.runModal() == .alertFirstButtonReturn else { return false }
            // 模态期间租约一直在场：批次开不起来，不需要再核会话闸门。
            outcome = history.restoreOutcome(entry: entry, force: true)
        }
        if outcome.legacyBackup {
            let alert = NSAlert()
            alert.messageText = "这份备份来自旧版本，缺少内容指纹。"
            alert.informativeText = "Sizqo 无法验证它的完整性。是否仍要尝试用这份备份恢复原图？"
            alert.addButton(withTitle: "仍要恢复")
            alert.addButton(withTitle: "取消")
            guard alert.runModal() == .alertFirstButtonReturn else { return false }
            // 用户确认的只是"完整性未知"，备份字节对不上记录的照样拒绝（§2.3）。
            outcome = history.restoreOutcome(entry: entry, force: false, acceptLegacy: true)
        }
        guard outcome.success else {
            showToast(outcome.error ?? "恢复失败")
            return false
        }
        // 恢复成功 = 这条事务的最终结局已定（原图回去了 / 产物删掉了），
        // 同一 id 的 journal 必须顺手销账：留着的话，下次启动会因 status=restored
        // 把它当成"未提交事务"回滚，覆盖用户恢复之后的编辑。
        transactions.finish(entry.id)
        markItemsRestored(forSource: entry.sourcePath)
        refreshHistory()
        // 非 replace 模式的原图从没被盖过，后端做的是"删掉这次的压缩产物" ——
        // 报「已恢复原图」等于把一件没发生过的事说给用户听。
        let message = entry.outputMode == "replace"
            ? "已恢复原图: \(entry.fileName)"
            : "已删除这次压缩结果: \(entry.fileName)"
        // §48 恢复成功与"产物没删掉"可以同时成立：一句 toast 说清两件事。
        showToast(outcome.warning.map { "\(message)；\($0)" } ?? message)
        return true
    }

    /// 后端确认恢复成功后，把指向同一源路径的行改成「已恢复」。
    private func markItemsRestored(forSource sourcePath: String) {
        for index in items.indices where canonicalPath(items[index].path) == sourcePath && items[index].status == .done {
            items[index].result = nil
            items[index].status = .restored
            let size = fileLength(sourcePath)
            if size >= 0 { items[index].fileSize = size }
        }
    }

    func restoreFile(path: String) {
        guard let entry = history.findLatest(forSource: path) else {
            showToast("找不到这条历史记录")
            return
        }
        restore(entry: entry)
    }

    func restoreAll() {
        guard !restoreBlockedBySession else {
            showToast(restoreBlockedText)
            return
        }
        let restorable = items.filter { $0.status == .done && $0.result?.hasProduct == true }
        guard !restorable.isEmpty else { return }
        let alert = NSAlert()
        alert.messageText = "确定要恢复全部已压缩成功的原图吗？"
        alert.addButton(withTitle: "恢复")
        alert.addButton(withTitle: "取消")
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        // §4.3 整批持同一把维护租约：批内逐条恢复，压缩会话起不来。
        // 与 Rust restore_all 一把租约的形状一致。
        guard let lease = scheduler.beginMaintenance() else {
            showToast(restoreBlockedText)
            return
        }
        defer { lease.release() }

        var restored = 0
        var conflicts = 0
        var legacyBackups = 0
        var failed = 0
        var cleanupWarnings = 0
        for item in restorable {
            guard let file = item.result?.file,
                  let entry = history.findLatest(forSource: file) else { failed += 1; continue }
            let outcome = history.restoreOutcome(entry: entry, force: false)
            if outcome.success {
                // 与 restore(entry:) 同一份销账：恢复终局后不许留悬空 journal。
                transactions.finish(entry.id)
                restored += 1
                if outcome.warning != nil { cleanupWarnings += 1 }
                markItemsRestored(forSource: entry.sourcePath)
            } else if outcome.conflict {
                conflicts += 1
            } else if outcome.legacyBackup {
                // 批量不接受 legacy 确认：老备份逐条在单文件恢复里确认（§2.3），
                // 整批"全部接受"会让用户在没看清的情况下回写无法验证的内容。
                legacyBackups += 1
            } else {
                failed += 1
            }
        }
        refreshHistory()
        // 冲突不静默覆盖：整批里遇到的都记下来，让用户去历史页逐个确认。
        var message = "已恢复 \(restored) 个文件到原图"
        if conflicts > 0 { message += "，\(conflicts) 个文件压缩后又被修改过，请在历史记录里逐个确认" }
        if legacyBackups > 0 { message += "，\(legacyBackups) 个备份来自旧版本，请在历史记录里逐个确认恢复" }
        if cleanupWarnings > 0 { message += "，\(cleanupWarnings) 个压缩结果未能删除" }
        if failed > 0 { message += "，\(failed) 个未能恢复" }
        showToast(message)
    }

    // MARK: - Save as

    func saveResult(path: String) {
        guard let result = items.first(where: { $0.path == path })?.result,
              let outPath = result.outputPath else {
            showToast("无法保存：找不到压缩文件")
            return
        }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = (outPath as NSString).lastPathComponent
        if panel.runModal() == .OK, let dest = panel.url {
            if atomicCopyFile(from: outPath, to: dest.path) {
                showToast("已保存到: \(dest.lastPathComponent)")
            } else {
                showToast("保存失败，请重试")
            }
        }
    }

    // MARK: - Export all

    func exportAll() {
        let doneItems = items.filter { $0.status == .done && $0.result?.hasProduct == true }
        guard !doneItems.isEmpty else { return }
        let suffix = Self.normalizedOutputSuffix(doneItems.first?.result?.options?.outputSuffix ?? options.outputSuffix)
        let fm = FileManager.default
        var count = 0
        var failed = 0
        for item in doneItems {
            guard let outPath = item.result?.outputPath else { continue }
            let dir = Self.outputDirectory(of: item.path)
            let ext = (outPath as NSString).pathExtension
            // §54 导出用的是**同一个**名字分配器：上一次导出的 `foo_compressed.jpg` 已经在
            // 目录里，这次就取 `foo_compressed_1.jpg`，绝不把上一次的成果盖掉。
            let naming = OutputNaming.withSuffix(
                stem: Self.fileStem(of: item.path), suffix: suffix, extension: ext.isEmpty ? "png" : ext
            )
            // 产物已经躺在导出的位置上（后缀模式 + 同一个后缀）：不必复制第二份。
            if (dir as NSString).appendingPathComponent(naming.candidate(0)) == outPath {
                count += 1
                continue
            }
            guard let claimed = OutputNameAllocator.claim(in: dir, naming) else { failed += 1; continue }
            var exported = false
            if let bytes = fm.contents(atPath: outPath), let staged = StagedWrite.claim(target: claimed.path) {
                // 导出同样只许顶掉我们自己刚认领的占位（§28 同款所有权门）。
                exported = staged.writeSynced(bytes) &&
                    staged.publishIfPlaceholderMatches(
                        to: claimed.path,
                        placeholderIdentity: OutputTransactionStore.fileIdentity(claimed.path))
                if !exported { staged.discard() }
            }
            if exported {
                count += 1
            } else {
                // 复制没成：占住的空名字收回去，别在用户目录里留一个 0 字节的"导出成果"。
                claimed.release()
                failed += 1
            }
        }
        showToast(failed == 0
            ? "已导出 \(count) 个文件到原目录（\(suffix) 后缀）"
            : "已导出 \(count) 个文件到原目录（\(suffix) 后缀），\(failed) 个没能导出")
    }

    // MARK: - Copy log

    func copyLog(path: String) {
        guard let item = items.first(where: { $0.path == path }),
              let result = item.result else { return }
        let opts = result.options ?? options
        var lines: [String] = []
        lines.append("版本: Swift")
        lines.append("=== Sizqo 压缩日志 ===")
        lines.append("")
        lines.append("文件: \(result.file)")
        // 日志里那两行写的是**契约原词**（outcome / reasonCode），不是中文句子：
        // 截图要能反查到后端到底判了什么（与前端 copyCompressLog 同一套）。
        let logged = result.contractLogStatus
        lines.append("状态: \(logged.status)")
        if !logged.reason.isEmpty { lines.append("原因: \(logged.reason)") }
        lines.append("")
        lines.append("--- 本轮请求 ---")
        lines.append("quality: \(opts.quality)")
        lines.append("outputFormat: \(opts.outputFormat.rawValue)")
        lines.append("maxDimensionPx: \(opts.maxDimensionPx.map(String.init) ?? "(不限)")")
        lines.append("maxOutputBytes: \(opts.maxOutputBytes.map(String.init) ?? "(不限)")")
        lines.append("outputMode: \(opts.outputMode.rawValue)")
        lines.append("outputSuffix: \(opts.outputSuffix)")
        lines.append("")
        lines.append("--- 压缩结果 ---")
        if result.hasProduct {
            lines.append("原始大小: \(CompressResult.formatBytesJS(result.originalSize)) (\(result.originalSize) bytes)")
            lines.append("压缩后大小: \(CompressResult.formatBytesJS(result.compressedSize)) (\(result.compressedSize) bytes)")
            lines.append("压缩率: \(result.savingsSignedText)")
            lines.append("输出格式: \(result.outType.isEmpty ? "(未知)" : result.outType)")
            lines.append("算法: \(result.algorithm.isEmpty ? "(未知)" : result.algorithm)")
            if let ms = result.elapsedMs { lines.append("耗时: \(ms) ms") }
            if let budget = result.targetBytes {
                lines.append("指定体积: \(CompressResult.formatBytesJS(budget)) · \(result.targetMet == true ? "已达成" : "未达成（照实报最接近的一次）")")
            }
            for warning in result.warnings { lines.append("说明: \(warning)") }
        } else {
            // 没有产物不等于失败：跳过 / 取消同样是"这一轮到此为止，原图没动"。
            lines.append("没有产物 · 原文件未改动（\(result.contractStatusText)）")
            let why = result.contractReasonText
            if !why.isEmpty { lines.append("为什么: \(why)") }
        }
        lines.append("")
        lines.append("--- 错误信息 ---")
        lines.append(result.error ?? "(无)")
        let text = lines.joined(separator: "\n")
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        showToast("压缩日志已复制到剪贴板")
    }

    // MARK: - Open in Finder

    func openInFinder(path: String) {
        NSWorkspace.shared.selectFile(path, inFileViewerRootedAtPath: "")
    }

    // MARK: - Compare integration

    /// 对比窗口重新压缩预览：写入临时文件，不改动用户目录，返回新结果。
    func recompressForCompare(path: String, quality: Int) -> CompressResult? {
        guard let idx = items.firstIndex(where: { $0.path == path }),
              let existing = items[idx].result else { return nil }
        var opts = options
        opts.quality = quality
        // 预览是对原图按指定质量的一次裸重编码：意图附加的尺寸上限 / 预算不参与，
        // 否则对比窗里滑到的质量会和实际发送的阶梯混在一起，读数失去意义。
        opts.maxDimensionPx = nil
        opts.maxOutputBytes = nil
        opts.outputMode = .suffix
        opts.outputFormat = OutputFormat(rawValue: existing.outType) ?? .original
        // 预览用引擎（非 smart，且不做「原图已最优」替换判断）
        let engineResult = CompressionEngine.compress(file: path, options: opts)
        guard engineResult.success else { return nil }
        let dir = NSTemporaryDirectory() + "sizqo-display"
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let previewPath = (dir as NSString).appendingPathComponent("recompress-\(Int(Date().timeIntervalSince1970 * 1_000_000_000)).\(engineResult.outType)")
        do {
            try engineResult.compressed.write(to: URL(fileURLWithPath: previewPath), options: .atomic)
        } catch {
            return nil
        }
        let originalSize = existing.originalSize
        var newResult = CompressResult(engine: engineResult, file: path, originalSize: originalSize, options: opts)
        newResult.outputPath = previewPath
        newResult.backupPath = existing.backupPath
        newResult.outputMode = existing.outputMode

        // 主窗口保留真实输出/备份元数据，仅同步体积、算法与压缩率
        // （与 Tauri compare-recompressed 处理一致），确保另存为/恢复/导出仍指向真实结果。
        // 结论字段同样保留正式产物的那一份：`init(engine:)` 不写 outcome（默认 nil），
        // 直接合并会把 done 行的契约冲掉 —— hasProduct 变 false、恢复/对比按钮消失、
        // 体积账少计，而预览的体积/压缩率描述的是临时文件，不是落盘的产物。
        var merged = newResult
        merged.outputPath = existing.outputPath
        // hasProduct 等契约字段由 outcome 派生，保住结论就保住了整条行契约。
        merged.outcome = existing.outcome
        merged.reasonCode = existing.reasonCode
        items[idx].result = merged
        return newResult
    }

    /// 对比窗口恢复后同步主窗口行状态（真正的恢复已走统一服务）
    func markRestored(path: String) {
        guard let idx = items.firstIndex(where: { $0.path == path && $0.status == .done }) else { return }
        items[idx].result = nil
        items[idx].status = .restored
        refreshHistory()
        showToast("已恢复原图: \(items[idx].fileName)")
    }

    func restoreFromCompare(path: String) -> Bool {
        guard let entry = history.findLatest(forSource: path) else {
            showToast("找不到这条历史记录")
            return false
        }
        return restore(entry: entry)
    }

    // MARK: - Helpers

    nonisolated private static func uniqueFilePaths(_ paths: [String]) -> [String] {
        var seen = Set<String>()
        var out: [String] = []
        for raw in paths {
            let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if value.isEmpty || seen.contains(value) { continue }
            seen.insert(value)
            out.append(value)
        }
        return out
    }

    private func uniqueFilePaths(_ paths: [String]) -> [String] { Self.uniqueFilePaths(paths) }

    /// 覆盖式复制：FileManager.copyItem 目标存在即失败，此处对齐 Rust fs::copy 的覆盖语义。
    @discardableResult
    nonisolated static func copyOverwriting(from src: String, to dst: String) -> Bool {
        copyFileOverwriting(from: src, to: dst)
    }

    nonisolated static func normalizedOutputSuffix(_ suffix: String) -> String {
        var s = suffix.trimmingCharacters(in: .whitespacesAndNewlines)
        var out = ""
        for ch in s.unicodeScalars {
            if ch == "/" || ch == "\\" || ch.value == 0 || CharacterSet.controlCharacters.contains(ch) {
                out.append("_")
            } else {
                out.unicodeScalars.append(ch)
            }
        }
        s = String(out)
        if s.isEmpty { s = "_compressed" }
        return String(s.prefix(64))
    }

    /// 产物放在源文件所在的目录里（与 Rust `output_directory` 同一句）。
    nonisolated private static func outputDirectory(of path: String) -> String {
        let dir = (path as NSString).deletingLastPathComponent
        return dir.isEmpty ? "." : dir
    }

    /// 不带扩展名的文件名，用作候选名的词干（与 Rust `file_stem_of` 同一句）。
    nonisolated private static func fileStem(of path: String) -> String {
        let stem = ((path as NSString).lastPathComponent as NSString).deletingPathExtension
        return stem.isEmpty ? "file" : stem
    }

    /// 字节属于哪种格式，文件就得叫什么扩展名（与 Rust `same_output_format` 同一份判定）。
    ///
    /// jpeg/jpg、heif/heic 是同一种格式的不同拼法：把它们判成"转换"会让每个 `.jpeg`
    /// 都在原图旁边长出一个 `.jpg`。
    nonisolated private static func sameOutputFormat(path: String, outType: String) -> Bool {
        let current = ((path as NSString).pathExtension).lowercased()
        return current == outType
            || (current == "jpeg" && outType == "jpg")
            || (current == "jpg" && outType == "jpeg")
            || (current == "heif" && outType == "heic")
    }

    /// 落盘没成，收回这次占住的输出名。
    ///
    /// 同格式覆盖的目标**就是用户原图**，那种情况下"收回占位"等于删原图 —— 所以界线
    /// 就是 `target == path` 这一条：只有我们新建出来的空占位才归自己删。
    nonisolated private static func releaseOutputPlaceholder(_ target: String, source path: String) {
        guard target != path else { return }
        // §28 审查修复：占位可能已被外部删掉、又换成别人的文件 ——
        // 删除前必须证明这个 0 字节文件还是我们认领的那个 inode。
        if let attributes = try? FileManager.default.attributesOfItem(atPath: target),
           let size = attributes[.size] as? NSNumber, size.int64Value == 0,
           let identity = OutputTransactionStore.fileIdentity(target) {
            try? FileManager.default.removeItem(atPath: target)
            _ = identity  // 大小为零 + 路径仍在申请记录内才走到这里；非空文件绝不删
        }
    }

    /// 最深的、真正包住这个文件的来源根（§39 §40）。
    /// 与 Tauri `source_root::relative_to_roots` 同一套判据：只认传进来的根，不按文件名猜。
    nonisolated private static func deepestRoot(file: String, roots: [String]) -> String? {
        let canonicalFile = canonicalPath(file)
        var best: (depth: Int, root: String)?
        for root in roots {
            var rootPath = canonicalPath(root)
            if rootPath.hasSuffix("/") { rootPath = String(rootPath.dropLast()) }
            guard canonicalFile.hasPrefix(rootPath + "/") else { continue }
            let depth = rootPath.split(separator: "/").count
            if best == nil || depth > best!.depth {
                best = (depth, rootPath)
            }
        }
        return best?.root
    }

    nonisolated private static func relativePathFromRoots(file: String, roots: [String]) -> String? {
        guard let root = deepestRoot(file: file, roots: roots) else { return nil }
        return String(canonicalPath(file).dropFirst(root.count + 1))
    }
}

/// 简单的线程安全计数盒（批次进度跨线程累加）
/// 只用来记"这一轮跑过几个"的诊断盒子。总体进度不再由它算
/// （见 `AppState.compressProgress`：那是队列的派生值）。
final class CounterBox: @unchecked Sendable {
    private var _value = 0
    private let lock = NSLock()
    var value: Int { lock.lock(); defer { lock.unlock() }; return _value }
    func increment() { lock.lock(); _value += 1; lock.unlock() }
}
