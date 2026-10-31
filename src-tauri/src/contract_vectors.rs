//! §9.2 双引擎契约测试向量（Rust 侧执行器）。
//!
//! `tests/contracts/*.json` 里的每一组场景由 Rust 与 Swift **各自**对同一份
//! 期望作答 —— 改了一条安全逻辑而另一条引擎没跟上时，这里直接失败。
//! 场景语义与 Swift 侧 `HistoryStoreCheck` 的 `runContractVectors` 一一对应；
//! 新增向量必须两边同时实现，缺一边即失败（向量文件里的 scenario 就是契约）。
#![cfg(test)]

use crate::history::{HistoryStore, HistoryStatus};
use crate::output_transaction::{TransactionStore, ReplaceTransaction, TransactionMode, SCHEMA_VERSION};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

fn contracts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests")
        .join("contracts")
}

fn hex(bytes: &[u8]) -> String {
    crate::source_identity::sha256_bytes(bytes)
}

fn write_bytes(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, bytes).unwrap();
}

/// 钉住 mtime：模拟"原地换内容、时间戳没动"（与 source_identity 测试同一手法）。
#[cfg(unix)]
fn freeze_mtime(path: &Path, moment: SystemTime) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::time::{Duration, UNIX_EPOCH};
    let since = moment.duration_since(UNIX_EPOCH).unwrap_or(Duration::from_secs(0));
    let spec = libc::timespec {
        tv_sec: since.as_secs() as libc::time_t,
        tv_nsec: since.subsec_nanos() as libc::time_t,
    };
    let times = [spec, spec];
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    unsafe { libc::utimensat(libc::AT_FDCWD, name.as_ptr(), times.as_ptr(), 0) };
}

/// Windows 上这个桩不能是空的：`fast_gate_ignores_same_size_same_mtime_edit` 这类向量的
/// 前提就是"时间戳没动"，钉不住 mtime 时快速门会如实报"文件变了"，于是向量结果
/// 与 Swift 期望不符 —— 而这条不符会掩盖后面所有向量的 Windows 表现。
#[cfg(windows)]
fn freeze_mtime(path: &Path, moment: SystemTime) {
    crate::win32_fs::set_times(path, moment, moment)
        .unwrap_or_else(|detail| panic!("{}: 钉住 mtime 失败（{detail}）", path.display()));
}

#[cfg(not(any(unix, windows)))]
fn freeze_mtime(_path: &Path, _moment: SystemTime) {}

fn store_in(dir: &Path) -> HistoryStore {
    HistoryStore::new(dir.join("history")).unwrap()
}

fn journal_in(dir: &Path) -> TransactionStore {
    TransactionStore::new(dir.join("history").join("transactions"))
}

