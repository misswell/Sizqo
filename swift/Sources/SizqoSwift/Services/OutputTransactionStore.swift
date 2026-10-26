import Foundation

// Sizqo Swift 原生线 —— 落盘事务日志（crash journal）与输出命名。
//
// 为什么需要它：backup 写完、压缩结果覆盖到用户文件上、history.add 还没跑 ——
// 就在这个窗口里崩溃或强杀，App 下次启动将完全不知道发生过这次压缩：
// 历史里没有记录，备份目录里躺着一份没人认领的原图，用户看到的却是压缩后的文件。
// transaction 文件就是那段时间的记账凭证。原图快照不在这里做（§24：编码前就备好），
// 这一步只负责复核它：
//
//   ① 复核源文件  ② 独占目标名 + 独占临时名  ③ 写 transaction
//   ④ 写满候选 → flush + fsync  ⑤ 改名前再核一次指纹与备份  ⑥ rename 覆盖目标
//   ⑦ history.add  ⑧ 销账
//
// 启动时凡是**不能被历史证明已完整提交**的事务，按 §29 的两种模式各自收拾：
// 覆盖模式一律回滚成真正原图，新建模式只删自己生成的那个文件。
// 宁可丢掉一次压缩结果，也不能丢掉原图。
//
// 与 src-tauri/src/output_transaction.rs 同一套语义。

// MARK: - 覆盖失败的分类

/// 每一步的失败都要给出**不同**的处置说明：用户必须知道原图现在到底在哪。
enum OutputWriteError {
    case backupFailed(String)
    case transactionFailed(String)
    case outputWriteFailed(String)
    case historyWriteFailed
    case rollbackFailed(String)
    /// 评估说"该落盘"，输出方式却给不出目标位置（folder 没选目录 / 未知的模式）。
    /// 这不是跳过，是配置没兑现。
    case invalidDestination(String)
    /// §28 排到第 10 000 个名字还在撞车：一个字节都不许写，让用户自己清。
    case collision(String)
    /// §25 §26 覆盖前复核发现源文件已经不是这次压缩所依据的那份字节。
    /// 处置只有「拒绝覆盖」一种：候选丢掉，用户当前文件原样留着 ——
    /// 拿改动后的文件当"原图"备份，恢复出来的东西谁都认不出。
    case sourceChanged(String)
    /// §24 连"这份文件是什么"都读不出来（stat 失败 / 内容指纹读不动）。
    /// 编码前就走到这里的话，原图一个字节都没被碰过。
    case sourceUnreadable(String)

    var userMessage: String {
        switch self {
        case .backupFailed(let detail):
            return "无法保存原图备份，已跳过覆盖（\(detail)）"
        case .transactionFailed(let detail):
            return "无法登记这次覆盖，已跳过覆盖（\(detail)）"
        case .outputWriteFailed(let detail):
            return "压缩结果写入失败，原图未被覆盖（\(detail)）"
        case .historyWriteFailed:
            return "压缩结果已生成，但历史记录保存失败，已自动恢复原图"
        case .rollbackFailed(let detail):
            return "历史记录保存失败，且自动恢复原图也没成功：原图还在那份备份里，"
                + "请不要清理备份，重试恢复即可（\(detail)）"
        case .invalidDestination(let detail):
            return detail
        case .collision(let detail):
            return "目标位置已有同名文件，且已经多到无法再起一个名字（\(detail)）"
        case .sourceChanged(let detail):
            return "这个文件在压缩期间被改动过，已放弃覆盖，你现在的版本保持原样（\(detail)）"
        case .sourceUnreadable(let detail):
            return "读不出这个文件的当前状态，已跳过覆盖（\(detail)）"
        }
    }

    /// 同一件事在结果契约里的那个原因码：UI 选句子读它，不读上面那句人话（§59）。
    var reasonCode: ReasonCode {
        switch self {
        case .backupFailed: return .backupFailed
        case .transactionFailed: return .transactionFailed
        case .outputWriteFailed: return .outputWriteFailed
        case .historyWriteFailed: return .historyWriteFailed
        case .rollbackFailed: return .rollbackFailed
        case .invalidDestination: return .invalidInput
        case .collision: return .outputCollision
        case .sourceChanged: return .sourceChanged
        case .sourceUnreadable: return .sourceReadFailed
        }
    }
}

