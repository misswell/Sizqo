import Foundation

/// 统一结果契约（重构方案 §3 §4 §6 §10 §11 §70）。
///
/// 三轨同源：`src-tauri/src/result_model.rs` 与 `frontend/result_state.js` 是同一份模型的
/// 另外两个副本，这里的枚举字面量必须和它们逐字一致。
///
/// 这份模型存在的唯一理由：`success` 一个布尔同时背过「编码成功」「压缩成功」「没必要
/// 再压」「原图已达标」四件事，于是后端说「没收益」、界面读成「完成」，磁盘上什么都没有。
/// 从此三段各自回答一个问题，谁也不许替谁回答：
///
/// ```text
/// EncodeVerdict   引擎这一轮交回了什么字节？（引擎自己知道，别人猜不了）
/// Evaluation      这些字节值不值得落盘？（只有 evaluate 一处回答）
/// ResultOutcome   这次任务对用户的最终结论是什么？（落盘之后才允许写）
/// ```
enum EncodeVerdict {
    /// 交回的是这次编码的产物，且 `outType` 说的就是这些字节的格式。
    case candidate
    /// 交回的是**原图字节本身**：这一轮没有产出任何更优的东西。
    /// 是「没收益」，不是「失败」，绝不是「压缩成功」。
    case keptOriginal
    /// 连候选都没给：格式不支持、输入无效或解码失败。
    case rejected
    /// 编码这一步本身失败（含 CLI 报错后回落到原始字节）。
    case failed
}

/// 用户可感知的最终结论。与队列生命周期（pending/running/done/…）是两件事。
///
/// `committed` 与 `targetUnmet` 是唯一两个允许带 outputPath 的取值（§6.1）。
enum ResultOutcome: String {
    case committed
    case skippedNoBenefit = "skipped_no_benefit"
    case skippedAlreadyUnderTarget = "skipped_already_under_target"
    case targetUnmet = "target_unmet"
    case cancelled
    case failed

    /// §6.1 只有这两种结论才允许存在输出文件。
    var carriesOutput: Bool { self == .committed || self == .targetUnmet }
}

/// 结论的理由。UI 只读这个字段选句子，不再解析 `error` 文案（§59）。
enum ReasonCode: String {
    case alreadyUnderTarget = "already_under_target"
    case noSizeReduction = "no_size_reduction"
    case outputLargerThanSource = "output_larger_than_source"
    case targetUnmet = "target_unmet"
    case sourceReadFailed = "source_read_failed"
    case sourceNotFound = "source_not_found"
    case permissionDenied = "permission_denied"
    case sourceChanged = "source_changed"
    case unsupportedFormat = "unsupported_format"
    case invalidInput = "invalid_input"
    case decodeFailed = "decode_failed"
    case encodeFailed = "encode_failed"
    case outputValidationFailed = "output_validation_failed"
    case outputWriteFailed = "output_write_failed"
    case outputCollision = "output_collision"
    case backupFailed = "backup_failed"
    case transactionFailed = "transaction_failed"
    case historyWriteFailed = "history_write_failed"
    case rollbackFailed = "rollback_failed"
    case outputMissing = "output_missing"
    case cancelled
    case stopped
    /// 未被任何生产者分类过的失败。「还不知道」必须显式可见，而不是冒充成功。
    case unknown
}

/// §11 Evaluate：唯一一处回答「这些字节值不值得落盘」。
enum Evaluation: Equatable {
    case commit(targetUnmet: Bool)
    case skip(ReasonCode)
    case fail(ReasonCode)

