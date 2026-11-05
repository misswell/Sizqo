// Sizqo - 落盘事务日志（crash journal）与输出命名。
//
// 为什么需要它：backup 写完、压缩结果覆盖到用户文件上、`history.add` 还没跑 ——
// 就在这个窗口里崩溃或强杀，App 下次启动将完全不知道发生过这次压缩：
// 历史里没有记录，备份目录里躺着一份没人认领的原图，用户看到的却是压缩后的文件。
// transaction 文件就是那段时间的记账凭证。原图快照不在这里做（§24：编码前就备好），
// 这一步只负责复核它：
//
//   ① 复核源文件  ② 独占目标名 + 独占临时名  ③ 写 transaction
//   ④ 写满候选 → flush + fsync  ⑤ 改名前再核一次指纹与备份  ⑥ rename 覆盖目标
//   ⑦ history.add  ⑧ 删 transaction
//
// 启动时凡是**不能被历史证明已完整提交**的事务，按 §29 的两种模式各自收拾：
// 覆盖模式一律回滚成真正原图，新建模式只删自己生成的那个文件。
// 宁可丢掉一次压缩结果，也不能丢掉原图。

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::history::{write_atomic, HistoryStore};

/// §29 落盘的两种模式。它是**一处**决定：备份做不做、事务记成哪一类、
/// 崩溃后允许怎么收拾残局，全从这里派生，不许在三个地方各判一次。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionMode {
    /// 压缩结果要放回用户原文件的位置（同格式覆盖，或跨格式转换后删掉原图）。
    /// 恢复依据是备份：证明不了提交完成，就把原图写回去。
    ReplaceExisting,
    /// 原图一个字节都不动，产物是新文件（后缀 / 输出目录 / 导出）。
    /// 没有备份可依赖，也绝不该碰任何原图：证明不了提交，就只删自己生成的那个文件。
    CreateNew,
}

/// 一次覆盖（或一次新建）的事务记录，落 `<appdata>/history/transactions/<id>.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceTransaction {
    pub id: String,
    /// 与最终 `HistoryEntry.id` 同一个值：事务是否提交，就靠历史里有没有这个 id 判定。
    pub history_id: String,
    pub source_path: String,
    pub output_path: String,
    pub backup_path: String,
    #[serde(default)]
    pub temp_output_path: Option<String>,
    pub original_size: u64,
    pub expected_output_size: u64,
    /// §2.5 启动恢复的身份证据：这次事务认得哪份"改动前的原图"。
    /// 有了它，回滚前就能证明源位置上的字节确实是这次事务的产物或仍是原图。
    #[serde(default)]
    pub original_sha256: Option<String>,
    /// 这次写出的产物的内容指纹（同格式 replace 时即源位置将变成的字节）。
    /// 老日志没有这两个字段：按 legacy 兼容逻辑处理，不伪装成新版本记录。
    #[serde(default)]
    pub output_sha256: Option<String>,
    /// §3.1 备份的内容指纹。回滚前先验它：备份字节和记账时刻对不上就拒绝
    /// 自动恢复 —— 宁可保留现场，也不能把一份被换过的文件当原图写回去。
    #[serde(default)]
    pub backup_sha256: Option<String>,
    /// 输出占位文件的 `(dev,ino)`。恢复时它是"这个 0 字节文件是我们的占位"的凭证；
    /// 没有凭证的空文件一律不删（§2.5 同款身份门）。
    #[serde(default)]
    pub placeholder_identity: Option<(u64, u64)>,
    /// §28 审查修复：临时文件**认领那一刻**的 `(dev,ino)`。
    ///
    /// 原子交换成功后，临时路径上躺的是**被换出来的外部字节**，而不是我们的候选半成品；
    /// 只看路径去删就是把用户的文件当垃圾清掉。这条凭证让所有清理入口都能回答
    /// "这个路径上的 inode 还是不是我们 O_EXCL 认领的那个" —— 答不出就不删。
    /// 老日志没有这个字段（None）：清理时证明不了归属，宁可留孤儿临时文件。
    #[serde(default)]
    pub temp_identity: Option<(u64, u64)>,
    pub created_at: i64,
    /// PNG → JPG 这类跨格式 replace：压缩结果是个新文件，回滚时要一并删掉。
    pub cross_format: bool,
    /// §29 老日志里没有这个字段：它们只可能来自覆盖模式，按覆盖模式读。
    #[serde(default = "default_mode")]
    pub mode: TransactionMode,
    /// §3.1 记录版本。0 = 老版本日志（无此字段）：走独立的兼容恢复逻辑，
    /// 凭证要求对它不生效；2 = 本版本：覆盖事务必须带着完整的三份指纹落账。
    #[serde(default)]
    pub schema_version: u32,
    /// §2.3 可信度随事务走：这份备份在记账时是 LegacyUnverified（老版本
    /// 留下、没有内容指纹可验）。它的 `backupSha256` 只证明"记账以来没变过"，
    /// **不能**证明它是正确的原图 —— 自动回滚不得借现场哈希把它洗成可信，
    /// 一律转人工恢复；GUI 在用户看过双方状态后凭确认放行。
    ///
    /// 缺字段 = RC.1 的事务（有 schemaVersion=2 但还没这个字段）：**保守按
    /// true 处理** —— RC.1 的覆盖事务从未核实过备份可信度，升级不能让
    /// 旧问题借默认值复活。RC.2 起写入方总是显式落这个字段。
    #[serde(default = "default_backup_legacy_unverified")]
    pub backup_legacy_unverified: bool,
}

/// RC.1 迁移默认值：缺字段按"未验证"处理（保守）。
fn default_backup_legacy_unverified() -> bool {
    true
}

fn default_mode() -> TransactionMode {
    TransactionMode::ReplaceExisting
}

/// 当前落账格式版本。`SCHEMA_VERSION` 及以上的记录在 `prepare` 时强制校验凭证。
pub const SCHEMA_VERSION: u32 = 2;

impl ReplaceTransaction {
    /// §3.1 凭证完整性门：新版本事务缺少必要身份信息时拒绝落账。
    /// 一条没有凭证的覆盖日志 = 崩溃后谁都证明不了那次覆盖动的是谁的原图。
    pub fn validate_for_prepare(&self) -> Result<(), String> {
        if self.schema_version < SCHEMA_VERSION {
            return Ok(());
        }
        match self.mode {
            TransactionMode::ReplaceExisting => {
                if self.backup_path.is_empty() {
                    return Err("覆盖事务缺少备份路径".into());
                }
                if self.original_sha256.is_none()
                    || self.output_sha256.is_none()
                    || self.backup_sha256.is_none()
                {
                    return Err(
                        "覆盖事务缺少身份凭证（原图/备份/产物 SHA-256），拒绝落账".into()
                    );
                }
                Ok(())
            }
            TransactionMode::CreateNew => {
                if self.output_sha256.is_none() {
                    return Err("新建事务缺少产物内容指纹，拒绝落账".into());
                }
                Ok(())
            }
        }
    }
}

/// 文件系统身份 `(dev,ino)`。
///
/// unix 走 `stat()`；Windows 走 `GetFileInformationByHandle` 的
/// `(dwVolumeSerialNumber, nFileIndex)` —— 同一卷内唯一、重开不变、被换成新文件就变，
/// 与 unix 的 `ino` 同语义。打不开/查不到就返回 `None`：所有依赖它的凭证门在
/// 拿不到身份时一律按"证明不了归属 → 保留"处理，方向永远是宁可不删。
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(windows)]
    {
        crate::win32_fs::file_identity(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// 还开着的句柄上的 `(dev,ino)`：一次 `fstat` / `GetFileInformationByHandle`，
/// 不重开路径。占位凭证一律从这里取 —— 本轮加固（docs/HARDENING_ROADMAP_2026-11.md
/// F1）把"关掉句柄再按路径 stat"定为违规：两步之间路径就可能换一个 inode，
/// 凭证从根上就不可信。
fn identity_of_open(file: &fs::File) -> Option<(u64, u64)> {
    crate::source_identity::fd_identity(file).ok().and_then(|id| id.identity)
}

pub const TRANSACTIONS_DIR: &str = "transactions";

#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// 历史已记录，只是删日志前崩溃：补删日志。
    pub committed: usize,
    /// 回滚成真正原图的事务数。
    pub rolled_back: usize,
    /// §3.4 无法证明归属、现场原样保留、需要人工恢复的事务数。
    pub needs_review: usize,
    pub warnings: Vec<String>,
}

pub struct TransactionStore {
    root: PathBuf,
}

impl TransactionStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path_of(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    /// 覆盖发生**之前**登记。写不进日志就不许覆盖 —— 没有记账凭证的覆盖等于不可恢复。
    pub fn prepare(&self, txn: &ReplaceTransaction) -> Result<(), String> {
        txn.validate_for_prepare()?;
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let json = serde_json::to_string(txn).map_err(|e| e.to_string())?;
        write_atomic(&self.path_of(&txn.id), json.as_bytes())?;
        // §2.2 目录项落盘：CreateNew 的日志虽是尽力而为，但写就得写到底。
        // 走 history::sync_dir —— 那里才是按平台要求开句柄的唯一一处。
        // 以前这里直接 `fs::File::open(目录)`：Windows 上不带 backup semantics
        // 根本打不开目录，等于 prepare() 在 Windows 恒失败、整条提交线被拒。
        crate::history::sync_dir(&self.root)
    }

    /// 记账凭证必须**先于覆盖落到目录里**：只写进内核缓存的日志，断电后和没写一样，
    /// 而那时用户原图已经被盖掉了。ReplaceExisting（图片与视频）一律走这里。
    ///
    /// `prepare` 本身就已经把目录项落盘了（日志不落盘就没有任何恢复依据，
    /// CreateNew 也一样不许只写进缓存），所以这里不再重复一次目录 fsync ——
    /// 以前两道 sync 挨在一起，第二道只是把同一件事再做一遍。
    pub fn prepare_durable(&self, txn: &ReplaceTransaction) -> Result<(), String> {
        self.prepare(txn)
    }

    /// 事务已完整提交（历史也写成了）：销账。
    pub fn finish(&self, id: &str) {
        let _ = fs::remove_file(self.path_of(id));
    }

    pub fn list(&self) -> Vec<ReplaceTransaction> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<ReplaceTransaction>(&raw).ok())
            {
                Some(txn) => found.push(txn),
                // 半个日志文件：无法判定它代表哪次覆盖，改名留下现场，绝不据此删任何东西。
                None => {
                    let _ = fs::rename(&path, path.with_extension("unparsed"));
                }
            }
        }
        found.sort_by_key(|txn| txn.created_at);
        found
    }

    /// 读不懂的事务日志（.unparsed）：无法证明它背后的覆盖没有发生过。
    fn unparsed_count(&self) -> usize {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return 0;
        };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("unparsed"))
            .count()
    }

    pub fn has_pending(&self) -> bool {
        // 先 list()：它会把半个日志改名成 .unparsed；随后计数才看得到它。
        // 半个日志文件也算未完成事务：它背后的覆盖可能发生过，相关备份绝不能
        // 因为"没有可解析的未完成事务"而被当成孤儿清掉（§4 审查意见）。
        let pending = !self.list().is_empty();
        self.unparsed_count() > 0 || pending
    }

    /// §2.3 未完成事务还认领着的备份目录 key。退出清理的引用核对必须把它们
    /// 当作存活引用 —— 历史里还没有这条记录，只看 history 会把备份当孤儿清掉。
    /// `.unparsed` 日志解析不出引用，所以它们的存在本身就让整个清理跳过
    /// （`has_pending`），两层保护各管各的。
    pub fn pending_backup_keys(&self) -> Vec<String> {
        self.list()
            .iter()
            .filter(|txn| !txn.backup_path.is_empty())
            .filter_map(|txn| crate::history::backup_key_of(Some(txn.backup_path.as_str())))
            .collect()
    }

    /// 启动时第一件事：把上次没走完的事务结清。
    ///
    /// 判定只依赖一条事实 —— 历史里有没有这条 `history_id`。有则整条链路都走完了，
    /// 只欠删日志；没有则证明不了提交完成，按模式各自的下场收拾：ReplaceExisting 一律
    /// 回滚成真正原图（宁可丢掉一次压缩结果，也不能丢掉原图），CreateNew 只删这次新建的文件。
    pub fn recover(&self, history: &HistoryStore) -> RecoveryReport {
        let mut report = RecoveryReport::default();
        let unparsed = self.unparsed_count();
        if unparsed > 0 {
            report.warnings.push(format!(
                "{unparsed} 个事务日志无法解析（已保留为 .unparsed）：无法证明其覆盖是否发生，\
                 相关原图备份不会被任何清理触碰，请在恢复原图后人工确认"
            ));
        }
        for txn in self.list() {
            if history.contains_committed(&txn.history_id) {
                self.finish(&txn.id);
                report.committed += 1;
                continue;
            }
            match rollback(&txn) {
                Ok(()) => {
                    self.finish(&txn.id);
                    report.rolled_back += 1;
                }
                Err(error) => {
                    if let Some(detail) = error.strip_prefix(ROLLBACK_NEEDS_REVIEW) {
                        report.needs_review += 1;
                        report.warnings.push(format!(
                            "{} 需要人工恢复（事务现场已保留，原图备份未删除）: {detail}",
                            txn.source_path
                        ));
                    } else {
                        report.warnings.push(format!(
                            "{} 回滚失败，原图仍在备份里: {error}",
                            txn.source_path
                        ));
                    }
                }
            }
        }
        report
    }
}

/// 回滚拒绝自动处理、需要人工介入的错误都带这个前缀；`recover()` 据此
/// 归入"需要人工恢复"计数，而不是和普通失败混在一起。
pub const ROLLBACK_NEEDS_REVIEW: &str = "NEEDS_REVIEW: ";

// MARK: - §3.4 人工恢复：现场分类与处置

/// 一个未完成事务此刻的归属判定（只读，不动任何文件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingRecoveryStatus {
    /// 备份已经不在：恢复无从谈起。
    BackupMissing,
    /// 源文件不在原位：启动恢复本来就能自动处理，不进人工列表。
    SourceMissing,
    /// 源文件既不是承诺产物也不是原图：外部改过，需要用户裁决。
    ForeignModified,
    /// 老日志没有指纹，且源位置既非备份字节也证明不了归属。
    LegacyUnknown,
    /// 备份字节与事务凭证不符：备份被换过，需要用户裁决。
    VoucherMismatch,
    /// 自动恢复就能收口的（源=产物 → 回滚；源=原图 → 只清残骸）。
    AutoResolvable,
}

impl PendingRecoveryStatus {
    pub fn id(&self) -> &'static str {
        match self {
            Self::BackupMissing => "backup_missing",
            Self::SourceMissing => "source_missing",
            Self::ForeignModified => "foreign_modified",
            Self::LegacyUnknown => "legacy_unknown",
            Self::VoucherMismatch => "voucher_mismatch",
            Self::AutoResolvable => "auto_resolvable",
        }
    }
}

/// 只读分类：启动恢复无法自动收口的现场，人工恢复界面据此展示双方状态。
pub fn describe_pending(txn: &ReplaceTransaction) -> (PendingRecoveryStatus, String) {
    if txn.mode != TransactionMode::ReplaceExisting {
        return (PendingRecoveryStatus::AutoResolvable, "新建模式事务由启动恢复自动收口".into());
    }
    let backup = PathBuf::from(&txn.backup_path);
    let source = PathBuf::from(&txn.source_path);
    if !backup.exists() {
        return (PendingRecoveryStatus::BackupMissing, "原图备份已不在磁盘上".into());
    }
    if !source.exists() {
        return (PendingRecoveryStatus::SourceMissing, "源文件不在原位（启动恢复会自动写回原图）".into());
    }
    let current = match crate::source_identity::sha256_of(&source) {
        Ok(hash) => hash,
        Err(error) => {
            return (PendingRecoveryStatus::ForeignModified, format!("当前源文件读不出来（{error}）"))
        }
    };
    // 凭证核对（只读）：备份与记账时对不上、或**读不出来**都单独成类 ——
    // 忽略读取失败会把"回滚会失败"的现场错标成 AutoResolvable，
    // 人工恢复列表又过滤 AutoResolvable，这条现场就永远到不了 GUI。
    if let Some(expected) = txn.backup_sha256.as_deref() {
        match crate::source_identity::sha256_of(&backup) {
            Ok(actual) if actual != expected => {
                return (PendingRecoveryStatus::VoucherMismatch, "备份字节与事务凭证不符".into());
            }
            Err(error) => {
                return (
                    PendingRecoveryStatus::VoucherMismatch,
                    format!("备份读不出来（{error}），无法核对事务凭证"),
                );
            }
            Ok(_) => {}
        }
    }
    if txn.output_sha256.as_deref() == Some(current.as_str()) {
        if txn.backup_legacy_unverified {
            return (PendingRecoveryStatus::LegacyUnknown,
                "备份未经真实性验证（老版本记录）：自动回滚会拒绝写回，请人工确认".into());
        }
        return (PendingRecoveryStatus::AutoResolvable, "源位置是本次产物，启动恢复会自动回滚".into());
    }
    if txn.original_sha256.as_deref() == Some(current.as_str()) {
        if txn.backup_legacy_unverified {
            return (PendingRecoveryStatus::LegacyUnknown,
                "备份未经真实性验证：原图虽仍在原位，处置前请人工确认备份来历".into());
        }
        return (PendingRecoveryStatus::AutoResolvable, "原图仍在原位，启动恢复只清残骸".into());
    }
    if txn.output_sha256.is_none() && txn.original_sha256.is_none() {
        return (PendingRecoveryStatus::LegacyUnknown, "老版本日志没有身份记录，无法证明当前文件的归属".into());
    }
    (PendingRecoveryStatus::ForeignModified, "源文件在事务结束前被外部修改过".into())
}

