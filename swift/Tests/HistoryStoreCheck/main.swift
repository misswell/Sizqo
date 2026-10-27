// Sizqo Swift 线 —— 历史 / 原图备份 / 暂停 / CPU 上限的行为自检。
//
// 跑法：bash scripts/test_swift_history.sh
// Swift 线用 swiftc 直接编译、没有 Package.swift 测试 target，所以这里是可执行自检而不是 XCTest。
//
// 守的是规范里最不能坏的几条：备份绝不覆盖真正原图、清理只删 Sizqo
// 自己的副本、清理唯一的时机是正常退出（崩溃现场那份可能是唯一的原图，启动时一个都不动）、
// 恢复必须原子写回、压缩后被外部改过必须先问用户、暂停和改 CPU 上限都只拦
// 「还没开始」的任务、CPU 上限真的限住同时跑的任务数。

import Foundation
import ImageIO
import CoreGraphics
import UniformTypeIdentifiers

var failures = 0

func check(_ condition: Bool, _ message: String) {
    if condition {
        print("  ok   \(message)")
    } else {
        failures += 1
        print("  FAIL \(message)")
    }
}

let fm = FileManager.default

func freshRoot(_ tag: String) -> String {
    let dir = NSTemporaryDirectory() + "sizqo-swift-check-\(tag)-\(UUID().uuidString)"
    try? fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
    return dir
}

func tempStore(_ tag: String, at root: String? = nil) -> HistoryStore {
    HistoryStore(root: root ?? freshRoot(tag))
}

func makeFile(_ path: String, _ bytes: [UInt8]) {
    try? fm.createDirectory(
        atPath: (path as NSString).deletingLastPathComponent, withIntermediateDirectories: true
    )
    fm.createFile(atPath: path, contents: Data(bytes))
}

func writeData(_ path: String, _ bytes: [UInt8]) {
    try? Data(bytes).write(to: URL(fileURLWithPath: path))
}

func readBytes(_ path: String) -> [UInt8] {
    Array(fm.contents(atPath: path) ?? Data())
}

// MARK: - 并发观测（CPU 并行上限自检）

/// 「同时在跑几个」的峰值记录器。
final class PeakMeter: @unchecked Sendable {
    private let lock = NSLock()
    private var running = 0
    private var peakValue = 0
    private var doneValue = 0

    func begin() {
        lock.lock()
        running += 1
        peakValue = max(peakValue, running)
        lock.unlock()
    }

    func end() {
        lock.lock()
        running -= 1
        doneValue += 1
        lock.unlock()
    }

    var peak: Int {
        lock.lock()
        defer { lock.unlock() }
        return peakValue
    }

    var done: Int {
        lock.lock()
        defer { lock.unlock() }
        return doneValue
    }
}

typealias TaskGroup = DispatchGroup

/// 铺下 tasks 个任务让它们通过调度器跑，每个占住名额 holdSeconds 秒，不等待收尾。
///
/// 队列必须是并发队列：worker 会在 acquire 里堵着等名额，串行队列会自己等自己。
func launchTasks(
    _ scheduler: CompressionScheduler, tasks: Int, holdSeconds: Double
) -> (meter: PeakMeter, group: TaskGroup) {
    let meter = PeakMeter()
    let group = TaskGroup()
    let queue = DispatchQueue(label: "sizqo-scheduler-check", attributes: .concurrent)
    for _ in 0..<tasks {
        group.enter()
        queue.async {
            let permit = scheduler.acquire()
            meter.begin()
            Thread.sleep(forTimeInterval: holdSeconds)
            meter.end()
            permit.release()
            group.leave()
        }
    }
    return (meter, group)
}

/// 等某个条件成立，超时返回 false —— 测试里宁可慢一秒，不要死等。
func waitUntil(timeout: TimeInterval = 1.0, _ condition: () -> Bool) -> Bool {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
        if Date() >= deadline { return false }
        Thread.sleep(forTimeInterval: 0.002)
    }
    return true
}

/// 把一条记录改老，用来模拟"跨过保留期后再次启动"。
func ageEntry(_ id: String, daysAgo: Double, in store: HistoryStore) {
    var entries = store.list()
    for index in entries.indices where entries[index].id == id {
        entries[index].createdAt = SizqoClock.nowMillis - Int64(daysAgo * Double(historyDayMillis))
    }
    guard let data = try? JSONEncoder().encode(entries) else { return }
    _ = writeAtomic(path: store.historyFile, data: data)
}

func result(file: String, size: Int64, outType: String = "png") -> CompressResult {
    var item = CompressResult(
        engine: EngineResult(success: true, compressed: Data([1, 2, 3]), outType: outType, algorithm: "mozjpeg"),
        file: file,
        originalSize: 1000
    )
    item.compressedSize = size
    return item
}

/// 契约里全部 23 种理由。列在这儿而不是抄两遍，是为了让「漏了一种」在两个地方同时炸。
let allReasonCodes: [ReasonCode] = [
    .alreadyUnderTarget, .noSizeReduction, .outputLargerThanSource, .targetUnmet, .sourceReadFailed,
    .sourceNotFound, .permissionDenied, .sourceChanged, .unsupportedFormat, .invalidInput,
    .decodeFailed, .encodeFailed,
    .outputValidationFailed, .outputWriteFailed, .outputCollision, .backupFailed,
    .transactionFailed, .historyWriteFailed, .rollbackFailed, .outputMissing, .cancelled, .stopped, .unknown,
]

// ─── 1. 同一张图重压多次：备份永远是第一次压缩之前的真正原图 ────────────────
print("[1] ensureBackup 绝不覆盖真正原图")
do {
    let store = tempStore("1")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-1/a.png")
    makeFile(source, [1, 1, 1])

    let first = store.ensureBackup(for: source)
    writeData(source, [9, 9, 9]) // 模拟第一次压缩覆盖了原图
    // 生产顺序：第一次压缩提交后先记历史（产物大小 + mtime 入账），下一轮的
    // ensureBackup 靠它认出「现在的字节是我们自己的产物」→ 复用最早的备份。
    let round1 = HistoryEntry.record(
        source: source, result: result(file: source, size: 3),
        output: source, backup: first, retentionDays: 3
    )
    _ = store.add(round1)
    let second = store.ensureBackup(for: source)

    check(first != nil, "第一次备份成功")
    check(first == second, "同一张图复用同一个备份槽位")
    check(first.flatMap { readBytes($0) } == [1, 1, 1], "备份里仍是真正的原图，不是上一版压缩结果")
    check(readBytes(source) == [9, 9, 9], "重压不改动磁盘上的当前版本")

    // 用户在两轮之间自己改了文件（不是我们的产物）：那份字节必须有属于
    // 自己的一份快照，否则 replace 一提交就再也找不回来。
    writeData(source, [7, 7, 7, 7])
    let third = store.ensureBackup(for: source)
    check(third != nil && third != first, "外部编辑的字节有自己的备份槽位")
    check(readBytes(third!) == [7, 7, 7, 7], "新槽位备份的是用户的编辑")
    check(readBytes(first!) == [1, 1, 1], "最早的备份不许被覆盖")
}

// ─── 2. 备份 key 跨启动稳定 ────────────────────────────────────────────────
print("[2] backupKey 跨启动稳定")
do {
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-2/b.jpg")
    makeFile(source, [4, 4])
    let keyA = HistoryStore.backupKey(forPath: source)
    let keyB = tempStore("2").ensureBackup(for: source).flatMap { HistoryStore.key(ofBackupPath: $0) }
    check(keyA == keyB, "备份目录名 = 源路径的稳定哈希")
    check(keyA.count == 16, "key 是 16 位十六进制")
    check(tempStore("2").ensureBackup(for: source).flatMap { HistoryStore.key(ofBackupPath: $0) } == keyA,
          "换一个 store 实例（相当于重启）仍指向同一备份")

    // 哈希撞槽时往后挪位，不能串了别人的原图
    let store = tempStore("2b")
    let taken = canonicalPath(NSTemporaryDirectory() + "sizqo-check-2b/taken.png")
    makeFile(taken, [7])
    let dir = store.backupsDir + "/" + HistoryStore.backupKey(forPath: taken)
    try? fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
    writeData(dir + "/" + HistoryStore.metaFileName,
              Array("{\"sourcePath\":\"/somewhere/else\",\"createdAt\":1}".utf8))
    let moved = store.ensureBackup(for: taken)
    check(moved != nil && moved != dir + "/original.png", "槽位被别的源占用时挪到下一个槽位")
    check(moved.map { readBytes($0) } == [7], "挪位后备份的仍是本图真正原图")
}

// ─── 3. 退出清理只删 Sizqo 自己的记录与备份 ───────────────────────────
// 这里测的是"按天保留"那一支，所以显式写 3 天，不用 Retention.defaultDays
// （默认档已经变成 0 = 不保留，见 [19]）。
print("[3] applyRetentionOnExit 只清自己的副本")
do {
    let store = tempStore("3")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-3/c.png")
    makeFile(source, [7, 7, 7])
    let userOutput = ((source as NSString).deletingPathExtension) + "_compressed.png"
    makeFile(userOutput, [8, 8])

    let backup = store.ensureBackup(for: source)!
    let entry = HistoryEntry.record(
        source: source,
        result: result(file: source, size: 3),
        output: source,
        backup: backup,
        retentionDays: 3
    )
    store.add(entry)
    ageEntry(entry.id, daysAgo: 10, in: store)

    let report = store.applyRetentionOnExit(
        retentionDays: 3, runStartedAt: SizqoClock.nowMillis, previousRunEndedCleanly: true)
    check(report.removedEntries == 1, "过期历史记录被删除")
    check(report.removedBackups == 1, "无人引用的原图备份被删除")
    check(store.list().isEmpty, "历史页清空")
    check(!fm.fileExists(atPath: backup), "备份文件本体已删除")
    check(fm.fileExists(atPath: source), "用户的原图绝不因历史过期而被删")
    check(fm.fileExists(atPath: userOutput), "用户的压缩结果绝不因历史过期而被删")

    let freshBackup = store.ensureBackup(for: userOutput)!
    let fresh = HistoryEntry.record(
        source: userOutput,
        result: result(file: userOutput, size: 2),
        output: userOutput,
        backup: freshBackup,
        retentionDays: 30
    )
    store.add(fresh)
    let second = store.applyRetentionOnExit(
        retentionDays: 3, runStartedAt: store.runStartedAt, previousRunEndedCleanly: true)
    check(second.keptBackups == 1 && second.removedBackups == 0, "未过期备份被保留")
    check(fm.fileExists(atPath: userOutput) && fm.fileExists(atPath: source), "保留期内用户文件不受影响")
}

// ─── 4. 恢复：原子写回真正原图 + 清跨格式孤儿 + 共享备份一起标已恢复 ────────
print("[4] restore 原子写回并共享备份")
do {
    let store = tempStore("4")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-4/d.png")
    makeFile(source, [1, 2, 3])
    let backup = store.ensureBackup(for: source)!

    // 跨格式 replace：压缩结果是个新文件
    let output = ((source as NSString).deletingPathExtension) + ".jpg"
    writeData(output, [5, 5, 5, 5])
    writeData(source, [6, 6, 6])

    var compressed = result(file: source, size: 4, outType: "jpg")
    compressed.outputPath = output
    compressed.outputMode = "replace"
    let first = HistoryEntry.record(source: source, result: compressed, output: output,
                                    backup: backup, retentionDays: 3)
    store.add(first)
    let second = HistoryEntry.record(source: source, result: compressed, output: output,
                                     backup: backup, retentionDays: 3)
    store.add(second)

    let ids = try? store.restore(entry: first, force: false)
    check(ids?.historyIds.count == 2, "共享同一备份的两条记录一起处理（\(ids?.historyIds.count ?? 0) 条）")
    check(readBytes(source) == [1, 2, 3], "写回的是真正的原图")
    check(!fm.fileExists(atPath: output), "跨格式的压缩结果不留孤儿")
    check(!fm.fileExists(atPath: backup), "备份目录已清理")
    let listed = store.list()
    check(listed.count == 2 && listed.allSatisfy { $0.status == .restored }, "两条记录都是「已恢复」")
    check(listed.allSatisfy { $0.backupPath == nil && !$0.backupExists }, "已恢复记录不再引用备份")
    let again = store.restoreOutcome(entry: listed[0], force: false)
    check(!again.success && !again.conflict, "重复恢复被拒：\(again.error ?? "")")
    // 目录里没有残留的 .sizqo-restore-*.tmp（原子替换不留临时文件）
    let leftovers = ((try? fm.contentsOfDirectory(atPath: (source as NSString).deletingLastPathComponent)) ?? [])
        .filter { $0.hasPrefix(".sizqo-restore-") }
    check(leftovers.isEmpty, "恢复不留临时文件")
}

// ─── 5. 压缩后被外部改过：先报冲突，force 才覆盖 ───────────────────────────
print("[5] 外部修改检测")
do {
    let store = tempStore("5")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-5/e.png")
    makeFile(source, [1, 0, 0])
    let backup = store.ensureBackup(for: source)!
    writeData(source, [2, 2, 2]) // 压缩结果落盘
    var compressed = result(file: source, size: 3)
    compressed.outputPath = source
    compressed.outputMode = "replace"
    let entry = HistoryEntry.record(source: source, result: compressed, output: source,
                                    backup: backup, retentionDays: 3)
    store.add(entry)
    check(!HistoryStore.hasConflict(entry), "刚压完不算冲突")

    writeData(source, [3, 3, 3, 3, 3]) // 用户又用别的 App 改过
    let refreshed = store.find(entry.id)!
    check(HistoryStore.hasConflict(refreshed), "体积变了 = 压缩后被改过")

    let outcome = store.restoreOutcome(entry: refreshed, force: false)
    check(!outcome.success && outcome.conflict, "不 force 时只报冲突")
    check(readBytes(source) == [3, 3, 3, 3, 3], "冲突时当前版本保持原样")
    check(fm.fileExists(atPath: backup), "冲突时备份不被销毁")

    let forced = store.restoreOutcome(entry: refreshed, force: true)
    check(forced.success, "用户确认后 force 恢复成功")
    check(readBytes(source) == [1, 0, 0], "force 后写回真正原图")
}

// ─── 6. mtime 抖动容忍窗 ───────────────────────────────────────────────────
print("[6] mtime 抖动容忍 2 秒")
do {
    let store = tempStore("6")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-6/f.png")
    makeFile(source, [1, 1])
    let backup = store.ensureBackup(for: source)!
    writeData(source, [2, 2])
    var compressed = result(file: source, size: 2)
    compressed.outputPath = source
    compressed.outputMode = "replace"
    var entry = HistoryEntry.record(source: source, result: compressed, output: source,
                                    backup: backup, retentionDays: 3)
    let current = fileMtimeMillis(source) ?? 0
    entry.outputModifiedAt = current - 1_000
    check(!HistoryStore.hasConflict(entry), "1 秒抖动不误报")
    entry.outputModifiedAt = current - 60_000
    check(HistoryStore.hasConflict(entry), "1 分钟差 = 真被改过")
    // 非覆盖模式的产物被外部改过同样算冲突：撤销 = 删产物，静默删掉用户的编辑
    // 就是丢数据（与覆盖模式同一份判据）。
    entry.outputMode = "suffix"
    entry.outputPath = source
    check(HistoryStore.hasConflict(entry), "非覆盖产物被改过 = 冲突")
    entry.outputModifiedAt = current - 1_000
    check(!HistoryStore.hasConflict(entry), "非覆盖产物没被改过 = 不冲突")
}