// MARK: - 事务记录

/// §29 落盘的两种模式。它是**一处**决定：备份做不做、事务记成哪一类、崩溃后允许
/// 怎么收拾残局，全从这里派生，不许在三个地方各判一次。
enum TransactionMode: String, Codable {
    /// 压缩结果要放回用户原文件的位置（同格式覆盖，或跨格式转换后删掉原图）。
    /// 恢复依据是备份：证明不了提交完成，就把原图写回去。
    case replaceExisting = "replace_existing"
    /// 原图一个字节都不动，产物是新文件（后缀 / 输出目录 / 导出）。
    /// 没有备份可依赖，也绝不该碰任何原图：证明不了提交，只删自己生成的那个文件。
    case createNew = "create_new"
}

/// §29 落盘模式在 Commit 里的那**一处**决定：要不要备份、目标名字凭什么可用、
/// 事务记成哪一类、崩溃后允许怎么收拾 —— 全部从这里派生。
///
/// 这里以前躺的是两个独立布尔（`isReplace` 决定记账、`sameFile` 决定跨格式），
/// 后缀模式既不进事务也没有独占名字，于是 `photo_compressed.jpg` 存在时第二次压缩
/// 直接把它盖掉，而用户以为"各留一份"。
enum OutputPlacement {
    /// 压缩结果放回原文件位置。同格式时目标就是原路径；跨格式时原图会被删掉、
    /// 产物是分配出来的新名字 —— 两种都动过原图，所以都靠备份恢复。
    case replaceExisting(crossFormat: Bool)
    /// 后缀 / 输出目录：名字必须独占申请，恢复只许删自己生成的文件。
    case createNew

    var transactionMode: TransactionMode {
        switch self {
        case .replaceExisting: return .replaceExisting
        case .createNew: return .createNew
        }
    }

    var crossFormat: Bool {
        if case .replaceExisting(let converted) = self { return converted }
        return false
    }

    /// 覆盖模式的记账凭证必须落到目录里（AGENTS 规则 8：durable precommit journal）；
    /// 新建模式丢了日志最多下次启动多留一个空占位，不值得每个文件多一次目录 fsync。
    func journal(_ store: OutputTransactionStore, _ txn: ReplaceTransaction) -> String? {
        switch self {
        case .replaceExisting: return store.prepareDurable(txn)
        case .createNew: return store.prepare(txn)
        }
    }
}

struct ReplaceTransaction: Codable {
    let id: String
    /// 与最终 `HistoryEntry.id` 同一个值：事务是否提交，就靠历史里有没有这个 id 判定。
    let historyId: String
    let sourcePath: String
    let outputPath: String
    let backupPath: String
    /// 压缩结果的临时文件名（rename 后通常已消失，留着只为回滚时清残骸）。
    var tempOutputPath: String?
    let originalSize: Int64
    let expectedOutputSize: Int64
    /// §2.5 启动恢复的身份证据：这次事务认得哪份"改动前的原图"、哪份"我们写出的产物"。
    /// 有了它们，回滚前就能证明源位置上的字节确实是这次事务的产物或仍是原图。
    /// 老日志没有这两个字段：按升级前的恢复契约处理（一律回滚）。
    var originalSha256: String?
    var outputSha256: String?
    let createdAt: Int64
    /// PNG → JPG 这类跨格式 replace：压缩结果是个新文件，回滚时要一并删掉。
    let crossFormat: Bool
    let mode: TransactionMode

    init(id: String, historyId: String, sourcePath: String, outputPath: String,
         backupPath: String, tempOutputPath: String? = nil, originalSize: Int64,
         expectedOutputSize: Int64, originalSha256: String? = nil,
         outputSha256: String? = nil, createdAt: Int64, crossFormat: Bool,
         mode: TransactionMode = .replaceExisting) {
        self.id = id
        self.historyId = historyId
        self.sourcePath = sourcePath
        self.outputPath = outputPath
        self.backupPath = backupPath
        self.tempOutputPath = tempOutputPath
        self.originalSize = originalSize
        self.expectedOutputSize = expectedOutputSize
        self.originalSha256 = originalSha256
        self.outputSha256 = outputSha256
        self.createdAt = createdAt
        self.crossFormat = crossFormat
        self.mode = mode
    }