/// 人工恢复的"恢复原图"执行：调用方（GUI）必须已经把当前文件与备份的
/// 状态展示给用户并拿到明确确认 —— 这里**有意**绕过"证明不了归属就拒绝"
/// 的自动门：人工恢复的全部意义就是让用户看过之后自己拍板。
/// 备份凭证核对仍然执行：凭证不符只随 `confirm_voucher_mismatch` 放行。
pub fn manual_restore(
    txn: &ReplaceTransaction,
    confirm_overwrite: bool,
    confirm_voucher_mismatch: bool,
) -> Result<(), String> {
    if txn.mode != TransactionMode::ReplaceExisting {
        retire_generated_output(txn);
        return Ok(());
    }
    let backup = PathBuf::from(&txn.backup_path);
    let source = PathBuf::from(&txn.source_path);
    if !backup.exists() {
        return Err("备份已不存在，无法恢复原图".into());
    }
    if !confirm_overwrite {
        return Err("必须由用户确认覆盖当前位置的内容".into());
    }
    if txn.backup_sha256.is_some() {
        // 凭证核对分层：**不匹配** = 用户在弹窗里看到了警告、明确选择继续 →
        // 凭确认放行；**读不出来** = 连内容是什么都不知道，永远拒绝。
        let actual = crate::source_identity::sha256_of(&backup)
            .map_err(|error| format!("备份读不出来（{error}），无法恢复原图"))?;
        if actual != txn.backup_sha256.as_deref().unwrap_or_default() {
            if !confirm_voucher_mismatch {
                return Err(
                    "备份内容与事务凭证不符，需要用户在了解风险后明确确认".into(),
                );
            }
        }
    }
    HistoryStore::write_backup_back_to_source(&backup, &source)?;
    remove_generated_output(txn);
    Ok(())
}

/// 用备份把真正原图写回源位置，再清掉这次生成的压缩结果。
///
/// 备份本身**不删**：回滚半途失败时它是唯一的原图副本，留给历史页/下次启动处理。
///
/// §2.5：动手之前必须先确认源位置上的字节归属。能证明它是本次事务写出的产物，
/// 才允许用备份盖回去；证明它还是没动过的原图，就什么都不用盖；证明不了归属
/// （外部在崩溃后改过它）就保留现场 —— 备份与日志都留着，绝不自动覆盖。
///
/// §3.1：日志里记了备份指纹（v2）的，写回之前先验备份字节 —— 备份被换过就
/// 拒绝自动恢复，宁可保留现场也不能把一份来历不明的文件当原图写回去。
///
/// §3.4：老日志没有身份记录的，**不再无条件回滚**（旧契约会把用户在崩溃后
/// 写入的新内容盖掉）。尽力识别：源位置字节等于备份字节 → 原图仍在原位，
/// 只清产物；其余无法证明归属 → 保留现场，交人工恢复。
///
/// CreateNew 没有备份，也从未动过任何原图：它只许收拾自己生成的那个文件（§29）。
pub fn rollback(txn: &ReplaceTransaction) -> Result<(), String> {
    match txn.mode {
        TransactionMode::ReplaceExisting => {
            let backup = PathBuf::from(&txn.backup_path);
            let source = PathBuf::from(&txn.source_path);
            if !backup.exists() {
                return Err("备份已不存在，无法自动恢复原图".into());
            }
            if !source.exists() {
                if txn.backup_legacy_unverified {
                    // 未经真实性验证的备份也不该在无人看过的情况下写回：
                    // 源文件不在原位可能是用户自己移走的，现场交人工裁决。
                    return Err(format!("{ROLLBACK_NEEDS_REVIEW}备份未经真实性验证（老版本记录），源文件又不在原位；不自动写回，请在人工恢复界面确认"));
                }
                // 源文件已经不在原位：按"提交没完成"对待，把原图写回去。
                // 写回目标应当是空位：三级台阶装进去，位置被外部占了就拒绝。
                verify_backup_voucher(txn, &backup)?;
                write_back_to_vacated_path(&backup, &source)?;
                remove_generated_output(txn);
                return Ok(());
            }
            let current = match crate::source_identity::sha256_of(&source) {
                Ok(hash) => hash,
                Err(error) => {
                    return Err(format!(
                        "当前源文件读不出来（{error}），无法确认它是不是本次事务的产物；备份与日志原样保留"
                    ))
                }
            };
            if txn.output_sha256.as_deref() == Some(current.as_str()) {
                if txn.backup_legacy_unverified {
                    // 能证明源位置是本次产物，但备份本身证明不了是原图：
                    // 自动回滚等于把未经确认的内容当原图写回 —— 转人工。
                    return Err(format!("{ROLLBACK_NEEDS_REVIEW}备份未经真实性验证（老版本记录），不自动写回；原图备份与事务日志都保留在原处，请在人工恢复界面确认"));
                }
                // 源位置上就是这次写出的字节：安全回滚。写回走带产物身份复核的
                // 提交原语 —— 复核到改名之间被外部换掉的文件不再被盖掉（本轮 F5）。
                verify_backup_voucher(txn, &backup)?;
                let expected = match crate::source_identity::SourceIdentity::read(&source) {
                    Ok(identity) => identity,
                    Err(error) => {
                        return Err(format!(
                            "源位置身份读不出来（{error}），不自动覆盖；备份与事务日志保留在原处"
                        ))
                    }
                };
                let expected = crate::source_identity::SourceIdentity {
                    sha256: txn.output_sha256.clone(),
                    ..expected
                };
                write_back_verified(&backup, &source, &expected)?;
                remove_generated_output(txn);
                return Ok(());
            }
            if txn.original_sha256.as_deref() == Some(current.as_str()) {
                // 源文件还是没动过的原图：什么都没有被覆盖，只需清掉自己的残骸。
                remove_generated_output(txn);
                return Ok(());
            }
            if txn.output_sha256.is_none() && txn.original_sha256.is_none() {
                // §3.4 老日志没有身份记录：尽力识别，证明不了就保留现场。
                // 唯一能自动做的：当前字节和备份字节相同 → 原图本来就在原位。
                let backup_bytes = match crate::source_identity::sha256_of(&backup) {
                    Ok(hash) => hash,
                    Err(error) => {
                        return Err(format!("{ROLLBACK_NEEDS_REVIEW}老版本事务的备份读不出来（{error}），无法证明当前文件的归属；不自动覆盖，请人工确认后恢复原图"))
                    }
                };
                if backup_bytes == current {
                    remove_generated_output(txn);
                    return Ok(());
                }
                return Err(format!(
                    "{ROLLBACK_NEEDS_REVIEW}老版本事务无法证明当前文件的归属；不自动覆盖，\
                     原图备份与事务日志都保留在原处（备份: {backup:?}），请在确认后人工恢复原图"
                ));
            }
            Err(
                "源文件在事务结束前被外部修改过，无法确认归属；不自动覆盖，原图备份与事务日志都保留在原处"
                    .into(),
            )
        }
        TransactionMode::CreateNew => {
            retire_generated_output(txn);
            Ok(())
        }
    }
}

/// §3.1 备份凭证：日志里记了备份指纹的，写回之前先确认备份还是记账时的那份字节。
/// 没记指纹的（老日志）跳过 —— 那条路径由 §3.4 的归属判定兜底。
fn verify_backup_voucher(txn: &ReplaceTransaction, backup: &Path) -> Result<(), String> {
    let Some(expected) = txn.backup_sha256.as_deref() else {
        return Ok(());
    };
    let actual = crate::source_identity::sha256_of(backup)
        .map_err(|error| format!("备份读不出来（{error}），不自动恢复原图；备份与日志原样保留"))?;
    if actual != expected {
        return Err(format!(
            "备份内容与事务凭证不符（{expected} → {actual}），不自动恢复原图；备份与日志原样保留"
        ));
    }
    Ok(())
}

/// 删掉这次压缩产生的新文件与临时文件。同格式 replace 的输出就是源文件本身，绝不删。
///
/// 跨格式的输出文件也只删"能证明是这次生成的"：内容指纹命中，或老日志体积对上；
/// 其它体积/内容说明这位置已经被别人的内容占了，宁可留下也不误删。
fn remove_generated_output(txn: &ReplaceTransaction) {
    let source = PathBuf::from(&txn.source_path);
    if txn.cross_format || !crate::history::same_file(&PathBuf::from(&txn.output_path), &source) {
        let output = Path::new(&txn.output_path);
        let ours = match fs::metadata(output) {
            // 空占位：与新建模式同款凭证 —— journal 里的 (dev,ino) 对得上才删。
            Ok(meta) if meta.len() == 0 => {
                matches!(
                    (&txn.placeholder_identity, file_identity(output)),
                    (Some(expected), Some(actual)) if expected == &actual
                )
            }
            Ok(_) => match txn.output_sha256.as_deref() {
                // 新日志只删内容指纹对得上的产物；别人占的位置不动。
                Some(expected) => crate::source_identity::sha256_of(output)
                    .ok()
                    .as_deref()
                    == Some(expected),
                // 老日志没有指纹：只删大小正好等于这次产出尺寸的文件。
                // 旧契约的"无条件清"会把用户在崩溃后放进这个路径的内容一并删掉。
                None => {
                    fs::metadata(output).map(|meta| meta.len()).unwrap_or(0)
                        == txn.expected_output_size
                        && txn.expected_output_size > 0
                }
            },
            Err(_) => false,
        };
        if ours {
            let _ = fs::remove_file(output);
        }
    }
    remove_temp(txn);
}

/// 新建模式清账：只删**能证明是这次生成的**那个文件。
///
/// 空占位要出示凭证：journal 里记的 `(dev,ino)` 与磁盘上的实际身份对得上才算数 ——
/// 大小为零不构成身份，外部在崩溃后放进同一路径的空文件分文不动。
/// 非空产物按内容指纹对账（§2.5 同款身份门）：大小相同不等于内容还是我们写出去的
/// 那份，用户恰好改成了同样大小的内容时绝不能删。没有指纹可对账的日志一律保留。
fn retire_generated_output(txn: &ReplaceTransaction) {
    let output = Path::new(&txn.output_path);
    match fs::metadata(output) {
        Ok(meta) if meta.len() == 0 => {
            let ours = match (&txn.placeholder_identity, file_identity(output)) {
                (Some(expected), Some(actual)) => expected == &actual,
                _ => false,
            };
            if ours {
                let _ = fs::remove_file(output);
            }
        }
        Ok(meta) if meta.len() == txn.expected_output_size => {
            let ours = match txn.output_sha256.as_deref() {
                Some(expected) => crate::source_identity::sha256_of(output)
                    .ok()
                    .as_deref()
                    == Some(expected),
                None => false,
            };
            if ours {
                let _ = fs::remove_file(output);
            }
        }
        Ok(_) => {}
        Err(_) => {}
    }
    remove_temp(txn);
}

/// 删事务的临时文件。**归属门**：这条路径不保证还躺着我们的候选。
///
/// 原子交换成功 → 验证失败 → 换回也失败 → 冲突迁移也失败时，被换出来的**外部字节**
/// 恰好停在临时路径上。以前这里只查 `.protected` 标记，而标记是 `fs::write` 写下去的
/// —— 写失败（磁盘满、只读目录）就等于没有保护，用户的字节照删不误。
/// 现在的第一依据是记账时落盘的 `(dev,ino)`：inode 对得上 = 还是我们独占认领的那个
/// 文件；对不上但字节正是这次承诺的产物 = 也是我们写的；两条都证明不了就原样留着。
/// 标记仍然先查，它是"这里的字节属于外部"的显式声明，压过一切归属推断。
fn remove_temp(txn: &ReplaceTransaction) {
    let Some(temp) = txn.temp_output_path.as_deref() else {
        return;
    };
    let path = Path::new(temp);
    // 正常提交时 rename 已经把它变成目标了：无物可清，也不必报"证明不了归属"。
    if !path.exists() {
        return;
    }
    if temp_is_protected(path) {
        log::warn!("事务临时路径带保护标记（交换失败的外部内容），已原样保留: {temp}");
        return;
    }
    if !temp_is_ours(path, txn.temp_identity, txn.output_sha256.as_deref()) {
        log::warn!("事务临时路径上的字节证明不了归属，已原样保留: {temp}");
        return;
    }
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(protected_marker_path(path));
}

/// `.protected` 标记的路径：临时文件名 + `.protected`。
fn protected_marker_path(temp: &Path) -> PathBuf {
    let mut name = temp.as_os_str().to_os_string();
    name.push(".protected");
    PathBuf::from(name)
}

/// 这条临时路径是否被显式声明为"外部字节所在"。
fn temp_is_protected(temp: &Path) -> bool {
    protected_marker_path(temp).exists()
}

/// 尽力写下保护标记。
///
/// 失败必须让调用方知道：以前的 `let _ = fs::write(...)` 把"没写成"和"写成了"当成
/// 同一种结果，而这条路径接下来就会被某个清理入口按"没有标记 = 我们的半成品"删掉。
fn write_protected_marker(temp: &Path) -> Result<(), String> {
    fs::write(protected_marker_path(temp), now_marker()).map_err(|e| e.to_string())
}

/// 临时路径上是不是我们自己的字节：inode 凭证对得上，或内容正是这次承诺的产物。
fn temp_is_ours(
    temp: &Path,
    claimed_identity: Option<(u64, u64)>,
    expected_sha256: Option<&str>,
) -> bool {
    if let (Some(claimed), Some(actual)) = (claimed_identity, file_identity(temp)) {
        if claimed == actual {
            return true;
        }
    }
    // inode 对不上或拿不到（非 unix）：退到内容对账。字节都不同就没人能证明
    // 这条路径上的东西是这次事务写的，一律保留。
    match expected_sha256 {
        Some(expected) => {
            crate::source_identity::sha256_of(temp)
                .ok()
                .as_deref()
                == Some(expected)
        }
        None => false,
    }
}

fn now_marker() -> String {
    crate::history::now_millis().to_string()
}

// MARK: - §3.3 原子交换提交：先交换，再验证被换出来的那份字节

/// 原子交换发布的失败。`swapped = true` 表示覆盖**已经发生**（交换成功、
/// 验证没过、且换回失败）：调用方必须走回滚并保留现场，绝不能当
/// "什么都没发生"收场，也不能用 withdraw 假装无事。
#[derive(Debug)]
pub struct SwapPublishError {
    pub swapped: bool,
    pub detail: String,
}

impl std::fmt::Display for SwapPublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.detail)
    }
}

/// 覆盖模式的最后一道竞态：验证结束到 rename 之间目标文件仍可能被换。
/// 这里用平台原子的交换原语把"验证"与"提交"的顺序反过来 —— 先交换，
/// 再验证**被换出来**的那份字节；验证通过 = 提交同时成立，不存在中间窗口。
///
/// - macOS: `renameatx_np(RENAME_SWAP)`；Linux: `renameat2(RENAME_EXCHANGE)`。
/// - 平台/文件系统不支持原子交换（ENOSYS/EINVAL/EOPNOTSUPP）时退回保守路径：
///   先 `fingerprint_intact` 再 rename —— 与旧实现的最后一道门同级，不倒退。
/// - 验证不一致且换回成功：目标原样，本次提交整体撤销（调用方按未发生收场）。
/// - 换回失败：**保留现场** —— 目标位置此刻是我们的产物、用户内容在交换出来的
///   临时路径上；尽力把用户内容改名成看得见的 `*.sizqo-conflict-N` 防止它被
///   当作我们的临时文件清掉；调用方必须走回滚（用备份恢复原图）而不是 withdraw。
pub fn publish_replace_if_identity_matches(
    staged: &Path,
    target: &Path,
    expected: &crate::source_identity::SourceIdentity,
) -> Result<(), SwapPublishError> {
    match atomic_swap(staged, target) {
        Ok(()) => {}
        Err(error) if swap_unsupported(&error) => {
            // §28 审查修复（P1-4）：没有原子交换≠只能"看一眼指纹再 rename"。
            // 那条路既留着复核与改名之间的窗口，又允许 rename 盖掉窗口里
            // 外部新放的文件 —— 换成隔离改名 + 不覆盖占名。
            return publish_replace_via_quarantine(staged, target, expected);
        }
        Err(error) => {
            return Err(SwapPublishError {
                swapped: false,
                detail: format!("原子交换失败: {error}"),
            });
        }
    }
    // 交换成功：staged 现在持有"被换出来的目标旧内容"。
    if let crate::source_identity::SourceCheck::Changed(detail) = expected.verify(staged) {
        // 被换出来的不是我们压缩所依据的那份原图：换回去，撤销这次提交。
        match atomic_swap(staged, target) {
            Ok(()) => {
                return Err(SwapPublishError {
                    swapped: false,
                    detail: format!(
                        "目标文件在提交前一刻被外部修改（{detail}）；已交换回原状，本次提交已撤销"
                    ),
                });
            }
            Err(swap_back) => {
                let relocated = match relocate_to_conflict_name(staged, target) {
                    Some(conflict) => conflict,
                    // 迁移也失败：外部内容停在临时路径上。写保护标记 ——
                    // 事务清理（remove_temp）见到标记绝不删这条路径。
                    None => {
                        if let Err(detail) = write_protected_marker(staged) {
                            // 标记没写成也不能当作"没有外部内容"：临时路径的 inode
                            // 已经不是记账时认领的那个，remove_temp 的归属门照样拒绝删除。
                            log::error!("保护标记写入失败（归属门仍会拒绝清理）: {detail}");
                        }
                        staged.to_path_buf()
                    }
                };
                return Err(SwapPublishError {
                    swapped: true,
                    detail: format!(
                        "目标文件在提交前一刻被外部修改（{detail}），交换回滚也失败（{swap_back}）；\
                         原内容已保留在 {relocated:?}，原图备份与事务日志都保留在原处，请人工确认"
                    ),
                });
            }
        }
    }
    // 验证通过：提交成立。旧内容的可信副本在备份里，这份交换出来的可以删了。
    if let Err(error) = fs::remove_file(staged) {
        log::warn!("提交后被换出的旧文件删除失败（原图备份仍在）: {error}");
    }
    Ok(())
}