    /// 编码阶段交回的字节，配上体积目标与用户是否明确要求换格式，才构成最终判据。
    ///
    /// 三条不容混淆的规则：
    /// - 显式换格式（§18.2）只看「有没有产出那个格式」，不看有没有变小；
    /// - 压缩（§18.1）只有变小才提交，一样大和更大都是跳过；
    /// - 引擎交回原图字节（`keptOriginal`）时，哪怕用户要的是转换，也不能提交 ——
    ///   落盘的会是原图的副本，那不是「转换成功」。
    static func evaluate(
        verdict: EncodeVerdict,
        engineReason: ReasonCode,
        engineSuccess: Bool,
        encodedBytes: Int,
        originalSize: Int,
        formatConversion: Bool,
        targetMet: Bool?
    ) -> Evaluation {
        let unmet = targetMet == false
        switch verdict {
        case .rejected: return .fail(engineReason)
        case .failed: return .fail(engineReason == .unknown ? .encodeFailed : engineReason)
        case .keptOriginal: return .skip(engineReason)
        case .candidate:
            // 判据与 success 打架 = 生产者忘了标 verdict。宁可判失败，不许冒充成功。
            if !engineSuccess { return .fail(.unknown) }
            if formatConversion { return .commit(targetUnmet: unmet) }
            if encodedBytes == 0 { return .fail(.encodeFailed) }
            if encodedBytes > originalSize { return .skip(.outputLargerThanSource) }
            if encodedBytes == originalSize { return .skip(.noSizeReduction) }
            return .commit(targetUnmet: unmet)
        }
    }

    /// 结论落到结果契约上的那一对字段。Commit 的最终结论要等落盘真的发生，这里不预先承诺。
    var preWriteOutcome: (outcome: ResultOutcome?, reason: ReasonCode?) {
        switch self {
        case .commit: return (nil, nil)
        case .skip(.alreadyUnderTarget): return (.skippedAlreadyUnderTarget, .alreadyUnderTarget)
        case .skip(let reason): return (.skippedNoBenefit, reason)
        case .fail(let reason): return (.failed, reason)
        }
    }

    /// §13 Publish：只有文件真的落盘、历史真的记了，才允许问这句话。
    /// 压到了文件却没达目标不是失败，也不是达成 —— 它是有产物、承诺没兑现（§16）。
    var committedOutcome: (outcome: ResultOutcome, reason: ReasonCode?) {
        switch self {
        case .commit(targetUnmet: true): return (.targetUnmet, .targetUnmet)
        case .commit: return (.committed, nil)
        case .skip(.alreadyUnderTarget): return (.skippedAlreadyUnderTarget, .alreadyUnderTarget)
        case .skip(let reason): return (.skippedNoBenefit, reason)
        case .fail(let reason): return (.failed, reason)
        }
    }
}

/// §6 §141 发布前的最后一道核对：结论与字段必须自洽。
///
/// 违反它的不是用户能看到的措辞，而是后端的 bug，所以它必须硬到能让调用方把结果降级。
func invariantViolation(
    mediaKind: String,
    file: String,
    outcome: ResultOutcome?,
    outputPath: String?
) -> String? {
    guard let outcome else {
        return "\(mediaKind) \(file) 没有结论就被发布了（outcome 未赋值）"
    }
    if outcome.carriesOutput && outputPath == nil {
        return "\(mediaKind) \(file) 结论是 \(outcome.rawValue)，却没有输出路径"
    }
    if !outcome.carriesOutput && outputPath != nil {
        return "\(mediaKind) \(file) 结论是 \(outcome.rawValue)，却带着输出路径 \(outputPath!)"
    }
    return nil
}

/// 一条结果允许哪些操作（§8 §68）。没有产物 = 指向那个文件的按钮一个都不许出现，
/// 而不是「点了再报错」。
struct ResultCapabilities: Equatable {
    var save: Bool
    var compare: Bool
    var restore: Bool
    var finder: Bool
    var retry: Bool
    var log: Bool
}

/// 结果契约在 CompressResult 上的唯一解释器。
///
/// 队列行、详情栏、行内按钮、体积账、导出、对比、恢复全部只读这里；
/// 任何页面都不许再自己看 `success` / `outputPath` / `error` 文案反推结论。
extension CompressResult {
    /// 把一条结果读成界面要用的三个事实。判据只有这一处。
    ///
    /// §139 第四阶段：`success` 已删除，也不再按 outputPath 猜「有没有产物」。
    /// 没有结论就没有产物 —— 发布前核对会把这种结果降级成失败（§141）。
    var facts: (outcome: ResultOutcome?, reason: ReasonCode?, hasProduct: Bool) {
        guard let outcome else { return (nil, nil, false) }
        return (outcome, reasonCode, outcome.carriesOutput)
    }

