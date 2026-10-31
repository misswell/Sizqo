// ContractVectors.swift —— §9.2 双引擎契约向量的 Swift 执行器。
//
// `tests/contracts/*.json` 里的每一组场景由 Rust 与 Swift **各自**对同一份
// 期望作答。本文件只提供 `runContractVectors()`，由 main.swift 在汇总前调用；
// 场景语义与 Rust 侧 `src/contract_vectors.rs` 一一对应，新增向量必须两边
// 同时实现。共享工具（makeFile/writeData/result/freshRoot/canonicalPath/check）
// 来自 main.swift。

import Foundation

func contractRepoRoot() -> String {
    URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()   // HistoryStoreCheck
        .deletingLastPathComponent()   // Tests
        .deletingLastPathComponent()   // SizqoSwift
        .deletingLastPathComponent()   // swift
        .path
}

func contractFreezeMtime(_ path: String, nanos: Int64) {
    var times = [timespec(tv_sec: 0, tv_nsec: 0), timespec(tv_sec: 0, tv_nsec: 0)]
    times[0] = timespec(tv_sec: Int(nanos / 1_000_000_000), tv_nsec: Int(nanos % 1_000_000_000))
    times[1] = times[0]
    let rc = path.withCString { utimensat(AT_FDCWD, $0, &times, 0) }
    check(rc == 0, "契约工具：时间戳钉得住（errno \(errno)）")
}

func contractDowngradeMetaToV1(_ backup: String, _ source: String) {
    let metaDir = (backup as NSString).deletingLastPathComponent
    let legacy = "{\"version\":1,\"sourcePath\":\"\(canonicalPath(source))\",\"createdAt\":1}"
    writeData((metaDir as NSString).appendingPathComponent("backup-meta.json"), Array(legacy.utf8))
}

func contractReplaceTxn(source: String, backup: String, outputSha256: String,
                        id: String, schemaVersion: Int) -> ReplaceTransaction {
    ReplaceTransaction(
        id: "t-\(id)", historyId: id, sourcePath: source, outputPath: source,
        backupPath: backup, tempOutputPath: nil,
        originalSize: Int64(readBytes(backup).count),
        expectedOutputSize: Int64(readBytes(source).count),
        originalSha256: try? sha256OfFile(backup).get(),
        outputSha256: outputSha256,
        backupSha256: try? sha256OfFile(backup).get(),
        placeholderIdentity: nil,
        createdAt: SizqoClock.nowMillis, crossFormat: false,
        mode: .replaceExisting, schemaVersion: schemaVersion)
}

func contractLegacyTxn(source: String, backup: String, id: String) -> ReplaceTransaction {
    ReplaceTransaction(
        id: "t-\(id)", historyId: id, sourcePath: source, outputPath: source,
        backupPath: backup, tempOutputPath: nil,
        originalSize: 100, expectedOutputSize: 10,
        originalSha256: nil, outputSha256: nil, backupSha256: nil,
        placeholderIdentity: nil,
        createdAt: SizqoClock.nowMillis, crossFormat: false,
        mode: .replaceExisting, schemaVersion: 0)
}