/// §2.3 legacy 备份：把来历记录降级成 v1（无指纹）。
fn downgrade_meta_to_v1(backup_path: &Path, source: &Path) {
    let dir = backup_path.parent().unwrap();
    let meta = serde_json::json!({
        "version": 1,
        "sourcePath": source.to_string_lossy(),
        "createdAt": crate::history::now_millis(),
    });
    fs::write(dir.join("backup-meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
}

fn v2_replace_txn(source: &Path, backup: &Path, history_id: &str) -> ReplaceTransaction {
    let output = hex(&fs::read(source).unwrap());
    v2_replace_txn_with_output(source, backup, &output, history_id)
}

/// 显式给产物哈希：崩溃现场"源位置的字节"与"承诺产物"可以不同
/// （外部在恢复前改过源文件的场景必须能表达）。
fn v2_replace_txn_with_output(
    source: &Path,
    backup: &Path,
    output_sha256: &str,
    history_id: &str,
) -> ReplaceTransaction {
    let original = hex(&fs::read(backup).unwrap());
    ReplaceTransaction {
        id: format!("t-{history_id}"),
        history_id: history_id.into(),
        source_path: source.to_string_lossy().into(),
        output_path: source.to_string_lossy().into(),
        backup_path: backup.to_string_lossy().into(),
        temp_output_path: None,
        original_size: fs::metadata(backup).map(|m| m.len()).unwrap_or(0),
        expected_output_size: fs::metadata(source).map(|m| m.len()).unwrap_or(0),
        original_sha256: Some(original.clone()),
        output_sha256: Some(output_sha256.to_string()),
        backup_sha256: Some(original),
        placeholder_identity: None,
        temp_identity: None,
        created_at: crate::history::now_millis(),
        cross_format: false,
        mode: TransactionMode::ReplaceExisting,
        schema_version: SCHEMA_VERSION,
        backup_legacy_unverified: false,
    }
}

fn legacy_txn(source: &Path, backup: &Path, history_id: &str) -> ReplaceTransaction {
    ReplaceTransaction {
        id: format!("t-{history_id}"),
        history_id: history_id.into(),
        source_path: source.to_string_lossy().into(),
        output_path: source.to_string_lossy().into(),
        backup_path: backup.to_string_lossy().into(),
        temp_output_path: None,
        original_size: 100,
        expected_output_size: 10,
        original_sha256: None,
        output_sha256: None,
        backup_sha256: None,
        placeholder_identity: None,
        temp_identity: None,
        created_at: crate::history::now_millis(),
        cross_format: false,
        mode: TransactionMode::ReplaceExisting,
        schema_version: 0,
        backup_legacy_unverified: false,
    }
}

fn run_scenario(scenario: &str, dir: &Path) -> String {
    let original = dir.join("original.bin");
    let compressed = dir.join("compressed.bin");
    let foreign = dir.join("foreign.bin");
    let backup = dir.join("backup.bin");
    match scenario {
        // ── file_identity ────────────────────────────────────────────
        "same_size_same_mtime_edit_detected" | "fast_gate_ignores_same_size_same_mtime_edit" => {
            write_bytes(&original, b"aaaa");
            let identity = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();
            write_bytes(&original, b"bbbb");
            freeze_mtime(&original, identity.modified.unwrap());
            let check = if scenario.starts_with("same_size") {
                identity.verify(&original)
            } else {
                identity.fingerprint_intact(&original)
            };
            match check {
                crate::source_identity::SourceCheck::Unchanged => "unchanged".into(),
                crate::source_identity::SourceCheck::Changed(_) => "changed".into(),
            }
        }
        "size_change_detected_by_fingerprint" => {
            write_bytes(&original, b"aaaa");
            let identity = crate::source_identity::SourceIdentity::read(&original).unwrap();
            write_bytes(&original, b"aaaaAAAA");
            match identity.fingerprint_intact(&original) {
                crate::source_identity::SourceCheck::Unchanged => "unchanged".into(),
                crate::source_identity::SourceCheck::Changed(_) => "changed".into(),
            }
        }
        "untouched_file_passes_verify" => {
            write_bytes(&original, b"same");
            let identity = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();
            match identity.verify(&original) {
                crate::source_identity::SourceCheck::Unchanged => "unchanged".into(),
                crate::source_identity::SourceCheck::Changed(_) => "changed".into(),
            }
        }
        "snapshot_refuses_when_baseline_differs_from_current_bytes" => {
            // §2.2 P0 修复回归：基线捕获 A → 文件被改成 B → 快照必须拒绝，
            // 绝不把 B 存成"这次压缩的原图"（否则 A→B→A 场景下恢复交出 B）。
            let store = store_in(dir);
            write_bytes(&original, b"baseline-A");
            // 调用方（压缩流程）手里握着基线 A 的指纹，磁盘上已经是 B。
            write_bytes(&original, b"changed-B!!");
            let outcome = store.ensure_backup_verified(&original, Some(&hex(b"baseline-A")));
            match outcome {
                Err(error) if error.contains("采集基线之后被修改") => "refused".into(),
                Ok(_) => "snapshotWritten".into(),
                Err(_) => "refused-other".into(),
            }
        }
        "missing_file_reported_changed" => {
            write_bytes(&original, b"gone");
            let identity = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();
            fs::remove_file(&original).unwrap();
            match identity.verify(&original) {
                crate::source_identity::SourceCheck::Unchanged => "unchanged".into(),
                crate::source_identity::SourceCheck::Changed(_) => "changed".into(),
            }
        }
        // ── backup_verification ──────────────────────────────────────
        "backup_reused_when_current_bytes_match_record" => {
            let store = store_in(dir);
            write_bytes(&original, b"true-original");
            let first = store.ensure_backup_verified(&original, Some(&hex(b"true-original"))).unwrap();
            let second = store.ensure_backup_verified(&original, Some(&hex(b"true-original"))).unwrap();
            if first == second { "reused".into() } else { "newSlot".into() }
        }
        "backup_new_slot_for_user_edit" | "backup_new_slot_when_backup_bytes_tampered" => {
            let store = store_in(dir);
            write_bytes(&original, b"true-original");
            let first = store.ensure_backup_verified(&original, Some(&hex(b"true-original"))).unwrap();
            if scenario == "backup_new_slot_when_backup_bytes_tampered" {
                // 副本被外部换掉：记录还在，字节不是那份字节。
                write_bytes(&first, b"tampered-bytes!!");
            }
            write_bytes(&original, b"user-edited!!");
            let second = store.ensure_backup_verified(&original, Some(&hex(b"user-edited!!"))).unwrap();
            if first == second { "reused".into() } else { "newSlot".into() }
        }
        "legacy_backup_without_evidence_not_reused" => {
            let store = store_in(dir);
            write_bytes(&original, b"true-original");
            let first = store.ensure_backup_verified(&original, Some(&hex(b"true-original"))).unwrap();
            downgrade_meta_to_v1(&first, &original);
            write_bytes(&original, b"user-edited!!");
            let second = store.ensure_backup_verified(&original, Some(&hex(b"user-edited!!"))).unwrap();
            if first == second { "reused".into() } else { "newSlot".into() }
        }
        "legacy_backup_upgraded_with_history_evidence" => {
            let store = store_in(dir);
            write_bytes(&original, b"true-original");
            let first = store.ensure_backup_verified(&original, Some(&hex(b"true-original"))).unwrap();
            // 当前字节是上一轮我们自己的产物，历史里带着两份指纹。
            write_bytes(&original, b"own-product");
            // sample_entry 存的是传入路径的字面量：历史与备份内部比对用的都是
            // 规范路径，这里必须先 canonicalize（macOS /var → /private/var）。
            let original = original.canonicalize().unwrap();
            let mut entry = crate::history::sample_entry(&original, &first, "round-1");
            entry.status = HistoryStatus::Compressed;
            store.add(entry).unwrap();
            downgrade_meta_to_v1(&first, &original);
            let second = store.ensure_backup_verified(&original, Some(&hex(b"own-product"))).unwrap();
            if first == second { "reused".into() } else { "newSlot".into() }
        }
        "backup_chain_own_product_reuses_first_slot" => {
            let store = store_in(dir);
            write_bytes(&original, b"true-original");
            let first = store.ensure_backup_verified(&original, Some(&hex(b"true-original"))).unwrap();
            write_bytes(&original, b"compressed-once");
            let original = original.canonicalize().unwrap();
            let mut entry = crate::history::sample_entry(&original, &first, "round-1");
            entry.status = HistoryStatus::Compressed;
            store.add(entry).unwrap();
            let second = store.ensure_backup_verified(&original, Some(&hex(b"compressed-once"))).unwrap();
            if first == second { "reused".into() } else { "newSlot".into() }
        }
        // ── history_conflict ─────────────────────────────────────────
        "conflict_when_output_content_differs" | "no_conflict_when_output_content_matches" => {
            let store = store_in(dir);
            write_bytes(&original, b"true-original");
            let backup_path = store.ensure_backup_verified(&original, Some(&hex(b"true-original"))).unwrap();
            // 产物=源文件（replace）：历史记的是"压缩结果"那份字节。
            write_bytes(&original, b"compressed!");
            let mut entry = crate::history::sample_entry(&original, &backup_path, "e1");
            entry.status = HistoryStatus::Compressed;
            let conflict = if scenario.contains("content_differs") {
                // 同大小同 mtime 的原地改动：只有内容指纹查得出。
                let mtime = entry.output_modified_at.unwrap();
                write_bytes(&original, b"compressdX!");
                freeze_mtime(&original, SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(mtime as u64));
                HistoryStore::has_conflict(&entry)
            } else {
                HistoryStore::has_conflict(&entry)
            };
            if conflict { "conflict".into() } else { "noConflict".into() }
        }
        "legacy_entry_conflict_by_mtime_drift" | "legacy_entry_no_conflict_when_size_mtime_match" => {
            let _ = store_in(dir);
            write_bytes(&original, b"payload12");
            let mut entry = crate::history::sample_entry(&original, &original, "legacy");
            entry.output_sha256 = None;
            entry.source_sha256 = None;
            entry.backup_sha256 = None;
            let now = crate::history::now_millis();
            entry.output_modified_at = Some(if scenario.contains("drift") { now - 60_000 } else { now });
            if HistoryStore::has_conflict(&entry) { "conflict".into() } else { "noConflict".into() }
        }
        // ── transaction_recovery ─────────────────────────────────────
        "rollback_restores_when_source_is_staged_output" | "residue_only_when_source_is_pristine_original"
        | "scene_preserved_when_source_is_foreign" | "voucher_mismatch_refuses_restore" => {
            let store = store_in(dir);
            let journal = journal_in(dir);
            write_bytes(&backup, b"true-original");
            write_bytes(&compressed, b"compressed!");
            let source_now = match scenario {
                "rollback_restores_when_source_is_staged_output" => &compressed,
                "residue_only_when_source_is_pristine_original" => &backup,
                "voucher_mismatch_refuses_restore" => &compressed,
                _ => &foreign,
            };
            write_bytes(&foreign, b"user-content");
            // 场景里 source == original（覆盖目标与原图同路径）。
            fs::copy(source_now, &original).unwrap();
            // 承诺产物固定为压缩结果；foreign 现场源位置是别人的字节，
            // 与承诺产物对不上 —— 这正是"证明不了归属"要拦的情况。
            let output_at_prepare = if scenario == "scene_preserved_when_source_is_foreign" {
                hex(&fs::read(&compressed).unwrap())
            } else {
                hex(&fs::read(&original).unwrap())
            };
            let txn = v2_replace_txn_with_output(&original, &backup, &output_at_prepare, "crash");
            if scenario == "voucher_mismatch_refuses_restore" {
                // 记账之后备份被换掉：凭证与字节对不上。
                write_bytes(&backup, b"tampered-backup");
            }
            journal.prepare(&txn).unwrap();
            let report = journal.recover(&store);
            let source_bytes = fs::read(&original).unwrap();
            if scenario == "rollback_restores_when_source_is_staged_output" {
                if report.rolled_back == 1 && source_bytes == b"true-original" { "restored".into() } else { "preserved".into() }
            } else if scenario == "residue_only_when_source_is_pristine_original" {
                if report.rolled_back == 1 && source_bytes == b"true-original" { "residueOnly".into() } else { "preserved".into() }
            } else {
                // 证明不了归属：现场保留（源文件原样、日志还在）。
                let expected_bytes = match scenario {
                    "voucher_mismatch_refuses_restore" => &compressed,
                    _ => &foreign,
                };
                if report.rolled_back == 0
                    && journal.has_pending()
                    && source_bytes == fs::read(expected_bytes).unwrap()
                {
                    "preserved".into()
                } else {
                    "restored".into()
                }
            }
        }
        "legacy_backup_voucher_not_auto_trusted" => {
            // §2.3 P1 修复回归：LegacyUnverified 备份的现场哈希只证明
            // "记账以来没变"，不证明它是原图 —— 带标记的事务即使源位置
            // 是本次产物，自动回滚也必须转人工，而不是把备份洗成可信原图。
            let store = store_in(dir);
            let journal = journal_in(dir);
            write_bytes(&backup, b"true-original");
            write_bytes(&original, b"compressed!");
            let mut txn = v2_replace_txn_with_output(
                &original, &backup, &hex(b"compressed!"), "launder");
            txn.backup_legacy_unverified = true;
            journal.prepare(&txn).unwrap();
            let report = journal.recover(&store);
            if report.needs_review >= 1
                && report.rolled_back == 0
                && journal.has_pending()
                && fs::read(&original).unwrap() == b"compressed!"
            {
                "preserved".into()
            } else {
                "restored".into()
            }
        }
        "manual_restore_with_confirmation_restores" | "manual_restore_refuses_without_confirmation" => {
            // §3.4 人工恢复：GUI 已展示双方状态后用户拍板 —— 确认在场才允许
            // 覆盖"证明不了归属"的现场；没有确认时一个字节都不许动。
            let store = store_in(dir);
            let journal = journal_in(dir);
            write_bytes(&backup, b"true-original");
            write_bytes(&foreign, b"user-content");
            write_bytes(&compressed, b"compressed!");
            fs::copy(&foreign, &original).unwrap();
            let txn = v2_replace_txn_with_output(
                &original, &backup, &hex(b"compressed!"), "manual");
            journal.prepare(&txn).unwrap();
            let confirm = scenario.starts_with("manual_restore_with");
            // 凭证一致的现场：确认位即放行凭证层。
            let outcome = crate::output_transaction::manual_restore(&txn, confirm, confirm);
            if outcome.is_err() { return "refused".into(); }
            journal.finish(&txn.id);
            if fs::read(&original).unwrap() == b"true-original" { "restoredByUser".into() } else { "preserved".into() }
        }
        "legacy_unknown_source_preserved" | "legacy_source_equals_backup_residue_only" => {
            let store = store_in(dir);
            let journal = journal_in(dir);
            write_bytes(&backup, b"true-original");
            let source_now = if scenario == "legacy_unknown_source_preserved" { &foreign } else { &backup };
            write_bytes(&foreign, b"user-content");
            fs::copy(source_now, &original).unwrap();
            journal.prepare(&legacy_txn(&original, &backup, "legacy-crash")).unwrap();
            let report = journal.recover(&store);
            let source_bytes = fs::read(&original).unwrap();
            if scenario == "legacy_unknown_source_preserved" {
                if report.needs_review >= 1 && source_bytes == b"user-content" && journal.has_pending() { "preserved".into() } else { "restored".into() }
            } else if report.rolled_back == 1 && source_bytes == b"true-original" { "residueOnly".into() } else { "preserved".into() }
        }
        // ── output_retirement ────────────────────────────────────────
        "create_new_deleted_when_hash_matches" | "create_new_kept_when_content_differs" => {
            let store = store_in(dir);
            let journal = journal_in(dir);
            let output = dir.join("out.png");
            let content: &[u8] = if scenario.contains("hash_matches") { b"our-product" } else { b"not-ours-ma" };
            write_bytes(&output, content);
            let mut txn = legacy_txn(&original, &backup, "retire");
            txn.mode = TransactionMode::CreateNew;
            txn.output_path = output.to_string_lossy().into();
            txn.expected_output_size = content.len() as u64;
            txn.output_sha256 = Some(hex(b"our-product"));
            txn.schema_version = SCHEMA_VERSION;
            journal.prepare(&txn).unwrap();
            let _ = journal.recover(&store);
            // CreateNew 的回滚恒为 Ok：契约看的是"能证明是自己的产物才删"。
            match (!output.exists(), scenario.contains("hash_matches")) {
                (true, true) => "deleted".into(),
                (false, false) => "kept".into(),
                _ => "mismatch".into(),
            }
        }
        "empty_placeholder_deleted_with_credential" | "empty_placeholder_kept_without_credential" => {
            let store = store_in(dir);
            let journal = journal_in(dir);
            let output = dir.join("claimed.png");
            write_bytes(&output, b"");
            let mut txn = legacy_txn(&original, &backup, "placeholder");
            txn.mode = TransactionMode::CreateNew;
            txn.output_path = output.to_string_lossy().into();
            txn.expected_output_size = 0;
            if scenario.contains("with_credential") {
                txn.placeholder_identity = crate::output_transaction::file_identity(&output);
            }
            txn.schema_version = 0;
            journal.prepare(&txn).unwrap();
            let _ = journal.recover(&store);
            if output.exists() { "kept".into() } else { "deleted".into() }
        }
        // ── lifecycle_gate ───────────────────────────────────────────
        "maintenance_blocks_batch" | "batch_blocks_maintenance" | "released_gate_allows_both" => {
            use crate::commands::CompressionScheduler;
            let scheduler = Arc::new(CompressionScheduler::with_memory_budget(4, 64 * 1024 * 1024));
            let outcome = match scenario {
                "maintenance_blocks_batch" => {
                    let _lease = scheduler.begin_maintenance();
                    if scheduler.try_begin_batch() { "allowed".into() } else { "blocked".into() }
                }
                "batch_blocks_maintenance" => {
                    assert!(scheduler.try_begin_batch(), "空闲调度器必须能开批次");
                    if scheduler.begin_maintenance().is_some() { "allowed".into() } else { "blocked".into() }
                }
                _ => {
                    let lease = scheduler.begin_maintenance();
                    drop(lease);
                    if scheduler.try_begin_batch() { "allowed".into() } else { "blocked".into() }
                }
            };
            outcome
        }
        other => format!("unimplemented:{other}"),
    }
}

#[test]
fn rust_executes_every_contract_vector_and_matches_swift_expectations() {
    let dir = contracts_dir();
    let mut checked = 0;
    let mut mismatches: Vec<String> = Vec::new();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("tests/contracts 目录必须存在")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "契约向量不能为空");
    for file in files {
        let raw = fs::read_to_string(&file).unwrap();
        let document: Value = serde_json::from_str(&raw).unwrap();
        for vector in document["vectors"].as_array().expect("vectors 数组") {
            let scenario = vector["scenario"].as_str().expect("scenario 字段").to_string();
            let expected = vector["expected"]["outcome"].as_str().expect("expected.outcome").to_string();
            let temp = tempfile::tempdir().unwrap();
            let actual = run_scenario(&scenario, temp.path());
            checked += 1;
            // 逐条收集，不在第一条不一致时就中止：某个平台上第一个错位会掩盖后面
            // 每一条的真实表现，而日志里"只报了 1 条"和"只有 1 条"看着一模一样。
            if actual != expected {
                mismatches.push(format!(
                    "{} :: {scenario}（实际 {actual}，期望 {expected}）",
                    file.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "契约向量不一致 {} 条（本轮跑了 {checked} 条）:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    assert!(checked >= 20, "契约向量数量不应缩水（实际 {checked}）");
    println!("契约向量 {checked} 条与 Swift 期望一致");
}