    /// §6.1 这一项在磁盘上到底有没有产物。
    var hasProduct: Bool { facts.hasProduct }

    /// §2 §67 生命周期由**结论**派生：跳过、取消、未达目标都是「这一轮处理完了」，
    /// 只有 outcome=failed 才是 Failed。没有结论同样不许读成 Done。
    var contractQueueStatus: QueueStatus {
        guard let outcome = facts.outcome else { return .failed }
        return outcome == .failed ? .failed : .done
    }

    /// §8 §68 一个结果允许哪些操作，只在这里回答一次。
    var capabilities: ResultCapabilities {
        guard hasProduct else {
            // 跳过 / 取消 / 失败三者的差别只在「怎么说」，不在「能给什么按钮」。
            return ResultCapabilities(save: false, compare: false, restore: false,
                                      finder: false, retry: true, log: true)
        }
        return ResultCapabilities(save: true, compare: true, restore: true, finder: true,
                                  retry: facts.outcome == .targetUnmet, log: true)
    }

    /// §70 结论的用户措辞。后端那句原话只进悬停和日志，不再是任何状态的载体。
    var contractStatusText: String {
        let facts = self.facts
        switch facts.outcome {
        case .targetUnmet: return "已完成 · 未达到目标"
        case .cancelled: return "已取消"
        case .failed: return "处理失败"
        case .skippedAlreadyUnderTarget, .skippedNoBenefit: return "已跳过"
        case .committed, .none: break
        }
        if facts.hasProduct { return "已完成" }
        return "未产出文件"
    }

    /// §70 结论下面那一行「为什么」。只按 reasonCode 选句子，不解析后端文案。
    var contractReasonText: String {
        guard let reason = facts.reason else { return "" }
        switch reason {
        case .alreadyUnderTarget: return "原文件已经小于目标体积"
        case .noSizeReduction: return "压缩后没有变小"
        case .outputLargerThanSource: return "压缩后文件更大"
        case .targetUnmet: return "未达到目标体积"
        case .cancelled: return "这一项被取消，没有开始处理"
        case .outputWriteFailed: return "压缩结果写入失败"
        case .historyWriteFailed: return "历史记录保存失败，已自动恢复原图"
        case .unsupportedFormat: return "这个格式暂不支持"
        case .invalidInput: return "输入无效"
        case .decodeFailed: return "无法解码这个文件"
        case .encodeFailed: return "编码失败"
        case .sourceReadFailed: return "无法读取源文件"
        case .sourceNotFound: return "源文件不存在"
        case .permissionDenied: return "没有权限访问这个文件"
        case .sourceChanged: return "源文件在处理期间被改动，已保留你的版本"
        case .backupFailed: return "无法保存原图备份"
        case .transactionFailed: return "无法登记这次覆盖"
        case .rollbackFailed: return "自动恢复原图也没成功，备份仍在"
        case .outputCollision: return "目标位置已有同名文件"
        case .outputValidationFailed: return "输出校验未通过"
        case .outputMissing: return "压缩结果已不存在"
        case .stopped: return "已停止，未开始"
        case .unknown: return "原因未上报"
        }
    }

    /// 悬停 / 详情里那句「为什么」：契约选的句子打头，后端原话跟在后面备查。
    var contractDetailText: String {
        let reason = contractReasonText
        let backend = (error ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if reason.isEmpty { return backend }
        if backend.isEmpty { return reason }
        return "\(reason) · \(backend)"
    }

    /// §69 行上的图形由同一份结论派生：三角只说「没有产物」或「产物没达标」，
    /// 红色只留给真的坏了。
    var contractShowsWarning: Bool {
        hasProduct == false || facts.outcome == .targetUnmet
    }

    /// 日志里那两行结论写的是**契约原词**（outcome / reasonCode），不是中文句子。
    /// 没有结论就照实写 unknown，不再从旧旗标反推一个 success。
    var contractLogStatus: (status: String, reason: String) {
        if let outcome = facts.outcome {
            return (outcome.rawValue, facts.reason?.rawValue ?? "")
        }
        return ("unknown", "")
    }
}
