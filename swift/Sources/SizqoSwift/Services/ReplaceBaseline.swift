import Foundation

/// §24 §25 编码**之前**就要备齐的覆盖依据：这次压缩基于哪一份字节，以及那份字节的快照。
///
/// 为什么必须早于编码：覆盖模式的备份以前写在 Commit 里，也就是"压完了才留原图"。
/// 用户在编码期间改了这张图时，Commit 备份到的是**改动后**的内容 —— 历史从此声称它能
/// 恢复原图，恢复出来的却是一张谁都没见过的图。快照提前到编码前，复核才有了对象。
///
/// 这里而不是 AppState：`Result` 的成功分支要写成 `case .success`，而 AppState 里
/// 「除引擎判据和契约回写外不许读 success」是一条逐行扫源码的守卫（§36）。那条守卫
/// 的锋利比这里省一个文件更重要，而这块逻辑本来就是文件安全，不是视图模型状态。
enum ReplaceBaseline {
    /// 覆盖模式：编码前读到的指纹 + 编码前写好的原图快照。
    case ready(identity: SourceIdentity, backup: String)
    /// 后缀 / 输出目录：原图从不动，没有要复核的对象，也不需要快照。
    case notReplace
    /// 覆盖依据没能备齐。结论照旧由 Publish 那一步写，所以这里只带错误、不提前下结论。
    case unavailable(OutputWriteError)
}

/// 跑 §25 流程的前三步（读指纹 → 立刻留快照 → 才许编码）。
///
/// 非覆盖模式一次磁盘写都不做：那些模式的原图从未被盖过，快照与复核都没有对象。
nonisolated func captureReplaceBaseline(
    path: String, options: CompressOptions, history: HistoryStore
) -> ReplaceBaseline {
    guard options.outputMode == .replace else { return .notReplace }
    let identity: SourceIdentity
    switch SourceIdentity.readWithHash(path) {
    case .failure(let error): return .unavailable(.sourceUnreadable(error.message))
    case .success(let value): identity = value
    }
    // 快照必须在编码之前就落到磁盘：晚一步来的就不是「原图」了。
    // 复用旧备份前先对当前字节指纹：两轮之间用户改过原图时，旧备份已经不代表
    // 「这一次被盖掉的原图」，恢复它会静默回滚用户的编辑。
    guard let backup = history.ensureBackupVerified(for: path, currentSha256: identity.sha256) else {
        return .unavailable(.backupFailed("备份或它的来历记录没能写成"))
    }
    return .ready(identity: identity, backup: backup)
}