// ─── 7. 后缀/目录模式：撤销 = 只删这次生成的压缩结果 ───────────────────────
print("[7] 非覆盖模式的撤销")
do {
    let store = tempStore("7")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-7/g.png")
    makeFile(source, [1, 1, 1])
    let output = ((source as NSString).deletingPathExtension) + "_compressed.png"
    makeFile(output, [2, 2])
    var compressed = result(file: source, size: 2)
    compressed.outputPath = output
    compressed.outputMode = "suffix"
    let entry = HistoryEntry.record(source: source, result: compressed, output: output,
                                    backup: nil, retentionDays: 3)
    store.add(entry)

    let ids = try? store.restore(entry: entry, force: false)
    check(ids?.historyIds == [entry.id], "撤销返回被处理的记录 id")
    check(!fm.fileExists(atPath: output), "这次生成的压缩结果被删掉")
    check(readBytes(source) == [1, 1, 1], "原图从头到尾没被碰过")
    check(store.find(entry.id) == nil, "撤销后这条历史不再存在")
}

// ─── 8. 主队列按源路径取最近一条未恢复记录 ─────────────────────────────────
print("[8] findLatest(forSource:)")
do {
    let store = tempStore("8")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-8/h.png")
    makeFile(source, [1])
    var compressed = result(file: source, size: 1)
    compressed.outputPath = source
    compressed.outputMode = "replace"
    let backup = store.ensureBackup(for: source)!
    let older = HistoryEntry.record(source: source, result: compressed, output: source,
                                    backup: backup, retentionDays: 3)
    store.add(older)
    Thread.sleep(forTimeInterval: 0.01)
    let newer = HistoryEntry.record(source: source, result: compressed, output: source,
                                    backup: store.ensureBackup(for: source), retentionDays: 3)
    store.add(newer)
    check(store.findLatest(forSource: source)?.id == newer.id, "取最近一条未恢复的记录")
    // 符号链接写法（/var ↔ /private/var）也要命中同一条记录
    check(store.findLatest(forSource: NSTemporaryDirectory().replacingOccurrences(of: "/private", with: "")
                              + "sizqo-check-8/h.png")?.id == newer.id, "路径规范化后才比对")
    _ = try? store.restore(entry: newer, force: false)
    check(store.findLatest(forSource: source) == nil, "共享备份的兄弟记录一起标已恢复")
}

// ─── 9. 清空历史只删自己的备份 ─────────────────────────────────────────────
print("[9] clear 清空历史")
do {
    let store = tempStore("9")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-9/i.png")
    makeFile(source, [1, 2])
    var compressed = result(file: source, size: 2)
    compressed.outputPath = source
    compressed.outputMode = "replace"
    let backup = store.ensureBackup(for: source)!
    store.add(HistoryEntry.record(source: source, result: compressed, output: source,
                                  backup: backup, retentionDays: 3))
    let report = store.clear()
    check(report.removedEntries == 1 && report.removedBackups == 1, "记录与备份一起清掉")
    check(store.list().isEmpty, "历史为空")
    check(fm.fileExists(atPath: source), "用户图片仍在")
}

// ─── 10. 保留天数持久化与钳制 ──────────────────────────────────────────────
print("[10] 保留天数持久化")
do {
    let root = freshRoot("10")
    let settings = SettingsStore(root: root)
    // 写死 0，不让断言变成"默认值等于默认值"的同义反复。
    check(Retention.defaultDays == 0 && Retention.noRetain == 0, "默认档就是「不保留」")
    check(settings.load().originalRetentionDays == 0, "缺文件时按默认「不保留」")
    settings.setRetentionDays(14)
    check(SettingsStore(root: root).load().originalRetentionDays == 14, "重启后仍是 14 天")
    settings.setRetentionDays(0)
    check(SettingsStore(root: root).load().originalRetentionDays == 0,
          "「不保留」存得住，不被当成没设置")
    check(Retention.clamp(0) == 0
            && Retention.clamp(-5) == Retention.minDays
            && Retention.clamp(9000) == Retention.maxDays,
          "clamp 保住 0，只有越界才夹进 1…30")
    check(Retention.label(0) == "不保留" && Retention.label(7) == "保留 7 天",
          "「不保留」不许写成「保留 0 天」")
    writeData(root + "/settings.json", Array("not json".utf8))
    check(SettingsStore(root: root).load().originalRetentionDays == 0,
          "settings.json 损坏时回落默认值，不崩")
}

// ─── 11. 备份写不成就不碰用户文件 ──────────────────────────────────────────
print("[11] 备份失败时绝不覆盖原图")
do {
    let root = freshRoot("11")
    writeData(root + "/backups", [0]) // backups 位置做成普通文件 → 建目录必失败
    let store = HistoryStore(root: root)
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-11/j.png")
    makeFile(source, [1, 2, 3])
    check(store.ensureBackup(for: source) == nil, "备份写不成时返回 nil")
    check(readBytes(source) == [1, 2, 3], "调用方据此跳过覆盖，用户文件保持原样")
}

// ─── 12. 调度闸门只拦新任务（暂停与 CPU 上限共用一把锁） ────────────────────
print("[12] CompressionScheduler：暂停只拦新任务")
do {
    let scheduler = CompressionScheduler(maxParallelism: 3)
    let meter = PeakMeter()
    check(!scheduler.isPaused, "初始未暂停")
    check(scheduler.phase == .idle, "没有批次在跑时状态为 idle")

    check(scheduler.tryBeginBatch(), "占住这一轮会话")
    check(scheduler.phase == .running, "批次开始后状态为 running")
    scheduler.pause()
    check(scheduler.isPaused, "pause 后处于暂停")
    check(scheduler.phase == .paused, "状态为 paused")

    let group = DispatchGroup()
    group.enter()
    DispatchQueue.global().async {
        let permit = scheduler.acquire()
        meter.begin()
        permit.release()
        meter.end()
        group.leave()
    }
    Thread.sleep(forTimeInterval: 0.15)
    check(meter.peak == 0, "暂停期间不开工新任务（正在跑的不受影响）")

    scheduler.resume()
    group.wait()
    check(meter.peak == 1, "resume 后等待中的任务继续")

    // §37：正在跑的那一轮被拒的第二入口抢不动，也不许把它的暂停解除。
    scheduler.pause()
    check(!scheduler.tryBeginBatch(), "已经有一轮在跑时，第二个入口必须被拒")
    check(scheduler.isPaused, "被拒的入口不许把正在跑那一轮的暂停悄悄解除")
    check(scheduler.phase == .paused, "被拒的入口也不许改写正在跑那一轮的阶段")
    scheduler.resume()
    scheduler.endBatch()
    check(scheduler.tryBeginBatch(), "上一轮收尾后，下一轮才准占住会话")
    check(!scheduler.isPaused, "新批次不继承上一批的暂停")
    scheduler.pause()
    scheduler.endBatch()
    check(!scheduler.isPaused, "批次收尾必然解除暂停")
    check(scheduler.phase == .idle, "批次收尾后回到 idle")
}

// ─── 13. history.json 字段命名与 Tauri 线一致 ─────────────────────────────
print("[13] history.json 为 camelCase")
do {
    let store = tempStore("13")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-13/k.png")
    makeFile(source, [1])
    var compressed = result(file: source, size: 1)
    compressed.outputPath = source
    compressed.outputMode = "replace"
    store.add(HistoryEntry.record(source: source, result: compressed, output: source,
                                  backup: store.ensureBackup(for: source), retentionDays: 3))
    let raw = String(data: fm.contents(atPath: store.historyFile) ?? Data(), encoding: .utf8) ?? ""
    for key in ["sourcePath", "outputPath", "backupPath", "originalSize", "compressedSize",
                "outputMode", "outputModifiedAt", "createdAt", "expiresAt", "outType",
                "status", "sourceExists", "backupExists"] {
        check(raw.contains("\"\(key)\""), "字段 \(key)")
    }
    check(raw.contains("mozjpeg"), "算法名照常落库")

    _ = try? store.restore(entry: store.list()[0], force: false)
    let restored = String(data: fm.contents(atPath: store.historyFile) ?? Data(), encoding: .utf8) ?? ""
    check(restored.contains("\"restoredAt\""), "恢复之后才写入 restoredAt")
}

// ─── 14. CPU 并行上限真正限住同时跑的任务数（方案 §68） ─────────────────────
print("[14] CPU 上限限住并发任务数")
do {
    let four = CompressionScheduler(maxParallelism: 4)
    check(four.tryBeginBatch(maxParallelism: 4), "上限 4 的那一轮占住会话")
    let (meter4, group4) = launchTasks(four, tasks: 100, holdSeconds: 0.002)
    group4.wait()
    check(meter4.done == 100, "100 个任务一个都没少")
    check(meter4.peak <= 4, "上限 4 时全程峰值并发 \(meter4.peak) 从未超过 4")
    check(meter4.peak >= 2, "预算内确实并行，不是退化成串行（峰值 \(meter4.peak)）")
    four.endBatch()

    let one = CompressionScheduler(maxParallelism: 1)
    check(one.tryBeginBatch(maxParallelism: 1), "上限 1 的那一轮占住会话")
    let (meter1, group1) = launchTasks(one, tasks: 30, holdSeconds: 0.001)
    group1.wait()
    check(meter1.peak == 1, "上限 1 时严格串行（峰值 \(meter1.peak)）")
    one.endBatch()

    // 0 / 负数这类无效值不能把闸门拆成「不限」。
    check(CompressionScheduler(maxParallelism: 0).currentLimit == 1, "上限 0 兜底为 1")
    check(CompressionScheduler(maxParallelism: -5).currentLimit == 1, "负数上限兜底为 1")
}

// ─── 15. 运行中改上限：不抢占、立刻生效（方案 §59/§60） ─────────────────────
print("[15] 运行中下调 / 上调上限")
do {
    let scheduler = CompressionScheduler(maxParallelism: 8)
    check(scheduler.tryBeginBatch(maxParallelism: 8), "上限 8 的那一轮占住会话")
    let (meter, group) = launchTasks(scheduler, tasks: 16, holdSeconds: 0.06)
    check(waitUntil { scheduler.activeJobs == 8 }, "上限 8 时确实同时跑着 8 个")

    scheduler.setMaxParallelism(2)
    check(scheduler.activeJobs == 8, "下调到 2 不抢回已在跑的任务，8 个各自跑完")

    // 第一波结束后，新启动的只能按新上限来。
    _ = waitUntil(timeout: 2.0) { scheduler.activeJobs <= 2 }
    var observed = 0
    for _ in 0..<15 {
        observed = max(observed, scheduler.activeJobs)
        Thread.sleep(forTimeInterval: 0.01)
    }
    check(observed <= 2, "第一波退去后同时跑的不超过新上限 2（实测 \(observed)）")

    group.wait()
    check(meter.done == 16, "改上限期间没有任务被丢弃")
    check(meter.peak > 2, "下调前起跑的那一批没被打断（峰值 \(meter.peak)）")
    scheduler.endBatch()

    // 2 → 6：等待中的 worker 要立刻被唤醒，而不是靠 250ms 超时兜底。
    let up = CompressionScheduler(maxParallelism: 2)
    check(up.tryBeginBatch(maxParallelism: 2), "上限 2 的那一轮占住会话")
    let (_, upGroup) = launchTasks(up, tasks: 20, holdSeconds: 0.06)
    check(waitUntil { up.activeJobs == 2 }, "上调前先按 2 跑着")
    up.setMaxParallelism(6)
    let woke = Date()
    let reached = waitUntil(timeout: 1.0) { up.activeJobs == 6 }
    let latency = Date().timeIntervalSince(woke)
    check(reached, "2 → 6 后并发升到 6（实测 \(up.activeJobs)）")
    check(latency < 0.1, "唤醒立刻发生，不靠超时兜底（\(Int(latency * 1000))ms）")
    upGroup.wait()
    up.endBatch()
}

// ─── 16. 暂停与 CPU 上限叠加 ────────────────────────────────────────────────
print("[16] 暂停 × CPU 上限")
do {
    let scheduler = CompressionScheduler(maxParallelism: 2)
    check(scheduler.tryBeginBatch(maxParallelism: 2), "上限 2 的那一轮占住会话")
    let (meter, group) = launchTasks(scheduler, tasks: 12, holdSeconds: 0.02)
    check(waitUntil { scheduler.activeJobs == 2 }, "上限 2 生效")

    scheduler.pause()
    let frozen = scheduler.activeJobs
    check(frozen > 0 && frozen <= 2, "暂停时正在跑的任务继续跑完（\(frozen) 个）")
    check(waitUntil(timeout: 1.0) { scheduler.activeJobs == 0 }, "跑完后不再启动新任务")
    let idleSamples = scheduler.activeJobs
    Thread.sleep(forTimeInterval: 0.05)
    check(idleSamples == 0 && scheduler.activeJobs == 0, "暂停期间保持 0 个在跑")

    scheduler.resume()
    group.wait()
    check(meter.done == 12, "恢复后剩余任务全部完成")
    check(meter.peak <= 2, "暂停叠加上限，全程峰值仍不超过 2（实测 \(meter.peak)）")
    scheduler.endBatch()
}

// ─── 17. 每个子进程只占 1 份预算（方案 §51–§54/§57） ────────────────────────
print("[17] CPUResourcePolicy")
do {
    let policy = CPUResourcePolicy(threadLimit: 1)
    check(policy.flags(for: "avifenc") == ["--jobs", "1"], "avifenc 显式 --jobs 1")
    check(policy.flags(for: "oxipng") == ["--threads", "1"], "oxipng 显式 --threads 1")
    check(policy.flags(for: "cwebp").isEmpty, "cwebp 单线程时不带 -mt")
    check(policy.flags(for: "pngquant").isEmpty, "pngquant 没有线程开关，不加无谓参数")
    check(policy.flags(for: "gifsicle").isEmpty, "gifsicle 同上")
    check(CPUResourcePolicy(threadLimit: 4).flags(for: "cwebp") == ["-mt"], "多线程才允许 -mt")
    check(policy.environment["RAYON_NUM_THREADS"] == "1"
              && policy.environment["OMP_NUM_THREADS"] == "1",
          "线程类环境变量压到 1 份")
    // 真子进程环境必须无条件带上预算，不能只在内置动态库存在时才设置。
    check(CLIRunner.environment()["RAYON_NUM_THREADS"] == "1", "子进程环境无条件生效")
    check(CLIRunner.environment()["OMP_NUM_THREADS"] == "1", "OMP 同样无条件生效")
}