/// 把已经 fsync 的备份副本写回源位置 —— 带**产物身份复核**的提交（本轮加固 F5）。
///
/// 恢复原图与事务回滚共用这一条：无条件 rename 的旧写法在"冲突检查 → 提交"之间
/// 留着覆盖窗口，检查完到改名前被外部换掉的文件会被直接盖掉。现在与覆盖提交走
/// 同一条纪律：先原子交换、再验证被换出来的那份的身份与内容指纹（`expected` =
/// 检查时刻的产物身份），对不上换回撤销；交换不可用走隔离路线，绝不回到
/// "看一眼再 rename"。装不进去/对不上都拒绝，原图备份原样保留。
pub(crate) fn write_back_verified(
    backup: &Path,
    target: &Path,
    expected: &crate::source_identity::SourceIdentity,
) -> Result<(), String> {
    let staged = StagedOutput::copy_staged(backup, target)?;
    match publish_replace_if_identity_matches(&staged.temp, target, expected) {
        Ok(()) => {
            staged.settle();
            let _ = crate::history::sync_dir(target.parent().unwrap_or_else(|| Path::new("/")));
            Ok(())
        }
        Err(error) => {
            staged.discard();
            Err(error.detail)
        }
    }
}

/// 备份写回一个**应当是空位**的位置（跨格式替换后的原图位、或回滚时源文件已不在
/// 原位）：三级台阶装进去，位置被人占了就拒绝 —— 那个位置上的字节不是我们承诺
/// 要恢复的东西，无条件 rename 会把用户的文件盖掉（本轮加固 F5 同源问题）。
pub(crate) fn write_back_to_vacated_path(backup: &Path, target: &Path) -> Result<(), String> {
    let staged = StagedOutput::copy_staged(backup, target)?;
    if let Err(error) = install_at_free_path(&staged.temp, target) {
        staged.discard();
        return Err(error);
    }
    staged.settle();
    let _ = crate::history::sync_dir(target.parent().unwrap_or_else(|| Path::new("/")));
    Ok(())
}

// MARK: - §28 跨格式替换：删掉原图时的归属证明

/// 一次隔离名认领：路径 + 占位的 `(dev,ino)` 凭证（从还开着的句柄上取）。
struct QuarantineClaim {
    path: PathBuf,
    identity: Option<(u64, u64)>,
}

/// 在同目录独占认领一个隔离名（O_EXCL）：改名过去不可能盖掉任何人的文件 ——
/// 前提是动手之前核得出这个名字还在我们手里（本轮加固 F2）。
fn claim_quarantine_name(file: &Path) -> Result<QuarantineClaim, String> {
    let directory = file
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let named = tempfile::Builder::new()
        .prefix(".sizqo-retire-")
        .suffix(".keep")
        .tempfile_in(directory)
        .map_err(|error| format!("无法认领隔离名（{error}）"))?;
    let identity = identity_of_open(named.as_file());
    let path = named
        .into_temp_path()
        .keep()
        .map_err(|error| format!("隔离名落盘失败（{error}）"))?;
    Ok(QuarantineClaim { path, identity })
}

/// 隔离占位还是我们的吗？对不上 = O_EXCL 占位被外部换成自己的文件，
/// 此时往这个名字上 rename 就是覆盖 —— 拒绝，外来字节分文不动。
fn quarantined_still_ours(claim: &QuarantineClaim) -> Result<(), String> {
    if file_identity(&claim.path) == claim.identity {
        Ok(())
    } else {
        Err(format!(
            "隔离名上的占位已不是本次认领的那个（{}），放弃覆盖以保护它",
            claim.path.display()
        ))
    }
}

/// 原子的「不覆盖改名」：目标已存在就失败，绝不替换任何人的文件。
///
/// `fs::rename` 在三个平台上都是覆盖式的（POSIX rename 无条件替换目标；Rust 在
/// Windows 上带 `MOVEFILE_REPLACE_EXISTING`）。凡是「把候选放进一个必须是空位的
/// 名字」，都必须走这一条 —— `exists()` 查一下再 rename，查空与改名之间就是
/// 上一轮报出的覆盖竞态（P1-a / P1-b）。
fn rename_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if forced_unsupported(ForcePrimitive::NoReplaceRename) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "test: no-replace rename forced unavailable",
        ));
    }
    rename_no_replace_platform(from, to)
}

/// macOS：`renamex_np(RENAME_EXCL)` —— 目标存在时返回 `EEXIST`。
#[cfg(target_vendor = "apple")]
fn rename_no_replace_platform(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Linux：`renameat2(RENAME_NOREPLACE)` —— 老内核或不认这面旗的文件系统回 `EINVAL`。
#[cfg(all(unix, not(target_vendor = "apple")))]
fn rename_no_replace_platform(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Windows：`MoveFileExW` **不带** `MOVEFILE_REPLACE_EXISTING` —— 目标存在即失败。
/// `fs::rename` 恰好带了这个旗，所以它不能用来实现"不覆盖"。
#[cfg(windows)]
fn rename_no_replace_platform(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let mut from_wide: Vec<u16> = from.as_os_str().encode_wide().collect();
    from_wide.push(0);
    let mut to_wide: Vec<u16> = to.as_os_str().encode_wide().collect();
    to_wide.push(0);
    let ok = unsafe { win32_move_file_ex_w(from_wide.as_ptr(), to_wide.as_ptr(), 0) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    #[link_name = "MoveFileExW"]
    fn win32_move_file_ex_w(from: *const u16, to: *const u16, flags: u32) -> i32;
}

#[cfg(not(any(unix, windows)))]
fn rename_no_replace_platform(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no-replace rename is unavailable on this platform",
    ))
}

/// 这块文件系统/内核不支持「不覆盖改名」吗？不支持就往下退一级，真错误照实报。
fn no_replace_unsupported(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        // ENOTSUP 与 EOPNOTSUPP 在 Linux/macOS 上是同一个值，只写一个避免重复模式。
        matches!(
            error.raw_os_error(),
            Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// 用 `O_EXCL` 把目标名独占下来，返回认领到的 `(dev,ino)` 供提交前核对。
///
/// 这是各平台、各文件系统都原子的一步（`open(O_CREAT|O_EXCL)` / `CreateFileW` 的
/// `CREATE_NEW`）：别人建不出这个名字，之后 rename 覆盖的只可能是我们刚建的那个
/// 0 字节文件 —— 前提是提交前核得出"还是那个文件"。凭证从**还开着的句柄**上取
/// （本轮加固 F1：句柄即弃、按路径重 stat 的旧写法，凭证本身就隔着一窗口）。
fn claim_empty_placeholder(target: &Path) -> Result<Option<(u64, u64)>, String> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
    {
        Ok(file) => {
            let identity = identity_of_open(&file);
            drop(file);
            Ok(identity)
        }
        Err(error) => Err(format!("目标位置已被外部占用（{error}），放弃覆盖以保护它")),
    }
}

/// 把候选放到**必须为空**的目标位上，任何一级都不许覆盖已存在的文件。
///
/// 三级台阶，每一级都原子，最坏结果是拒绝而不是替换别人的字节：
///   ① `rename_no_replace` —— 原生"目标已存在就失败"的改名，一步到位；
///   ② 硬链接占名 —— 目标存在必然失败，成功则候选的旧名收回（同一 inode）；
///   ③ `O_EXCL` 占位 + rename 覆盖**自己的**占位 —— 覆盖对象从"任何人"缩小到
///      "我们刚创建的那个空文件"。
///
/// 上一轮在②失败后落到「`exists()` 查空 + `fs::rename`」，查空与改名之间外部
/// 塞进来的文件会被直接盖掉（P1-a）。
fn install_at_free_path(candidate: &Path, target: &Path) -> Result<(), String> {
    match rename_no_replace(candidate, target) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!("目标位置已被外部占用（{error}），放弃覆盖以保护它"));
        }
        Err(error) if no_replace_unsupported(&error) => {
            log::debug!("无覆盖改名不可用（{error}），退回硬链接占名");
        }
        Err(error) => {
            return Err(format!(
                "{} → {}: {error}",
                candidate.display(),
                target.display()
            ));
        }
    }

    if !hard_links_unavailable() {
        match fs::hard_link(candidate, target) {
            Ok(()) => {
                // 同一个 inode 已经落在目标位上：候选那个名字收回，字节一个都不动。
                if let Err(error) = fs::remove_file(candidate) {
                    log::warn!("占名后的候选旧名收回失败（字节已在目标位置）: {error}");
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(format!("目标位置已被外部占用（{error}），放弃覆盖以保护它"));
            }
            Err(error) => {
                log::debug!("硬链接占名不可用（{error}），退回独占占位");
            }
        }
    }

    let claimed = claim_empty_placeholder(target)?;
    #[cfg(test)]
    inject_placeholder_steal_fault(target);
    // 本轮加固 F1：提交之前必须证明目标位上还是我们刚认领的那个占位。
    // 占位被外部删掉再放自己的文件进来时，这里的 rename 就是覆盖别人 ——
    // 身份对不上（或证明不了）就拒绝，绝不用覆盖式 rename 碰运气。
    if file_identity(target) != claimed {
        return Err(format!(
            "目标位上的占位文件已不是本次认领的那个（{}），放弃覆盖以保护它",
            target.display()
        ));
    }
    match fs::rename(candidate, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            // 只收走证明得了归属的占位：仍是刚才认领的那个 inode。
            if claimed.is_some() && file_identity(target) == claimed {
                let _ = fs::remove_file(target);
            }
            Err(format!(
                "{} → {}: {error}",
                candidate.display(),
                target.display()
            ))
        }
    }
}

/// 测试用：把某一级台阶强制成"这台机器不支持"，让下面几级真的被跑到。
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum ForcePrimitive {
    NoReplaceRename,
    HardLink,
}