    private enum CodingKeys: String, CodingKey {
        case id, historyId, sourcePath, outputPath, backupPath, tempOutputPath
        case originalSize, expectedOutputSize, originalSha256, outputSha256
        case createdAt, crossFormat, mode
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        historyId = try container.decode(String.self, forKey: .historyId)
        sourcePath = try container.decode(String.self, forKey: .sourcePath)
        outputPath = try container.decode(String.self, forKey: .outputPath)
        backupPath = try container.decode(String.self, forKey: .backupPath)
        tempOutputPath = try container.decodeIfPresent(String.self, forKey: .tempOutputPath)
        originalSize = try container.decode(Int64.self, forKey: .originalSize)
        expectedOutputSize = try container.decode(Int64.self, forKey: .expectedOutputSize)
        originalSha256 = try container.decodeIfPresent(String.self, forKey: .originalSha256)
        outputSha256 = try container.decodeIfPresent(String.self, forKey: .outputSha256)
        createdAt = try container.decode(Int64.self, forKey: .createdAt)
        crossFormat = try container.decode(Bool.self, forKey: .crossFormat)
        // §29 老日志里没有这个字段：它们只可能来自覆盖模式，按覆盖模式读。
        // 认成 createNew 的话，恢复逻辑会当成"没动过原图"而不再写回备份，原图就回不来了。
        mode = try container.decodeIfPresent(TransactionMode.self, forKey: .mode) ?? .replaceExisting
    }
}

struct RecoveryReport {
    /// 历史已记录，只是删日志前崩溃：补删日志。
    var committed = 0
    /// 回滚成真正原图的事务数。
    var rolledBack = 0
    var warnings: [String] = []
}

// MARK: - §28 §54 输出文件名：全应用唯一的分配者

/// 一个用户可见的产物名怎么排出来。
///
/// §54 要求 suffix / folder / export 用同一个分配器：「不能覆盖」这件事只要有一处自己
/// 拼名字，就有一处会静默盖掉邻居的文件。
enum OutputNaming {
    /// `foo_compressed.jpg` → `foo_compressed_1.jpg` → `foo_compressed_2.jpg`…
    /// folder / export 用的就是 `suffix` 为空串的这一支（`foo.jpg` → `foo_1.jpg`）。
    case withSuffix(stem: String, suffix: String, extension: String)
    /// 跨格式覆盖：`photo.png` 转 webp → `photo.webp` → `photo_converted.webp` →
    /// `photo_converted_2.webp`… 首选名字保持"原图换扩展名"，撞了才带 `_converted`。
    case converted(stem: String, extension: String)

    /// 第 `index` 个候选名（0 = 不冲突时该用的那个）。
    ///
    /// §54 要的就是这条序列只有一份实现：谁要"往后再排一位"都得从这里拿，
    /// 免得某处拼 `_1`、另一处拼 `(1)`，两套规则在同一目录里打架。
    func candidate(_ index: Int) -> String {
        switch self {
        case .withSuffix(let stem, let suffix, let ext):
            return index == 0 ? "\(stem)\(suffix).\(ext)" : "\(stem)\(suffix)_\(index).\(ext)"
        case .converted(let stem, let ext):
            switch index {
            case 0: return "\(stem).\(ext)"
            case 1: return "\(stem)_converted.\(ext)"
            default: return "\(stem)_converted_\(index).\(ext)"
            }
        }
    }
}

/// 拿到就等于**已经占住**了这个名字：磁盘上多了一个我们建的 0 字节文件。
struct ClaimedOutput {
    let path: String

    /// 落盘前的某一步没做成：撤掉占位，别给用户留个空文件。
    func release() {
        try? FileManager.default.removeItem(atPath: path)
    }
}

enum OutputNameAllocator {
    /// 冲突到第几个名字为止。正常目录到不了这里，但绝不允许无限排下去。
    static let maxNameConflicts = 10_000