// ─── 18. cpuThreadLimit 落盘、取值与文案（方案 §46/§47/§63） ────────────────
print("[18] 设置持久化与展示文案")
do {
    let root = freshRoot("18")
    let store = SettingsStore(root: root)
    check(store.load().cpuThreadLimit == nil, "缺省为自动")
    _ = store.setCpuThreadLimit(4)
    check(store.load().cpuThreadLimit == 4, "改过的上限重启后还在")
    let raw = String(data: fm.contents(atPath: store.path) ?? Data(), encoding: .utf8) ?? ""
    check(raw.contains("\"cpuThreadLimit\""), "与 Tauri 线同名 camelCase 字段")
    check(raw.contains("\"originalRetentionDays\""), "同一份 settings.json，不另开文件")
    _ = store.setRetentionDays(7)
    check(store.load().cpuThreadLimit == 4 && store.load().originalRetentionDays == 7,
          "改保留时间不会顺手抹掉 CPU 上限")
    _ = store.setCpuThreadLimit(nil)
    let auto = String(data: fm.contents(atPath: store.path) ?? Data(), encoding: .utf8) ?? ""
    // Swift 的 Codable 对 nil 是「不写这个键」，Tauri 线写 null —— 两边读出来都是自动。
    check(store.load().cpuThreadLimit == nil && !auto.contains(": 4"),
          "回到自动后文件里不再留着旧数字")
    try? #"{"originalRetentionDays":3,"cpuThreadLimit":0}"#
        .write(toFile: store.path, atomically: true, encoding: .utf8)
    check(store.load().cpuThreadLimit == nil, "0 视为无效，回落自动")

    check(CPULimit.effective(configured: nil, detected: 10) == 3, "自动档不默认吃满全核")
    check(CPULimit.effective(configured: 12, detected: 8) == 8, "换到核少的机器静默收敛 12 → 8")
    check(CPULimit.effective(configured: 1, detected: 8) == 1, "最低档 1 份")
    check(CPULimit.effective(configured: nil, detected: 1) == 1, "单核机器自动档也只有 1")

    let m5 = CpuInfo(architecture: "aarch64", logicalCpus: 10, physicalCpus: 10,
                     performanceCpus: 4, efficiencyCpus: 6, modelName: "Apple M5",
                     availableParallelism: 10)
    check(m5.isAppleSilicon, "Apple M5 认作 Apple Silicon")
    check(CPUStatusText.device(m5) == "Apple M5 · ARM64", "设备行：型号 · 架构")
    check(CPUStatusText.cores(m5) == "10 核 CPU（4 性能核 + 6 能效核）", "Apple Silicon 报 P/E 核")
    check(CPUStatusText.limitLabel(info: m5, configured: nil, effective: 3) == "自动（3）",
          "自动档显示实际取值")
    check(CPUStatusText.limitLabel(info: m5, configured: 4, effective: 4) == "4 / 10", "手动档 4 / 10")
    check(CPUStatusText.limitLabel(info: m5, configured: 10, effective: 10) == "10 / 10（全部）",
          "吃满时标注（全部）")
    check(CPUStatusText.summary(info: m5, configured: nil, effective: 3) == " · CPU 自动",
          "自动档摘要不假装知道具体数字")
    check(CPUStatusText.summary(info: m5, configured: 4, effective: 4) == " · CPU 4/10",
          "手动档摘要 CPU 4/10")

    // aarch64 ≠ Apple Silicon：ARM Windows / Linux 不能凭空报性能核。
    let winArm = CpuInfo(architecture: "aarch64", logicalCpus: 8, physicalCpus: 8,
                         performanceCpus: nil, efficiencyCpus: nil,
                         modelName: "Snapdragon X Elite", availableParallelism: 8)
    check(!winArm.isAppleSilicon, "arm64 不能猜成 Apple Silicon")
    check(!CPUStatusText.cores(winArm).contains("性能核"), "非 Apple 平台不提性能核")

    let intel = CpuInfo(architecture: "x86_64", logicalCpus: 12, physicalCpus: 6,
                        performanceCpus: nil, efficiencyCpus: nil,
                        modelName: "Intel Core i5-8500", availableParallelism: 12)
    check(CPUStatusText.cores(intel) == "6 个物理核心 · 12 个逻辑处理器", "Intel 报物理 / 逻辑")

    let live = CpuInfo.detect()
    check(live.budgetCeiling >= 1, "本机并行能力至少 1 份")
    check(live.architecture == "aarch64" || live.architecture == "x86_64",
          "本机架构 \(live.architecture)")
}

// ─── 19. 「不保留」（默认档）：清理只挂在正常退出 ────────────────────────────
print("[19] 不保留：退出才清，启动不清")
do {
    let root = freshRoot("19")
    let store = tempStore("19", at: root)
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-19/i.png")
    makeFile(source, [1, 9])
    let userOutput = ((source as NSString).deletingPathExtension) + "_compressed.png"
    makeFile(userOutput, [2, 9])

    let backup = store.ensureBackup(for: source)!
    let entry = HistoryEntry.record(
        source: source,
        result: result(file: source, size: 2),
        output: source,
        backup: backup,
        retentionDays: Retention.noRetain
    )
    check(entry.expiresAt == entry.createdAt, "「不保留」没有几天后到期这回事")
    store.add(entry)
    // 上一次崩溃 / 强杀留下的现场：这份备份可能就是唯一还活着的原图。
    ageEntry(entry.id, daysAgo: 10, in: store)
    let orphan = store.ensureBackup(for: userOutput)!

    check(!store.previousRunEndedCleanly, "没有退出记号 = 上次没结清账，宁可多留")
    let survived = store.applyRetentionOnExit(
        retentionDays: Retention.noRetain,
        runStartedAt: store.runStartedAt,
        previousRunEndedCleanly: false
    )
    check(survived.removedEntries == 0 && store.list().count == 1,
          "上次崩溃留下的记录，本次退出仍然留着")
    check(fm.fileExists(atPath: backup) && readBytes(backup) == [1, 9],
          "还有人引用的原图备份绝不在退出时被删，且仍是真正的原图")
    check(!fm.fileExists(atPath: orphan), "无人引用的孤儿备份照旧扫掉")
    let rows = store.list()
    check(rows.count == 1 && rows[0].backupExists, "留着的那条仍然能一键恢复")
    check(store.restoreOutcome(entry: rows[0], force: true).success
          && readBytes(source) == [1, 9], "崩溃现场的原图恢复回来了")

    // 结清过账的下一次退出不再享有豁免：不留下"说不保留却永久占着磁盘"的死角。
    let leftover = store.ensureBackup(for: source)!
    writeData(source, [3, 9])
    let stale = HistoryEntry.record(
        source: source,
        result: result(file: source, size: 2),
        output: source,
        backup: leftover,
        retentionDays: Retention.noRetain
    )
    store.add(stale)
    ageEntry(stale.id, daysAgo: 10, in: store)
    let collected = store.applyRetentionOnExit(
        retentionDays: Retention.noRetain,
        runStartedAt: SizqoClock.nowMillis,
        previousRunEndedCleanly: true
    )
    check(collected.removedEntries == 2 && store.list().isEmpty,
          "正常退出结清账之后，记录和原图备份一起走（\(collected.removedEntries) 条）")
    check(!fm.fileExists(atPath: leftover), "备份目录已删除")
    check(fm.fileExists(atPath: source) && fm.fileExists(atPath: userOutput),
          "用户文件一个都不碰")

    // 记号是"上次到底结清过账没有"的唯一证据：写一次、读一次即抹掉。
    store.markCleanExit()
    check(HistoryStore(root: root).previousRunEndedCleanly, "退出记号留给下一次启动")
    check(!fm.fileExists(atPath: root + "/\(HistoryStore.cleanExitFileName)"),
          "记号读一次就抹掉，不会一直算成上上次")

    // 不保留 ≠ 关掉备份：覆盖前照旧必须先有备份，这条不变量不能让新档位废掉。
    let again = store.ensureBackup(for: source)
    check(again != nil && readBytes(again!) == [3, 9], "「不保留」照样先备份再覆盖")
    check(Retention.options.first == Retention.noRetain, "设置页下拉第一档就是「不保留」")

    // 记录还在、备份却被外部删了：只能说实话，不能假装恢复成功。
    let goneBackup = store.ensureBackup(for: source)!
    writeData(source, [4, 9])
    store.add(HistoryEntry.record(
        source: source,
        result: result(file: source, size: 2),
        output: source,
        backup: goneBackup,
        retentionDays: 30
    ))
    try? fm.removeItem(atPath: goneBackup)
    let gone = store.restoreOutcome(entry: store.list()[0], force: false)
    check(!gone.success && !gone.conflict, "备份没了就不假装恢复成功")
    check(gone.error == "原图备份已清理，无法恢复", "说清是备份没了：\(gone.error ?? "")")
}

// ─── 20. history.json 读不出来：隔离现场 + 按备份重建 + 本次禁止清理 ─────────
print("[20] 历史损坏不得变成\"备份没人引用\"")
do {
    let root = freshRoot("20")
    let store = HistoryStore(root: root)
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-20/k.png")
    makeFile(source, [1, 2, 3])
    let backup = store.ensureBackup(for: source)!
    store.add(HistoryEntry.record(
        source: source, result: result(file: source, size: 3), output: source,
        backup: backup, retentionDays: 3
    ))
    // 崩溃留下的半个 history.json：老实现把它当空历史，下一次启动就把所有备份删光。
    writeData(store.historyFile, Array("[{\"id\":\"x\",\"sourcePa".utf8))

    let reopened = HistoryStore(root: root)
    // 生产代码的顺序：先取启动报告（落日志）。清理不在启动跑，见 [19]。
    let startup = reopened.takeStartupReport()
    check(startup?.recoveredEntries == 1, "启动报告统计到重建条目")
    check(reopened.takeStartupReport() == nil, "启动报告只报一次")
    let report = reopened.applyRetentionOnExit(
        retentionDays: 3, runStartedAt: reopened.runStartedAt, previousRunEndedCleanly: true)
    check(report.removedEntries == 0 && report.removedBackups == 0,
          "损坏现场一次备份都不许删（removedBackups \(report.removedBackups)）")
    check(fm.fileExists(atPath: backup) && readBytes(backup) == [1, 2, 3],
          "原图备份完整保留，内容仍是真正原图")
    check(reopened.cleanupIsLocked(), "本次启动锁死备份清理")
    check(report.warnings.contains { $0.contains("已损坏") },
          "报告说清是损坏：\(report.warnings.joined(separator: " / "))")
    check(((try? fm.contentsOfDirectory(atPath: root)) ?? [])
            .contains { $0.hasPrefix("history.corrupt-") },
          "损坏现场被改名留档，而不是被 [] 覆盖")

    // 来历记录是 v1（只有 sourcePath/createdAt）也必须认：老用户的历史就靠它重建。
    let rebuilt = reopened.list()
    check(rebuilt.count == 1, "按备份重建出 1 条恢复入口（实际 \(rebuilt.count)）")
    check(rebuilt.first?.status == .recoveryAvailable, "状态是 recoveryAvailable")
    check(rebuilt.first?.algorithm == HistoryStore.recoveryAlgorithm, "算法位标明不是真实压缩")
    check(rebuilt.first?.backupPath == backup, "重建条目带着备份位置")
    check(rebuilt.first?.sourcePath == source, "重建条目认得原图属于谁")

    // 锁定期内连退出清理也不许动手：历史不可信时备份可能是原图唯一的副本。
    let quit = reopened.applyRetentionOnExit(
        retentionDays: Retention.noRetain,
        runStartedAt: reopened.runStartedAt,
        previousRunEndedCleanly: false)
    check(quit.removedBackups == 0 && fm.fileExists(atPath: backup),
          "损坏锁定期内退出也不清备份")

    // 重建条目必须真的能一键恢复，而且不误报冲突（明细填的是当下实测值）。
    let restored = reopened.restoreOutcome(entry: rebuilt[0], force: false)
    check(restored.success, "重建出来的条目能直接恢复：\(restored.error ?? "")")
    check(readBytes(source) == [1, 2, 3], "恢复后源文件回到真正原图")
    check(!fm.fileExists(atPath: backup), "恢复成功后备份才让位")
}

// ─── 21. 覆盖事务：不能被历史证明提交的覆盖一律回滚 ─────────────────────────
print("[21] replace 事务与崩溃恢复")
func transaction(_ id: String, source: String, output: String, backup: String,
                 crossFormat: Bool = false) -> ReplaceTransaction {
    ReplaceTransaction(
        id: id, historyId: id, sourcePath: source, outputPath: output, backupPath: backup,
        tempOutputPath: nil, originalSize: 3, expectedOutputSize: 1,
        createdAt: SizqoClock.nowMillis, crossFormat: crossFormat
    )
}

do {
    let root = freshRoot("21")
    let store = HistoryStore(root: root)
    let journal = OutputTransactionStore(root: root)
    let dir = NSTemporaryDirectory() + "sizqo-check-21"
    let source = canonicalPath(dir + "/a.png")
    makeFile(source, [9, 9, 9])                    // 崩溃现场：源文件已是压缩结果
    let backupFile = dir + "/original.png"
    writeData(backupFile, [1, 2, 3])               // 备份里是真正的原图

    check(journal.prepare(transaction("t-missing", source: source,
                                      output: source, backup: backupFile)) == nil,
          "覆盖前的记账凭证写得成")
    check(journal.hasPending(), "覆盖前登记的记账凭证还在")
    let report = journal.recover(store)
    check(report.rolledBack == 1 && report.warnings.isEmpty,
          "历史证明不了提交 → 自动回滚（\(report.warnings.joined(separator: " / "))）")
    check(readBytes(source) == [1, 2, 3], "源文件回到真正原图")
    check(!journal.hasPending(), "回滚完成即销账")
    check(fm.fileExists(atPath: backupFile), "回滚不许顺手删备份：它可能是原图唯一的副本")

    // 已提交：历史里有同 id 的记录 → 只补删日志，绝不碰用户文件。
    makeFile(source, [9, 9, 9])
    store.add(HistoryEntry.record(withId: "t-committed", source: source,
                                  result: result(file: source, size: 3), output: source,
                                  backup: backupFile, retentionDays: 3))
    _ = journal.prepare(transaction("t-committed", source: source,
                                    output: source, backup: backupFile))
    let committed = journal.recover(store)
    check(committed.committed == 1 && committed.rolledBack == 0, "已提交的事务只销账")
    check(readBytes(source) == [9, 9, 9], "已提交的事务绝不动用户文件")

    // 跨格式：PNG → JPG 的回滚要连转换出来的新文件一起删。
    let png = canonicalPath(dir + "/photo.png")
    makeFile(png, [9, 9])
    let jpg = dir + "/photo.jpg"
    writeData(jpg, [8, 8])
    writeData(backupFile, [1, 2, 3])
    _ = journal.prepare(transaction("t-cross", source: png,
                                    output: jpg, backup: backupFile, crossFormat: true))
    check(journal.recover(store).rolledBack == 1, "跨格式事务被回滚")
    check(readBytes(png) == [1, 2, 3], "跨格式回滚写回真正原图")
    check(!fm.fileExists(atPath: jpg), "跨格式回滚删掉转换出来的新文件")

    // 半个日志文件：认不出代表哪次覆盖，就一个文件都不许动。
    let brokenDir = freshRoot("21b")
    let brokenStore = HistoryStore(root: brokenDir)
    let brokenJournal = OutputTransactionStore(root: brokenDir)
    let kept = canonicalPath(brokenDir + "/kept.png")
    makeFile(kept, [9, 9])
    try? fm.createDirectory(atPath: brokenJournal.root, withIntermediateDirectories: true)
    writeData(brokenJournal.root + "/broken.json", Array("{ half".utf8))
    let unparsed = brokenJournal.recover(brokenStore)
    check(unparsed.rolledBack == 0, "读不懂的日志不做回滚")
    check(readBytes(kept) == [9, 9], "读不懂的日志一个字节都不删")
    check(fm.fileExists(atPath: brokenJournal.root + "/broken.unparsed"), "现场改名留档")
}