#[cfg(test)]
thread_local! {
    static TEST_FORCE_UNAVAILABLE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn forced_unsupported(which: ForcePrimitive) -> bool {
    let bit = match which {
        ForcePrimitive::NoReplaceRename => 1u8,
        ForcePrimitive::HardLink => 2u8,
    };
    TEST_FORCE_UNAVAILABLE.with(|cell| cell.get() & bit != 0)
}

#[cfg(test)]
pub(crate) fn set_forced_unsupported(which: ForcePrimitive, active: bool) {
    TEST_FORCE_UNAVAILABLE.with(|cell| {
        let bit = match which {
            ForcePrimitive::NoReplaceRename => 1u8,
            ForcePrimitive::HardLink => 2u8,
        };
        let current = cell.get();
        cell.set(if active { current | bit } else { current & !bit });
    });
}

#[cfg(not(test))]
fn hard_links_unavailable() -> bool {
    false
}

#[cfg(test)]
fn hard_links_unavailable() -> bool {
    forced_unsupported(ForcePrimitive::HardLink)
}

/// 没有原子交换时（Windows，或不支持 exchange 的文件系统）怎么把覆盖做到尽量同级。
///
/// 旧写法是「复核指纹 → `fs::rename`」：复核与改名之间塞得进一次外部改写，而 rename
/// 会把那个位置上任何新出现的字节直接盖掉（上一轮的 P1-4）。这里改成三步：
///   ① 目标改名进独占隔离名 —— 此后 target 要么空着，要么属于外部新放的文件；
///   ② 复验隔离出来的那份字节（`verify` 返回 `Some(说明)` = 不是我们的）：不是就
///      放回原位并拒绝；
///   ③ 用**不会覆盖**的方式把候选放进 target（见 `install_at_free_path`：原生
///      无覆盖改名 → 硬链接占名 → O_EXCL 占位 + 覆盖自己的占位），
///      放不进去就把原内容放回原位。
/// 三级台阶之外没有第四种写法：装不进去就是拒绝提交，绝不回到「查空 + rename」。
///
/// `verify` 是唯一的分叉点：同格式覆盖验的是**内容指纹**，跨格式/新建验的是
/// **占位 inode 凭证**。除了"谁算我们那份字节"之外，两条路的风险处置必须一模一样。
fn publish_via_quarantine(
    staged: &Path,
    target: &Path,
    verify: impl Fn(&Path) -> Option<String>,
) -> Result<(), SwapPublishError> {
    let refused = |detail: String| SwapPublishError {
        swapped: false,
        detail,
    };

    let quarantine = match claim_quarantine_name(target) {
        Ok(claim) => claim,
        Err(detail) => return Err(refused(format!("{detail}，未做任何覆盖"))),
    };
    if let Err(detail) = quarantined_still_ours(&quarantine) {
        return Err(refused(format!("{detail}，未做任何覆盖")));
    }
    if let Err(error) = fs::rename(target, &quarantine.path) {
        let _ = fs::remove_file(&quarantine.path);
        return Err(refused(format!(
            "无法把目标移入隔离名（{error}），放弃覆盖以保护当前位置的字节"
        )));
    }

    if let Some(detail) = verify(&quarantine.path) {
        return Err(SwapPublishError {
            swapped: false,
            detail: match restore_quarantined(&quarantine.path, target) {
                Restored::InPlace => format!(
                    "{detail}；已放回原位，本次提交已撤销"
                ),
                Restored::InPlaceForeignKept(kept) => format!(
                    "{detail}，原位又被外部占用；\
                     原内容放回原位，外部那份保留在 {}，未删除",
                    kept.display()
                ),
                Restored::Kept(kept) => format!(
                    "{detail} 且无法放回原位；\
                     字节保留在 {}，请人工确认",
                    kept.display()
                ),
            },
        });
    }

    #[cfg(test)]
    inject_install_fault(target);
    if let Err(detail) = install_at_free_path(staged, target) {
        return Err(SwapPublishError {
            swapped: false,
            detail: match restore_quarantined(&quarantine.path, target) {
                Restored::InPlace => format!("{detail}；原图已放回原位，本次提交已撤销"),
                Restored::InPlaceForeignKept(kept) => format!(
                    "{detail}；原图已回到原位，外部那份保留在 {}，未删除",
                    kept.display()
                ),
                Restored::Kept(kept) => format!(
                    "{detail}；原图字节保留在 {}，请人工确认",
                    kept.display()
                ),
            },
        });
    }

    // 覆盖已经成立：隔离名上的就是被换出的原图（刚复验过，备份里还有一份可信副本）。
    if let Err(error) = fs::remove_file(&quarantine.path) {
        log::warn!("提交后被换出的旧文件删除失败（原图备份仍在）: {error}");
    }
    Ok(())
}

/// 同格式覆盖的隔离路线：验的是**内容指纹**。
fn publish_replace_via_quarantine(
    staged: &Path,
    target: &Path,
    expected: &crate::source_identity::SourceIdentity,
) -> Result<(), SwapPublishError> {
    use crate::source_identity::SourceCheck;
    publish_via_quarantine(staged, target, |path| match expected.verify(path) {
        SourceCheck::Changed(detail) => Some(format!("目标文件在提交前一刻被外部修改（{detail}）")),
        SourceCheck::Unchanged => None,
    })
}

/// 跨格式 / 新建输出的隔离路线：验的是**占位 inode 凭证**。
fn publish_claimed_via_quarantine(
    staged: &Path,
    target: &Path,
    expected: (u64, u64),
) -> Result<(), String> {
    publish_via_quarantine(staged, target, |path| {
        if file_identity(path) == Some(expected) {
            None
        } else {
            Some(format!(
                "输出位置已被外部改动（{} 不是本次认领的占位文件）",
                target.display()
            ))
        }
    })
    .map_err(|error| error.detail)
}

/// 交换回滚失败后的现场处置：停在临时路径上的字节属于外部。
///
/// 先尽力迁到用户看得见的冲突名；迁不动就写 `.protected` 标记（事务清理见到标记
/// 绝不删这条路径）。两条都没走通也绝不删 —— 临时路径的 inode 已不是记账时认领的
/// 那个，`remove_temp` 的归属门照样拒绝清理。
fn protect_foreign_bytes(staged: &Path, target: &Path) -> PathBuf {
    match relocate_to_conflict_name(staged, target) {
        Some(conflict) => conflict,
        None => {
            if let Err(detail) = write_protected_marker(staged) {
                log::error!("保护标记写入失败（归属门仍会拒绝清理）: {detail}");
            }
            staged.to_path_buf()
        }
    }
}

/// 隔离后的字节回到原位的结果，三种现场各有各的实话。
enum Restored {
    /// 原图已回到原位，磁盘上没有需要额外处置的东西。
    InPlace,
    /// 原图已回到原位，外部那份被原子交换进了隔离名 —— 那条路径不是我们的，留着。
    InPlaceForeignKept(PathBuf),
    /// 放回失败（原位被外部占用且交换不可用，或改名本身没成）：字节仍在返回的路径上。
    Kept(PathBuf),
}

/// 跨格式替换（PNG → WebP 这一类）里「删掉原图」那一步。
///
/// 以前这里是「复核一次 → `fs::remove_file`」，复核与删除之间隔着一次系统调用：
/// 复核看到的是旧字节，删掉的却是**此刻路径上的任何东西**。而且更早的版本连这一步
/// 复核都不拦（记了"已保留原文件"的警告却照样删 —— 本轮审查的 P0-1）。
///
/// 现在拆成三步，每一步都只删得到自己证明得了归属的字节：
///   ① 动手前最后一次完整复核（stat 三项 + 内容哈希）：不是原图就**根本不进删除**；
///   ② 原子改名到一个独占认领的隔离名 —— 从这一刻起，外部程序在这个目录里怎么折腾
///      都改不到我们即将删除的那份字节；
///   ③ 复验隔离出来的这份字节：对得上才删；对不上（改名窗口里还有人持有 fd 在写）
///      就放回原位，原位被外部占了就用原子交换顶回来。
/// 任何一条路走不通，结果都是"字节还在磁盘上"+ 一句说实话的警告，绝不会是
/// "报了保留却把文件删了"。
///
/// `Ok(())` = 原图已删除；`Err(detail)` = 原图保留，detail 是给用户看的原话。
pub fn retire_original(
    file: &Path,
    identity: &crate::source_identity::SourceIdentity,
) -> Result<(), String> {
    use crate::source_identity::SourceCheck;

    if let SourceCheck::Changed(detail) = identity.verify(file) {
        return Err(format!("原文件在提交窗口内被外部修改（{detail}），已保留"));
    }

    // ② 独占认领隔离名：O_EXCL 保证这个名字在我们手里，改名过去不会盖掉任何人。
    let quarantine =
        claim_quarantine_name(file).map_err(|detail| format!("{detail}，原图已保留"))?;
    if let Err(detail) = quarantined_still_ours(&quarantine) {
        return Err(format!("{detail}，原图已保留"));
    }

    if let Err(error) = fs::rename(file, &quarantine.path) {
        let _ = fs::remove_file(&quarantine.path);
        return Err(format!("原图改名隔离失败（{error}），已保留"));
    }

    // ③ 复验：改名之后这条路径归我们独占，此刻的字节就是即将被删除的字节。
    #[cfg(test)]
    inject_retire_fault(&quarantine.path, file);
    if let SourceCheck::Changed(detail) = identity.verify(&quarantine.path) {
        return Err(match restore_quarantined(&quarantine.path, file) {
            Restored::InPlace => format!(
                "原文件在隔离窗口内被外部修改（{detail}），新版本是磁盘上的那份，已放回原位并保留"
            ),
            Restored::InPlaceForeignKept(kept) => format!(
                "原文件在隔离窗口内被外部修改（{detail}），已放回原位；外部那份保留在 {}，未删除",
                kept.display()
            ),
            Restored::Kept(kept) => format!(
                "原文件在隔离窗口内被外部修改（{detail}），原位又被外部占用；字节保留在 {}，请人工确认",
                kept.display()
            ),
        });
    }

    #[cfg(test)]
    inject_retire_post_verify_fault(&quarantine.path);
    // ④ 删除前再看一眼体积：改名不会让外部**已经持有的文件句柄**失效，验证通过之后
    //    那支 fd 仍能往同一个 inode 里追加。这一眼拦得住"又长了字节"的那一类；
    //    同长度原地改写拦不住 —— 那一小段的兜底是覆盖前就做好的原图备份，不是这里。
    if let Ok(meta) = fs::metadata(&quarantine.path) {
        if meta.len() != identity.size {
            return Err(match restore_quarantined(&quarantine.path, file) {
                Restored::InPlace => format!(
                    "原文件在复验之后仍被外部写入（{} → {} 字节），已放回原位并保留",
                    identity.size,
                    meta.len()
                ),
                Restored::InPlaceForeignKept(kept) => format!(
                    "原文件在复验之后仍被外部写入（{} → {} 字节），已放回原位；外部那份保留在 {}，未删除",
                    identity.size,
                    meta.len(),
                    kept.display()
                ),
                Restored::Kept(kept) => format!(
                    "原文件在复验之后仍被外部写入（{} → {} 字节），原位又被外部占用；字节保留在 {}，请人工确认",
                    identity.size,
                    meta.len(),
                    kept.display()
                ),
            });
        }
    }
    match fs::remove_file(&quarantine.path) {
        Ok(()) => Ok(()),
        Err(error) => Err(match restore_quarantined(&quarantine.path, file) {
            Restored::InPlace => format!("原图删除失败（{error}），已放回原位"),
            Restored::InPlaceForeignKept(kept) => format!(
                "原图删除失败（{error}），已放回原位；外部那份保留在 {}，未删除",
                kept.display()
            ),
            Restored::Kept(kept) => format!(
                "原图删除失败（{error}），字节仍在 {}，请人工确认",
                kept.display()
            ),
        }),
    }
}

/// 用户**明确确认**要删的文件走这条路（后缀撤销 + force）：跳过内容复核，
/// 但隔离认领与提交前身份复核一样不少 —— 确认的是"这份产物可以删"，
/// 不是"路径上随便什么都可以删"。
pub fn retire_confirmed(file: &Path) -> Result<(), String> {
    let quarantine =
        claim_quarantine_name(file).map_err(|detail| format!("{detail}，文件已保留"))?;
    if let Err(detail) = quarantined_still_ours(&quarantine) {
        return Err(format!("{detail}，文件已保留"));
    }
    if let Err(error) = fs::rename(file, &quarantine.path) {
        let _ = fs::remove_file(&quarantine.path);
        return Err(format!("文件改名隔离失败（{error}），已保留"));
    }
    match fs::remove_file(&quarantine.path) {
        Ok(()) => Ok(()),
        Err(error) => Err(match restore_quarantined(&quarantine.path, file) {
            Restored::InPlace => format!("文件删除失败（{error}），已放回原位"),
            Restored::InPlaceForeignKept(kept) => format!(
                "文件删除失败（{error}），已放回原位；外部那份保留在 {}，未删除",
                kept.display()
            ),
            Restored::Kept(kept) => format!("文件删除失败（{error}），字节仍在 {}，请人工确认", kept.display()),
        }),
    }
}

/// 把隔离出来的字节放回原位。
///
/// 原位已经被外部放了东西时，只有原子交换能保证「放回原图」不等于「盖掉别人的文件」；
/// 交换成功则外部那份落到隔离名上 —— 那条路径不是我们的，一个字节都不许动。
/// 不支持原子交换的平台（Windows 走 `atomic_swap` 的 Unsupported 分支）在这种现场
/// 一律保留隔离名并回报路径，绝不 rename 覆盖。
///
/// 原位是空的时**也不许**「查空 + rename」：查空与改名之间外部新放进去的文件会被
/// rename 顶掉（上一轮的 P1-b）。这里走与占名同一条三级台阶（`install_at_free_path`），
/// 装不进去就转去原子交换 —— 两条路都保不住时，字节留在隔离名上等人工确认。
fn restore_quarantined(quarantine: &Path, original: &Path) -> Restored {
    if original.exists() {
        return restore_over_occupied(quarantine, original);
    }
    match install_at_free_path(quarantine, original) {
        Ok(()) => Restored::InPlace,
        // 空位在装回的过程中被外部占住了：只有交换能同时保住两边的字节。
        Err(_) if original.exists() => restore_over_occupied(quarantine, original),
        Err(_) => Restored::Kept(quarantine.to_path_buf()),
    }
}

/// 原位已被别人占着：交换是唯一能"放回原图又不销毁对方"的原语。
fn restore_over_occupied(quarantine: &Path, original: &Path) -> Restored {
    match atomic_swap(quarantine, original) {
        Ok(()) => Restored::InPlaceForeignKept(quarantine.to_path_buf()),
        Err(_) => Restored::Kept(quarantine.to_path_buf()),
    }
}

/// §9.1 仅测试使用的故障注入点：原图改名进隔离名之后、复验之前动手。
/// 0 = 关；1 = 隔离名上还有另一个持有 fd 的写者在改这份字节；
/// 2 = 外部在原位置上放进了新版本（本轮审查 P0-1 报的那次事故现场）；
/// 3 = 两条同时发生 —— 复验失败且原位已被别人占住，「放回原图」只剩原子交换一条路；
/// 4 = 复验**通过之后**才写：用户上一轮问的那个极窄窗口（改名不让别人的 fd 失效）。
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_FAULT_IN_RETIRE_WINDOW: std::cell::Cell<u8>
        = const { std::cell::Cell::new(0) };
    /// 目标位空出来之后、候选放进去之前：外部把这份空位占了。
    /// 旧写法（复核指纹 + `fs::rename`）正是在这里把 rename 盖到别人身上（上一轮 P1-4）。
    /// 1 = install 窗口直接占位；2 = 认领 O_EXCL 占位后把它偷换成外来文件（本轮 F1）。
    pub(crate) static TEST_FAULT_IN_INSTALL_WINDOW: std::cell::Cell<u8>
        = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn set_fault_in_retire_window(mode: u8) {
    TEST_FAULT_IN_RETIRE_WINDOW.with(|cell| cell.set(mode));
}

#[cfg(test)]
pub(crate) fn set_fault_in_install_window(mode: u8) {
    TEST_FAULT_IN_INSTALL_WINDOW.with(|cell| cell.set(mode));
}

/// 测试用：往隔离窗口里注入上面几种现场（正式产品里这些开关不存在）。
#[cfg(test)]
fn inject_retire_fault(quarantine: &Path, original: &Path) {
    use std::io::Write;
    match TEST_FAULT_IN_RETIRE_WINDOW.with(std::cell::Cell::get) {
        1 | 3 => {
            if let Ok(mut handle) = fs::OpenOptions::new().write(true).open(quarantine) {
                let _ = handle.write_all(b"still-writing");
                let _ = handle.sync_all();
            }
        }
        _ => {}
    }
    match TEST_FAULT_IN_RETIRE_WINDOW.with(std::cell::Cell::get) {
        2 | 3 => {
            let _ = fs::write(original, b"external-new-version");
        }
        _ => {}
    }
}

/// 测试用：复验**已经通过**之后、删除之前，另一个持有 fd 的写者继续写 ——
/// 改名不会让人家的文件句柄失效，所以这个窗口真实存在（上一轮问的 P2）。
#[cfg(test)]
fn inject_retire_post_verify_fault(quarantine: &Path) {
    use std::io::Write;
    if TEST_FAULT_IN_RETIRE_WINDOW.with(std::cell::Cell::get) != 4 {
        return;
    }
    if let Ok(mut handle) = fs::OpenOptions::new().append(true).open(quarantine) {
        let _ = handle.write_all(b"-late-write");
        let _ = handle.sync_all();
    }
}

/// 测试用：候选放进原位之前，外部占住了刚空出来的目标位。
#[cfg(test)]
fn inject_install_fault(target: &Path) {
    if TEST_FAULT_IN_INSTALL_WINDOW.with(std::cell::Cell::get) == 1 {
        let _ = fs::write(target, b"external-in-window");
    }
}

/// 测试用：O_EXCL 占位认领之后、提交之前，外部把占位删掉换成自己的文件 ——
/// 本轮加固 F1 的现场。旧写法在这里会把 rename 盖到外来文件上。
#[cfg(test)]
fn inject_placeholder_steal_fault(target: &Path) {
    if TEST_FAULT_IN_INSTALL_WINDOW.with(std::cell::Cell::get) == 2 {
        let _ = fs::remove_file(target);
        let _ = fs::write(target, b"external-stolen-placeholder");
    }
}

/// 把停在临时路径上的外来字节迁到用户看得见的冲突保留名 —— 选名与占用原子地
/// 一步完成：直接用「不覆盖改名」去抢 `.sizqo-conflict-N`，被占就换下一个号
/// （本轮加固 F3：旧写法 `exists()` 选名 + rename，两步之间被占的名字会被盖掉）。
/// 无覆盖改名不可用时退到与 `install_at_free_path` 同一条最底级台阶。
/// 全部装不进去返回 None，调用方走保护标记兜底 —— 绝不 rename 覆盖。
fn relocate_to_conflict_name(staged: &Path, target: &Path) -> Option<PathBuf> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_string();
    for index in 0..1000u32 {
        let candidate = dir.join(format!("{name}.sizqo-conflict-{index}"));
        match rename_no_replace(staged, &candidate) {
            Ok(()) => return Some(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) if no_replace_unsupported(&error) => {
                match relocate_via_exclusive_placeholder(staged, &candidate) {
                    RelocateStep::Placed(path) => return Some(path),
                    RelocateStep::Occupied => continue,
                    RelocateStep::Failed => return None,
                }
            }
            Err(_) => return None,
        }
    }
    None
}

/// 迁移外来字节的单步结果。
enum RelocateStep {
    Placed(PathBuf),
    /// 这个号被别人占着：换下一个号，别人的字节分文不动。
    Occupied,
    /// 真失败（IO 错误、或占位被换成外来文件证明不了归属）：不再换号。
    Failed,
}

/// 无覆盖改名不可用时的最后一级迁移：O_EXCL 占位 + 提交前身份复核，
/// 与 `install_at_free_path` 台阶③同一套纪律（本轮加固 F1 同款守卫）。
fn relocate_via_exclusive_placeholder(staged: &Path, candidate: &Path) -> RelocateStep {
    let claimed = match claim_empty_placeholder(candidate) {
        Ok(claimed) => claimed,
        Err(_) => return RelocateStep::Occupied,
    };
    if file_identity(candidate) != claimed {
        return RelocateStep::Failed;
    }
    if fs::rename(staged, candidate).is_ok() {
        return RelocateStep::Placed(candidate.to_path_buf());
    }
    if file_identity(candidate) == claimed {
        let _ = fs::remove_file(candidate);
    }
    RelocateStep::Failed
}

/// 这块文件系统/内核不支持原子交换吗？支持失败按真错误处理，绝不静默降级。
#[cfg(unix)]
fn swap_unsupported(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)
    )
}

#[cfg(not(unix))]
fn swap_unsupported(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Unsupported
}

#[cfg(unix)]
fn atomic_swap(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let a = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let b = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    atomic_swap_unix(&a, &b)
}

/// macOS 10.13+ 的 `renameatx_np(RENAME_SWAP)`：两个路径在一次系统调用里互换。
#[cfg(target_vendor = "apple")]
fn atomic_swap_unix(a: &std::ffi::CString, b: &std::ffi::CString) -> std::io::Result<()> {
    let rc = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Linux 3.15+ 的 `renameat2(RENAME_EXCHANGE)`（glibc 2.28 前无包装函数，走 syscall）。
#[cfg(all(unix, not(target_vendor = "apple")))]
fn atomic_swap_unix(a: &std::ffi::CString, b: &std::ffi::CString) -> std::io::Result<()> {
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn atomic_swap(_a: &Path, _b: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic swap is unavailable on this platform",
    ))
}

// MARK: - §28 §54 输出文件名：全程序唯一的分配者

/// 一个用户可见的产物名怎么排出来。
///
/// §54 要求 suffix / folder / export 用同一个分配器，因为「不能覆盖」这件事只要有一处
/// 自己拼名字，就有一处会静默盖掉邻居的文件。
#[derive(Debug, Clone)]
pub enum OutputNaming {
    /// `foo_compressed.jpg` → `foo_compressed_1.jpg` → `foo_compressed_2.jpg`…
    /// folder / export 用的就是 `suffix` 为空串的这一支（`foo.jpg` → `foo_1.jpg`）。
    WithSuffix {
        stem: String,
        suffix: String,
        extension: String,
    },
    /// 跨格式覆盖：`photo.png` 转 webp → `photo.webp` → `photo_converted.webp` →
    /// `photo_converted_2.webp`…首选名字保持"原图换扩展名"，撞了才带 `_converted`。
    Converted { stem: String, extension: String },
}

/// 冲突到第几个名字为止。正常目录到不了这里，但绝不允许 `for index in 1..` 那样空转。
pub const MAX_NAME_CONFLICTS: u32 = 10_000;

impl OutputNaming {
    /// 第 `index` 个候选名（0 = 不冲突时应该用的那个）。
    ///
    /// §54 要的就是这条序列只有一份实现：谁要"往后再排一位"都得从这里拿，
    /// 免得某处自己拼 `_1`、另一处拼 `(1)`，两套规则在同一目录里打架。
    pub fn candidate(&self, index: u32) -> String {
        match self {
            OutputNaming::WithSuffix {
                stem,
                suffix,
                extension,
            } => {
                let tail = if index == 0 {
                    String::new()
                } else {
                    format!("_{index}")
                };
                format!("{stem}{suffix}{tail}.{extension}")
            }
            OutputNaming::Converted { stem, extension } => {
                let tail = match index {
                    0 => String::new(),
                    1 => "_converted".to_string(),
                    n => format!("_converted_{n}"),
                };
                format!("{stem}{tail}.{extension}")
            }
        }
    }
}

/// 拿到就等于**已经占住**了这个名字：磁盘上多了一个我们建的 0 字节文件。
pub struct ClaimedOutput {
    path: PathBuf,
    /// 占位文件的 `(dev,ino)`（非 unix 平台为 None）。有了它，"这个路径上现在
    /// 躺着的还是不是我们认领的那个占位"就有了可验证的凭证 —— 外部程序即使
    /// 删掉占位又建了自己的同名文件，提交和清理也都不会碰它。
    identity: Option<(u64, u64)>,
}