/// 单个场景的执行：与 Rust `run_scenario` 的分支一一对应，返回 outcome 字符串。
func contractRunScenario(_ scenario: String, _ dir: String) -> String {
    let original = canonicalPath(dir + "/original.bin")
    let compressed = dir + "/compressed.bin"
    let foreign = dir + "/foreign.bin"
    let backup = dir + "/backup.bin"

    switch scenario {
    // ── file_identity ────────────────────────────────────────────────
    case "same_size_same_mtime_edit_detected", "fast_gate_ignores_same_size_same_mtime_edit":
        makeFile(original, [97, 97, 97, 97])
        guard case .success(let identity) = SourceIdentity.readWithHash(original) else { return "error" }
        writeData(original, [98, 98, 98, 98])
        contractFreezeMtime(original, nanos: identity.modified)
        let unchanged: Bool
        if scenario.hasPrefix("same_size") {
            if case .unchanged = identity.verify(at: original) { unchanged = true } else { unchanged = false }
        } else {
            if case .unchanged = identity.fingerprintIntact(at: original) { unchanged = true } else { unchanged = false }
        }
        return unchanged ? "unchanged" : "changed"
    case "size_change_detected_by_fingerprint":
        makeFile(original, [97, 97, 97, 97])
        guard case .success(let identity) = SourceIdentity.read(original) else { return "error" }
        writeData(original, [97, 97, 97, 97, 65, 65, 65, 65])
        if case .unchanged = identity.fingerprintIntact(at: original) { return "unchanged" }
        return "changed"
    case "untouched_file_passes_verify":
        makeFile(original, [115, 97, 109, 101])
        guard case .success(let identity) = SourceIdentity.readWithHash(original) else { return "error" }
        if case .unchanged = identity.verify(at: original) { return "unchanged" }
        return "changed"
    case "missing_file_reported_changed":
        makeFile(original, [103, 111, 110, 101])
        guard case .success(let identity) = SourceIdentity.readWithHash(original) else { return "error" }
        try? FileManager.default.removeItem(atPath: original)
        if case .unchanged = identity.verify(at: original) { return "unchanged" }
        return "changed"
    // ── backup_verification ──────────────────────────────────────────
    case "backup_reused_when_current_bytes_match_record":
        let store = HistoryStore(root: dir + "/history")
        makeFile(original, [116, 114, 117, 101])
        let hash = sha256OfData(Data([116, 114, 117, 101]))
        let first = store.ensureBackupVerified(for: original, currentSha256: hash)
        let second = store.ensureBackupVerified(for: original, currentSha256: hash)
        return (first != nil && first == second) ? "reused" : "newSlot"
    case "backup_new_slot_for_user_edit", "backup_new_slot_when_backup_bytes_tampered":
        let store = HistoryStore(root: dir + "/history")
        makeFile(original, [116, 114, 117, 101])
        let first = store.ensureBackupVerified(for: original, currentSha256: sha256OfData(Data([116, 114, 117, 101])))
        if scenario == "backup_new_slot_when_backup_bytes_tampered", let first = first {
            writeData(first, [116, 97, 109, 112])
        }
        writeData(original, [117, 115, 101, 114])
        let second = store.ensureBackupVerified(for: original, currentSha256: sha256OfData(Data([117, 115, 101, 114])))
        return (first != nil && first == second) ? "reused" : "newSlot"
    case "legacy_backup_without_evidence_not_reused":
        let store = HistoryStore(root: dir + "/history")
        makeFile(original, [116, 114, 117, 101])
        let first = store.ensureBackupVerified(for: original, currentSha256: sha256OfData(Data([116, 114, 117, 101])))
        if let first = first {
            contractDowngradeMetaToV1(first, original)
        }
        writeData(original, [117, 115, 101, 114])
        let second = store.ensureBackupVerified(for: original, currentSha256: sha256OfData(Data([117, 115, 101, 114])))
        return (first != nil && first == second) ? "reused" : "newSlot"
    case "legacy_backup_upgraded_with_history_evidence", "backup_chain_own_product_reuses_first_slot":
        let store = HistoryStore(root: dir + "/history")
        makeFile(original, [116, 114, 117, 101])
        let first = store.ensureBackupVerified(for: original, currentSha256: sha256OfData(Data([116, 114, 117, 101])))
        writeData(original, [111, 119, 110, 112])
        if let first = first {
            let entry = HistoryEntry.record(
                source: original,
                result: result(file: original, size: 4),
                output: original, backup: first, retentionDays: 3)
            store.add(entry)
        }
        if scenario == "legacy_backup_upgraded_with_history_evidence", let first = first {
            contractDowngradeMetaToV1(first, original)
        }
        let second = store.ensureBackupVerified(for: original, currentSha256: sha256OfData(Data([111, 119, 110, 112])))
        return (first != nil && first == second) ? "reused" : "newSlot"
    // ── history_conflict ─────────────────────────────────────────────
    case "conflict_when_output_content_differs", "no_conflict_when_output_content_matches":
        let store = HistoryStore(root: dir + "/history")
        makeFile(original, [116, 114, 117, 101])
        let backupPath = store.ensureBackupVerified(for: original, currentSha256: sha256OfData(Data([116, 114, 117, 101])))
        writeData(original, [99, 111, 109, 112])
        guard let backupPath = backupPath else { return "error" }
        let entry = HistoryEntry.record(
            source: original,
            result: result(file: original, size: 4),
            output: original, backup: backupPath, retentionDays: 3)
        if scenario.contains("content_differs") {
            // 同大小、同 mtime 的原地改动：只有内容指纹查得出。
            writeData(original, [99, 111, 110, 116])
            contractFreezeMtime(original, nanos: (entry.outputModifiedAt ?? 0) * 1_000_000)
        }
        return HistoryStore.hasConflict(entry) ? "conflict" : "noConflict"
    case "legacy_entry_conflict_by_mtime_drift", "legacy_entry_no_conflict_when_size_mtime_match":
        makeFile(original, [112, 97, 121, 108])
        guard case .success(let identity) = SourceIdentity.readWithHash(original) else { return "error" }
        var entry = HistoryEntry.record(
            source: original,
            result: result(file: original, size: 4),
            output: original, backup: nil, retentionDays: 3)
        entry.outputSha256 = nil
        entry.sourceSha256 = nil
        entry.backupSha256 = nil
        entry.outputModifiedAt = identity.modified / 1_000_000 + (scenario.contains("drift") ? -60_000 : 0)
        return HistoryStore.hasConflict(entry) ? "conflict" : "noConflict"
    // ── transaction_recovery ─────────────────────────────────────────
    case "rollback_restores_when_source_is_staged_output", "residue_only_when_source_is_pristine_original",
         "scene_preserved_when_source_is_foreign", "voucher_mismatch_refuses_restore":
        let store = HistoryStore(root: dir + "/history")
        let journal = OutputTransactionStore(root: (dir as NSString).appendingPathComponent("history/transactions"))
        makeFile(backup, [116, 114, 117, 101])
        makeFile(compressed, [99, 111, 109, 112])
        makeFile(foreign, [117, 115, 101, 114])
        switch scenario {
        case "rollback_restores_when_source_is_staged_output", "voucher_mismatch_refuses_restore":
            makeFile(original, [99, 111, 109, 112])
        case "residue_only_when_source_is_pristine_original":
            makeFile(original, [116, 114, 117, 101])
        default:
            makeFile(original, [117, 115, 101, 114])
        }
        let outputHash = (scenario == "scene_preserved_when_source_is_foreign")
            ? sha256OfData(Data([99, 111, 109, 112]))
            : (try? sha256OfFile(original).get()) ?? ""
        let txn = contractReplaceTxn(source: original, backup: backup,
                                     outputSha256: outputHash, id: "crash", schemaVersion: 2)
        check(journal.prepare(txn) == nil, "契约 \(scenario)：v2 凭证必须落得了账")
        if scenario == "voucher_mismatch_refuses_restore" {
            // 记账**之后**备份被换掉：凭证与字节对不上。
            makeFile(backup, [116, 97, 109, 112])
        }
        let report = journal.recover(store)
        let sourceBytes = readBytes(original)
        switch scenario {
        case "rollback_restores_when_source_is_staged_output":
            return (report.rolledBack == 1 && sourceBytes == [116, 114, 117, 101]) ? "restored" : "preserved"
        case "residue_only_when_source_is_pristine_original":
            return (report.rolledBack == 1 && sourceBytes == [116, 114, 117, 101]) ? "residueOnly" : "preserved"
        default:
            let expected: [UInt8] = scenario == "voucher_mismatch_refuses_restore"
                ? [99, 111, 109, 112] : [117, 115, 101, 114]
            return (report.rolledBack == 0 && journal.hasPending() && sourceBytes == expected)
                ? "preserved" : "restored"
        }
    case "manual_restore_with_confirmation_restores", "manual_restore_refuses_without_confirmation":
        // §3.4 人工恢复：确认在场才允许覆盖"证明不了归属"的现场。
        let store = HistoryStore(root: dir + "/history")
        let journal = OutputTransactionStore(root: (dir as NSString).appendingPathComponent("history/transactions"))
        makeFile(backup, [116, 114, 117, 101])
        makeFile(foreign, [117, 115, 101, 114])
        makeFile(compressed, [99, 111, 109, 112])
        makeFile(original, [117, 115, 101, 114])
        let txn = contractReplaceTxn(source: original, backup: backup,
                                     outputSha256: sha256OfData(Data([99, 111, 109, 112])),
                                     id: "manual", schemaVersion: 2)
        check(journal.prepare(txn) == nil, "契约 \(scenario)：凭证落账")
        let confirm = scenario.hasPrefix("manual_restore_with")
        let error = journal.manualRestore(txn, confirmOverwrite: confirm)
        if error != nil { return "refused" }
        journal.finish(txn.id)
        return readBytes(original) == [116, 114, 117, 101] ? "restoredByUser" : "preserved"
    case "legacy_unknown_source_preserved", "legacy_source_equals_backup_residue_only":
        let store = HistoryStore(root: dir + "/history")
        let journal = OutputTransactionStore(root: (dir as NSString).appendingPathComponent("history/transactions"))
        makeFile(backup, [116, 114, 117, 101])
        makeFile(foreign, [117, 115, 101, 114])
        if scenario == "legacy_unknown_source_preserved" {
            makeFile(original, [117, 115, 101, 114])
        } else {
            makeFile(original, [116, 114, 117, 101])
        }
        journal.prepare(contractLegacyTxn(source: original, backup: backup, id: "legacy-crash"))
        let report = journal.recover(store)
        let sourceBytes = readBytes(original)
        if scenario == "legacy_unknown_source_preserved" {
            return (report.needsReview >= 1 && sourceBytes == [117, 115, 101, 114] && journal.hasPending())
                ? "preserved" : "restored"
        }
        return (report.rolledBack == 1 && sourceBytes == [116, 114, 117, 101]) ? "residueOnly" : "preserved"
    // ── output_retirement ────────────────────────────────────────────
    case "create_new_deleted_when_hash_matches", "create_new_kept_when_content_differs":
        let store = HistoryStore(root: dir + "/history")
        let journal = OutputTransactionStore(root: (dir as NSString).appendingPathComponent("history/transactions"))
        let output = dir + "/out.png"
        let ours: [UInt8] = [111, 117, 114, 112, 114, 111, 100]      // "our-prod"
        let theirs: [UInt8] = [110, 111, 116, 111, 117, 114, 115]    // "notours"
        if scenario.contains("hash_matches") {
            makeFile(output, ours)
        } else {
            makeFile(output, theirs)
        }
        var txn = contractLegacyTxn(source: original, backup: backup, id: "retire")
        txn = ReplaceTransaction(
            id: txn.id, historyId: txn.historyId, sourcePath: txn.sourcePath,
            outputPath: output, backupPath: "", tempOutputPath: nil,
            originalSize: txn.originalSize, expectedOutputSize: 7,
            originalSha256: nil,
            outputSha256: sha256OfData(Data(ours)),
            backupSha256: nil, placeholderIdentity: nil,
            createdAt: txn.createdAt, crossFormat: false,
            mode: .createNew, schemaVersion: 2)
        check(journal.prepare(txn) == nil, "契约 \(scenario)：createNew 凭证必须落得了账")
        _ = journal.recover(store)
        let gone = !FileManager.default.fileExists(atPath: output)
        let shouldDelete = scenario.contains("hash_matches")
        if gone && shouldDelete { return "deleted" }
        if !gone && !shouldDelete { return "kept" }
        return "mismatch"
    case "empty_placeholder_deleted_with_credential", "empty_placeholder_kept_without_credential":
        let store = HistoryStore(root: dir + "/history")
        let journal = OutputTransactionStore(root: (dir as NSString).appendingPathComponent("history/transactions"))
        let output = dir + "/claimed.png"
        makeFile(output, [])
        var txn = contractLegacyTxn(source: original, backup: backup, id: "placeholder")
        txn = ReplaceTransaction(
            id: txn.id, historyId: txn.historyId, sourcePath: txn.sourcePath,
            outputPath: output, backupPath: "", tempOutputPath: nil,
            originalSize: txn.originalSize, expectedOutputSize: 0,
            originalSha256: nil, outputSha256: nil, backupSha256: nil,
            placeholderIdentity: scenario.contains("with_credential")
                ? OutputTransactionStore.fileIdentity(output)
                : nil,
            createdAt: txn.createdAt, crossFormat: false,
            mode: .createNew, schemaVersion: 0)
        journal.prepare(txn)
        _ = journal.recover(store)
        return FileManager.default.fileExists(atPath: output) ? "kept" : "deleted"
    // ── lifecycle_gate ───────────────────────────────────────────────
    case "maintenance_blocks_batch", "batch_blocks_maintenance", "released_gate_allows_both":
        let scheduler = CompressionScheduler(maxParallelism: 4)
        switch scenario {
        case "maintenance_blocks_batch":
            let lease = scheduler.beginMaintenance()
            defer { lease?.release() }
            return scheduler.tryBeginBatch() ? "allowed" : "blocked"
        case "batch_blocks_maintenance":
            check(scheduler.tryBeginBatch(), "契约：空闲调度器必须能开批次")
            return scheduler.beginMaintenance() != nil ? "allowed" : "blocked"
        default:
            let lease = scheduler.beginMaintenance()
            lease?.release()
            return scheduler.tryBeginBatch() ? "allowed" : "blocked"
        }
    default:
        return "unimplemented"
    }
}