    /// §28「绝对禁止覆盖」的实现方式：`O_EXCL` 独占创建，撞到已存在就换下一个名字。
    ///
    /// 为什么不是 `fileExists` 探一下再写 —— 探测到写入之间那段永远是空的：同一批里两个
    /// 任务（或另一个 App 实例）可以同时判定 `photo_compressed.jpg` "不存在"，后落盘的
    /// 那个把前一个的产物整个盖掉，而用户以为各留了一份。占位文件把这个名字在整个事务
    /// 期间都锁在我们手里，之后的 rename 覆盖的只可能是我们自己的 0 字节占位。
    static func claim(in directory: String, _ naming: OutputNaming) -> ClaimedOutput? {
        for index in 0..<maxNameConflicts {
            let path = (directory as NSString).appendingPathComponent(naming.candidate(index))
            let handle = Darwin.open(path, O_WRONLY | O_CREAT | O_EXCL, 0o644)
            if handle >= 0 {
                Darwin.close(handle)
                return ClaimedOutput(path: path)
            }
            // 名字已被占（EEXIST）就排下一个；别的错误（目录不存在、没权限）不是撞车，
            // 继续排下去只会把同一个毛病重复一万次。
            if errno != EEXIST { return nil }
        }
        return nil
    }
}

// MARK: - 压缩结果的落盘：同目录临时文件 → fsync → rename

/// §30 全应用只有一处产生临时文件名：和目标同目录、O_EXCL 认领、随机后缀。
///
/// 毫秒/纳秒时间戳在同一条队列并发压几十张图时会撞名。撞上的两个任务算出同一个
/// `.tmp`，后写的把前一个的半成品盖掉，rename 过去的就是别人的字节 —— 而压缩结果
/// 恰好是那种"看起来成功了、内容却是另一张图"的事故。
enum StagedTemp {
    /// 认领到一个只属于本次写入的临时路径；失败返回 nil（调用方放弃这次落盘）。
    static func claim(in directory: String, prefix: String) -> String? {
        for _ in 0..<16 {
            let path = (directory as NSString).appendingPathComponent(
                "\(prefix)-\(UUID().uuidString.lowercased()).tmp"
            )
            let handle = Darwin.open(path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
            if handle >= 0 {
                Darwin.close(handle)
                return path
            }
            if errno != EEXIST { return nil }
        }
        return nil
    }
}

/// 临时文件必须和目标同目录，否则 rename 跨挂载点就不是原子操作了。
struct StagedWrite {
    let temp: String

    /// §31 只独占临时名，一个字节都不写。Commit 用这条：先把名字记进事务日志，
    /// 再让数据落盘。反过来做的话，崩在"写完临时文件"与"改名"之间的那个 `.tmp`
    /// 恰好没有任何记录知道它在场，用户目录里就永远多出一个孤儿。
    static func claim(target: String) -> StagedWrite? {
        let parent = (target as NSString).deletingLastPathComponent
        guard let temp = StagedTemp.claim(in: parent, prefix: ".sizqo-write") else { return nil }
        return StagedWrite(temp: temp)
    }

    /// 写满已独占的临时文件 → fsync → 原子改名到目标。
    /// false = 没写成；目标保持原样，半成品已清掉。
    func writeAndPublish(_ bytes: Data, to target: String) -> Bool {
        guard writeSynced(bytes) else {
            discard()
            return false
        }
        return publish(to: target)
    }

    /// 写满 → flush + fsync，但**还不改名**。
    ///
    /// 覆盖模式要在"候选已经落盘"和"用户的文件被盖掉"之间插一道复核（§31 的
    /// source identity + backup check），所以这两步必须能分开调用。
    /// false = 没写满；半成品由调用方收尾（`discard`）。
    func writeSynced(_ bytes: Data) -> Bool {
        do {
            try bytes.write(to: URL(fileURLWithPath: temp), options: [])
        } catch {
            return false
        }
        // 落盘确认：崩溃后 rename 过去的不能是个只在内核缓存里的文件。
        guard syncFile(at: temp) else { return false }
        return true
    }

    /// 把已经 fsync 的临时文件原子改名到目标。改名之前该核的都必须已经核过。
    /// false = 没改成；目标保持原样，半成品已清掉。
    func publish(to target: String) -> Bool {
        guard Darwin.rename(temp, target) == 0 else {
            discard()
            return false
        }
        return true
    }

    /// 独占 + 写入 + 改名一条龙：调用方不需要在中途记任何东西时使用。
    /// nil = 没写成；目标文件保持原样，临时文件已清掉。
    static func write(target: String, bytes: Data) -> StagedWrite? {
        guard let staged = claim(target: target) else { return nil }
        return staged.writeAndPublish(bytes, to: target) ? staged : nil
    }