impl ClaimedOutput {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 占位文件的文件系统身份（dev,ino）。
    pub fn identity(&self) -> Option<(u64, u64)> {
        self.identity
    }

    pub fn into_path(self) -> PathBuf {
        self.path
    }

    /// 后端名字归调用方持有，但落盘前的某一步没做成：撤掉占位，别给用户留空文件。
    /// 只有当路径上仍然是**我们认领的那个 inode** 才动手；外部放进去的内容分文不动。
    pub fn release(self) {
        if self.still_ours() {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn still_ours(&self) -> bool {
        match (self.identity, file_identity(&self.path)) {
            (Some(expected), Some(actual)) => expected == actual,
            // 取不到身份（其他平台、或这个位置根本打不开）：文件已经不在了才无事可做，
            // 还在就一律不碰 —— 证明不了归属就不删。
            (None, None) => !self.path.exists(),
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn size(&self) -> u64 {
        fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(u64::MAX)
    }
}

pub struct OutputNameAllocator;

impl OutputNameAllocator {
    /// §28「绝对禁止覆盖」的实现方式：`O_EXCL` 独占创建，撞到已存在就换下一个名字。
    ///
    /// 为什么不是 `exists()` 探一下再写 —— 探测到写入之间那段永远是空的：同一批里两个
    /// 任务（或另一个 App 实例）可以同时判定 `photo_compressed.jpg` "不存在"，后落盘的
    /// 那个把前一个的产物整个盖掉，而用户以为各留了一份。占位文件把这个名字在整个事务
    /// 期间都锁在我们手里，之后的 rename 覆盖的只可能是我们自己的 0 字节占位。
    pub fn claim(directory: &Path, naming: &OutputNaming) -> Result<ClaimedOutput, String> {
        for index in 0..MAX_NAME_CONFLICTS {
            let path = directory.join(naming.candidate(index));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                // 句柄在这里就丢掉：我们要的只是"这个名字已经名花有主"这件事。
                // 占位的 (dev,ino) 从**还开着的句柄**上取（本轮加固 F1）：
                // 它就是提交时"这个路径上是谁的文件"的凭证。
                Ok(file) => {
                    let identity = identity_of_open(&file);
                    drop(file);
                    return Ok(ClaimedOutput { path, identity });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("{}: {error}", directory.display()));
                }
            }
        }
        Err(format!(
            "{} 里同名文件太多（{} 个以上），请清理后重试",
            directory.display(),
            MAX_NAME_CONFLICTS
        ))
    }
}

/// 分块把一个文件复制进已经打开的临时文件：视频的原图可能有几个 GB，整份进内存不行。
fn copy_file_into(file: &mut fs::File, source: &Path) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut reader = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 压缩结果的落盘：同目录独占临时文件 → flush + fsync → rename 覆盖目标。
///
/// 临时文件必须和目标同目录，否则 rename 跨挂载点就不是原子操作了。
pub struct StagedOutput {
    pub temp: PathBuf,
    /// 认领这个临时名时的 `(dev,ino)`：`discard` 的归属凭证。
    /// 原子交换会把外部字节放到这条路径上，届时 inode 变了 —— 凭证对不上就不许删。
    claimed_identity: Option<(u64, u64)>,
    /// 已经写下去的那份字节的内容指纹：inode 凭证不可用时的归属依据。
    /// `RefCell` 而不是 `&mut self`：`withdraw` 闭包按不可变借用持有 staged，
    /// 而写候选的那一步要在同一个借用期内记下这份指纹。
    written_sha256: std::cell::RefCell<Option<String>>,
}
impl StagedOutput {
    /// 写一批字节再原子改名到目标（history.json、设置文件这些不需要中途记账的落盘）。
    pub fn write(target: &Path, bytes: &[u8]) -> Result<Self, String> {
        Self::stage(
            target,
            ".sizqo-write-",
            Some(crate::source_identity::sha256_bytes(bytes)),
            |file| {
                use std::io::Write;
                file.write_all(bytes).map_err(|e| e.to_string())
            },
        )
    }

    /// 整份复制一个文件再原子改名到目标（原图备份、把备份写回源文件）。
    /// 分块读写而不是整份进内存：视频的原图可能有几个 GB。
    pub fn copy_from(source: &Path, target: &Path) -> Result<Self, String> {
        Self::stage(target, ".sizqo-copy-", None, |file| {
            copy_file_into(file, source)
        })
    }

    /// 整份复制到同目录临时名但**不改名**：恢复原图这类"提交前还要凭证核验"的
    /// 落盘，由调用方拿着 staged 走带身份复核的提交原语（本轮加固 F5）。
    /// 写失败时自己收走临时文件；临时名与 copy_from 同一套独占分配。
    pub fn copy_staged(source: &Path, target: &Path) -> Result<Self, String> {
        let staged = Self::claim(target, ".sizqo-copy-")?;
        if let Err(error) = staged.fill(|file| copy_file_into(file, source)) {
            staged.discard();
            return Err(error);
        }
        Ok(staged)
    }

    /// §30 全程序只有这一处产生临时文件名：和目标同目录、O_EXCL、随机后缀。
    ///
    /// 一条队列同时跑几十上百个文件，用毫秒/纳秒时间戳命名就是让两个任务算出同一个
    /// `.tmp` —— 后写的那个把前一个的半成品盖掉，rename 过去的就成了别人的字节。
    ///
    /// 独占和写入刻意分成两步：Commit 要在**一个字节落盘之前**把这个名字记进事务日志
    /// （§31）。反过来做的话，崩在"写完临时文件"和"改名"之间时，唯一知道那个 `.tmp`
    /// 存在的记录恰好还没写，用户目录里就永远多出一个没人认领的孤儿。
    pub fn claim(target: &Path, prefix: &str) -> Result<Self, String> {
        // 相对路径的 parent() 会是 Some("")，那种目录名不能拿去开文件：按当前目录算。
        let parent = target
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let named = tempfile::Builder::new()
            .prefix(prefix)
            .suffix(".tmp")
            .tempfile_in(parent)
            .map_err(|e| e.to_string())?;
        // keep() 之后这份临时文件归我们收尾：TempPath 一 drop 就把它删掉，而改名完成后
        // 那个 inode 是用户要的产物，不许被"自动清理"顺手删了。
        let temp = named
            .into_temp_path()
            .keep()
            .map_err(|error| error.error.to_string())?;
        // 认领后立即记下 inode：之后任何一次清理都只认这份凭证。
        // 读不出来（非 unix / 刚建好就被移走）时记 None —— 清理侧据此拒绝删除。
        Ok(Self {
            claimed_identity: file_identity(&temp),
            written_sha256: std::cell::RefCell::new(None),
            temp,
        })
    }

    /// 这个临时名认领时的 `(dev,ino)`，写进事务日志当归属凭证。
    pub fn identity(&self) -> Option<(u64, u64)> {
        self.claimed_identity
    }

    /// 写字节 → flush → fsync，但**还不改名**。
    ///
    /// 覆盖模式要在"候选已经落盘"和"用户的文件被盖掉"之间插一道复核（§31 的
    /// source identity + backup check），所以这两步必须能分开调用。
    pub fn write_synced(&self, bytes: &[u8]) -> Result<(), String> {
        // 先记下"我们写下去的是哪份字节"：inode 凭证不可用的平台上，这就是
        // discard 唯一的归属依据。算的是内存里这份缓冲，不额外读盘。
        *self.written_sha256.borrow_mut() = Some(crate::source_identity::sha256_bytes(bytes));
        self.fill(|file| {
            use std::io::Write;
            file.write_all(bytes).map_err(|e| e.to_string())
        })
    }

    fn fill(&self, fill: impl FnOnce(&mut fs::File) -> Result<(), String>) -> Result<(), String> {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&self.temp)
            .map_err(|e| e.to_string())?;
        fill(&mut file)?;
        file.flush().map_err(|e| e.to_string())?;
        // 落盘确认：崩溃后 rename 过去的不能是个只在内核缓存里的文件。
        file.sync_all().map_err(|e| e.to_string())
    }

    /// 把已经 fsync 的临时文件原子改名到目标。改名之前该核的都必须已经核过。
    pub fn publish(&self, target: &Path) -> Result<(), String> {
        fs::rename(&self.temp, target)
            .map_err(|e| format!("{} → {}: {e}", self.temp.display(), target.display()))
    }

    /// 带**占位凭证**的提交：目标路径上必须还是我们认领的那个 inode 才允许 rename。
    ///
    /// 占位创建之后、提交之前，外部程序仍可能删掉 0 字节占位、在同一个路径上放进
    /// 自己的文件 —— 没有这道凭证，rename 会把别人的文件整个盖掉（§2 审查意见 A）。
    ///
    /// 复核与 rename 之间那段仍然是空的（上一轮的 P1-c）：这里和同格式覆盖走同一条
    /// 顺序 —— **先交换、再验证被换出来的那份**，验证与提交压成一次系统调用。
    /// 交换不可用时退回隔离路径（`publish_claimed_via_quarantine`），绝不"看一眼再
    /// 无条件 rename"。凭证缺失时**拒绝提交**（本轮加固 F4）：证明不了占位归属还
    /// 照样 rename，就是在"无法确认安全性"时覆盖文件 —— 失败关闭，不许降级。
    pub fn publish_claimed(
        &self,
        target: &Path,
        identity: Option<(u64, u64)>,
    ) -> Result<(), String> {
        let Some(expected) = identity else {
            return Err(format!(
                "无法取得输出占位的身份凭证（{}），放弃提交以保护目标位置的字节",
                target.display()
            ));
        };
        match atomic_swap(&self.temp, target) {
            Ok(()) => {
                // 交换成功：self.temp 现在持有被换出来的那份（应该是我们的占位）。
                if file_identity(&self.temp) == Some(expected) {
                    let _ = fs::remove_file(&self.temp);
                    return Ok(());
                }
                // 换出来的不是我们的占位：换回去，外部文件一个字节都不动。
                if atomic_swap(&self.temp, target).is_ok() {
                    return Err(format!(
                        "输出位置已被外部改动（{} 不是本次认领的占位文件），放弃提交以保护它",
                        target.display()
                    ));
                }
                let kept = protect_foreign_bytes(&self.temp, target);
                Err(format!(
                    "输出位置在提交前一刻被外部改动，交换回滚也失败；那份内容保留在 {}，未删除",
                    kept.display()
                ))
            }
            Err(error) if swap_unsupported(&error) => {
                publish_claimed_via_quarantine(&self.temp, target, expected)
            }
            // 目标位上连占位都没了（被外部删掉）：空位仍是空位，用不覆盖的方式装进去，
            // 而不是因为"验不了凭证"把用户这次的结果整个丢掉。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                install_at_free_path(&self.temp, target)
            }
            Err(error) => Err(format!("原子交换失败: {error}")),
        }
    }

    /// 占名 + 写入 + 改名一条龙：调用方不需要在中途记任何东西时使用。
    fn stage(
        target: &Path,
        prefix: &str,
        written_sha256: Option<String>,
        fill: impl FnOnce(&mut fs::File) -> Result<(), String>,
    ) -> Result<Self, String> {
        let staged = Self::claim(target, prefix)?;
        *staged.written_sha256.borrow_mut() = written_sha256;
        match staged.fill(fill).and_then(|()| staged.publish(target)) {
            Ok(()) => Ok(staged),
            Err(error) => {
                staged.discard();
                Err(error)
            }
        }
    }

    /// 事后清理临时文件（正常情况下 rename 已经把它变成目标了）。
    ///
    /// §28 审查修复：这条路径**不保证还是我们的**。原子交换换回失败时，被换出来的
    /// 外部字节恰好停在这里 —— 只看路径删，就是把用户的文件当半成品清掉。
    /// 两道门：① 带 `.protected` 标记的一律保留；② inode 仍是本次独占认领的那个，
    /// 或内容哈希正是我们写下去的那份，才允许删。两个都证明不了就原样留着 ——
    /// 宁可留一个孤儿 `.tmp`，也不删一份来历不明的字节。
    pub fn discard(&self) {
        // 正常提交时 rename 已经把它变成目标了：无物可清，不必报"证明不了归属"。
        if !self.temp.exists() {
            return;
        }
        if temp_is_protected(&self.temp) {
            log::warn!("临时路径带保护标记，已原样保留: {}", self.temp.display());
            return;
        }
        if temp_is_ours(
            &self.temp,
            self.claimed_identity,
            self.written_sha256.borrow().as_deref(),
        ) {
            let _ = fs::remove_file(&self.temp);
        } else {
            log::warn!(
                "临时路径上的字节证明不了归属，已原样保留: {}",
                self.temp.display()
            );
        }
    }

    /// 原子改名已经完成，这份暂存身份到此结束：显式收尾，调用方不必再记着那个名字。
    pub fn settle(self) {}

    #[cfg(test)]
    pub fn temp_exists(&self) -> bool {
        self.temp.exists()
    }