// ─── 21b. 带指纹的新日志：启动恢复先证明源位置字节的归属，再决定动不动手 ────
print("[21b] 事务身份哈希：证明得了归属才动手")
do {
    let root = freshRoot("21c")
    let store = HistoryStore(root: root)
    let journal = OutputTransactionStore(root: root)
    let dir = NSTemporaryDirectory() + "sizqo-check-21c"
    try? fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
    let source = canonicalPath(dir + "/a.png")
    let backupFile = dir + "/original.png"
    writeData(backupFile, [1, 2, 3])
    let originalHash = sha256OfData(Data([1, 2, 3]))
    let outputHash = sha256OfData(Data([9, 9, 9]))
    let record = { (id: String, crossFormat: Bool, outputPath: String) -> ReplaceTransaction in
        ReplaceTransaction(
            id: id, historyId: id, sourcePath: source, outputPath: outputPath,
            backupPath: backupFile, tempOutputPath: nil,
            originalSize: 3, expectedOutputSize: 3,
            originalSha256: originalHash, outputSha256: outputHash,
            createdAt: SizqoClock.nowMillis, crossFormat: crossFormat)
    }

    // 源位置就是这次写出的字节（rename 后、历史落盘前崩溃）→ 安全回滚成原图。
    makeFile(source, [9, 9, 9])
    _ = journal.prepare(record("t-hash-output", false, source))
    let staged = journal.recover(store)
    check(staged.rolledBack == 1 && staged.warnings.isEmpty,
          "源位置上是本次产物 → 回滚（\(staged.warnings.joined(separator: " / "))）")
    check(readBytes(source) == [1, 2, 3], "回滚写回真正原图")

    // 源文件还是没动过的原图（rename 之前崩溃）→ 不做无谓的重写，只清残骸。
    makeFile(source, [1, 2, 3])
    _ = journal.prepare(record("t-hash-pristine", false, source))
    check(journal.recover(store).rolledBack == 1, "原图没动过的事务照常销账")
    check(readBytes(source) == [1, 2, 3], "原图一个字节都不该被重写")

    // 崩溃之后、重启之前源文件被外部改过 → 绝不盖回去：备份与日志都留着。
    makeFile(source, [7, 7, 7])
    _ = journal.prepare(record("t-hash-foreign", false, source))
    let foreign = journal.recover(store)
    check(foreign.rolledBack == 0 && foreign.warnings.count == 1,
          "外部修改过的事务不许自动回滚：\(foreign.warnings.joined(separator: " / "))")
    check(readBytes(source) == [7, 7, 7], "外部修改必须原样保留")
    check(fm.fileExists(atPath: backupFile), "唯一的原图副本不许动")
    check(journal.hasPending(), "事务日志必须留着，等下一次启动再试")

    // 跨格式：产物被外部换成别的内容时，只删对得上指纹的，别人占的位置不动。
    let png = canonicalPath(dir + "/photo.png")
    let webp = dir + "/photo.webp"
    makeFile(png, [1, 2, 3])
    writeData(webp, [5, 5])
    _ = journal.prepare(record("t-hash-cross", true, webp))
    _ = journal.recover(store)
    check(fm.fileExists(atPath: webp), "来历不明的内容不许当成自己的残骸删掉")
    check(readBytes(png) == [1, 2, 3], "原图原样保留")
}

// ─── 22. 备份必须"文件 + 来历记录"双全，缺一半就不算备份 ────────────────────
print("[22] 备份必须文件与来历记录双全")
do {
    let root = freshRoot("22")
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-22/m.png")
    makeFile(source, [1, 2, 3])
    let store = HistoryStore(root: root)
    let key = HistoryStore.backupKey(forPath: source)

    // 只有压缩结果、没有来历记录的目录：认不出是谁的原图，绝不能当成备份复用。
    // `history.json` 损坏后就靠 backup-meta.json 重建恢复入口，缺一半等于无从恢复。
    let unlabeled = root + "/backups/\(key)"
    try? fm.createDirectory(atPath: unlabeled, withIntermediateDirectories: true)
    writeData(unlabeled + "/original.png", [9, 9, 9])
    let backup = store.ensureBackup(for: source)
    check(backup != nil, "来历记录缺失时另找槽位重写，而不是返回一个认不出的备份")
    check(backup != unlabeled + "/original.png", "绝不复用没有来历记录的那份")
    check(readBytes(backup!) == [1, 2, 3], "新槽位里是真正原图，不是那张认不出的残骸")
    check(readBytes(unlabeled + "/original.png") == [9, 9, 9],
          "残骸留在原地交给清理流程，不悄悄改写别的东西")

    let metaFile = (backup! as NSString).deletingLastPathComponent
        + "/\(HistoryStore.metaFileName)"
    check(fm.fileExists(atPath: metaFile), "备份写成时来历记录必须一起落盘")
    // JSON 会把路径里的 / 转义成 \/，不还原回来永远比不中。
    let metaRaw = ((String(data: fm.contents(atPath: metaFile) ?? Data(), encoding: .utf8) ?? "")
        .replacingOccurrences(of: "\\/", with: "/"))
    check(metaRaw.contains("\"version\""), "来历记录带版本号：v1 与 v2 要能分开认")
    check(metaRaw.contains(source), "来历记录认得这份原图属于谁")
    check(readBytes(source) == [1, 2, 3], "用户的源文件不受影响")
}

// ─── 23. 恢复的提交顺序：历史写失败时备份必须留着 ────────────────────────────
print("[23] 恢复先写文件，历史失败就保留备份")
do {
    let root = freshRoot("23")
    let store = HistoryStore(root: root)
    let source = canonicalPath(NSTemporaryDirectory() + "sizqo-check-23/n.png")
    makeFile(source, [1, 2, 3])
    let backup = store.ensureBackup(for: source)!
    let entry = HistoryEntry.record(
        source: source, result: result(file: source, size: 3), output: source,
        backup: backup, retentionDays: 3
    )
    store.add(entry)
    writeData(source, [9, 9, 9])   // 模拟已被压缩结果覆盖
    let recorded = store.list()[0]
    // 让 history.json 的落盘必然失败，但**读得到**：根目录设成只读。
    // 若把 history.json 本身做成目录，读也一起坏了，测的就是另一件事（见 [20]）。
    chmod(root, 0o555)

    let outcome = store.restoreOutcome(entry: recorded, force: true)
    chmod(root, 0o755)
    check(!outcome.success, "历史没落盘就不能报\"恢复成功\"")
    check(outcome.error?.contains("文件已恢复") == true,
          "说清文件已回到原图：\(outcome.error ?? "")")
    check(readBytes(source) == [1, 2, 3], "文件确实先回到了原图")
    check(fm.fileExists(atPath: backup), "备份留着，用户重试即可收敛")
}

// ─── 24. 同格式 replace 的输出就是源文件：恢复后绝不删它 ─────────────────────
print("[24] 删压缩输出前必须判同一性")
do {
    let root = freshRoot("24")
    let store = HistoryStore(root: root)
    let realDir = NSTemporaryDirectory() + "sizqo-check-24"
    let source = canonicalPath(realDir + "/o.png")
    makeFile(source, [1, 2, 3])
    let backup = store.ensureBackup(for: source)!
    // 规范路径是 /var/…（resolvingSymlinksInPath 会去掉 /private 前缀），
    // 而 /private/var/… 是同一个文件的另一种写法 —— 字面不等，历史里的
    // outputPath 记录的恰恰是当时那个原始字符串。
    let alias = "/private" + source
    check(alias != source && canonicalPath(alias) == source,
          "构造出同文件不同字面的路径对")
    let entry = HistoryEntry.record(
        source: source, result: result(file: source, size: 3), output: alias,
        backup: backup, retentionDays: 3
    )
    store.add(entry)
    writeData(source, [9, 9, 9])
    let outcome = store.restoreOutcome(entry: store.list()[0], force: true)
    check(outcome.success, "恢复成功：\(outcome.error ?? "")")
    check(fm.fileExists(atPath: alias), "刚写回的原图没被当成压缩产物删掉")
    check(readBytes(source) == [1, 2, 3], "源文件内容确实是真正原图")
}

// ─── 25. 取消不解除暂停；被取消的任务在闸门之前就自己退出 ───────────────────
print("[25] 取消与暂停互不干扰")
do {
    let scheduler = CompressionScheduler(maxParallelism: 2)
    check(scheduler.tryBeginBatch(), "占住这一轮会话")
    scheduler.pause()
    let meter = PeakMeter()
    let group = DispatchGroup()
    group.enter()
    DispatchQueue.global().async {
        // 已取消：哪怕闸门关着，也不该在这儿等下去。
        if case .acquired(let permit) = scheduler.acquireOutcome(cancelled: { true }) {
            meter.begin(); meter.end(); permit.release()
        }
        group.leave()
    }
    _ = group.wait(timeout: .now() + 2)
    check(meter.peak == 0, "取消的任务不占 CPU 名额")
    check(scheduler.isPaused, "取消不把暂停一起解除")

    // wakeWaiters：只唤醒等待者，闸门该关着还是关着。
    scheduler.wakeWaiters()
    check(scheduler.isPaused, "wakeWaiters 不解除暂停")
    let running = DispatchGroup()
    running.enter()
    DispatchQueue.global().async {
        if case .acquired(let permit) = scheduler.acquireOutcome(cancelled: { false }) {
            permit.release()
        }
        running.leave()
    }
    Thread.sleep(forTimeInterval: 0.1)
    check(running.wait(timeout: .now()) == .timedOut, "暂停中等待者仍被拦住")
    scheduler.resume()
    running.wait()
    check(!scheduler.isPaused, "resume 后闸门开")
    scheduler.endBatch()
    check(scheduler.phase == .idle, "批次收尾回到 idle")
}

// ─── 26. 历史页每一行的按钮 = 这条记录此刻真能做到的事 ──────────────────────
print("[26] historyRowActions 与压缩完成行对齐")
do {
    let store = tempStore("26")
    let dir = NSTemporaryDirectory() + "sizqo-check-26"
    let tail: [HistoryRowAction] = [.finder, .copyLog]

    // ① replace + 备份还在 → 有「恢复原图」，没有「删除这次压缩结果」
    let covered = canonicalPath(dir + "/a.png")
    makeFile(covered, [1, 2, 3])
    var replaceMode = result(file: covered, size: 3)
    replaceMode.outputMode = "replace"
    let replaceEntry = HistoryEntry.record(
        source: covered, result: replaceMode, output: covered,
        backup: store.ensureBackup(for: covered), retentionDays: 3)
    store.add(replaceEntry)
    check(historyRowActions(store.find(replaceEntry.id)!) == [.saveAs, .compare, .restore] + tail,
          "覆盖模式：另存为 + 对比 + 恢复原图，而不是删产物")

    // ② 后缀模式：原图从没被盖过，对等的反悔是删掉产物，而不是"恢复"
    let kept = canonicalPath(dir + "/b.png")
    let keptOut = ((kept as NSString).deletingPathExtension) + "_compressed.png"
    makeFile(kept, [7])
    makeFile(keptOut, [8, 8])
    var suffixMode = result(file: kept, size: 2)
    suffixMode.outputMode = "suffix"
    let suffixEntry = HistoryEntry.record(
        source: kept, result: suffixMode, output: keptOut,
        backup: nil, retentionDays: 3)
    store.add(suffixEntry)
    check(historyRowActions(store.find(suffixEntry.id)!) == [.saveAs, .compare, .deleteOutput] + tail,
          "后缀模式：给「删除这次压缩结果」，绝不给「恢复原图」")

    // ③ 用户自己把压缩产物删了：指向它的三个按钮一起收起
    try? fm.removeItem(atPath: keptOut)
    check(historyRowActions(store.find(suffixEntry.id)!) == tail,
          "压缩结果已不在了，这一行就不配再有反悔按钮")

    // ④ 备份被清掉：恢复不能继续挂在页面上骗人
    let orphan = canonicalPath(dir + "/c.png")
    makeFile(orphan, [4, 5, 6])
    var backupGone = result(file: orphan, size: 3)
    backupGone.outputMode = "replace"
    let goneEntry = HistoryEntry.record(
        source: orphan, result: backupGone, output: orphan,
        backup: store.ensureBackup(for: orphan), retentionDays: 3)
    store.add(goneEntry)
    check(historyRowActions(store.find(goneEntry.id)!).contains(.restore),
          "备份在时先给恢复")
    try? fm.removeItem(atPath: store.find(goneEntry.id)!.backupPath!)
    check(historyRowActions(store.find(goneEntry.id)!) == [.saveAs] + tail,
          "原图备份已清理 → 恢复和对比一起收起")

    // ⑤ 已恢复的记录：反悔已经用掉了，不再给第二次
    let undone = canonicalPath(dir + "/d.png")
    makeFile(undone, [9])
    var again = result(file: undone, size: 1)
    again.outputMode = "replace"
    let restoredEntry = HistoryEntry.record(
        source: undone, result: again, output: undone,
        backup: store.ensureBackup(for: undone), retentionDays: 3)
    store.add(restoredEntry)
    _ = try? store.restore(entry: store.find(restoredEntry.id)!, force: false)
    let doneActions = historyRowActions(store.find(restoredEntry.id)!)
    check(!doneActions.contains(.restore) && !doneActions.contains(.deleteOutput),
          "已恢复的行不再提供反悔按钮")
    check(doneActions == tail,
          "已恢复的行不许再拿写回后的原图冒充产物：另存为 / 对比 / 播放输出都不出现，实际 \(doneActions)")

    // ⑥ history.json 损坏后按备份重建的条目：明细丢了，恢复仍然必须在
    let rebuiltSource = canonicalPath(dir + "/e.png")
    makeFile(rebuiltSource, [1])
    let rebuiltBackup = canonicalPath(dir + "/e-backup.png")
    makeFile(rebuiltBackup, [2])
    let rebuilt = HistoryEntry(
        id: "recovery-row", createdAt: SizqoClock.nowMillis, expiresAt: SizqoClock.nowMillis,
        sourcePath: rebuiltSource, outputPath: rebuiltSource,
        fileName: "e.png", outputMode: "replace",
        originalSize: 1, compressedSize: 1, savings: 0, outType: "png", algorithm: "",
        backupPath: rebuiltBackup, status: .recoveryAvailable,
        restoredAt: nil, outputModifiedAt: nil,
        sourceExists: true, backupExists: true, outputExists: true)
    check(historyRowActions(rebuilt) == [.restore] + tail,
          "重建条目只给恢复与日志 —— 没有压缩明细，不许拿源文件冒充产物")
}

// ─── 27. 停止整批：等待中的作废，已经在跑的跑完（与 Rust / Tauri 线同语义） ──
//
// 线程安全的标志盒子：后台 worker 用它把"我拿到名额了吗 / 我退出了吗"记下来。
final class FlagBox: @unchecked Sendable {
    private let lock = NSLock()
    private var flags: Set<String> = []

    func mark(_ name: String) {
        lock.lock()
        flags.insert(name)
        lock.unlock()
    }

    func has(_ name: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return flags.contains(name)
    }
}

