import Foundation

// MARK: - Cancel tracking (thread-safe, nonisolated)

/// 取消与提交竞争同一个状态位（P2-14 / T08）：一把 NSLock 下的三本账。
///
/// 旧实现只有一张"已取消"集合：提交链从复核到 rename 之间任何时刻被 insert，
/// 都会出现"界面说取消了、后台仍在覆盖"的撕裂。现在取消与提交互斥 ——
/// 取消先到则提交必须输（`tryBeginCommit` 返回 false）；提交先到（committing
/// 在账）则取消必须被拒（`requestCancel` 返回 false），提交链收尾用 endCommit 销账。
final class CancelBox: @unchecked Sendable {
    /// 已记取消、还没被消费的项。
    private var cancelled: Set<String> = []
    /// 正在提交链里的项：跨过最后一道取消闸之后，取消对它们无效。
    private var committing: Set<String> = []
    private let lock = NSLock()

    /// 记一笔取消。正在提交的项拒绝取消（返回 false，调用方必须如实告知
    /// "正在提交，无法取消"，绝不能照常把行标成已移除）。
    @discardableResult
    func requestCancel(_ path: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        if committing.contains(path) { return false }
        cancelled.insert(path)
        return true
    }

    /// 提交登记（P2-14 / T08 的提交闸）：取消先到 → false（取消赢）；
    /// 否则登记 committing → true。登记之后到 endCommit 之前，取消一律被拒。
    func tryBeginCommit(_ path: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        if cancelled.contains(path) { return false }
        committing.insert(path)
        return true
    }

    /// 提交链（成功或失败）收尾：销掉 committing 账。
    func endCommit(_ path: String) {
        lock.lock(); committing.remove(path); lock.unlock()
    }

    /// 这一项目前是不是「已记取消」。
    func isCancelled(_ path: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return cancelled.contains(path)
    }

    /// 原语义：消费取消（读走并清掉那一笔）。
    @discardableResult
    func take(_ path: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard cancelled.contains(path) else { return false }
        cancelled.remove(path)
        return true
    }

    /// 旧接口：与 requestCancel 同一语义（提交中的项拒绝记账）。
    func insert(_ p: String) { requestCancel(p) }
    /// 旧接口：只读「已取消」账，committing 不算取消。
    func contains(_ p: String) -> Bool { isCancelled(p) }

    /// 新一轮开工前清账：取消与提交两本账一起清（上一轮的残余不许泄进下一轮）。
    func removeAll() {
        lock.lock(); cancelled.removeAll(); committing.removeAll(); lock.unlock()
    }
}