    /// 事后清理临时文件（正常情况下 rename 已经把它变成目标了）。
    func discard() {
        try? FileManager.default.removeItem(atPath: temp)
    }
}

// MARK: - 事务存储

/// 事务存储。除了一个只读的 `root`，没有任何进程内状态：每一次记账都是
/// 独立文件的原子写（id 唯一，互不覆盖），所以可以安全地跨线程用。
final class OutputTransactionStore: @unchecked Sendable {
    static let dirName = "transactions"

    let root: String

    init(root: String = HistoryStore.appDataRoot()) {
        self.root = (root as NSString).appendingPathComponent(Self.dirName)
    }

    private func path(of id: String) -> String {
        (root as NSString).appendingPathComponent("\(id).json")
    }

    /// 覆盖发生**之前**登记。写不进日志就不许覆盖 —— 没有记账凭证的覆盖等于不可恢复。
    func prepare(_ txn: ReplaceTransaction) -> String? {
        try? FileManager.default.createDirectory(atPath: root, withIntermediateDirectories: true)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let data = try? encoder.encode(txn) else {
            return "事务日志编码失败"
        }
        guard writeAtomic(path: path(of: txn.id), data: data) else {
            return "无法写入覆盖事务日志"
        }
        return nil
    }

    /// 记账凭证必须**先于覆盖落到目录里**：只写进内核缓存的日志，断电后和没写一样，
    /// 而那时用户原图已经被盖掉了（AGENTS 规则 8：durable precommit journal）。
    ///
    /// 只有 ReplaceExisting 需要这一层：它动的是用户的原图。CreateNew 从不动原图，日志
    /// 丢了最多下次启动多留一个空占位，不值得每张图都多一次目录 fsync。
    func prepareDurable(_ txn: ReplaceTransaction) -> String? {
        if let error = prepare(txn) { return error }
        // 只写进内核缓存的日志，断电后和没写一样 —— 而那时用户原图可能已经被盖掉。
        guard syncFile(at: root) else { return "无法把覆盖事务日志落盘" }
        return nil
    }

    /// 事务已完整提交（历史也写成了）：销账。
    func finish(_ id: String) {
        do { try FileManager.default.removeItem(atPath: path(of: id)) }
        catch CocoaError.fileNoSuchFile { /* 已经销过账，幂等 */ }
        catch {
            // 销账失败不许无声：journal 留在盘上，条目一旦被恢复（status=restored），
            // 下次启动会把它当成"未提交事务"回滚，覆盖用户恢复之后的编辑。
            NSLog("Sizqo 事务销账失败（journal 保留）: %@", error.localizedDescription)
        }
    }

    func list() -> [ReplaceTransaction] {
        let fm = FileManager.default
        guard let names = try? fm.contentsOfDirectory(atPath: root) else { return [] }
        let decoder = JSONDecoder()
        var found: [ReplaceTransaction] = []
        for name in names.sorted() {
            guard name.hasSuffix(".json") else { continue }
            let file = (root as NSString).appendingPathComponent(name)
            guard let data = fm.contents(atPath: file) else { continue }
            if let txn = try? decoder.decode(ReplaceTransaction.self, from: data) {
                found.append(txn)
            } else {
                // 半个日志文件：无法判定它代表哪次覆盖，改名留下现场，绝不据此删任何东西。
                let kept = (root as NSString).appendingPathComponent(
                    (name as NSString).deletingPathExtension + ".unparsed")
                try? fm.removeItem(atPath: kept)
                _ = Darwin.rename(file, kept)
            }
        }
        return found.sorted { $0.createdAt < $1.createdAt }
    }

    func hasPending() -> Bool {
        !list().isEmpty
    }

    /// 启动时第一件事：把上次没走完的事务结清。**必须早于任何清理** ——
    /// 回滚要用的那份备份如果被清理当成孤儿扫掉，原图就真没了。
    ///
    /// 判定只依赖一条事实：历史里有没有这条 `historyId`。没有就证明不了提交完成，
    /// 按 §29 的模式各自收拾 —— 覆盖模式回滚成真正原图，新建模式删掉这次生成的文件。
    func recover(_ history: HistoryStore) -> RecoveryReport {
        var report = RecoveryReport()
        for txn in list() {
            if history.containsCommitted(txn.historyId) {
                finish(txn.id)
                report.committed += 1
                continue
            }
            if let error = Self.rollback(txn) {
                report.warnings.append("\(txn.sourcePath) 回滚失败，原图仍在备份里: \(error)")
            } else {
                finish(txn.id)
                report.rolledBack += 1
            }
        }
        return report
    }