print("[27] 停止是批次级的，且不可逆")
do {
    let scheduler = CompressionScheduler(maxParallelism: 2)
    check(scheduler.tryBeginBatch(), "占住这一轮会话")
    let meter = PeakMeter()
    let running = DispatchGroup()
    // 先占满两个名额 —— 这两个就是"正在压缩的图片"。
    for _ in 0..<2 {
        running.enter()
        DispatchQueue.global().async {
            let permit = scheduler.acquire()
            meter.begin()
            Thread.sleep(forTimeInterval: 0.3)
            meter.end()
            permit.release()
            running.leave()
        }
    }
    check(waitUntil { scheduler.activeJobs == 2 }, "两个名额先被占满")

    // 第三个文件堵在闸门上等名额：停止之后它必须自己退出，而不是等着开工。
    let third = FlagBox()
    let thirdGroup = DispatchGroup()
    thirdGroup.enter()
    DispatchQueue.global().async {
        if case .acquired(let permit) = scheduler.acquireOutcome(cancelled: { false }) {
            third.mark("permit")
            permit.release()
        }
        if case .stopped = scheduler.acquireOutcome(cancelled: { false }) {
            third.mark("stopped")
        }
        third.mark("exited")
        thirdGroup.leave()
    }
    Thread.sleep(forTimeInterval: 0.1)
    check(!third.has("exited"), "名额满时第三个文件该在闸门上等")

    scheduler.stop()
    check(scheduler.phase == .stopping, "停止后状态为 stopping")
    check(scheduler.isStopping, "isStopping 同步为真")
    _ = thirdGroup.wait(timeout: .now() + 2)
    check(waitUntil { third.has("exited") }, "停止后等待中的文件自己退出，不再开工")
    check(!third.has("permit"), "被停止作废的文件不许开工")
    check(third.has("stopped"), "等闸门的文件拿到的必须是 stopped（队列里它还是 pending）")
    if case .stopped = scheduler.acquireOutcome(cancelled: { false }) {
        check(true, "停止后闸门对后来者是关的")
    } else {
        check(false, "停止后闸门对后来者是关的")
    }

    // 停止不是暂停：误调 resume() 也不许把批次放回来。
    scheduler.resume()
    if case .stopped = scheduler.acquireOutcome(cancelled: { false }) {
        check(true, "停止不可逆：resume 不放行")
    } else {
        check(false, "停止不可逆：resume 不放行")
    }
    check(scheduler.phase == .stopping, "resume 之后状态仍是 stopping")

    _ = running.wait(timeout: .now() + 3)
    check(meter.done == 2, "已经在跑的两个照旧跑完，一个都没被掐掉")
    check(meter.peak == 2, "停止不打断在跑的任务（峰值 \(meter.peak)）")

    scheduler.endBatch()
    check(scheduler.phase == .idle, "批次收尾回到 idle")
    check(!scheduler.isStopping, "停止标志不泄漏到下一批")
    check(scheduler.tryBeginBatch(), "占住这一轮会话")
    check(scheduler.phase == .running, "新一批从 running 开始")
    scheduler.endBatch()
}

// ─── 28. 暂停中按停止：必须结束整批，而不是等用户点「继续」 ────────────────
print("[28] 暂停中停止")
do {
    let scheduler = CompressionScheduler(maxParallelism: 1)
    check(scheduler.tryBeginBatch(), "占住这一轮会话")
    let held = scheduler.acquire()
    scheduler.pause()
    check(scheduler.phase == .paused, "先暂停")

    let waiting = FlagBox()
    let waitingGroup = DispatchGroup()
    waitingGroup.enter()
    DispatchQueue.global().async {
        if case .acquired(let permit) = scheduler.acquireOutcome(cancelled: { false }) {
            waiting.mark("permit")
            permit.release()
        }
        if case .stopped = scheduler.acquireOutcome(cancelled: { false }) {
            waiting.mark("stopped")
        }
        waiting.mark("exited")
        waitingGroup.leave()
    }
    Thread.sleep(forTimeInterval: 0.1)
    check(!waiting.has("exited"), "暂停期间等待者被拦住")

    scheduler.stop()
    check(scheduler.phase == .stopping, "停止盖过暂停：状态是 stopping 而不是 paused")
    _ = waitingGroup.wait(timeout: .now() + 2)
    check(waitUntil { waiting.has("exited") },
          "暂停中停止：等待者必须退出（判定顺序 取消 → 停止 → 暂停，不能反过来）")
    check(!waiting.has("permit"), "停止作废的文件不许开工")
    check(waiting.has("stopped"), "暂停中停止：等待者拿到 stopped，而不是被当成取消")
    check(scheduler.activeJobs == 1, "在跑的那个照旧跑完")
    held.release()
    scheduler.endBatch()
    check(scheduler.phase == .idle, "暂停中停止也要回到 idle")
}

// ─── 29. 队列状态与会话状态分家（与 Tauri 前端同语义）─────────────────────
//
// 「停止」= 结束这一轮、保留剩余任务；「取消」= 把这个文件移出队列。
// 老实现两者共用一个 .cancelled，于是停止之后剩下的图全部变成"已跳过"，
// 用户再也压不动它们 —— 这一组就是钉住那个不再发生。
print("[29] 队列状态与会话状态分家")
do {
    // 持久状态只有六种，取消不再是其中之一。
    let persisted: [QueueStatus] = [.pending, .running, .done, .failed, .removed, .restored]
    check(persisted.map(\.rawValue) == ["pending", "running", "done", "failed", "removed", "restored"],
          "队列持久状态就是这六个字面量")
    check(QueueStatus(rawValue: "cancelled") == nil,
          "cancelled 不再是队列状态：停止不算取消，取消用 removed")
    check(QueueItem(path: "/a.png", fileName: "a.png", fileSize: 1).status == .pending,
          "新导入的项从 pending 开始")

    // 进度是队列的派生值：只数 pending/running/done/failed。
    let progress = QueueProgress(items: [
        QueueItem(path: "/1.png", fileName: "1.png", fileSize: 0, status: .done),
        QueueItem(path: "/2.png", fileName: "2.png", fileSize: 0, status: .failed),
        QueueItem(path: "/3.png", fileName: "3.png", fileSize: 0, status: .pending),
        QueueItem(path: "/4.png", fileName: "4.png", fileSize: 0, status: .removed),
        QueueItem(path: "/5.png", fileName: "5.png", fileSize: 0, status: .restored),
    ])
    check(progress.total == 3, "removed / restored 不计入队列总数（实际 \(progress.total)）")
    check(progress.processed == 2, "失败也算处理过，否则进度永远到不了终点")
    check(progress.pending == 1 && progress.failed == 1 && progress.done == 1,
          "四类计数各就各位")
    check(progress.fraction > 0.66 && progress.fraction < 0.67, "进度 = 已处理 / 总数")
    check(QueueProgress(items: []).fraction == 0, "空队列进度为 0，不除零")
    // 关键：停止之后剩下的 pending 仍然算在分母里 —— 进度不会从 2/3 掉回 0/1。
    check(progress.summaryText == "2 / 3 已处理 · 失败 1", "摘要把失败数说出来")
}

// ─── 30. 压缩设置的静态默认值（与 Tauri 线同语义）─────────────────────────
//
// 用户导入后先看到的应该是「压缩设置 + 当前参数摘要」这一行，想改再点开。
// Tauri 线在 index.html 的 #settingsPanel 上写死 collapsed（tests/settings-collapse.cjs 钉住），
// Swift 线对应 settingsExpanded 的初值 —— 这里读源码确认它仍是 false，
// 且没有任何地方把它自动置真（那会让"默认折叠"在某条路径上悄悄失效）。
print("[30] 压缩设置默认折叠")
do {
    let appStateSource = try String(
        contentsOfFile: URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Sources/SizqoSwift/ViewModels/AppState.swift").path,
        encoding: .utf8)
    check(appStateSource.contains("@Published var settingsExpanded = false"),
          "settingsExpanded 初值必须是 false（默认折叠）")
    check(!appStateSource.contains("settingsExpanded = true"),
          "AppState 里不许有任何自动展开设置面板的路径")
    check(appStateSource.contains("settingsExpanded.toggle()") == false,
          "展开 / 收起只由视图层的点击驱动，AppState 自己不切换")
    let contentSource = try String(
        contentsOfFile: URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Sources/SizqoSwift/Views/ContentView.swift").path,
        encoding: .utf8)
    check(contentSource.contains("appState.settingsExpanded.toggle()"),
          "只有点标题那一下才切换（ContentView 的 SettingsPanelView 里）")
}

print("[31] JPEG 编码管道真跑（mozjpeg 的 PPM + stdin 链路）")
let jpegPipelineCheck: () -> Void = {
    // 生成一张 1600×1600 渐变 JPEG：足够大，cjpeg 产物必然超过 64KB 管道缓冲，
    // 顺带钉住「stdin 必须并发写」——同步先写完再读 stdout 会永久互锁。
    let side = 1600
    // noneSkipLast：第 4 字节是填充位——直接写裸像素时不写 alpha 也不会像
    // premultipliedLast 那样把 RGB 预乘成 0（那会编出一张纯色图，死锁覆盖就没了）。
    guard let ctx = CGContext(
        data: nil, width: side, height: side, bitsPerComponent: 8,
        bytesPerRow: side * 4, space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
    ), let raw = ctx.data else {
        check(false, "渐变测试图生成失败（环境问题，不是被测代码）")
        return
    }
    let px = raw.bindMemory(to: UInt8.self, capacity: side * side * 4)
    // 逐像素 LCG 噪声：纯渐变（哪怕叠乘法式抖动）ImageIO q95 都能压到 40KB 以下，
    // 死锁回归要求 cjpeg 产物真的超过 64KB 管道缓冲——只有真随机噪声压不动。
    var seed: UInt32 = 20260930
    func nextRand() -> UInt8 {
        seed = seed &* 1664525 &+ 1013904223
        return UInt8(truncatingIfNeeded: seed >> 24)
    }
    for y in 0..<side {
        for x in 0..<side {
            let o = (y * side + x) * 4
            px[o] = nextRand()
            px[o + 1] = nextRand()
            px[o + 2] = nextRand()
        }
    }
    guard let cgImage = ctx.makeImage() else {
        check(false, "噪声图 makeImage 失败")
        return
    }
    let jpgURL = URL(fileURLWithPath: freshRoot("jpeg-pipeline")).appendingPathComponent("grad.jpg")
    let jpgData = NSMutableData()
    guard let dest = CGImageDestinationCreateWithData(
        jpgData, UTType.jpeg.identifier as CFString, 1, nil
    ) else {
        check(false, "ImageDestination 创建失败")
        return
    }
    CGImageDestinationAddImage(dest, cgImage, [kCGImageDestinationLossyCompressionQuality: 0.95] as CFDictionary)
    guard CGImageDestinationFinalize(dest), (jpgData as Data).count > 64 * 1024 else {
        check(false, "测试 JPEG 落盘失败或太小，死锁回归覆盖不到")
        return
    }
    try? (jpgData as Data).write(to: jpgURL)

    // 1) convertToPPM 必须出活：24bpp（width*3 + kCGImageAlphaNone）的 CGContext
    //    在 macOS 上恒返回 nil，v2.5.31~2.5.46 全线 JPEG 因此静默跳过 mozjpeg。
    guard let ppm = CompressionEngine.convertToPPM(file: jpgURL.path) else {
        check(false, "convertToPPM 返回 nil（JPEG 编码管道断了，mozjpeg 永远走不到）")
        return
    }
    let header = "P6\n\(side) \(side)\n255\n".data(using: .ascii)!
    check(ppm.count == header.count + side * side * 3, "PPM 尺寸 = 头 + width*height*3")
    check(ppm.prefix(header.count) == header, "PPM 头部 P6 + 尺寸正确")

    // 2) cjpeg stdin 全链路 + 30s 超时：同步写 stdin 的死锁在这里是失败而不是挂死
    let sem = DispatchSemaphore(value: 0)
    var cjpegOut: Data? = nil
    DispatchQueue.global().async {
        cjpegOut = CLIRunner.run(
            tool: "cjpeg",
            args: ["-quality", "75", "-optimize", "-progressive"],
            stdinData: ppm
        )
        sem.signal()
    }
    check(sem.wait(timeout: .now() + 30) == .success, "cjpeg 管道不死锁（stdin 并发写，产物超 64KB 缓冲）")
    if let out = cjpegOut {
        check(out.prefix(2) == Data([0xFF, 0xD8]), "cjpeg 产物是合法 JPEG（FFD8 魔数）")
        check(out.count < (jpgData as Data).count, "cjpeg 产物比 ImageIO 的 q95 原图小")
    } else {
        check(false, "cjpeg 经 stdin 没有产出（退出码非 0 或 stdout 为空）")
    }

    // 3) compressJPG 在 cjpeg 在场时必须选 mozjpeg 且产物更小
    if CLIRunner.toolURL(for: "cjpeg") != nil {
        var opts = CompressOptions()
        opts.quality = 75
        let r = CompressionEngine.compressJPG(file: jpgURL.path, options: opts)
        check(r.algorithm == "mozjpeg" && r.compressed.count < (jpgData as Data).count,
              "compressJPG 选 mozjpeg 且压得比原图小")
    } else {
        print("  ok   （本机没有 cjpeg，跳过算法选择断言）")
    }
}
jpegPipelineCheck()

print("[32] 主题「自动」必须真跟随系统（appearance = nil，不是快照）")
do {
    let appStateSource = try String(
        contentsOfFile: URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("Sources/SizqoSwift/ViewModels/AppState.swift").path,
        encoding: .utf8)
    check(appStateSource.contains("case .auto: NSApp.appearance = nil"),
          "auto 必须解成 nil（AppKit 才会实时跟随系统外观切换）")
    check(!appStateSource.contains("NSApp.appearance = ns"),
          "不许把 auto 固化成 aqua/darkAqua 快照——系统换主题后就不跟随了")
    check(appStateSource.contains("applyThemeAppearance"),
          "load/cycle 都要走同一个 applyThemeAppearance")
}

print("[33] Evaluate 是唯一回答「这些字节值不值得落盘」的地方")
do {
    func eval(
        _ verdict: EncodeVerdict, reason: ReasonCode = .unknown, success: Bool = true,
        bytes: Int = 800, original: Int = 1000, convert: Bool = false, targetMet: Bool? = nil
    ) -> Evaluation {
        Evaluation.evaluate(verdict: verdict, engineReason: reason, engineSuccess: success,
                            encodedBytes: bytes, originalSize: original,
                            formatConversion: convert, targetMet: targetMet)
    }
    check(eval(.candidate, bytes: 800) == .commit(targetUnmet: false), "压小了才提交")
    check(eval(.candidate, bytes: 1000) == .skip(.noSizeReduction), "一样大既不是收益也不是失败，绝不提交")
    check(eval(.candidate, bytes: 1200) == .skip(.outputLargerThanSource), "压完更大是已跳过（AGENTS.md 第 7 条）")
    check(eval(.keptOriginal, reason: .alreadyUnderTarget, bytes: 1000) == .skip(.alreadyUnderTarget),
          "交回原图字节 = 压根没进编码，不是压缩成功")
    check(eval(.keptOriginal, reason: .noSizeReduction, convert: true) == .skip(.noSizeReduction),
          "要换格式却没产出新格式：落盘的会是原图副本，不能算转换成功")
    check(eval(.candidate, bytes: 5000, original: 1000, convert: true) == .commit(targetUnmet: false),
          "用户明确要换格式时，判据是那个格式在不在，不是有没有变小")
    check(eval(.candidate, targetMet: false) == .commit(targetUnmet: true), "有产物却没打中预算 → 未达成")
    check(eval(.candidate, bytes: 0) == .fail(.encodeFailed), "零字节不可能是产物")
    check(eval(.candidate, success: false) == .fail(.unknown), "判据与 success 打架 → 判失败，不冒充成功")
    check(eval(.rejected, reason: .unsupportedFormat) == .fail(.unsupportedFormat), "连候选都没有 → 失败并带上原因")
    check(eval(.failed) == .fail(.encodeFailed), "失败却没标原因 → 归到编码失败，绝不留白")

    check(Evaluation.skip(.noSizeReduction).preWriteOutcome == (.skippedNoBenefit, .noSizeReduction), "跳过在写盘前就定案")
    check(Evaluation.commit(targetUnmet: false).preWriteOutcome.outcome == nil, "提交一支不许在落盘前预先承诺结论")
    check(Evaluation.commit(targetUnmet: true).committedOutcome == (.targetUnmet, .targetUnmet), "未达目标是带产物的合法结论")
    check(Evaluation.commit(targetUnmet: false).committedOutcome == (.committed, nil), "落盘之后才写 committed")
}