/// 读 tests/contracts/*.json 并逐条对照期望。任何不一致直接记失败。
func runContractVectors() {
    let dir = contractRepoRoot() + "/tests/contracts"
    let names = (try? FileManager.default.contentsOfDirectory(atPath: dir))?.sorted() ?? []
    check(!names.isEmpty, "契约向量目录不能为空（tests/contracts/）")
    var checked = 0
    for name in names where name.hasSuffix(".json") {
        guard let data = FileManager.default.contents(atPath: (dir as NSString).appendingPathComponent(name)),
              let document = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let vectors = document["vectors"] as? [[String: Any]] else {
            check(false, "契约文件解析失败: \(name)")
            continue
        }
        for vector in vectors {
            guard let scenario = vector["scenario"] as? String,
                  let expected = (vector["expected"] as? [String: Any])?["outcome"] as? String else {
                check(false, "契约向量缺少 scenario/expected: \(name)")
                continue
            }
            let temp = freshRoot("contract-\(name.replacingOccurrences(of: ".json", with: ""))")
            let actual = contractRunScenario(scenario, temp)
            checked += 1
            check(actual == expected, "契约向量不一致: \(name) :: \(scenario)（实际 \(actual)，期望 \(expected)）")
        }
    }
    check(checked >= 20, "契约向量数量不应缩水（实际 \(checked)）")
}