    #[cfg(test)]
    pub fn temp_name(&self) -> String {
        self.temp
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{now_millis, HistoryStore, MAX_HISTORY_ENTRIES};

    fn stores(dir: &Path) -> (HistoryStore, TransactionStore) {
        let root = dir.join("history");
        fs::create_dir_all(root.join(crate::history::BACKUPS_DIR)).unwrap();
        (
            HistoryStore::new(root.clone()).unwrap(),
            TransactionStore::new(root.join(TRANSACTIONS_DIR)),
        )
    }

    fn txn(source: &Path, backup: &Path, history_id: &str) -> ReplaceTransaction {
        ReplaceTransaction {
            id: format!("t-{history_id}"),
            history_id: history_id.into(),
            source_path: source.to_string_lossy().into_owned(),
            output_path: source.to_string_lossy().into_owned(),
            backup_path: backup.to_string_lossy().into_owned(),
            temp_output_path: None,
            original_size: 100,
            expected_output_size: 10,
            original_sha256: None,
            output_sha256: None,
            backup_sha256: None,
            placeholder_identity: None,
            temp_identity: None,
            created_at: now_millis(),
            cross_format: false,
            mode: TransactionMode::ReplaceExisting,
            schema_version: 0,
            backup_legacy_unverified: false,
        }
    }

    /// 现行版本（schema v2）的记账凭证：带完整的三份指纹 —— 源文件此刻的内容
    /// 当作"承诺产物"，备份内容当作"改动前的原图"。
    fn v2_txn(source: &Path, backup: &Path, history_id: &str) -> ReplaceTransaction {
        let mut record = txn(source, backup, history_id);
        record.original_sha256 = crate::source_identity::sha256_of(backup).ok();
        record.output_sha256 = crate::source_identity::sha256_of(source).ok();
        record.backup_sha256 = record.original_sha256.clone();
        record.schema_version = SCHEMA_VERSION;
        record
    }

    #[test]
    fn staged_write_replaces_the_target_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.png");
        fs::write(&target, b"old-bytes").unwrap();

        let staged = StagedOutput::write(&target, b"new").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!staged.temp_exists());
    }

    /// §30 临时名必须随机。这条判据盯的是并发：一条队列同时跑上百个文件，
    /// 用毫秒时间戳命名会让同一毫秒里的两个任务算出同一个 `.tmp`，
    /// 后写的那个把前一个的半成品盖掉 —— rename 过去的就是别人的字节。
    #[test]
    fn a_hundred_parallel_stagings_never_share_a_temp_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let workers: Vec<_> = (0..100)
            .map(|index| {
                let root = root.clone();
                std::thread::spawn(move || {
                    let target = root.join(format!("out-{index}.png"));
                    let bytes = format!("payload-{index}-{}", "x".repeat(index % 7)).into_bytes();
                    let staged = StagedOutput::write(&target, &bytes).expect("并发暂存必须成功");
                    assert!(!staged.temp_exists(), "rename 后临时名字该变成目标");
                    assert_eq!(fs::read(&target).unwrap(), bytes, "{index} 拿到了别人的字节");
                    staged.temp_name()
                })
            })
            .collect();
        let names: Vec<String> = workers
            .into_iter()
            .map(|worker| worker.join().expect("暂存线程不该 panic"))
            .collect();

        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "并发任务算出了同一个临时文件名");
        assert!(
            names.iter().all(|name| name.starts_with(".sizqo-write-")),
            "临时文件名不再认得出是 Sizqo 写的：{names:?}"
        );
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|item| {
                item.path()
                    .extension()
                    .map(|ext| ext == "tmp")
                    .unwrap_or(false)
            })
            .collect();
        assert!(leftovers.is_empty(), "落盘后还留着临时文件");
    }

    /// 备份与恢复走的是同一份暂存实现：整份复制、fsync、原子改名，不留残余。
    #[test]
    fn staging_a_copy_leaves_the_source_alone_and_the_target_complete() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.png");
        fs::write(&source, vec![7u8; 700_000]).unwrap();
        let target = dir.path().join("snapshot.png");

        StagedOutput::copy_from(&source, &target)
            .expect("整份复制必须原子落盘")
            .settle();

        assert_eq!(fs::read(&target).unwrap().len(), 700_000);
        assert!(source.exists(), "备份不许动用户原图");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|item| item.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "复制完还留着临时文件：{leftovers:?}");
    }

    #[test]
    fn a_transaction_the_history_never_recorded_rolls_back_to_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("a.png");
        fs::write(&source, b"true-original").unwrap();
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();

        // 崩溃现场：源文件已经是压缩结果，历史里却没有这条记录。
        // 现行版本的日志带着三份指纹，恢复靠它们证明归属。
        fs::write(&source, b"compressed").unwrap();
        store.prepare(&v2_txn(&source, &backup, "missing-from-history")).unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!store.has_pending());
        // 回滚靠的是备份，所以备份不能顺手删掉（回滚到一半失败时它是唯一副本）。
        assert!(backup.exists());
    }

    /// §3.1 凭证完整性门：新版本事务缺指纹不许落账；老版本记录不受新门约束。
    #[test]
    fn a_v2_journal_without_full_credentials_is_refused_at_prepare() {
        let dir = tempfile::tempdir().unwrap();
        let (_history, store) = stores(dir.path());
        let source = dir.path().join("a.png");
        let backup = dir.path().join("original.png");
        fs::write(&source, b"x").unwrap();
        fs::write(&backup, b"x").unwrap();

        let mut record = v2_txn(&source, &backup, "no-creds");
        record.backup_sha256 = None;
        assert!(
            store.prepare(&record).is_err(),
            "覆盖事务缺备份指纹必须拒绝落账"
        );
        record.backup_sha256 = Some("h".into());
        record.original_sha256 = None;
        assert!(store.prepare(&record).is_err(), "缺原图指纹同样拒绝");

        let mut new_record = create_new(&source, "no-output-hash", 10);
        new_record.schema_version = SCHEMA_VERSION;
        new_record.output_sha256 = None;
        assert!(store.prepare(&new_record).is_err(), "新建事务缺产物指纹拒绝落账");

        assert!(
            store.prepare(&txn(&source, &backup, "legacy-fine")).is_ok(),
            "老版本记录走兼容路径，不受新凭证门约束"
        );
    }

    /// §3.1 备份凭证：记账后备份字节被换掉，回滚拒绝自动恢复、保留现场。
    #[test]
    fn rollback_verifies_the_backup_voucher_before_writing_back() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("a.png");
        fs::write(&source, b"true-original").unwrap();
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();
        fs::write(&source, b"compressed").unwrap();
        let mut record = v2_txn(&source, &backup, "voucher");
        record.backup_sha256 = Some("deadbeef".into());
        store.prepare(&record).unwrap();
        fs::write(&backup, b"tampered-backup").unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 0, "备份字节和凭证对不上：拒绝自动恢复");
        assert_eq!(fs::read(&source).unwrap(), b"compressed");
        assert_eq!(fs::read(&backup).unwrap(), b"tampered-backup");
        assert!(store.has_pending(), "现场保留：备份与日志都留在原处");
    }

    /// §9.1 审查补充场景：legacy 日志 + 源文件已不在原位。
    /// 老契约仍然自动恢复 —— 源位置没有可被覆盖的用户内容，写回原图是
    /// 唯一说得通的动作；这也正是"升级用户崩溃后还能拿回原图"的通路。
    #[test]
    fn a_legacy_journal_with_a_missing_source_still_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("gone.png");
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();
        // 源文件整个不在了（用户移走 / 外部删除），日志还是老版本的无指纹格式。
        let record = txn(&source, &backup, "legacy-gone");
        store.prepare(&record).unwrap();
        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1, "源不在原位 = 没有可被覆盖的内容，写回原图");
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!store.has_pending());
    }

    /// §9.1 审查补充场景：备份字节读不出来时，现场必须被归为"需要人工"，
    /// 绝不 AutoResolvable（否则人工恢复列表过滤掉它，用户永远看不到）。
    #[cfg(unix)]
    #[test]
    fn an_unreadable_backup_is_never_classified_auto_resolvable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.png");
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();
        fs::write(&source, b"compressed").unwrap();
        let mut record = v2_txn(&source, &backup, "unreadable-voucher");
        // 记账后备份变得读不出来（权限剥离）。
        let mut mode = fs::metadata(&backup).unwrap().permissions();
        mode.set_mode(0o000);
        fs::set_permissions(&backup, mode).unwrap();
        let (status, detail) = describe_pending(&record);
        let _ = fs::remove_file(&backup);
        assert_eq!(status, PendingRecoveryStatus::VoucherMismatch, "{detail}");
        assert!(detail.contains("备份读不出来"), "{detail}");
        // 回滚本身也必须拒绝（凭证读不出来 = 无法证明要写回什么）。
        assert!(rollback(&record).is_err());
    }

    /// §2.3 审查修复回归：RC.1 事务（schemaVersion=2、带三指纹、**没有**
    /// backupLegacyUnverified 字段）升级到 RC.2 后，缺失字段保守按未验证
    /// 处理 —— 备份哈希一致也不能自动写回。
    #[test]
    fn an_rc1_transaction_without_the_legacy_field_is_treated_as_unverified() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("a.png");
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();
        fs::write(&source, b"compressed").unwrap();
        // RC.1 原始形状：schemaVersion=2、三指纹齐全、无 legacy 字段。
        let original_hash = crate::source_identity::sha256_of(&backup).unwrap();
        let output_hash = crate::source_identity::sha256_of(&source).unwrap();
        let rc1_json = serde_json::json!({
            "id": "t-rc1", "historyId": "rc1",
            "sourcePath": source.to_string_lossy(),
            "outputPath": source.to_string_lossy(),
            "backupPath": backup.to_string_lossy(),
            "originalSize": 13, "expectedOutputSize": 11,
            "originalSha256": original_hash,
            "outputSha256": output_hash,
            "backupSha256": original_hash,
            "createdAt": now_millis(),
            "crossFormat": false,
            "mode": "replace_existing",
            "schemaVersion": 2,
        });
        fs::create_dir_all(&store.root).unwrap();
        fs::write(
            store.root.join("t-rc1.json"),
            serde_json::to_vec(&rc1_json).unwrap(),
        )
        .unwrap();
        let loaded = store.list();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].backup_legacy_unverified, "缺字段必须保守按未验证");
        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 0, "RC.1 事务不得自动写回");
        assert!(report.needs_review >= 1);
        assert_eq!(fs::read(&source).unwrap(), b"compressed");
    }

    /// §3.3 原子交换：验证通过 = 提交同时成立，被换出的旧文件随后清掉。
    #[test]
    fn publish_replace_swaps_and_verifies_the_swapped_out_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.png");
        fs::write(&target, b"true-original").unwrap();
        let staged = dir.path().join(".sizqo-staged");
        fs::write(&staged, b"new-output").unwrap();
        let expected = crate::source_identity::SourceIdentity::read_with_hash(&target).unwrap();

        publish_replace_if_identity_matches(&staged, &target, &expected).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new-output");
        assert!(!staged.exists(), "验证通过后被换出的旧文件应当删掉");
    }

    /// §3.3 被换出来的不是预期原图：换回原状，目标与候选都原样保留。
    #[test]
    fn publish_replace_swaps_back_when_the_target_is_not_the_expected_original() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a.png");
        fs::write(&target, b"something-else-entirely").unwrap();
        let staged = dir.path().join(".sizqo-staged");
        fs::write(&staged, b"new-output").unwrap();
        let expected_file = dir.path().join("expected.png");
        fs::write(&expected_file, b"true-original").unwrap();
        let expected = crate::source_identity::SourceIdentity::read_with_hash(&expected_file).unwrap();

        let error = publish_replace_if_identity_matches(&staged, &target, &expected).unwrap_err();
        assert!(!error.swapped, "换回成功 = 什么都没发生");
        assert_eq!(fs::read(&target).unwrap(), b"something-else-entirely");
        assert_eq!(fs::read(&staged).unwrap(), b"new-output");
    }

    /// §3.4 恢复幂等：同一现场跑两次 recover，第二次无事可做、无警告；
    /// 无法证明归属的现场重复恢复只重复报告，绝不二次覆盖。
    #[test]
    fn recovery_is_idempotent_when_run_twice_on_the_same_scene() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("a.png");
        fs::write(&source, b"true-original").unwrap();
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();
        fs::write(&source, b"compressed").unwrap();
        store.prepare(&v2_txn(&source, &backup, "twice")).unwrap();

        let first = store.recover(&history);
        assert_eq!(first.rolled_back, 1);
        let second = store.recover(&history);
        assert_eq!(second.rolled_back, 0, "第二次恢复无事可做");
        assert_eq!(second.committed, 0);
        assert!(second.warnings.is_empty());
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!store.has_pending());

        // 人工恢复分支同样幂等：现场保留的事务重复恢复只重复报告。
        fs::write(&source, b"user-edited-content").unwrap();
        store.prepare(&txn(&source, &backup, "legacy-review")).unwrap();
        let first = store.recover(&history);
        assert_eq!(first.needs_review, 1);
        assert_eq!(fs::read(&source).unwrap(), b"user-edited-content");
        let second = store.recover(&history);
        assert_eq!(second.needs_review, 1, "重复恢复绝不二次覆盖");
        assert_eq!(fs::read(&source).unwrap(), b"user-edited-content");
    }

    #[test]
    fn a_committed_transaction_only_needs_its_journal_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("a.png");
        fs::write(&source, b"compressed").unwrap();
        let backup = history.ensure_backup(&source).expect("backup");
        let entry = crate::history::sample_entry(&source, &backup, "committed-1");
        history.add(entry).unwrap();

        store.prepare(&txn(&source, &backup, "committed-1")).unwrap();
        let report = store.recover(&history);
        assert_eq!(report.committed, 1);
        assert_eq!(fs::read(&source).unwrap(), b"compressed");
        assert!(backup.exists(), "已提交的事务不许动用户文件");
        assert!(!store.has_pending());
    }

    #[test]
    fn cross_format_rollback_removes_the_converted_file_but_not_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.png");
        fs::write(&source, b"true-original").unwrap();
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();
        let output = dir.path().join("photo.jpg");
        fs::write(&output, b"converted").unwrap();

        let mut record = v2_txn(&source, &backup, "gone");
        record.output_path = output.to_string_lossy().into_owned();
        record.cross_format = true;
        // 承诺产物是那个转换出来的文件：指纹对得上才许删。
        record.output_sha256 = crate::source_identity::sha256_of(&output).ok();
        store.prepare(&record).unwrap();

        assert_eq!(store.recover(&history).rolled_back, 1);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!output.exists());
    }

    #[test]
    fn an_unparsable_journal_is_kept_and_rolls_back_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        fs::create_dir_all(&store.root).unwrap();
        fs::write(store.root.join("broken.json"), b"{ half").unwrap();
        let source = dir.path().join("a.png");
        fs::write(&source, b"compressed").unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 0);
        assert_eq!(fs::read(&source).unwrap(), b"compressed");
        assert!(store.root.join("broken.unparsed").exists());
    }

    #[test]
    fn the_cap_only_ever_evicts_the_oldest_entries() {
        assert_eq!(MAX_HISTORY_ENTRIES, 10_000);
    }

    fn create_new(output: &Path, history_id: &str, expected: u64) -> ReplaceTransaction {
        ReplaceTransaction {
            id: format!("t-{history_id}"),
            history_id: history_id.into(),
            source_path: String::new(),
            output_path: output.to_string_lossy().into_owned(),
            backup_path: String::new(),
            temp_output_path: None,
            original_size: 100,
            expected_output_size: expected,
            original_sha256: None,
            output_sha256: None,
            backup_sha256: None,
            placeholder_identity: None,
            temp_identity: None,
            created_at: now_millis(),
            cross_format: false,
            mode: TransactionMode::CreateNew,
            schema_version: 0,
            backup_legacy_unverified: false,
        }
    }

    /// §29 新建模式崩在历史之前：产物必须删掉，而**同目录的原图一个字节都不许动**。
    /// 这里最怕的写法是恢复逻辑不管模式，一律"拿备份写回 source_path"——
    /// 后缀模式的 source 是用户没动过的原图，那样写就等于凭空造出一份覆盖。
    #[test]
    fn an_uncommitted_new_file_is_removed_without_touching_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"untouched-original").unwrap();
        let output = dir.path().join("photo_compressed.jpg");
        fs::write(&output, b"compressed").unwrap();

        let mut record = create_new(&output, "gone", 10);
        record.source_path = source.to_string_lossy().into_owned();
        // 真实事务在记账时就带着产物指纹（来自这次压缩的字节）。
        record.output_sha256 = Some(crate::source_identity::sha256_bytes(b"compressed"));
        store.prepare(&record).unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1);
        assert!(!output.exists(), "历史里没有这条记录，产物不该留在磁盘上");
        assert_eq!(fs::read(&source).unwrap(), b"untouched-original");
        assert!(!store.has_pending());
    }

    /// 历史已经记下了：只是删日志前崩了，产物归用户，动它一下都是毁数据。
    #[test]
    fn a_committed_new_file_keeps_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"original").unwrap();
        let output = dir.path().join("photo_compressed.jpg");
        fs::write(&output, b"compressed").unwrap();
        let backup = history.ensure_backup(&source).expect("backup");
        let entry = crate::history::sample_entry(&source, &backup, "committed-new");
        history.add(entry).unwrap();

        let mut record = create_new(&output, "committed-new", 10);
        record.source_path = source.to_string_lossy().into_owned();
        store.prepare(&record).unwrap();

        assert_eq!(store.recover(&history).committed, 1);
        assert!(output.exists(), "已提交的事务不许动用户文件");
        assert!(!store.has_pending());
    }

    /// 那个位置已经换成了别人的内容（体积对不上）：宁可留下一个来历不明的文件，
    /// 也不许拿它当自己的半成品删掉 —— 删用户文件永远不是恢复的选项。
    #[test]
    fn a_new_file_that_changed_size_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let output = dir.path().join("photo_compressed.jpg");
        fs::write(&output, b"somebody-else-wrote-this-and-it-is-longer").unwrap();

        let mut record = create_new(&output, "gone", 10);
        record.source_path = dir.path().join("photo.jpg").to_string_lossy().into_owned();
        store.prepare(&record).unwrap();

        assert_eq!(store.recover(&history).rolled_back, 1);
        assert!(output.exists(), "体积不是这次承诺的那个，就不是这次生成的");
    }

    /// 崩在"占好名字"与"改名"之间：磁盘上是 0 字节占位，同样归这次事务清理。
    #[test]
    fn an_empty_placeholder_from_a_crash_before_publish_is_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let output = dir.path().join("photo_compressed.jpg");
        fs::write(&output, b"").unwrap();

        let mut record = create_new(&output, "gone", 10);
        record.source_path = dir.path().join("photo.jpg").to_string_lossy().into_owned();
        // 真实事务在记账时带着认领占位的 (dev,ino) —— 空占位凭这个凭证清账。
        record.placeholder_identity = file_identity(&output);
        store.prepare(&record).unwrap();

        store.recover(&history);
        assert!(!output.exists(), "空占位凭身份凭证清账，没人在乎但它得走");
    }

    /// 上一版写的日志里没有 `mode`：那些事务只可能来自覆盖模式，必须按覆盖模式恢复，
    /// 否则老现场会被当成"只删新文件"，用户原图就再也回不来了（AGENTS 规则 10）。
    #[test]
    fn a_journal_without_the_mode_field_still_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        fs::create_dir_all(&store.root).unwrap();
        let source = dir.path().join("a.png");
        fs::write(&source, b"compressed").unwrap();
        let backup = dir.path().join("original.png");
        fs::write(&backup, b"true-original").unwrap();

        store
            .prepare_durable(&v2_txn(&source, &backup, "legacy"))
            .expect("旧日志必须能按 durable 写");
        // 模拟上一版的记账文件：没有 mode 这个字段。
        let path = store.path_of("t-legacy");
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        raw.as_object_mut().unwrap().remove("mode");
        fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

        let loaded = store.list();
        assert_eq!(loaded.len(), 1, "少了 mode 的日志不该被当成损坏现场");
        assert_eq!(loaded[0].mode, TransactionMode::ReplaceExisting);

        assert_eq!(store.recover(&history).rolled_back, 1);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
    }

    /// §2.5 启动恢复的第一道身份门：源文件已经被外部改过（既不是这次写出的产物，
    /// 也不是没动过的原图）时，绝不拿备份盖回去 —— 备份与事务日志都留着交还用户。
    #[test]
    fn rollback_refuses_to_clobber_an_externally_modified_source() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"original-bytes").unwrap();
        let backup = dir.path().join("backup").join("photo.jpg");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"original-bytes").unwrap();

        let mut record = txn(&source, &backup, "external-edit");
        record.original_sha256 = Some(crate::source_identity::sha256_of(&source).unwrap());
        record.output_sha256 = Some(crate::source_identity::sha256_bytes(b"compressed-bytes"));
        store.prepare(&record).unwrap();
        // 崩溃之后、应用重启之前，用户（或别的程序）改写了源文件。
        fs::write(&source, b"user-edited-content").unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 0, "证明不了归属就不得回滚");
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("外部修改"), "{}", report.warnings[0]);
        assert_eq!(
            fs::read(&source).unwrap(),
            b"user-edited-content",
            "外部修改必须原样保留"
        );
        assert!(backup.exists(), "唯一的原图副本不许动");
        assert!(store.has_pending(), "事务日志必须留着，等下一次启动再试");
    }

    /// §2.5：源位置上就是这次写出的字节（rename 之后、历史落盘之前崩溃）→ 安全回滚成原图。
    #[test]
    fn rollback_restores_the_original_when_the_source_is_the_staged_output() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"original-bytes").unwrap();
        let backup = dir.path().join("backup").join("photo.jpg");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"original-bytes").unwrap();

        let mut record = txn(&source, &backup, "half-committed");
        record.original_sha256 = Some(crate::source_identity::sha256_bytes(b"original-bytes"));
        record.output_sha256 = Some(crate::source_identity::sha256_bytes(b"compressed-bytes"));
        store.prepare(&record).unwrap();
        fs::write(&source, b"compressed-bytes").unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1, "产物身份命中 → 回滚");
        assert!(report.warnings.is_empty(), "{}", report.warnings.join(" / "));
        assert_eq!(fs::read(&source).unwrap(), b"original-bytes");
        assert!(!store.has_pending(), "回滚完成的事务必须销账");
    }

    /// §2.5：源文件还是没动过的原图（rename 之前崩溃）→ 不做无谓的覆盖，只清自己的残骸。
    #[test]
    fn rollback_skips_the_rewrite_when_the_source_is_still_the_pristine_original() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.jpg");
        fs::write(&source, b"original-bytes").unwrap();
        let backup = dir.path().join("backup").join("photo.jpg");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"original-bytes").unwrap();
        let temp = dir.path().join(".sizqo-write-x.tmp");
        fs::write(&temp, b"half-written").unwrap();

        let mut record = txn(&source, &backup, "pre-rename");
        record.original_sha256 = Some(crate::source_identity::sha256_bytes(b"original-bytes"));
        record.output_sha256 = Some(crate::source_identity::sha256_bytes(b"compressed-bytes"));
        record.temp_output_path = Some(temp.to_string_lossy().into_owned());
        // 生产路径记账时就带上临时名的认领凭证（`StagedOutput::identity()`）——
        // 归属门靠它判定"这条路径还是不是我们独占认领的那个文件"。
        // 没有凭证的老日志不会走到"清残骸"这条承诺上（见
        // `temp_cleanup_only_deletes_bytes_it_can_prove_ours`）。
        record.temp_identity = file_identity(&temp);
        store.prepare(&record).unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1, "原图还在原位 → 只清残骸、照常销账");
        assert!(report.warnings.is_empty(), "{}", report.warnings.join(" / "));
        assert_eq!(
            fs::read(&source).unwrap(),
            b"original-bytes",
            "原图一个字节都不该被重写"
        );
        assert!(!temp.exists(), "临时残骸要清掉");
        assert!(!store.has_pending());
    }

    /// 跨格式回滚只删"能证明是这次生成的"那个产物：内容指纹命中才删，别人占的位置不动。
    #[test]
    fn a_cross_format_output_is_only_removed_when_its_content_matches() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.png");
        fs::write(&source, b"original-bytes").unwrap();
        let backup = dir.path().join("backup").join("photo.png");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"original-bytes").unwrap();
        let output = dir.path().join("photo.webp");
        fs::write(&output, b"compressed-bytes").unwrap();

        let mut record = txn(&source, &backup, "cross-format");
        record.output_path = output.to_string_lossy().into_owned();
        record.cross_format = true;
        record.original_sha256 = Some(crate::source_identity::sha256_bytes(b"original-bytes"));
        record.output_sha256 = Some(crate::source_identity::sha256_bytes(b"compressed-bytes"));
        store.prepare(&record).unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1, "原图还在原位 → 清掉自己的产物");
        assert!(!output.exists(), "产物内容对得上，就是这次生成的");
        assert_eq!(fs::read(&source).unwrap(), b"original-bytes");

        // 同一套身份，但产物已被外部换成别的内容：不许删。
        fs::write(&output, b"someone-elses-file").unwrap();
        store.prepare(&record).unwrap();
        store.recover(&history);
        assert_eq!(
            fs::read(&output).unwrap(),
            b"someone-elses-file",
            "来历不明的内容不许当成自己的残骸删掉"
        );
    }

    /// 老日志（没有身份哈希）维持升级前的恢复契约：一律回滚，产物照原样清掉。
    /// "证明不了归属就不动手"这道强身份门只对带指纹的新日志生效。
    #[test]
    fn legacy_logs_are_identified_as_far_as_possible_and_never_clobber_unknown_content() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.jpg");
        let backup = dir.path().join("backup").join("photo.jpg");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"true-original").unwrap();

        // §3.4 能证明的先做：现场源文件还是原图字节（= 备份字节）→
        // 什么都不用盖，只清产物残骸，不算一次"回滚覆盖"。
        fs::write(&source, b"true-original").unwrap();
        let output = dir.path().join("photo.webp");
        fs::write(&output, b"converted").unwrap();
        let mut record = txn(&source, &backup, "legacy-pristine");
        record.output_path = output.to_string_lossy().into_owned();
        record.cross_format = true;
        record.expected_output_size = b"converted".len() as u64;
        store.prepare(&record).unwrap();
        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1, "原图仍在原位：只清产物，不重复写回");
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!output.exists(), "大小对得上的产物照清");

        // 源文件是用户在崩溃后写入的内容：旧契约"一律回滚"会把用户的新内容
        // 盖掉 —— 新契约保留现场（源文件、备份、事务日志），交人工恢复。
        fs::write(&source, b"user-edited-content").unwrap();
        let record = txn(&source, &backup, "legacy-unknown");
        store.prepare(&record).unwrap();
        let report = store.recover(&history);
        assert_eq!(report.needs_review, 1, "无法证明归属：必须交人工恢复");
        assert_eq!(fs::read(&source).unwrap(), b"user-edited-content");
        assert_eq!(fs::read(&backup).unwrap(), b"true-original");
        assert!(store.has_pending(), "事务日志必须留在现场");

        // 跨格式的老日志：大小对不上的"产物"绝不动 —— 那可能是别人的文件。
        let output = dir.path().join("photo.webp");
        fs::write(&output, b"someone-elses-file").unwrap();
        let mut record = txn(&source, &backup, "legacy-cross");
        record.output_path = output.to_string_lossy().into_owned();
        record.cross_format = true;
        store.prepare(&record).unwrap();
        store.recover(&history);
        assert!(
            output.exists(),
            "证明不了是我们产物的文件一个字节都不动"
        );
    }

    /// §2.5 身份门在新建模式清账上的对称面：同大小、不同内容的"产物"绝不是
    /// 我们的半成品 —— 恢复时保留现场，绝不凭体积删除。
    #[test]
    fn retire_never_deletes_a_same_size_file_whose_content_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let output = dir.path().join("photo_compressed.jpg");
        let mut record = create_new(&output, "same-size-edit", 10);
        record.output_sha256 = Some(crate::source_identity::sha256_bytes(b"0123456789"));
        store.prepare(&record).unwrap();
        // 崩溃后用户把输出文件换成了同样 10 字节的别的内容。
        fs::write(&output, b"USER EDITS").unwrap();

        store.recover(&history);
        assert_eq!(
            fs::read(&output).unwrap(),
            b"USER EDITS",
            "同大小不同内容 = 别人的文件，绝不能删"
        );

        // 内容对得上指纹的才归这次事务，照删。
        fs::write(&output, b"0123456789").unwrap();
        store.prepare(&record).unwrap();
        store.recover(&history);
        assert!(!output.exists(), "内容指纹命中才是自己的产物");
        let _ = history;
    }

    /// §4：读不懂的事务日志也算未完成事务 —— 启动恢复给出明确警告，
    /// hasPending 为真，任何"未引用备份"的清理都必须把它当作未完成事务对待。
    #[test]
    fn an_unparsable_journal_counts_as_pending_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        fs::create_dir_all(&store.root).unwrap();
        fs::write(store.root.join("broken.json"), "{ half a journal").unwrap();

        assert!(store.has_pending(), "损坏日志必须挡住一切依赖 has_pending 的清理");
        let report = store.recover(&history);
        assert!(report.warnings.iter().any(|w| w.contains("无法解析")), "{}", report.warnings.join(" / "));
        assert!(store.has_pending(), "警告之后现场仍在：hasPending 保持为真");
    }

    // MARK: - §28 P0-1 跨格式替换：删掉原图时的归属证明

    /// 目录里的隔离名现场。正常提交该是空 —— 残留就等于给用户留了一个看不懂的孤儿文件。
    fn quarantine_residue(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".sizqo-retire-")
                {
                    found.push(entry.path());
                }
            }
        }
        found.sort();
        found
    }

    /// 目录里现在躺着的每一串字节。跨平台断言用它写：只要求"谁的字节都还在"，
    /// 不要求在哪条路径上 —— 能否原子交换决定了放回的是原位还是隔离名。
    fn bytes_on_disk(dir: &Path) -> Vec<Vec<u8>> {
        let mut all = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(bytes) = fs::read(entry.path()) {
                    all.push(bytes);
                }
            }
        }
        all
    }

    fn holds(dir: &Path, needle: &[u8]) -> bool {
        bytes_on_disk(dir).iter().any(|bytes| bytes == needle)
    }

    /// P0-1 直证：复核不过 = **根本不进删除**。
    /// RC.3 的写法是"记一条已保留原文件的警告，然后照删"，用户刚改过的原图就此消失。
    #[test]
    fn cross_format_retire_refuses_to_delete_a_file_the_user_changed() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        fs::write(&original, b"true-original").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();

        // 用户在编码期间把这张图换成了另一份字节。
        fs::write(&original, b"USER VERSION").unwrap();

        let outcome = retire_original(&original, &baseline);
        assert!(outcome.is_err(), "身份不匹配绝不能进入删除: {outcome:?}");
        assert_eq!(
            fs::read(&original).unwrap(),
            b"USER VERSION",
            "用户当前版本必须原样留着"
        );
        assert!(
            quarantine_residue(dir.path()).is_empty(),
            "拒绝删除不许留下隔离名孤儿"
        );
    }

    /// 正常路径：身份一路对得上 → 原图让位给产物，且隔离名一起带走。
    #[test]
    fn an_unmodified_original_is_retired_without_leaving_quarantine_names() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        fs::write(&original, b"true-original").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();

        retire_original(&original, &baseline).expect("身份一致就该完成跨格式替换");
        assert!(!original.exists(), "跨格式替换的语义就是原图让位给产物");
        assert!(
            quarantine_residue(dir.path()).is_empty(),
            "删除成功必须把隔离名一起带走"
        );
    }

    /// 原图已经不在这个位置上：报"已保留"是假话，但更不许报成"删掉了"。
    #[test]
    fn an_already_missing_original_is_reported_instead_of_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        fs::write(&original, b"true-original").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();
        fs::remove_file(&original).unwrap();

        let outcome = retire_original(&original, &baseline);
        assert!(outcome.is_err(), "文件没了要说实话: {outcome:?}");
        assert!(quarantine_residue(dir.path()).is_empty());
    }

    /// 故障注入 ①：改名进隔离名之后，仍持有 fd 的写者继续往这份字节里写。
    /// 复验必须看出"隔离出来的已经不是刚才那份"，把最新字节放回原位并拒绝删除。
    #[test]
    fn a_writer_holding_the_fd_open_does_not_lose_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        // 与注入内容同尺寸：证明这道门不是只靠体积差异蒙对的。
        fs::write(&original, b"true-original").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();

        set_fault_in_retire_window(1);
        let outcome = retire_original(&original, &baseline);
        set_fault_in_retire_window(0);

        assert!(outcome.is_err(), "复验不过必须拒绝删除: {outcome:?}");
        assert_eq!(
            fs::read(&original).unwrap(),
            b"still-writing",
            "写者的最新字节放回原位，一个都不许丢"
        );
        assert!(quarantine_residue(dir.path()).is_empty());
    }

    /// 故障注入 ②：隔离窗口里外部在原位置放进了新版本。
    /// 这条路径上被删的只有**我们已经证明得了归属**的那份字节（隔离名里的原图），
    /// 外部那份一个字节都不动 —— RC.3 的"复核 + 无条件 remove_file"删的正是它。
    #[test]
    fn an_external_file_reoccupying_the_original_path_is_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        fs::write(&original, b"true-original").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();

        set_fault_in_retire_window(2);
        let outcome = retire_original(&original, &baseline);
        set_fault_in_retire_window(0);

        assert!(outcome.is_ok(), "删的是隔离名里那份已复验的原图: {outcome:?}");
        assert_eq!(
            fs::read(&original).unwrap(),
            b"external-new-version",
            "原位上外部放进去的那份不许被顶掉，也不许被当原图删掉"
        );
        assert!(quarantine_residue(dir.path()).is_empty());
    }

    /// 故障注入 ③：字节被改写 **且** 原位被外部占了 —— 放回原图时只有原子交换
    /// 能保证「放回」不等于「盖掉别人的文件」。两份字节都得活在磁盘上，
    /// 而活着的两条路径都不许是"我们假装删干净了"。
    #[test]
    fn restoring_the_original_never_clobbers_a_file_dropped_into_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        fs::write(&original, b"true-original").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();

        set_fault_in_retire_window(3);
        let outcome = retire_original(&original, &baseline);
        set_fault_in_retire_window(0);

        assert!(outcome.is_err(), "复验不过必须拒绝删除: {outcome:?}");
        assert!(
            holds(dir.path(), b"still-writing"),
            "写者的最新字节不许丢: {:?}",
            bytes_on_disk(dir.path())
        );
        assert!(
            holds(dir.path(), b"external-new-version"),
            "外部放进去的那份不许丢: {:?}",
            bytes_on_disk(dir.path())
        );
        assert!(
            !outcome.unwrap_err().contains("已删除"),
            "拒绝的现场里绝不该出现「删掉了」这种话"
        );
    }

    /// 临时路径的归属门（P0-2 在 Rust 侧的同一条依据）：四种现场各判一次。
    #[test]
    fn temp_cleanup_only_deletes_bytes_it_can_prove_ours() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join(".sizqo-write-1.tmp");
        let marker = protected_marker_path(&temp);
        let mut record = txn(&temp, &dir.path().join("backup"), "temp-gate");
        record.temp_output_path = Some(temp.to_string_lossy().into_owned());
        record.output_sha256 = Some(crate::source_identity::sha256_bytes(b"our-candidate"));

        // ① 无 inode 凭证、内容也不是承诺的产物 → 别人的文件，留着。
        fs::write(&temp, b"someone-else").unwrap();
        remove_temp(&record);
        assert_eq!(fs::read(&temp).unwrap(), b"someone-else");

        // ② 内容正是承诺的产物 → 我们写的半成品，照删。
        fs::write(&temp, b"our-candidate").unwrap();
        remove_temp(&record);
        assert!(!temp.exists(), "内容指纹命中才是自己的候选");

        // ③ 记账时认领的 `(dev,ino)` 对得上 → 还是那个独占认领来的文件，
        //    哪怕字节还没写完（拿不到 inode 的平台退化成"证明不了就留着"）。
        fs::write(&temp, b"half-written").unwrap();
        let mut record_with_inode = record.clone();
        record_with_inode.temp_identity = file_identity(&temp);
        record_with_inode.output_sha256 = Some(crate::source_identity::sha256_bytes(b"different"));
        remove_temp(&record_with_inode);
        if record_with_inode.temp_identity.is_some() {
            assert!(!temp.exists(), "inode 凭证命中 = 我们的临时文件");
        } else {
            assert_eq!(fs::read(&temp).unwrap(), b"half-written", "无凭证可依时一律保留");
        }

        // ④ 保护标记是"这里的字节属于外部"的显式声明，压过一切归属推断。
        fs::write(&temp, b"our-candidate").unwrap();
        fs::write(&marker, now_marker()).unwrap();
        let mut protected = record.clone();
        protected.temp_identity = file_identity(&temp);
        remove_temp(&protected);
        assert_eq!(
            fs::read(&temp).unwrap(),
            b"our-candidate",
            "带标记的现场一个字节都不许动"
        );
        assert!(marker.exists(), "标记没被顺手删掉：现场还在被声明保护");
    }

    /// §28 交换失败现场：保护标记写失败必须**报出来**，不许和"写成了"混成同一种结果。
    /// RC.3 的 `let _ = fs::write(...)` 就是在这里吞掉错误，下一条清理入口随即把
    /// "没有标记"当成"没有外部内容"，将停在临时路径上的用户字节当半成品删掉。
    /// 标记从此只是第一依据，兜底的是记账时落盘的 inode 凭证（上一测的③/④）。
    #[test]
    fn a_protected_marker_reports_whether_it_was_actually_written() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join(".sizqo-write-2.tmp");
        fs::write(&temp, b"user-content-after-failed-swapback").unwrap();

        write_protected_marker(&temp).expect("可写目录里标记必须写得上");
        assert!(temp_is_protected(&temp), "写成的标记要能被清理入口认出来");

        // 标记名字被一个目录占住 → `fs::write` 必失败，且必须回报错误而不是静默通过。
        let blocked = dir.path().join(".sizqo-write-3.tmp");
        fs::write(&blocked, b"x").unwrap();
        fs::create_dir(protected_marker_path(&blocked)).unwrap();
        let detail = write_protected_marker(&blocked)
            .expect_err("标记写不进时必须报错，不许假装写成了");
        assert!(!detail.is_empty(), "失败原因要带出去给日志用");
    }

    // MARK: - §28 P1-4 没有原子交换时的覆盖路径

    /// 占名门：目标位上已经有外部的文件时，「装候选」只能是拒绝，不能是覆盖。
    /// Windows 的 `fs::rename` 会 REPLACE_EXISTING —— 这里用硬链接把它变成原子判空。
    #[test]
    fn installing_a_candidate_never_overwrites_a_foreign_file_at_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate.tmp");
        let target = dir.path().join("photo.png");
        fs::write(&candidate, b"compressed-bytes").unwrap();
        fs::write(&target, b"external-version").unwrap();

        let outcome = install_at_free_path(&candidate, &target);
        assert!(outcome.is_err(), "目标被外部占了必须拒绝: {outcome:?}");
        assert_eq!(fs::read(&target).unwrap(), b"external-version", "别人的一份字节不动");
        assert_eq!(
            fs::read(&candidate).unwrap(),
            b"compressed-bytes",
            "候选也还在，调用方自己收尾"
        );
    }

    /// 空位则装得上，而且不留第二个名字（硬链接成功后收回候选旧名）。
    #[test]
    fn installing_a_candidate_into_a_free_slot_leaves_exactly_one_name() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate.tmp");
        let target = dir.path().join("photo.png");
        fs::write(&candidate, b"compressed-bytes").unwrap();

        install_at_free_path(&candidate, &target).expect("空位必须装得上");
        assert_eq!(fs::read(&target).unwrap(), b"compressed-bytes");
        assert!(!candidate.exists(), "候选那个名字必须随发布一起收走");
    }

    /// 隔离改名发布的正常路径：目标进隔离名 → 复验通过 → 候选进原位 → 隔离名带走。
    /// 这条走的正是 Windows 与"文件系统不支持 exchange"的分支，不许因为本机
    /// 有原子交换就当成没测过的代码。
    #[test]
    fn quarantine_publish_commits_and_leaves_no_names_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.png");
        let staged = dir.path().join("candidate.tmp");
        fs::write(&target, b"true-original").unwrap();
        fs::write(&staged, b"compressed-bytes").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&target).unwrap();

        publish_replace_via_quarantine(&staged, &target, &baseline)
            .expect("身份一致时隔离改名发布必须成立");
        assert_eq!(fs::read(&target).unwrap(), b"compressed-bytes");
        assert!(!staged.exists(), "候选的名字已改名进目标位");
        assert!(
            quarantine_residue(dir.path()).is_empty(),
            "被换出的原图交给备份恢复，隔离名不留"
        );
    }

    /// 隔离发布遇到基线不符：目标位上的字节已经不是这次编码所依据的那份 ——
    /// 拒绝，且那份字节留在原位。旧写法（`fingerprint_intact` + `fs::rename`）
    /// 恰恰会把候选盖到它身上。
    #[test]
    fn quarantine_publish_refuses_when_the_target_is_not_the_baseline_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.png");
        let staged = dir.path().join("candidate.tmp");
        fs::write(&target, b"true-original").unwrap();
        fs::write(&staged, b"compressed-bytes").unwrap();
        let stale = crate::source_identity::SourceIdentity::read_with_hash(&target).unwrap();
        fs::write(&target, b"external-new").unwrap();

        let error = publish_replace_via_quarantine(&staged, &target, &stale)
            .expect_err("基线不匹配必须拒绝提交");
        assert!(!error.swapped, "拒绝的现场里覆盖没有发生");
        assert_eq!(
            fs::read(&target).unwrap(),
            b"external-new",
            "外部那份原位留着，等它的主人"
        );
        assert_eq!(fs::read(&staged).unwrap(), b"compressed-bytes", "候选由调用方收尾");
    }

    /// P1-4 的竞态窗口本体：目标改名进隔离名、复验**通过**之后，外部把空出来的
    /// 原位占了。旧写法就在这里 rename 覆盖；现在「装候选」要求空位，装不进去
    /// 就把原图放回原位，而且两份字节一个都不许少。
    #[test]
    fn quarantine_publish_refuses_to_install_over_a_file_dropped_into_the_free_slot() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.png");
        let staged = dir.path().join("candidate.tmp");
        fs::write(&target, b"true-original").unwrap();
        fs::write(&staged, b"compressed-bytes").unwrap();
        let baseline = crate::source_identity::SourceIdentity::read_with_hash(&target).unwrap();

        set_fault_in_install_window(1);
        let outcome = publish_replace_via_quarantine(&staged, &target, &baseline);
        set_fault_in_install_window(0);

        let error = outcome.expect_err("原位被外部占了必须拒绝提交");
        assert!(!error.swapped, "拒绝的现场里覆盖没有发生");
        assert!(
            holds(dir.path(), b"external-in-window"),
            "外部占位的那份一个字节都不动: {:?}",
            bytes_on_disk(dir.path())
        );
        assert!(
            holds(dir.path(), b"true-original"),
            "被隔离出来的原图必须还活着: {:?}",
            bytes_on_disk(dir.path())
        );
        assert_eq!(fs::read(&staged).unwrap(), b"compressed-bytes", "候选也还在");
    }

    // MARK: - 无覆盖改名：三级台阶每一级都要被真的跑到

    /// 第一级（原生无覆盖改名）不可用时，硬链接那一级必须接得住：
    /// 目标保持空位就装得上，而且候选的旧名随发布一起收走。
    #[test]
    fn installing_a_candidate_falls_back_to_the_hard_link_rung() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate.tmp");
        let target = dir.path().join("photo.png");
        fs::write(&candidate, b"compressed-bytes").unwrap();

        set_forced_unsupported(ForcePrimitive::NoReplaceRename, true);
        let outcome = install_at_free_path(&candidate, &target);
        set_forced_unsupported(ForcePrimitive::NoReplaceRename, false);

        outcome.expect("空位必须装得上（硬链接那一级）");
        assert_eq!(fs::read(&target).unwrap(), b"compressed-bytes");
        assert!(!candidate.exists(), "候选旧名必须随发布收走");
    }

    /// 两级改名原语都没有时（exFAT 这类既不让硬链接、也不认 RENAME_EXCL 的盘），
    /// 最后一级是 O_EXCL 占位：覆盖对象从"任何人"缩小到"我们刚建的那个空文件"。
    /// 上一轮在这里写的是「exists() 查空 + fs::rename」，就是那条被报的竞态。
    #[test]
    fn installing_a_candidate_falls_back_to_an_exclusive_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate.tmp");
        let target = dir.path().join("photo.png");
        fs::write(&candidate, b"compressed-bytes").unwrap();

        set_forced_unsupported(ForcePrimitive::NoReplaceRename, true);
        set_forced_unsupported(ForcePrimitive::HardLink, true);
        let outcome = install_at_free_path(&candidate, &target);
        set_forced_unsupported(ForcePrimitive::NoReplaceRename, false);
        set_forced_unsupported(ForcePrimitive::HardLink, false);

        outcome.expect("占位那一级必须装得上");
        assert_eq!(fs::read(&target).unwrap(), b"compressed-bytes");
        assert!(!candidate.exists(), "候选旧名必须随发布收走");
    }

    /// 占位那一级也不许覆盖：目标位已经被别人的文件占住时，O_EXCL 必须失败，
    /// 结果只能是拒绝 —— 一级台阶都不该退化成"查空 + 覆盖式 rename"。
    #[test]
    fn the_last_install_rung_still_refuses_an_occupied_target() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate.tmp");
        let target = dir.path().join("photo.png");
        fs::write(&candidate, b"compressed-bytes").unwrap();
        fs::write(&target, b"external-version").unwrap();

        set_forced_unsupported(ForcePrimitive::NoReplaceRename, true);
        set_forced_unsupported(ForcePrimitive::HardLink, true);
        let outcome = install_at_free_path(&candidate, &target);
        set_forced_unsupported(ForcePrimitive::NoReplaceRename, false);
        set_forced_unsupported(ForcePrimitive::HardLink, false);

        assert!(outcome.is_err(), "被占住的目标只能是拒绝: {outcome:?}");
        assert_eq!(fs::read(&target).unwrap(), b"external-version", "别人的字节一个都不动");
        assert_eq!(fs::read(&candidate).unwrap(), b"compressed-bytes", "候选由调用方收尾");
    }

    /// 放回隔离出来的原图时，原位已被别人占住：**不许** rename 覆盖。
    /// 上一轮的 `restore_quarantined` 在"原位不存在"那条支路上就是「查空 + rename」，
    /// 查空与改名之间出现的文件会被直接顶掉（P1-b）。现在这条支路走三级台阶，
    /// 装不进去就转原子交换，两边字节都在。
    #[test]
    fn restoring_a_quarantined_file_never_overwrites_a_file_that_took_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        let quarantine = dir.path().join(".sizqo-retire-test.keep");
        fs::write(&quarantine, b"isolated-original").unwrap();
        // 外部在原位放进了自己的文件（原图被隔离之后才出现的那种现场）。
        fs::write(&original, b"external-took-the-slot").unwrap();

        let restored = restore_quarantined(&quarantine, &original);
        match restored {
            Restored::InPlaceForeignKept(kept) => {
                // 有原子交换（macOS / Linux）：原图回到原位，外部那份被换进隔离名。
                assert!(kept.exists(), "外部那份必须仍然存在，只是换了名字");
                assert_eq!(fs::read(&kept).unwrap(), b"external-took-the-slot");
                assert_eq!(fs::read(&original).unwrap(), b"isolated-original");
            }
            Restored::Kept(path) => {
                // 没有原子交换（Windows）：放回等于覆盖，所以一律保留 —— 原图字节留在
                // 隔离名上，原位继续归外部那份。两边都还在，一个都没丢。
                assert_eq!(fs::read(&path).unwrap(), b"isolated-original");
                assert_eq!(fs::read(&original).unwrap(), b"external-took-the-slot");
            }
            Restored::InPlace => panic!("原位被占住时绝不允许覆盖"),
        }
    }

    /// 用户上一轮问的那个极窄窗口：改名不会让外部**已持有的 fd** 失效，
    /// 所以复验通过之后照样可能有人往里追加。删除前那一眼看体积能把这类拦下来 ——
    /// 原图放回原位，追加进去的字节也一起保住，不是"验完就删"。
    #[test]
    fn a_writer_that_appends_after_verification_keeps_the_original_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("photo.png");
        fs::write(&original, b"original-bytes").unwrap();
        let identity = crate::source_identity::SourceIdentity::read_with_hash(&original).unwrap();

        set_fault_in_retire_window(4);
        let outcome = retire_original(&original, &identity);
        set_fault_in_retire_window(0);

        assert!(outcome.is_err(), "复验之后又被写入，必须在删除前拦住: {outcome:?}");
        assert!(original.exists(), "原图必须放回原位");
        let bytes = fs::read(&original).unwrap();
        assert!(
            bytes.starts_with(b"original-bytes") && bytes.ends_with(b"-late-write"),
            "原图与外部追加的字节都要在磁盘上: {bytes:?}"
        );
    }

    // MARK: - 本轮加固（docs/HARDENING_ROADMAP_2026-11.md F1-F6）的守门测试

    /// F1：O_EXCL 占位认领之后、提交之前，外部把占位删掉换成自己的文件 ——
    /// 旧写法在这里会把 rename 盖到外来文件上。现在提交前必须核出
    /// "目标位还是我们认领的那个占位"，核不出就拒绝。
    #[test]
    fn a_stolen_placeholder_refuses_the_final_rename() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("candidate.tmp");
        let target = dir.path().join("photo.png");
        fs::write(&candidate, b"compressed-bytes").unwrap();

        set_forced_unsupported(ForcePrimitive::NoReplaceRename, true);
        set_forced_unsupported(ForcePrimitive::HardLink, true);
        set_fault_in_install_window(2);
        let outcome = install_at_free_path(&candidate, &target);
        set_fault_in_install_window(0);
        set_forced_unsupported(ForcePrimitive::NoReplaceRename, false);
        set_forced_unsupported(ForcePrimitive::HardLink, false);

        let error = outcome.expect_err("占位被偷换后必须拒绝提交");
        assert!(error.contains("占位"), "拒绝理由要说明是占位被换: {error}");
        assert_eq!(
            fs::read(&target).unwrap(),
            b"external-stolen-placeholder",
            "外来字节一个都不许动"
        );
        assert_eq!(fs::read(&candidate).unwrap(), b"compressed-bytes", "候选原样留给调用方");
    }

    /// F2：隔离名占位被外部换成自己的文件时，改名过去就是覆盖 —— 凭证复核必须拦住。
    #[test]
    fn a_replaced_quarantine_placeholder_blocks_the_rename() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.png");
        fs::write(&file, b"original").unwrap();

        let claim = claim_quarantine_name(&file).unwrap();
        // 外部把占位删掉、在同一个名字上放了自己的文件。
        fs::remove_file(&claim.path).unwrap();
        fs::write(&claim.path, b"external-took-the-retire-name").unwrap();

        assert!(
            quarantined_still_ours(&claim).is_err(),
            "凭证对不上必须是拒绝"
        );
        assert_eq!(
            fs::read(&claim.path).unwrap(),
            b"external-took-the-retire-name",
            "外来字节原样"
        );
        assert_eq!(fs::read(&file).unwrap(), b"original", "原图不因隔离名被占而受影响");
        // 测试现场是外来字节，收尾直接清掉；产品路径上这里走的是拒绝分支，不碰它。
        fs::remove_file(&claim.path).unwrap();
    }

    /// F3：冲突保留名的选名与占用必须原子 —— 0 号被别人占着就跳到 1 号，
    /// 别人的字节分文不动（旧写法 exists() 选名 + rename 会盖掉窗口里被占的名字）。
    #[test]
    fn conflict_relocation_picks_the_next_free_name_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.png");
        let conflict0 = dir.path().join("photo.png.sizqo-conflict-0");
        let staged = dir.path().join(".sizqo-staged-foreign");
        fs::write(&conflict0, b"somebody-else-first").unwrap();
        fs::write(&staged, b"foreign-bytes-on-temp").unwrap();

        let relocated =
            relocate_to_conflict_name(&staged, &target).expect("必须找到下一个空位");
        assert!(
            relocated.ends_with("photo.png.sizqo-conflict-1"),
            "被占的 0 号要跳过: {relocated:?}"
        );
        assert_eq!(fs::read(&conflict0).unwrap(), b"somebody-else-first", "先来的人的字节原样");
        assert!(!staged.exists(), "外来字节整体迁走，不是复制");
        assert_eq!(fs::read(&relocated).unwrap(), b"foreign-bytes-on-temp");
    }

    /// F3 的最底级台阶：无覆盖改名不可用时，O_EXCL 占位 + 提交前身份复核
    /// 同样要跳过被占的名字装进下一个空位。
    #[test]
    fn conflict_relocation_without_no_replace_still_skips_taken_names() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.png");
        let conflict0 = dir.path().join("photo.png.sizqo-conflict-0");
        let staged = dir.path().join(".sizqo-staged-foreign");
        fs::write(&conflict0, b"somebody-else-first").unwrap();
        fs::write(&staged, b"foreign-bytes-on-temp").unwrap();

        set_forced_unsupported(ForcePrimitive::NoReplaceRename, true);
        let relocated = relocate_to_conflict_name(&staged, &target);
        set_forced_unsupported(ForcePrimitive::NoReplaceRename, false);

        let relocated = relocated.expect("台阶③也要接得住");
        assert!(relocated.ends_with("photo.png.sizqo-conflict-1"), "被占的 0 号要跳过: {relocated:?}");
        assert_eq!(fs::read(&conflict0).unwrap(), b"somebody-else-first");
        assert_eq!(fs::read(&relocated).unwrap(), b"foreign-bytes-on-temp");
    }

    /// F4：占位凭证缺失时不许退化成普通 rename —— 拒绝提交，目标位原样。
    #[test]
    fn publish_claimed_without_credentials_refuses_instead_of_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup.png");
        fs::write(&backup, b"staged-bytes").unwrap();
        let target = dir.path().join("photo.png");

        let staged = StagedOutput::copy_staged(&backup, &target).unwrap();
        let outcome = staged.publish_claimed(&target, None);
        assert!(outcome.is_err(), "凭证缺失只能是拒绝: {outcome:?}");
        assert!(!target.exists(), "拒绝后目标位不许出现任何东西");
        staged.discard();
    }

    /// F5：恢复写回的提交原语 —— 检查时刻的产物在提交前被"同大小同时间戳"
    /// 地换掉时，只有内容指纹拦得住；拒绝后外部那份字节与备份都必须原样。
    #[test]
    fn restore_write_back_refuses_a_product_replaced_after_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.png");
        fs::write(&target, b"compressed-product-18").unwrap();
        let backup = dir.path().join("backup");
        fs::write(&backup, b"true-original").unwrap();

        let expected = crate::source_identity::SourceIdentity::read_with_hash(&target).unwrap();
        // 检查之后、提交之前：同长度原地换内容，再把时间戳钉回去。
        fs::write(&target, b"compressed-product-XX").unwrap();
        crate::source_identity::SourceIdentity::freeze_mtime_to(&target, expected.modified.unwrap())
            .unwrap();

        let outcome = write_back_verified(&backup, &target, &expected);
        let error = outcome.expect_err("被换掉的产物必须拒绝写回");
        assert!(error.contains("外部修改"), "拒绝理由要对得上现场: {error}");
        assert_eq!(
            fs::read(&target).unwrap(),
            b"compressed-product-XX",
            "外部换上去的那份字节原样"
        );
        assert_eq!(fs::read(&backup).unwrap(), b"true-original", "备份原样");
    }

    /// force 撤销走 `retire_confirmed`：内容复核跳过，但隔离认领照旧，
    /// 删除只删得到那一处路径。
    #[test]
    fn retire_confirmed_deletes_without_content_review_but_not_without_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo_compressed.jpg");
        fs::write(&file, b"anything-user-confirmed-to-delete").unwrap();

        retire_confirmed(&file).unwrap();
        assert!(!file.exists(), "确认过的产物删除成立");
        assert!(
            !dir
                .path()
                .join("photo_compressed.jpg.sizqo-conflict-0")
                .exists(),
            "正常删除不产生冲突迁移现场"
        );
    }
}