print("[34] 一条结果有没有产物，只有契约一处回答")
do {
    func contract(
        _ outcome: ResultOutcome?, reason: ReasonCode? = nil,
        path: String? = nil, error: String? = nil
    ) -> CompressResult {
        var item = result(file: "/tmp/a.png", size: 800)
        item.outcome = outcome
        item.reasonCode = reason
        item.outputPath = path
        item.error = error
        return item
    }
    let committed = contract(.committed, path: "/tmp/a.min.png")
    let skipped = contract(.skippedNoBenefit, reason: .outputLargerThanSource, error: "原图已是最优，无需替换")
    let unmet = contract(.targetUnmet, reason: .targetUnmet, path: "/tmp/a.min.png")
    let failed = contract(.failed, reason: .decodeFailed, error: "macOS 无法解码此图像")

    check(committed.hasProduct && !skipped.hasProduct, "没落盘的那一支不许冒充产物")
    check(!skipped.hasProduct && skipped.contractQueueStatus == .done, "跳过是这一轮处理完了，不是失败")
    check(failed.contractQueueStatus == .failed, "只有真的坏了才是失败")
    check(skipped.contractStatusText == "已跳过", "跳过读成「已跳过」，不是「已完成」")
    check(unmet.contractStatusText == "已完成 · 未达到目标" && unmet.capabilities.retry, "未达目标要说清没兑现，且能重试")
    check(committed.capabilities == ResultCapabilities(save: true, compare: true, restore: true,
                                                        finder: true, retry: false, log: true),
          "已达成给全套按钮，唯独不给重试——重试不会得到不同结果")
    check(!skipped.capabilities.save && !skipped.capabilities.compare
            && !skipped.capabilities.restore && !skipped.capabilities.finder,
          "没有产物 → 指向那个文件的按钮一个都不许出现，而不是点了再报错")
    check(skipped.capabilities.retry && skipped.capabilities.log, "跳过至少能重试、能看日志")
    check(skipped.contractReasonText == "压缩后文件更大", "「为什么」按 reasonCode 选句子")
    check(skipped.contractDetailText.hasPrefix("压缩后文件更大 · ")
            && skipped.contractDetailText.contains("原图已是最优，无需替换"),
          "后端原话仍进悬停/详情备查，但不再充当任何状态")
    check(!committed.contractShowsWarning && skipped.contractShowsWarning && unmet.contractShowsWarning,
          "三角只说「没产物」或「产物没达标」")

    let allReasons = allReasonCodes
    let blankReasons = allReasons.filter { contract(.failed, reason: $0).contractReasonText.isEmpty }
    check(blankReasons.isEmpty, "有原因却没有句子可说，界面会留白让用户自己猜：\(blankReasons.map { $0.rawValue })")

    var noConclusion = result(file: "/tmp/old.png", size: 900)
    noConclusion.outputPath = "/tmp/old.min.png"
    check(!noConclusion.hasProduct && noConclusion.contractStatusText == "未产出文件",
          "§139 第四阶段：没有结论就不许拿 outputPath 猜一个产物")
    check(noConclusion.contractQueueStatus == .failed && noConclusion.contractLogStatus.status == "unknown",
          "没有结论的生命周期是 Failed、日志写 unknown，不反推 success")
    check(committed.contractLogStatus.status == "committed"
            && skipped.contractLogStatus.reason == "output_larger_than_source",
          "日志写契约原词，跨轨对账才认得出来")
}

print("[35] 发布前核对：结论与字段不自洽必须被拦下")
do {
    func violation(_ outcome: ResultOutcome?, _ path: String?) -> Bool {
        invariantViolation(mediaKind: "image", file: "a.png", outcome: outcome, outputPath: path) != nil
    }
    check(violation(nil, nil), "没有结论就被发布是后端 bug，不许默认读成完成")
    check(violation(.committed, nil), "说「已提交」却没有路径 = 指向空气")
    check(violation(.skippedNoBenefit, "/tmp/a.png"), "说「已跳过」却带着路径，会被历史/恢复/当成产物")
    check(!violation(.targetUnmet, "/tmp/a.png") && !violation(.failed, nil), "带产物的未达成与不带的失败都合法")
}

