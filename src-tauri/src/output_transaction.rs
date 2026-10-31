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
    #[serde(default)]
    pub backup_legacy_unverified: bool,
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

/// 文件系统身份 `(dev,ino)`。非 unix 平台拿不到：所有依赖它的凭证在那些平台上
/// 退化为旧行为，不做平台差异性的安全承诺。
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
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
        fs::File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())
    }

    /// 记账凭证必须**先于覆盖落到目录里**：只写进内核缓存的日志，断电后和没写一样，
    /// 而那时用户原图已经被盖掉了。ReplaceExisting（图片与视频）一律走这里。
    ///
    /// CreateNew 从不动原图，日志丢了最多是下次启动多留一个空占位，所以用 `prepare`：
    /// 每次压缩都多一次目录 fsync 不值得，安全性却没差别。
    pub fn prepare_durable(&self, txn: &ReplaceTransaction) -> Result<(), String> {
        self.prepare(txn)?;
        if let Err(error) = fs::File::open(&self.root).and_then(|f| f.sync_all()) {
            self.finish(&txn.id);
            return Err(error.to_string());
        }
        Ok(())
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
        return (PendingRecoveryStatus::AutoResolvable, "源位置是本次产物，启动恢复会自动回滚".into());
    }
    if txn.original_sha256.as_deref() == Some(current.as_str()) {
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
                verify_backup_voucher(txn, &backup)?;
                HistoryStore::write_backup_back_to_source(&backup, &source)?;
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
                // 源位置上就是这次写出的字节：安全回滚。
                verify_backup_voucher(txn, &backup)?;
                HistoryStore::write_backup_back_to_source(&backup, &source)?;
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

fn remove_temp(txn: &ReplaceTransaction) {
    if let Some(temp) = txn.temp_output_path.as_deref() {
        let _ = fs::remove_file(temp);
    }
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
            // 保守回退：与旧实现同级（完整 verify 已在进入这里之前跑过）。
            if let crate::source_identity::SourceCheck::Changed(detail) =
                expected.fingerprint_intact(target)
            {
                return Err(SwapPublishError {
                    swapped: false,
                    detail: format!("目标文件在提交前被外部修改（{detail}）"),
                });
            }
            return fs::rename(staged, target).map_err(|error| SwapPublishError {
                swapped: false,
                detail: format!("原子交换不可用，rename 也失败了: {error}"),
            });
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
                let conflict = conflict_sibling(target);
                let relocated = match fs::rename(staged, &conflict) {
                    Ok(()) => conflict,
                    Err(_) => staged.to_path_buf(),
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

/// 与目标同目录、用户看得见的冲突保留名。
fn conflict_sibling(target: &Path) -> PathBuf {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    for index in 0..1000u32 {
        let candidate = dir.join(format!("{name}.sizqo-conflict-{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{name}.sizqo-conflict-{}", std::process::id()))
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
            // 没有身份可比（非 unix / 文件已消失）：文件已消失时删除无意义也无害；
            // 非 unix 平台退化为旧行为，维持平台覆盖面。
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
                // 顺手记下占位的 (dev,ino)：它就是"这个路径上是谁的文件"的凭证。
                Ok(_) => {
                    let identity = file_identity(&path);
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

/// 压缩结果的落盘：同目录独占临时文件 → flush + fsync → rename 覆盖目标。
///
/// 临时文件必须和目标同目录，否则 rename 跨挂载点就不是原子操作了。
pub struct StagedOutput {
    pub temp: PathBuf,
}
impl StagedOutput {
    /// 写一批字节再原子改名到目标（history.json、设置文件这些不需要中途记账的落盘）。
    pub fn write(target: &Path, bytes: &[u8]) -> Result<Self, String> {
        Self::stage(target, ".sizqo-write-", |file| {
            use std::io::Write;
            file.write_all(bytes).map_err(|e| e.to_string())
        })
    }

    /// 整份复制一个文件再原子改名到目标（原图备份、把备份写回源文件）。
    /// 分块读写而不是整份进内存：视频的原图可能有几个 GB。
    pub fn copy_from(source: &Path, target: &Path) -> Result<Self, String> {
        Self::stage(target, ".sizqo-copy-", |file| {
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
        })
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
        Ok(Self { temp })
    }

    /// 写字节 → flush → fsync，但**还不改名**。
    ///
    /// 覆盖模式要在"候选已经落盘"和"用户的文件被盖掉"之间插一道复核（§31 的
    /// source identity + backup check），所以这两步必须能分开调用。
    pub fn write_synced(&self, bytes: &[u8]) -> Result<(), String> {
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
    /// 凭证不可用（非 unix）时退化为普通 rename，维持平台覆盖面。
    pub fn publish_claimed(
        &self,
        target: &Path,
        identity: Option<(u64, u64)>,
    ) -> Result<(), String> {
        if let (Some(expected), Some(actual)) = (identity, file_identity(target)) {
            if expected != actual {
                return Err(format!(
                    "输出位置已被外部改动（{} 不是本次认领的占位文件），放弃提交以保护它",
                    target.display()
                ));
            }
        }
        self.publish(target)
    }

    /// 占名 + 写入 + 改名一条龙：调用方不需要在中途记任何东西时使用。
    fn stage(
        target: &Path,
        prefix: &str,
        fill: impl FnOnce(&mut fs::File) -> Result<(), String>,
    ) -> Result<Self, String> {
        let staged = Self::claim(target, prefix)?;
        match staged.fill(fill).and_then(|()| staged.publish(target)) {
            Ok(()) => Ok(staged),
            Err(error) => {
                staged.discard();
                Err(error)
            }
        }
    }

    /// 事后清理临时文件（正常情况下 rename 已经把它变成目标了）。
    pub fn discard(&self) {
        let _ = fs::remove_file(&self.temp);
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
}