    /// 用备份把真正原图写回源位置，再清掉这次生成的压缩结果。
    ///
    /// 备份本身**不删**：回滚半途失败时它是唯一的原图副本，留给历史页/下次启动处理。
    ///
    /// §2.5：动手之前必须先确认源位置上的字节归属。带指纹的新日志里，能证明它是本次
    /// 写出的产物才允许盖回去；证明它还是原图就什么都不用盖；证明不了归属就保留现场。
    /// 老日志（没有指纹）维持升级前的恢复契约：一律回滚。
    ///
    /// CreateNew 没有备份，也从未动过任何原图：它只许收拾自己生成的那个文件（§29）。
    static func rollback(_ txn: ReplaceTransaction) -> String? {
        switch txn.mode {
        case .replaceExisting:
            guard fileExists(at: txn.backupPath) else {
                return "备份已不存在，无法自动恢复原图"
            }
            let restore: () -> String? = {
                do { try HistoryStore.writeBackupBack(to: txn.sourcePath, backup: txn.backupPath) }
                catch let error as RestoreError { return error.detail }
                catch { return error.localizedDescription }
                removeGeneratedOutput(txn)
                return nil
            }
            if !fileExists(at: txn.sourcePath) {
                // 源文件已经不在原位：按"提交没完成"对待，把原图写回去。
                return restore()
            }
            guard case .success(let current) = sha256OfFile(txn.sourcePath) else {
                return "当前源文件读不出来，无法确认它是不是本次事务的产物；备份与日志原样保留"
            }
            if txn.outputSha256 == current {
                // 源位置上就是这次写出的字节：安全回滚。
                return restore()
            }
            if txn.originalSha256 == current {
                // 源文件还是没动过的原图：什么都没有被覆盖，只需清掉自己的残骸。
                removeGeneratedOutput(txn)
                return nil
            }
            if txn.outputSha256 == nil && txn.originalSha256 == nil {
                // 老日志没有身份记录：维持升级前的恢复契约 —— 一律回滚。
                return restore()
            }
            return "源文件在事务结束前被外部修改过，无法确认归属；不自动覆盖，原图备份与事务日志都保留在原处"
        case .createNew:
            retireGeneratedOutput(txn)
            return nil
        }
    }

    /// 删掉这次压缩产生的新文件与临时文件。同格式 replace 的输出就是源文件本身，绝不删。
    ///
    /// 跨格式的输出文件也只删"能证明是这次生成的"：新日志按内容指纹判断；
    /// 老日志没有指纹，维持升级前契约照原样清。
    static func removeGeneratedOutput(_ txn: ReplaceTransaction) {
        if txn.crossFormat || !sameFile(txn.outputPath, txn.sourcePath) {
            let ours: Bool
            if let expected = txn.outputSha256 {
                ours = (try? sha256OfFile(txn.outputPath).get()) == expected
            } else {
                ours = true
            }
            if ours { try? FileManager.default.removeItem(atPath: txn.outputPath) }
        }
        removeTemp(txn)
    }

    /// 新建模式清账：只删**能证明是这次生成的**那个文件。
    ///
    /// 名字是独占申请来的，申请那一刻它不存在，所以空占位和"正好是我们承诺的体积"这两种
    /// 都归这次事务；其它体积说明这位置已经被别人的内容占了，宁可留个来历不明的文件，
    /// 也不许拿它当自己的半成品删（删用户文件永远不是恢复的选项）。
    static func retireGeneratedOutput(_ txn: ReplaceTransaction) {
        // `fileLength` 对不存在的文件给 -1：那种情况本来就没东西要删。
        let size = fileLength(txn.outputPath)
        if size == 0 || size == txn.expectedOutputSize {
            try? FileManager.default.removeItem(atPath: txn.outputPath)
        }
        removeTemp(txn)
    }

    static func removeTemp(_ txn: ReplaceTransaction) {
        if let temp = txn.tempOutputPath {
            try? FileManager.default.removeItem(atPath: temp)
        }
    }
}