print("[36] 三轨同源：字面量只有一份，视图不再自己判")
do {
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    func read(_ relative: String) throws -> String {
        try String(contentsOfFile: repoRoot.appendingPathComponent(relative).path, encoding: .utf8)
    }
    let rust = try read("src-tauri/src/result_model.rs")
    let web = try read("frontend/result_state.js")
    let own = try read("swift/Sources/SizqoSwift/Models/ResultContract.swift")

    let outcomeLiterals = ["committed", "skipped_no_benefit", "skipped_already_under_target",
                           "target_unmet", "cancelled", "failed"]
    let reasonLiterals = ["already_under_target", "no_size_reduction", "output_larger_than_source",
                          "target_unmet", "source_read_failed", "source_not_found", "permission_denied",
                          "source_changed", "unsupported_format",
                          "invalid_input", "decode_failed", "encode_failed", "output_validation_failed",
                          "output_write_failed", "output_collision", "backup_failed", "transaction_failed",
                          "history_write_failed", "rollback_failed", "output_missing", "cancelled", "stopped",
                          "unknown"]
    func missing(in source: String, literals: [String], quotedBy: (String) -> String) -> [String] {
        literals.filter { !source.contains(quotedBy($0)) }
    }
    let allLiterals = outcomeLiterals + reasonLiterals
    let rustMissing = missing(in: rust, literals: allLiterals, quotedBy: { "\"\($0)\"" })
    let webMissing = missing(in: web, literals: allLiterals, quotedBy: { "'\($0)'" })
    check(rustMissing.isEmpty, "同一份契约的字面量必须三轨一致，Direct 线缺：\(rustMissing)")
    check(webMissing.isEmpty, "同一份契约的字面量必须三轨一致，前端缺：\(webMissing)")

    // Swift 这边比的是**枚举真实的 rawValue**（那才是跨过 JSON 边界的东西），不是源码里的写法。
    let swiftOutcomes = [ResultOutcome.committed, .skippedNoBenefit, .skippedAlreadyUnderTarget,
                         .targetUnmet, .cancelled, .failed].map(\.rawValue)
    check(Set(swiftOutcomes) == Set(outcomeLiterals),
          "Swift 的结论取值与契约表对不上：\(Set(swiftOutcomes).symmetricDifference(Set(outcomeLiterals)).sorted())")
    check(Set(allReasonCodes.map(\.rawValue)) == Set(reasonLiterals),
          "Swift 的理由取值与契约表对不上，差集：\(Set(allReasonCodes.map(\.rawValue)).symmetricDifference(Set(reasonLiterals)).sorted())")
    check(Set(outcomeLiterals + reasonLiterals).count == outcomeLiterals.count + reasonLiterals.count - 2,
          "字面量表本身要自洽（target_unmet / cancelled 同时是结论与原因）")

    // §70 的措辞表也只有一个版本：同样的结论在两条线上必须说同一句话。
    let wording = ["已完成 · 未达到目标", "已取消", "处理失败", "已跳过", "已完成", "未产出文件"]
    let webMissingWording = wording.filter { !web.contains($0) || !own.contains($0) }
    check(webMissingWording.isEmpty, "§70 措辞表在某一轨上被改写了：\(webMissingWording)")

    // 注释里正是在解释这条禁令，所以扫描前先去掉注释行。
    func codeOnly(_ source: String) -> String {
        source.split(separator: "\n").map(String.init)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }
    let appState = codeOnly(try read("swift/Sources/SizqoSwift/ViewModels/AppState.swift"))
    // §139 第四阶段：CompressResult.success 已删除，连「回写旧旗标」都不再放行；
    // 只剩引擎自身判据（EngineResult）与恢复命令的结论（RestoreOutcome）。
    let allowed: [NSRegularExpression] = [
        try NSRegularExpression(pattern: #"(?:engineResult|outcome)\.success"#),
    ]
    let readers = appState.split(separator: "\n").map(String.init).filter { line in
        line.contains(".success") && !allowed.contains {
            $0.firstMatch(in: line, range: NSRange(line.startIndex..., in: line)) != nil
        }
    }
    check(readers.isEmpty, "AppState 里除引擎判据、命令结论和契约回写外，不许再读 success：\(readers)")

    for name in ["Views/ContentView.swift", "Views/CompareView.swift"] {
        let view = codeOnly(try read("swift/Sources/SizqoSwift/\(name)"))
        let paletteOnly = allowed + [try NSRegularExpression(pattern: #"SizqoPalette\.success"#)]
        let viewReaders = view.split(separator: "\n").map(String.init).filter { line in
            line.contains(".success") && !paletteOnly.contains {
                $0.firstMatch(in: line, range: NSRange(line.startIndex..., in: line)) != nil
            }
        }
        check(viewReaders.isEmpty, "视图里又长出了第二条 success 判据——两个窗口会各判一次：\(name) \(viewReaders)")
        check(view.contains("hasProduct") || view.contains("contract"),
              "\(name) 必须通过结果契约读结论")
    }
}

print("[37] §30 临时文件名随机：并发落盘不许撞名，也不许留下半成品")
do {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("sizqo-staged-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(atPath: dir.path, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(atPath: dir.path) }

    let lock = NSLock()
    var temps: [String] = []
    var problems: [String] = []
    DispatchQueue.concurrentPerform(iterations: 100) { index in
        let target = dir.appendingPathComponent("out-\(index).png").path
        let bytes = Data("payload-\(index)".utf8)
        guard let staged = StagedWrite.write(target: target, bytes: bytes) else {
            lock.withLock { problems.append("#\(index) 没能暂存") }
            return
        }
        lock.withLock { temps.append(staged.temp) }
        let onDisk = (try? Data(contentsOf: URL(fileURLWithPath: target))) ?? Data()
        if onDisk != bytes {
            lock.withLock { problems.append("#\(index) 落盘的是别人的字节") }
        }
    }
    check(problems.isEmpty, "100 个并发落盘互相踩了：\(problems)")
    check(Set(temps).count == temps.count, "并发任务算出了同一个临时文件名")
    check(temps.allSatisfy { $0.hasPrefix(dir.appendingPathComponent(".sizqo-write-").path) },
          "临时文件不再认得出是 Sizqo 写的")
    let leftovers = ((try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? [])
        .filter { $0.hasSuffix(".tmp") }
    check(leftovers.isEmpty, "落盘之后还留着临时文件：\(leftovers)")

    // §30 点名禁的是"用时钟拼临时名"：源码里再出现一次就直接失败，不靠并发测试兜。
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    let clockNamed = try ["Services/OutputTransactionStore.swift", "Services/HistoryStore.swift"]
        .flatMap { name -> [String] in
            let source = try String(
                contentsOfFile: repoRoot.appendingPathComponent("swift/Sources/SizqoSwift/\(name)").path,
                encoding: .utf8)
            let pattern = try NSRegularExpression(pattern: #"\(SizqoClock\.\w+\)[^"]*\.tmp"#)
            let range = NSRange(source.startIndex..., in: source)
            return pattern.matches(in: source, range: range).map { match in
                String(source[Range(match.range, in: source)!])
            }
        }
    check(clockNamed.isEmpty, "又有临时文件按时钟命名了：\(clockNamed)")
}

print("[38] §28 §29 §54 输出名只有一份分配器，事务按模式各自收场")
do {
    let dir = NSTemporaryDirectory() + "sizqo-check-38-\(UUID().uuidString)"
    try? fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
    defer { try? fm.removeItem(atPath: dir) }

    // ① 后缀模式：已存在的名字一律跳过，占位是 0 字节，邻居的字节一个都不许动。
    makeFile(dir + "/photo_compressed.jpg", [7, 7, 7])
    makeFile(dir + "/photo_compressed_1.jpg", [8, 8, 8])
    let suffixNaming = OutputNaming.withSuffix(stem: "photo", suffix: "_compressed", extension: "jpg")
    if let third = OutputNameAllocator.claim(in: dir, suffixNaming) {
        check(third.path == dir + "/photo_compressed_2.jpg",
              "前两个名字都被占了，该取第 3 个（实际 \(third.path)）")
        check(fileLength(third.path) == 0, "占位必须是 0 字节：名字先于内容归这次事务")
        third.release()
        check(!fm.fileExists(atPath: dir + "/photo_compressed_2.jpg"),
              "release 要能把没写成的占位收回去")
    } else {
        check(false, "两个名字被占了就该取第 3 个，结果一个都没拿到")
    }
    check(readBytes(dir + "/photo_compressed.jpg") == [7, 7, 7]
          && readBytes(dir + "/photo_compressed_1.jpg") == [8, 8, 8],
          "第二次压缩把第一次的产物盖掉了 —— 用户要的正是「各留一份」")

    // ② 跨格式覆盖：首选"原图换扩展名"，撞了才带 _converted（与 Rust 同一条序列）。
    let converted = OutputNaming.converted(stem: "photo", extension: "webp")
    check(converted.candidate(0) == "photo.webp"
          && converted.candidate(1) == "photo_converted.webp"
          && converted.candidate(2) == "photo_converted_2.webp",
          "跨格式的名字序列改了：三条轨得同时改，不能只动一处")
    makeFile(dir + "/photo.webp", [5, 5])
    if let first = OutputNameAllocator.claim(in: dir, converted) {
        check(first.path == dir + "/photo_converted.webp",
              "photo.webp 是邻居的成品，「转换」不许盖掉它（实际 \(first.path)）")
        if let second = OutputNameAllocator.claim(in: dir, converted) {
            check(second.path == dir + "/photo_converted_2.webp",
                  "两个 _converted 都在场，就排第 2 号")
            second.release()
        } else { check(false, "第二个 _converted 名字申请不到") }
        first.release()
    } else { check(false, "跨格式转换申请不到名字") }
    check(readBytes(dir + "/photo.webp") == [5, 5], "被占住的原名保持原样")

    // ③ §30 的并发承诺：同一条队列里 50 个任务抢同一个理想名，一个都不许踩。
    let bulkLock = NSLock()
    var bulk: [String] = []
    DispatchQueue.concurrentPerform(iterations: 50) { _ in
        let naming = OutputNaming.withSuffix(stem: "bulk", suffix: "_compressed", extension: "jpg")
        if let got = OutputNameAllocator.claim(in: dir, naming) {
            bulkLock.withLock { bulk.append(got.path) }
        }
    }
    check(bulk.count == 50, "50 个并发申请全该拿到名字（实际 \(bulk.count)）")
    check(Set(bulk).count == bulk.count, "两个任务拿到了同一个输出名")

    // ④ 老日志（0.3.0 之前没有 mode 字段）必须按覆盖模式恢复：认成新建就等于
    //    "原图没被动过"，回滚不再写回备份，用户的那张图永久停在压缩结果上。
    let legacyRoot = freshRoot("38")
    let legacyJournal = OutputTransactionStore(root: legacyRoot)
    let legacyStore = HistoryStore(root: legacyRoot)
    let legacySource = canonicalPath(dir + "/legacy.png")
    makeFile(legacySource, [9, 9, 9])
    let legacyBackup = dir + "/legacy-original.png"
    writeData(legacyBackup, [1, 2, 3])
    check(legacyJournal.prepareDurable(ReplaceTransaction(
        id: "t-legacy", historyId: "t-legacy", sourcePath: legacySource, outputPath: legacySource,
        backupPath: legacyBackup, tempOutputPath: nil, originalSize: 3, expectedOutputSize: 1,
        createdAt: SizqoClock.nowMillis, crossFormat: false)) == nil,
        "覆盖模式的记账凭证要连目录一起落到磁盘")
    let legacyFile = legacyJournal.root + "/t-legacy.json"
    let stripped = ((try? String(contentsOfFile: legacyFile, encoding: .utf8)) ?? "")
        .split(separator: "\n").filter { !$0.contains("\"mode\"") }.joined(separator: "\n")
    writeData(legacyFile, Array(stripped.utf8))
    let legacyLoaded = legacyJournal.list()
    check(legacyLoaded.count == 1 && legacyLoaded[0].mode == .replaceExisting,
          "没有 mode 的老日志读出来必须是覆盖模式")
    check(legacyJournal.recover(legacyStore).rolledBack == 1, "老日志照样回滚")
    check(readBytes(legacySource) == [1, 2, 3], "老日志把真正原图写了回去")

    // ⑤ 新建模式：证明不了提交就删掉自己生成的文件，但一个字节都不许碰原图（§32）。
    let newRoot = freshRoot("38b")
    let newJournal = OutputTransactionStore(root: newRoot)
    let newStore = HistoryStore(root: newRoot)
    let newSource = canonicalPath(dir + "/newsrc.png")
    makeFile(newSource, [4, 4, 4])
    let newOutput = dir + "/newsrc_compressed.png"
    func newTxn(_ id: String, outputSize: Int64, temp: String? = nil, outputSha256: String? = nil) -> ReplaceTransaction {
        ReplaceTransaction(
            id: id, historyId: id, sourcePath: newSource, outputPath: newOutput,
            backupPath: "", tempOutputPath: temp, originalSize: 4, expectedOutputSize: outputSize,
            originalSha256: nil, outputSha256: outputSha256,
            createdAt: SizqoClock.nowMillis, crossFormat: false, mode: .createNew
        )
    }
    writeData(newOutput, [9, 9])
    check(newJournal.prepare(newTxn("t-new", outputSize: 2,
                                    temp: dir + "/t-new.tmp",
                                    outputSha256: sha256OfData(Data([9, 9])))) == nil,
          "新建模式也要记账：没有这条日志，磁盘上多出来的产物没人认领")
    writeData(dir + "/t-new.tmp", [0])
    check(newJournal.recover(newStore).rolledBack == 1, "历史不认账 → 新建模式收场")
    check(!fm.fileExists(atPath: newOutput), "orphan output 被删掉")
    check(!fm.fileExists(atPath: dir + "/t-new.tmp"), "崩溃留下的临时文件一起清掉")
    check(readBytes(newSource) == [4, 4, 4], "新建模式的恢复绝不碰原图")

    // 体积对不上：这位置已被别人的内容占了，不许当成自己的半成品删掉。
    writeData(newOutput, [1, 2, 3, 4, 5])
    _ = newJournal.prepare(newTxn("t-keep", outputSize: 2))
    _ = newJournal.recover(newStore)
    check(fm.fileExists(atPath: newOutput) && readBytes(newOutput) == [1, 2, 3, 4, 5],
          "删掉一个来历不明的文件永远不是恢复的选项")

    // 历史已认账：只销账，产物留着。
    writeData(newOutput, [9, 9])
    _ = newJournal.prepare(newTxn("t-committed-new", outputSize: 2))
    _ = newStore.add(HistoryEntry.record(withId: "t-committed-new", source: newSource,
                                          result: result(file: newSource, size: 2), output: newOutput,
                                          backup: nil, retentionDays: 3))
    let newCommitted = newJournal.recover(newStore)
    check(newCommitted.committed == 1 && fm.fileExists(atPath: newOutput),
          "已提交的新建产物不许删")
}

print("[39] §28 §54 源码里不许有第二套拼名字的地方")
do {
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    let appState = ((try? String(
        contentsOfFile: repoRoot.appendingPathComponent(
            "swift/Sources/SizqoSwift/ViewModels/AppState.swift").path, encoding: .utf8)) ?? "")
        .split(separator: "\n").map(String.init)
        .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
    check(!appState.contains { $0.contains("availableSystemConversionPath") },
          "系统转换又自己拼名字了：§54 要求 suffix / folder / 转换共用同一个分配器")
    check(!appState.contains { $0.contains("for i in 1...") },
          "又出现无限排名的命名循环：撞多少次都不许一直猜下去")
    let lines = appState.joined(separator: "\n")
    check(lines.contains("guard let claimed = OutputNameAllocator.claim"),
          "落盘不再走独占申请：exists() 探测后再写会被另一个任务同时判成\"不存在\"")
    check(lines.contains("placement.journal(transactions, txn)"),
          "记账类别不再由落盘模式派生")
    check(lines.contains("return .invalidDestination(\"请先选择输出目录\")"),
          "folder 没选目录时必须报错，不许 return nil 冒充「压缩完成」")
    check(lines.contains("mode: placement.transactionMode"),
          "事务的 mode 又各判一次了")
    check(!lines.contains("guard let record = txn else"),
          "历史写失败时又给新建模式回了个 nil：§32 要求删掉产物并如实报错")
}

// ─── 40. §24–§27 覆盖依据：源文件身份、提前快照与快照指纹 ────────────────────
print("[40] §24 §25 §26 §27 源文件身份与快照指纹")
do {
    /// 把 atime/mtime 钉到指定纳秒，模拟「原地换内容、时间戳没动」。
    func freezeMtime(_ path: String, nanos: Int64) {
        var times = [timespec(tv_sec: 0, tv_nsec: 0), timespec(tv_sec: 0, tv_nsec: 0)]
        times[0] = timespec(tv_sec: Int(nanos / 1_000_000_000), tv_nsec: Int(nanos % 1_000_000_000))
        times[1] = times[0]
        let rc = path.withCString { utimensat(AT_FDCWD, $0, &times, 0) }
        check(rc == 0, "测试工具：时间戳钉得住（errno \(errno)）")
    }

    let dir = freshRoot("40")
    let source = canonicalPath(dir + "/a.png")
    // 同大小、不同内容：§26 要内容指纹，就是为了这一支只 stat 查不出来的改动。
    let original: [UInt8] = [65, 66, 67, 68, 69, 70, 71, 72]
    let edited: [UInt8] = [73, 74, 75, 76, 77, 78, 79, 80]
    check(original.count == edited.count, "测试前提：两份字节必须同大小，否则快速门就能看出来")
    makeFile(source, original)

    guard case let .success(before) = SourceIdentity.readWithHash(source) else {
        check(false, "编码前该读得出指纹与内容哈希"); exit(1)
    }
    check(before.size == 8 && before.sha256?.count == 64, "覆盖依据带内容指纹（64 位十六进制）")
    check(before.fingerprintIntact(at: source) == .unchanged && before.verify(at: source) == .unchanged,
          "没动过的文件两道门都放行")

    // ① 完整复核（含哈希）看得出改动，② 快速门看不出 —— 这正是 §25 把 verify 放在
    // 认领目标名之前、把快速门只当作改名前最后一道保险的原因。
    writeData(source, edited)
    freezeMtime(source, nanos: before.modified)
    let current = SourceIdentity.read(source)
    guard case let .success(present) = current else {
        check(false, "钉好时间戳的文件该 stat 得到"); exit(1)
    }
    check(!before.sameFingerprint(as: present) || before.sha256 != present.sha256,
          "测试前提：两份指纹至少有一项不同（内容哈希）")
    check(before.fingerprintIntact(at: source) == .unchanged,
          "快速门本该看不出同大小同时间戳的原地改动，否则这条测试没测到点上")
    if case .changed(let detail) = before.verify(at: source) {
        check(detail.contains("字节内容已经不是同一份"), "内容指纹查得出改动并说清是谁对不上：\(detail)")
    } else {
        check(false, "字节都换了还判成没变：内容哈希没生效")
    }

    writeData(source, original)
    freezeMtime(source, nanos: before.modified)
    check(before.verify(at: source) == .unchanged, "换回原本那份字节，复核重新放行")

    // 换了大小：不必读内容就该拦下。
    writeData(source, [1, 2, 3])
    check(before.fingerprintIntact(at: source) != .unchanged, "改过大小的文件要被快速门拦下")

    // 文件没了：读不出来就是证明不了，一样不许盖。
    let gone = dir + "/gone.png"
    makeFile(gone, original)
    guard case let .success(goneIdentity) = SourceIdentity.read(gone) else {
        check(false, "刚写的文件该读得出指纹"); exit(1)
    }
    check(goneIdentity.sha256 == nil, "只问指纹不许读内容")
    try? fm.removeItem(atPath: gone)
    if case .changed(let detail) = goneIdentity.verify(at: gone) {
        check(detail.contains("已经不在这个位置"), "文件没了要照实说：\(detail)")
    } else {
        check(false, "文件都没了还判成没变")
    }

    // §27 快照的两份指纹：编码前留的原图快照，字节必须和源文件同一份。
    let store = HistoryStore(root: dir + "/store")
    let snapshotSource = canonicalPath(dir + "/b.png")
    makeFile(snapshotSource, original)
    guard let backup = store.ensureBackup(for: snapshotSource) else {
        check(false, "快照该写得成"); exit(1)
    }
    let metaPath = ((backup as NSString).deletingLastPathComponent as NSString)
        .appendingPathComponent(HistoryStore.metaFileName)
    let recorded = ((try? JSONSerialization.jsonObject(
        with: fm.contents(atPath: metaPath) ?? Data())) as? [String: Any]) ?? [:]
    check(recorded["version"] as? Int == 3,
          "来历记录升到 v3：没有两份指纹就没法证明副本是原图")
    let sourceHash = recorded["sourceSha256"] as? String
    let copyHash = recorded["backupSha256"] as? String
    check(sourceHash?.count == 64 && sourceHash == copyHash,
          "源文件与副本两份指纹都落盘，而且相等（\(sourceHash ?? "nil") / \(copyHash ?? "nil")）")
    check(store.backupIsFaithful(backup) == nil, "指纹对得上的快照 = 可信")

    // 副本被人换过：改名前那道门必须拦下这次覆盖。
    writeData(backup, [9, 9, 9])
    if let detail = store.backupIsFaithful(backup) {
        check(detail.contains("已经不是当初那份字节"), "副本换过要拦下并说清：\(detail)")
    } else {
        check(false, "副本字节都变了还说可信")
    }

    // 快照不在磁盘上：恢复入口本身就是空的。
    try? fm.removeItem(atPath: backup)
    check(store.backupIsFaithful(backup)?.contains("不在磁盘上") == true,
          "快照没了要拦下")

    // 记录自相矛盾：两份指纹不等，当年复制就没成功过。
    let badSource = canonicalPath(dir + "/c.png")
    makeFile(badSource, original)
    guard let badBackup = store.ensureBackup(for: badSource) else {
        check(false, "第二份快照该写得成"); exit(1)
    }
    let badMeta = ((badBackup as NSString).deletingLastPathComponent as NSString)
        .appendingPathComponent(HistoryStore.metaFileName)
    let zero = String(repeating: "0", count: 64)
    let one = String(repeating: "1", count: 64)
    // 手写记录要字段齐全：Swift 合成的 Decodable 对非可选字段缺键就抛错，
    // 少写一项会让 readMeta 判成"没有记录"，测的就不是自相矛盾那一支了。
    writeData(badMeta, Array(#"{"version":3,"sourcePath":"\#(badSource)","createdAt":1,"originalSize":8,"originalExtension":"png","sourceSha256":"\#(zero)","backupSha256":"\#(one)"}"#.utf8))
    let contradiction = store.backupIsFaithful(badBackup)
    check(contradiction?.contains("自相矛盾") == true,
          "来历记录自己前后不一，也不能当备份用（实际回答：\(contradiction ?? "nil＝可信")）")

    // 老记录（v2，没有指纹）：证明不了 ≠ 证明坏了，升级用户的既有备份还得能用。
    let oldSource = canonicalPath(dir + "/d.png")
    makeFile(oldSource, original)
    guard let oldBackup = store.ensureBackup(for: oldSource) else {
        check(false, "第三份快照该写得成"); exit(1)
    }
    let oldMeta = ((oldBackup as NSString).deletingLastPathComponent as NSString)
        .appendingPathComponent(HistoryStore.metaFileName)
    writeData(oldMeta, Array(#"{"version":2,"sourcePath":"\#(oldSource)","createdAt":1,"originalSize":8,"originalExtension":"png"}"#
        .utf8))
    check(store.backupIsFaithful(oldBackup) == nil, "老记录只缺指纹，不该把用户的覆盖一路拦死")

    // §25 的顺序只能在源码里钉：快照必须早于编码，Commit 里不许再补一份。
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    func codeOnly(_ path: String) -> String {
        ((try? String(contentsOfFile: repoRoot.appendingPathComponent(path).path, encoding: .utf8)) ?? "")
            .split(separator: "\n").map(String.init)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }
    let appState = codeOnly("swift/Sources/SizqoSwift/ViewModels/AppState.swift")
    let captureAt = appState.range(of: "captureReplaceBaseline(path: path, options: options, history: history)")?.lowerBound
    let engineAt = appState.range(of: "CompressionEngine.compress")?.lowerBound
    check(captureAt != nil && engineAt != nil && captureAt! < engineAt!,
          "编码前没备覆盖依据：§24 要求备份先于压缩，晚来的就不是原图")
    check(appState.contains("identity.verify(at: path)"),
          "覆盖前不再完整复核源文件（含内容指纹）：§26 的最终确认丢了")
    check(appState.contains("staged.writeSynced(compressed)") && appState.contains("staged.publish(to: target)"),
          "候选落盘与改名又合成一步：改名前那道复核没有插进去的位置")
    check(appState.contains("history.backupIsFaithful(backup)"), "改名前不再确认快照字节：§27 的复核丢了")
    check(!appState.contains("ensureBackup(for:"),
          "Commit 里又现取了一次快照：§24 的提前备份被绕开，备到的可能是改动后的内容")
}

// ─── 41. §37 §38 同一时刻只有一个 compression session ──────────────────────
//
// 这一条不是形式问题。两轮会话共用同一个闸门时，先结束那一轮的收尾会把另一轮的
// 暂停位、停止位和取消账本一起清零 —— 用户看到的就是「按了暂停却还在压」「停止后
// 进度凭空多一截」。所以入口只有一个：抢到就跑，抢不到就拒绝。
print("[41] §37 §38 会话唯一，三条入口共用一套语义")
do {
    final class ClaimTally: @unchecked Sendable {
        private let lock = NSLock()
        private var value = 0
        func add() { lock.lock(); value += 1; lock.unlock() }
        var count: Int { lock.lock(); defer { lock.unlock() }; return value }
    }

    let race = CompressionScheduler(maxParallelism: 4)
    let tally = ClaimTally()
    let raceGroup = DispatchGroup()
    for _ in 0..<8 {
        raceGroup.enter()
        DispatchQueue.global().async {
            if race.tryBeginBatch(maxParallelism: 4) { tally.add() }
            raceGroup.leave()
        }
    }
    raceGroup.wait()
    check(tally.count == 1, "八个入口同时来抢，只有 1 个占得住这一轮（实测 \(tally.count) 个）")
    check(race.phase == .running, "抢到会话的那一轮就是 running")
    race.endBatch()
    check(race.tryBeginBatch(), "上一轮收尾后，下一轮才准占住会话")

    // 抢输了必须一个状态都不改：正在跑那一轮的停止位被第二个入口悄悄解除，
    // 就是「点了停止却还有文件被放进来」。
    let held = CompressionScheduler(maxParallelism: 2)
    check(held.tryBeginBatch(), "第一轮占住会话")
    held.stop()
    check(!held.tryBeginBatch(), "已经有一轮在跑时，第二个入口必须被拒")
    check(held.isStopping, "被拒的入口把正在跑那一轮的停止位解除了")
    check(held.phase == .stopping, "被拒的入口改写了正在跑那一轮的阶段")

    // §38 语义只有一份：Swift 不许留「无条件开一批」的后门，Rust 与视频也不许各造一套。
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    func codeOnly(_ path: String) -> String {
        ((try? String(contentsOfFile: repoRoot.appendingPathComponent(path).path, encoding: .utf8)) ?? "")
            .split(separator: "\n").map(String.init)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }
    check(!codeOnly("swift/Sources/SizqoSwift/Services/CompressionScheduler.swift").contains("func beginBatch("),
          "调度器又开出无条件占会话的后门：§37 只许 tryBeginBatch")
    let appState = codeOnly("swift/Sources/SizqoSwift/ViewModels/AppState.swift")
    check(appState.contains("guard scheduler.tryBeginBatch(maxParallelism: effectiveCpuThreadLimit) else"),
          "开跑不再先占会话凭证：两轮可以并跑，先结束那轮会把另一轮的暂停 / 停止 / 取消清零")
    let rustCommands = codeOnly("src-tauri/src/commands.rs")
    check(!rustCommands.contains("fn begin_batch("),
          "Rust 侧也留了无条件入口：§37 要求图片 / 视频 / 混合共用同一个 try_begin_batch")
    check(rustCommands.contains("if !state.compression.try_begin_batch()"),
          "Rust 的会话凭证不再走 try_begin_batch，等于绕开统一入口")
    let videoCommands = codeOnly("src-tauri/src/video_commands.rs")
    check(videoCommands.contains("SessionGuard::begin"),
          "视频不再从统一凭证开跑：§38 要求图片 / 视频 / 混合共用同一个 guard")
    check(!videoCommands.contains("try_begin_batch"),
          "视频侧又自己摆弄调度器的会话位：语义该只有一份")
}

// ─── 42. §39 §40 来源根在入队时定死 ────────────────────────────────────────
//
// 「输出到指定文件夹」要保住源目录层级。层级由一个来源根算出来 —— 只要这个根是
// 每批重新挑的，同一张图在重试 / 续跑后就可能落到另一个子目录。所以：入队时把
// 根和相对路径一起固定，之后谁都不许再读批次级的候选根。
print("[42] §39 §40 来源根与相对路径在入队时定死")
do {
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    func codeOnly(_ path: String) -> String {
        ((try? String(contentsOfFile: repoRoot.appendingPathComponent(path).path, encoding: .utf8)) ?? "")
            .split(separator: "\n").map(String.init)
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
    }
    let options = codeOnly("swift/Sources/SizqoSwift/Models/CompressOptions.swift")
    check(!options.contains("sourceRoots"),
          "批次级的 sourceRoots 又回来了：每批重新挑根就是「同一张图换了轮次换子目录」")
    let model = codeOnly("swift/Sources/SizqoSwift/Models/CompressResult.swift")
    check(model.contains("var sourceRoot: String = \"\"") && model.contains("var relativePath: String = \"\""),
          "队列项没有固定 sourceRoot / relativePath：§39 要求这两项在创建时确定")
    let appState = codeOnly("swift/Sources/SizqoSwift/ViewModels/AppState.swift")
    check(appState.contains("let root = Self.deepestRoot(file: path, roots: importRoots) ?? parent"),
          "入队没有按这次导入的根算来源根：又退回「每批重新猜」")
    check(appState.contains("sourceRoot: root,") && appState.contains("relativePath: relative"),
          "队列项没有把来源根与相对路径存下来，重试 / 续跑就只能再猜一次")
    check(!appState.contains("options.sourceRoots"),
          "输出路径还在读批次级来源根：§40 要求只读队列项固定下来的字段")
    check(appState.contains("let rel = relativePath.isEmpty ? (path as NSString).lastPathComponent : relativePath"),
          "目录输出没有使用队列项的 relativePath：层级仍来自当批的临时判断")
    check(appState.contains("history: store, transactions: journal, retentionDays: retention")
            && appState.contains("relativePath: relativePath, options: opts"),
          "relativePath 没有从队列项一路传到 Commit：中途断了等于没固定")
    let rustCommands = codeOnly("src-tauri/src/commands.rs")
    check(!rustCommands.contains("fallback_root"),
          "Rust 图片线又按「这一批只有一个目录 / 文件父目录」补根：§40 禁止重复猜")
    let rustVideo = codeOnly("src-tauri/src/video/transaction.rs")
    check(rustCommands.contains("crate::source_root::relative_to_roots")
            && rustVideo.contains("crate::source_root::relative_to_roots"),
          "图片线与视频线没有共用同一份来源根解析：同一次导入会在两条线上落到不同层级")
    let rustImport = codeOnly("src-tauri/src/video_commands.rs")
    check(rustImport.contains("source_root:String,relative_path:String"),
          "导入结果没有带回 sourceRoot / relativePath：前端入队时无值可存")
}

// ─── 43. §41 §42 §43 重试保持身份，进度不倒退 ─────────────────────────────
//
// 「重试」以前会把状态放回 pending，而进度按 done + failed 的瞬时值算，
// 于是 10/10 会在点下重试的一刻掉成 9/10。现在队列项多两个事实：
// attemptCount（开工时加一）与 everProcessed（至少走完过一次的闩锁）——
// 分子读闩锁，当前状态照旧可以变。
print("[43] §41 §42 §43 重试不换身份，进度不倒退")
do {
    var retried = QueueItem(path: "/tmp/retry.jpg", fileName: "retry.jpg", fileSize: 100)
    retried.everProcessed = true          // 第一次处理已经走完（失败也是走完）
    retried.attemptCount = 1
    retried.status = .failed

    let afterFailure = QueueProgress(items: [retried])
    check(afterFailure.processed == 1 && afterFailure.failed == 1,
          "失败的任务必须算「已处理」，否则 98 成功 + 2 失败的队列永远停在 98%")

    // 用户点「重试」：同一个项回到 pending；attemptCount 等真正开工才加。
    retried.status = .pending
    let waitingRetry = QueueProgress(items: [retried])
    check(waitingRetry.processed == 1, "§42 重试等待中 processed 倒退了")
    check(waitingRetry.retrying == 1, "§42 重试等待中没有单独的 retrying 计数")
    check(waitingRetry.summaryText.contains("正在重试 1 项"),
          "摘要没有说明分子为什么没退：\(waitingRetry.summaryText)")

    // 开工：pending → running，attemptCount += 1（同一次尝试只加一次）。
    retried.status = .running
    retried.attemptCount += 1
    let runningRetry = QueueProgress(items: [retried])
    check(runningRetry.processed == 1 && runningRetry.active == 1 && runningRetry.retrying == 1,
          "§41 §42 重试运行中的计数不对：\(runningRetry.summaryText)")

    // 重试成功：done + 产物 → committed；retrying 归零，processed 不变。
    var product = result(file: retried.path, size: 80)
    product.outcome = .committed
    product.outputPath = "/tmp/retry_compressed.jpg"
    retried.status = .done
    retried.result = product
    let succeeded = QueueProgress(items: [retried])
    check(succeeded.processed == 1 && succeeded.completed == 1 && succeeded.committed == 1
            && succeeded.skipped == 0 && succeeded.retrying == 0,
          "§43 committed / completed / retrying 的拆分不对")
    check(succeeded.summaryText == "1 / 1 已处理",
          "重试收尾后摘要不该再写重试：\(succeeded.summaryText)")

    // done 但没有产物 = skipped，同样算 completed、算 processed。
    var skippedItem = QueueItem(path: "/tmp/skip.jpg", fileName: "skip.jpg", fileSize: 100)
    skippedItem.status = .done
    skippedItem.everProcessed = true
    let skipped = QueueProgress(items: [skippedItem])
    check(skipped.completed == 1 && skipped.skipped == 1 && skipped.committed == 0 && skipped.processed == 1,
          "§43 没有产物的 completed 必须记在 skipped")

    // removed / restored 仍然不在这套账里（分母、分子都不含）。
    var restored = retried
    restored.status = .restored
    let withRestored = QueueProgress(items: [retried, restored])
    check(withRestored.total == 1 && withRestored.processed == 1,
          "restored 不该回到进度账里")

    // 源码护栏：重试不许删项重建；开工点加计数；完成点落闩锁。
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    let appState = (try? String(contentsOfFile: repoRoot.appendingPathComponent("swift/Sources/SizqoSwift/ViewModels/AppState.swift").path, encoding: .utf8)) ?? ""
    if let start = appState.range(of: "func retryFile(path: String)")?.lowerBound,
       let end = appState.range(of: "/// 摘要里跟着阶段走的那一小段")?.lowerBound, start < end {
        let retryBody = String(appState[start..<end])
        check(!retryBody.contains("items.remove") && !retryBody.contains("items.append"),
              "§41 retryFile 删掉旧项重建：队列项 id / 身份会换，历史与进度对不上")
        check(retryBody.contains("items[idx].status = .pending") && retryBody.contains("items[idx].result = nil"),
              "retryFile 没有把原来那一项放回 pending")
    } else {
        check(false, "找不到 retryFile 的边界，§41 的源码护栏没有真正跑到")
    }
    check(appState.contains("self.items[idx].attemptCount += 1"),
          "§41 开工没有累计 attemptCount")
    check(appState.contains("items[idx].everProcessed = true"),
          "§42 完成没有落 everProcessed 闩锁")
    let model = (try? String(contentsOfFile: repoRoot.appendingPathComponent("swift/Sources/SizqoSwift/Models/CompressResult.swift").path, encoding: .utf8)) ?? ""
    check(model.contains("var attemptCount: Int = 0") && model.contains("var everProcessed: Bool = false"),
          "QueueItem 缺少 attemptCount / everProcessed")
    check(!model.contains("var processed: Int { done + failed }"),
          "§42 进度又退回 done + failed 的瞬时值：重试会倒退")
}

print("[44] §44 压缩会话期间恢复不改写进度")
do {
    let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    func read(_ path: String) -> String {
        (try? String(contentsOfFile: repoRoot.appendingPathComponent(path).path, encoding: .utf8)) ?? ""
    }
    /// 截取一段函数体：边界找不到就返回 nil，由调用方判失败，而不是静默通过。
    func slice(_ source: String, from start: String, to end: String) -> String? {
        guard let lower = source.range(of: start)?.lowerBound,
              let upper = source.range(of: end, range: lower..<source.endIndex)?.lowerBound,
              lower < upper else { return nil }
        return String(source[lower..<upper])
    }

    let appState = read("swift/Sources/SizqoSwift/ViewModels/AppState.swift")
    check(appState.contains("var restoreBlockedBySession: Bool { isCompressing }"),
          "§44 恢复闸门没有绑定当前压缩会话")
    let blockedText = "压缩进行中，暂不能恢复；请先停止或等这一轮结束"
    check(appState.contains("var restoreBlockedText: String { \"\(blockedText)\" }"),
          "§44 恢复禁用说明缺失或被改写")
    // 两条线的用户看到同一句话；只有一份措辞来源，才不会被改出两种解释。
    check(read("frontend/app.js").contains(blockedText),
          "§44 Swift 与前端对同一条禁令说了两句话：\(blockedText)")

    // 三个入口都必须先拦再碰磁盘：恢复单条、恢复全部、删除结果。
    if let restore = slice(appState, from: "func restore(entry: HistoryEntry) -> Bool {",
                           to: "/// 后端确认恢复成功后") {
        check(restore.contains("guard !restoreBlockedBySession"),
              "§44 restore(entry:) 没有会话闸门：压缩进行中恢复会改写正在处理的源文件")
        if let gate = restore.range(of: "guard !restoreBlockedBySession"),
           let write = restore.range(of: "history.restoreOutcome") {
            check(gate.lowerBound < write.lowerBound, "§44 闸门放在了恢复动作之后，等于没拦")
        } else {
            check(false, "§44 restore(entry:) 的闸门 / 落盘顺序无法核对")
        }
    } else {
        check(false, "找不到 restore(entry:) 的边界，§44 源码护栏没有真正跑到")
    }

    if let all = slice(appState, from: "func restoreAll() {", to: "// MARK: - Save as") {
        check(all.contains("guard !restoreBlockedBySession"),
              "§44 restoreAll 绕过了统一服务，也没有自己的会话闸门")
        if let gate = all.range(of: "guard !restoreBlockedBySession"),
           let start = all.range(of: "let restorable") {
            check(gate.lowerBound < start.lowerBound, "§44 restoreAll 先开工后拦，顺序不对")
        } else {
            check(false, "§44 restoreAll 的闸门 / 开工顺序无法核对")
        }
    } else {
        check(false, "找不到 restoreAll 的边界，§44 源码护栏没有真正跑到")
    }

    if let del = slice(appState, from: "func deleteHistoryOutput(_ entry: HistoryEntry) {",
                       to: "/// 历史行的复制日志") {
        check(del.contains("guard !restoreBlockedBySession"),
              "§44 deleteHistoryOutput 没有会话闸门：会话期间会把正在用的产物删掉")
        if let gate = del.range(of: "guard !restoreBlockedBySession"),
           let alert = del.range(of: "let alert = NSAlert()") {
            check(gate.lowerBound < alert.lowerBound,
                  "§44 deleteHistoryOutput 先弹确认再拦截，用户白点一次")
        } else {
            check(false, "§44 deleteHistoryOutput 的闸门 / 确认顺序无法核对")
        }
    } else {
        check(false, "找不到 deleteHistoryOutput 的边界，§44 源码护栏没有真正跑到")
    }

    // 禁用不是隐藏：按钮仍在动作集里，只是带一个原因（与前端同一口径）。
    check(appState.contains("func historyActionBlocked(_ action: HistoryRowAction) -> Bool")
            && appState.contains("(action == .restore || action == .deleteOutput)"),
          "§44 历史页的会话禁用判定没有覆盖恢复/删除结果")
    let content = read("swift/Sources/SizqoSwift/Views/ContentView.swift")
    check(content.contains("disabled: blocked") && content.contains(".disabled(action.disabled)"),
          "§44 主队列行/详情没有把恢复按钮禁用")
    check(content.contains("blockedReason: blocked ? appState.restoreBlockedText : nil"),
          "§44 主队列的禁用按钮没说清为什么点不了")
    let history = read("swift/Sources/SizqoSwift/Views/HistoryView.swift")
    check(history.contains("appState.historyActionBlocked(action)") && history.contains(".disabled(blocked)"),
          "§44 历史页恢复按钮没有随会话禁用")
    let compare = read("swift/Sources/SizqoSwift/Views/CompareView.swift")
    check(compare.contains(".disabled(appState.restoreBlockedBySession)"),
          "§44 对比窗口的恢复按钮没有被会话拦住")
}

print(failures == 0
      ? "\n✓ Swift 历史 / 备份 / 暂停 / 停止 / 队列进度 / 设置默认折叠 / CPU 上限 / 结果契约 / 临时文件命名 / 输出命名与事务模式自检全部通过"
      : "\n✗ Swift 自检失败 \(failures) 项")
exit(failures == 0 ? 0 : 1)
