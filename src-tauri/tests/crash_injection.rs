//! §9.1 进程级崩溃注入：真实进程在每个事务阶段被 SIGKILL，父进程随后
//! 跑启动恢复并核对不变量（原图不丢、历史不假、恢复幂等）。
//!
//! 与手搓 journal 文件的单元测试不同，这里的崩溃现场由**同一份生产代码**
//! 在独立进程里走到指定阶段后直接被杀 —— 覆盖"编码中途强退"这类没有任何
//! 钩子能模拟的窗口。
//!
//! 用法：父测试用 `current_exe()` 重跑自身，`--sizqo-crash-child` 触发子模式。

#![cfg(unix)]

use sizqo_lib::history::{HistoryEntry, HistoryStore, HistoryStatus};
use sizqo_lib::output_transaction::{
    TransactionStore, ReplaceTransaction, TransactionMode, SCHEMA_VERSION,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ORIGINAL: &[u8] = b"true-original-bytes";
const COMPRESSED: &[u8] = b"compressed-bytes";

fn hex(bytes: &[u8]) -> String {
    sizqo_lib::source_identity::sha256_bytes(bytes)
}

fn child_dirs(workdir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    (
        workdir.join("photo.png"),                    // source / original
        workdir.join("original.png"),                 // backup（测试用独立文件，语义等同）
        workdir.join("history"),
        workdir.join("partial.tmp"),
    )
}

/// 子模式：走到指定阶段后自杀（SIGKILL），绝不正常退出。
fn run_crash_child(phase: &str, workdir: &Path) -> ! {
    let (source, backup, history_root, partial) = child_dirs(workdir);
    fs::write(&backup, ORIGINAL).unwrap();
    let history = HistoryStore::new(history_root.clone()).unwrap();
    let transactions = TransactionStore::new(history_root.join("transactions"));

    let original_hash = hex(ORIGINAL);
    let compressed_hash = hex(COMPRESSED);

    let mut txn = ReplaceTransaction {
        id: "crash-1".into(),
        history_id: "crash-1".into(),
        source_path: source.to_string_lossy().into(),
        output_path: source.to_string_lossy().into(),
        backup_path: backup.to_string_lossy().into(),
        temp_output_path: Some(partial.to_string_lossy().into()),
        original_size: ORIGINAL.len() as u64,
        expected_output_size: COMPRESSED.len() as u64,
        original_sha256: None,
        output_sha256: Some(compressed_hash.clone()),
        backup_sha256: Some(original_hash.clone()),
        placeholder_identity: None,
        created_at: sizqo_lib::history::now_millis(),
        cross_format: false,
        mode: TransactionMode::ReplaceExisting,
        schema_version: SCHEMA_VERSION,
    };

    match phase {
        // 备份落盘、事务还没记：现场只有备份与原图。
        "after_backup" => {
            fs::write(&source, ORIGINAL).unwrap();
        }
        // 记账完成、源文件还是原图（rename 之前）。
        "after_journal" => {
            fs::write(&source, ORIGINAL).unwrap();
            txn.original_sha256 = Some(original_hash);
            transactions.prepare_durable(&txn).unwrap();
        }
        // 临时产物写满、还没 rename。
        "after_partial" => {
            fs::write(&source, ORIGINAL).unwrap();
            fs::write(&partial, COMPRESSED).unwrap();
            txn.original_sha256 = Some(original_hash);
            transactions.prepare_durable(&txn).unwrap();
        }
        // rename 已发生：源位置是压缩结果，历史还没写 —— 最危险的窗口。
        "after_rename" => {
            fs::write(&source, COMPRESSED).unwrap();
            txn.original_sha256 = Some(original_hash);
            transactions.prepare_durable(&txn).unwrap();
        }
        // 历史已经落盘：这次覆盖有据可查，恢复只该销账。
        "after_history" => {
            fs::write(&source, COMPRESSED).unwrap();
            txn.original_sha256 = Some(original_hash);
            transactions.prepare_durable(&txn).unwrap();
            let entry = HistoryEntry {
                media_kind: "image".into(),
                video_metadata: None,
                id: "crash-1".into(),
                created_at: sizqo_lib::history::now_millis(),
                expires_at: sizqo_lib::history::now_millis() + 86_400_000,
                source_path: source.to_string_lossy().into(),
                output_path: Some(source.to_string_lossy().into()),
                file_name: "photo.png".into(),
                output_mode: "replace".into(),
                original_size: ORIGINAL.len() as u64,
                compressed_size: COMPRESSED.len() as u64,
                savings: 50.0,
                out_type: "png".into(),
                algorithm: "test".into(),
                backup_path: Some(backup.to_string_lossy().into()),
                status: HistoryStatus::Compressed,
                restored_at: None,
                output_modified_at: sizqo_lib::history::file_mtime_millis(&source),
                source_sha256: Some(hex(ORIGINAL)),
                output_sha256: Some(compressed_hash),
                backup_sha256: Some(hex(ORIGINAL)),
                source_exists: true,
                backup_exists: true,
                output_exists: true,
            };
            history.add(entry).unwrap();
        }
        // 老版本日志（无指纹）+ 覆盖已发生：新契约必须保留现场交人工。
        "legacy_after_rename" => {
            fs::write(&source, COMPRESSED).unwrap();
            txn.original_sha256 = None;
            txn.output_sha256 = None;
            txn.backup_sha256 = None;
            txn.schema_version = 0;
            transactions.prepare_durable(&txn).unwrap();
        }
        other => panic!("unknown crash phase: {other}"),
    }
    // 模拟进程被强杀/断电：不做任何清理，直接 SIGKILL。
    unsafe {
        libc::raise(libc::SIGKILL);
    }
    unreachable!("SIGKILL 之后不可能到达");
}

fn spawn_and_wait_for_crash(phase: &str, workdir: &Path) {
    let exe = std::env::current_exe().expect("test exe");
    let status = Command::new(exe)
        .env("SIZQO_CRASH_CHILD", phase)
        .env("SIZQO_CRASH_DIR", workdir)
        .status()
        .expect("spawn crash child");
    // SIGKILL 的子进程不正常退出；成功与否由父进程对磁盘现场的断言说了算。
    assert!(!status.success(), "{phase}: 子进程不该活着回来");
}

fn main() {
    // 子模式：本测试二进制被父进程用环境变量重新拉起，走到指定阶段后被 SIGKILL。
    if let (Ok(phase), Ok(workdir)) = (
        std::env::var("SIZQO_CRASH_CHILD"),
        std::env::var("SIZQO_CRASH_DIR"),
    ) {
        run_crash_child(&phase, Path::new(&workdir));
    }

    let parent = tempfile::tempdir().unwrap();
    let phases = [
        ("after_backup", "backup_only"),
        ("after_journal", "residue_only"),
        ("after_partial", "residue_only"),
        ("after_rename", "restored"),
        ("after_history", "committed"),
        ("legacy_after_rename", "preserved"),
    ];
    for (phase, expectation) in phases {
        let workdir = parent.path().join(phase);
        fs::create_dir_all(&workdir).unwrap();
        spawn_and_wait_for_crash(phase, &workdir);

        let (source, backup, history_root, partial) = child_dirs(&workdir);
        let history = HistoryStore::new(history_root.clone()).unwrap();
        let transactions = TransactionStore::new(history_root.join("transactions"));

        // 第一次恢复：按阶段断言终态。
        let report = transactions.recover(&history);
        match expectation {
            "backup_only" => {
                assert_eq!(report.rolled_back, 0, "{phase}: 无事务就无回滚");
                assert_eq!(fs::read(&source).unwrap(), ORIGINAL, "{phase}: 原图原样");
                assert_eq!(fs::read(&backup).unwrap(), ORIGINAL, "{phase}: 备份原样");
                assert!(!transactions.has_pending(), "{phase}: 无未完成事务");
            }
            "residue_only" => {
                assert_eq!(report.rolled_back, 1, "{phase}: 源还是原图，只清残骸");
                assert_eq!(fs::read(&source).unwrap(), ORIGINAL, "{phase}: 原图一个字节没动");
                assert!(!partial.exists(), "{phase}: 临时产物被收走");
                assert!(!transactions.has_pending(), "{phase}: 事务销账");
                assert_eq!(fs::read(&backup).unwrap(), ORIGINAL, "{phase}: 备份保留");
            }
            "restored" => {
                assert_eq!(report.rolled_back, 1, "{phase}: 覆盖已发生，必须回滚");
                assert_eq!(
                    fs::read(&source).unwrap(),
                    ORIGINAL,
                    "{phase}: 强杀后原图必须被恢复"
                );
                assert!(!transactions.has_pending(), "{phase}: 回滚完成即销账");
                assert_eq!(fs::read(&backup).unwrap(), ORIGINAL, "{phase}: 备份不许顺手删");
            }
            "committed" => {
                assert_eq!(report.committed, 1, "{phase}: 历史已记录 = 补删日志即可");
                assert_eq!(report.rolled_back, 0, "{phase}: 已提交的事务绝不回滚");
                assert_eq!(fs::read(&source).unwrap(), COMPRESSED, "{phase}: 用户文件保持压缩结果");
                assert!(!transactions.has_pending(), "{phase}: 日志已销账");
            }
            "preserved" => {
                assert_eq!(report.rolled_back, 0, "{phase}: 老日志证明不了归属，绝不自动回滚");
                assert!(report.needs_review >= 1, "{phase}: 必须计入人工恢复");
                assert_eq!(fs::read(&source).unwrap(), COMPRESSED, "{phase}: 用户当前内容原样");
                assert_eq!(fs::read(&backup).unwrap(), ORIGINAL, "{phase}: 备份保留");
                assert!(transactions.has_pending(), "{phase}: 事务日志留在现场");
            }
            other => panic!("{other}"),
        }

        // 第二次恢复：幂等 —— 绝不二次覆盖、不重复计数、现场不再变化。
        let snapshot = fs::read(&source).unwrap();
        let backup_snapshot = fs::read(&backup).unwrap();
        let second = transactions.recover(&history);
        assert_eq!(second.rolled_back, 0, "{phase}: 第二次恢复无事可做");
        assert_eq!(second.committed, 0, "{phase}: 第二次恢复不再补账");
        assert_eq!(fs::read(&source).unwrap(), snapshot, "{phase}: 二次恢复不动用户文件");
        assert_eq!(fs::read(&backup).unwrap(), backup_snapshot, "{phase}: 二次恢复不动备份");
        match expectation {
            "preserved" => {
                assert_eq!(second.needs_review, 1, "{phase}: 人工恢复场景每次如实报告");
                assert!(transactions.has_pending(), "{phase}: 现场保留到用户处理");
            }
            _ => assert!(!transactions.has_pending(), "{phase}: 已收口的现场保持收口"),
        }
    }
}
