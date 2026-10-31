// Sizqo - 持久化的压缩历史与原图备份。
//
// 与旧的 `$TMPDIR/sizqo-backups` 模型的根本区别：清理只针对 Sizqo 自己
// 写的副本 —— 用户的原图和压缩结果永远不在删除范围内。
//
// 清理只有一个时机：**正常退出**。启动时一律不动备份。
//
// `retentionDays == 0`（「不保留」，默认档）：本次运行的记录连同原图备份在退出时一起
// 走；覆盖原文件前照写不误（没有备份就不许覆盖）。上次异常退出遗留的那批多给一次机会。
// `retentionDays > 0`：退出时清掉超出天数窗口的记录和只被它们引用的备份。
//
// 为什么是退出而不是"下次启动"：崩溃现场那份备份可能是被覆盖原图唯一还活着的副本，
// 而"下次启动就删"恰好会在用户最可能需要它的时候把它抹掉。
//
// 读取是**严格**的：`history.json` 存在但解析不出来时，绝不退化成"空历史"。
// 空历史会让所有备份看起来无人引用，进而被清理掉 —— 那是拿用户原图换一个不报错。
// 损坏时改名留档、按 `backup-meta.json` 重建恢复入口，并且本次启动禁止任何备份清理。

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::engine::CompressResult;
use crate::sandbox_access::FileAccess;

pub const HISTORY_DIR: &str = "history";
pub const BACKUPS_DIR: &str = "backups";
const HISTORY_FILE: &str = "history.json";
const META_FILE: &str = "backup-meta.json";
/// 上次运行是否走完了退出清理。清理只有一个时机，所以得留个记号区分"上次崩了"。
const CLEAN_EXIT_FILE: &str = "clean-exit";
pub const DAY_MILLIS: i64 = 86_400_000;
/// mtime 比文件大小更容易被无关操作扰动；容忍 2 秒以内的写入抖动。
const MTIME_TOLERANCE_MILLIS: i64 = 2_000;
/// 历史条数的硬上限：清理逻辑再怎么出错，也不能让 history.json 无限膨胀。
/// 淘汰的是最老的记录，且只连带删它自己那份备份。
pub const MAX_HISTORY_ENTRIES: usize = 10_000;
/// `history.json` 损坏时从备份重建出来的条目用的算法标记：它不是一次真实压缩。
pub const RECOVERY_ALGORITHM: &str = "recovery";

pub fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

pub fn file_mtime_millis(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .ok()
}

/// 截断重写会让崩溃留下半个 history.json；先写 tmp、fsync、再 rename。
///
/// 临时名由 `StagedOutput` 决定（同目录 + O_EXCL + 随机，§30）：history.json、
/// settings.json、备份来历记录都从这一处走，谁都不再自己拼时间戳。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::output_transaction::StagedOutput::write(path, bytes)
        .map(crate::output_transaction::StagedOutput::settle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryStatus {
    /// 压缩结果仍在，原图备份可恢复
    Compressed,
    /// 已恢复到真正原图
    Restored,
    /// §48 原图已经写回，但压缩结果清理失败。恢复本身成功，告警必须让用户看见。
    RestoredWithWarning,
    /// 压缩结果或原图位置已不存在（外部改名/删除）
    Missing,
    /// `history.json` 损坏后从备份目录的 `backup-meta.json` 重建出来的条目：
    /// 备份确实还在，但这次压缩的明细（算法、节省率）已经无从得知。
    RecoveryAvailable,
}

impl HistoryStatus {
    /// 原图已经回到用户手里：这些状态不再接受第二次"覆盖式恢复"。
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Restored | Self::RestoredWithWarning)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    #[serde(default = "default_media_kind")]
    pub media_kind: String,
    #[serde(default)]
    pub video_metadata: Option<serde_json::Value>,
    pub id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub source_path: String,
    pub output_path: Option<String>,
    pub file_name: String,
    pub output_mode: String,
    pub original_size: u64,
    pub compressed_size: u64,
    pub savings: f64,
    pub out_type: String,
    pub algorithm: String,
    /// 只有 replace 模式才有：覆盖原文件前保存的那份真正原图。
    pub backup_path: Option<String>,
    pub status: HistoryStatus,
    pub restored_at: Option<i64>,
    /// 压缩结果写入完成时的 mtime，用于检测压缩后被外部编辑器改过的文件。
    pub output_modified_at: Option<i64>,
    /// §45 §47 §73 三份内容指纹：恢复冲突判定不再只看大小和 mtime。
    /// 老记录没有这三个字段，读取时保持 `None`，恢复退回大小/mtime 判据。
    #[serde(default)]
    pub source_sha256: Option<String>,
    #[serde(default)]
    pub output_sha256: Option<String>,
    #[serde(default)]
    pub backup_sha256: Option<String>,
    /// 派生字段：读取时刷新，不落库使用。
    #[serde(default)]
    pub source_exists: bool,
    #[serde(default)]
    pub backup_exists: bool,
    /// 压缩结果此刻还在不在：历史页的「另存为 / 对比 / 删除这次压缩结果」都靠它决定，
    /// 用户手动删过产物后这些按钮就不该出现。
    #[serde(default)]
    pub output_exists: bool,
}

fn default_media_kind() -> String { "image".into() }

static ENTRY_SEQUENCE: AtomicU32 = AtomicU32::new(0);

impl HistoryEntry {
    /// 事务id 必须在覆盖开始之前就定下来：`ReplaceTransaction` 和历史记录靠它对齐，
    /// 启动时"历史里有没有这个 id"就是这次覆盖是否完整提交的唯一判据。
    pub fn new_id(source: &Path) -> String {
        let sequence = ENTRY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        format!(
            "{}-{}-{}",
            now_nanos(),
            sequence,
            HistoryStore::backup_key(source)
        )
    }

    /// 一次成功产出对应一条历史。`expires_at` 只用于展示：退出清理按 `created_at`
    /// 判定，所以同一张图重压多次时，备份的存活期顺延到最后一次相关压缩之后。
    pub fn record(
        source: &Path,
        result: &CompressResult,
        output: &Path,
        backup: Option<&Path>,
        retention_days: u32,
    ) -> Self {
        Self::record_with_id(
            Self::new_id(source),
            source,
            result,
            output,
            backup,
            retention_days,
        )
    }

    pub fn record_with_id(
        id: String,
        source: &Path,
        result: &CompressResult,
        output: &Path,
        backup: Option<&Path>,
        retention_days: u32,
    ) -> Self {
        let created_at = now_millis();
        // §47 冲突判定要能回答"当前字节还是不是压缩产物"。替换模式下源文件此时
        // 已经是产物，真正原图在 backup 里 —— source 指纹取备份那一份。
        let backup_sha256 = backup.and_then(|path| crate::source_identity::sha256_of(path).ok());
        let source_sha256 = backup_sha256
            .clone()
            .or_else(|| crate::source_identity::sha256_of(source).ok());
        let output_sha256 = crate::source_identity::sha256_of(output).ok();
        Self {
            media_kind: "image".into(),
            video_metadata: None,
            id,
            created_at,
            // 「不保留」没有"几天后到期"这回事：这份备份的寿命就是这次运行。
            expires_at: if retention_days == crate::app_settings::KEEP_UNTIL_QUIT {
                created_at
            } else {
                created_at + retention_days.max(1) as i64 * DAY_MILLIS
            },
            source_path: canonical_path(source),
            output_path: Some(output.to_string_lossy().into_owned()),
            file_name: source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "image".into()),
            output_mode: result
                .output_mode
                .clone()
                .unwrap_or_else(|| "replace".into()),
            original_size: result.original_size,
            compressed_size: result.compressed_size,
            savings: result.savings,
            out_type: result.out_type.clone(),
            algorithm: result.algorithm.clone(),
            backup_path: backup.map(|path| path.to_string_lossy().into_owned()),
            status: HistoryStatus::Compressed,
            restored_at: None,
            output_modified_at: file_mtime_millis(output),
            source_sha256,
            output_sha256,
            backup_sha256,
            source_exists: true,
            backup_exists: backup.is_some(),
            output_exists: output.exists(),
        }
    }
}

/// 每份原图备份的来历。`history.json` 一旦损坏，这份元数据就是重建恢复入口的
/// 唯一依据 —— 所以 version、大小、扩展名都必须写全，不能只留路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupMeta {
    /// 1 = 只有 sourcePath/createdAt（老版本）；2 = 带大小、mtime、扩展名；
    /// 3 = §27 的快照指纹；4 = 再带源文件的 `(dev,ino)`。
    #[serde(default = "meta_version_v1")]
    version: u32,
    source_path: String,
    created_at: i64,
    #[serde(default)]
    original_size: u64,
    #[serde(default)]
    original_modified_at: Option<i64>,
    #[serde(default)]
    original_extension: String,
    /// §27 备份本身就是"本次任务开始时的原始文件快照"：
    /// 快照当时源文件的指纹 + 快照文件自己的指纹。
    ///
    /// 两者必须相等，不等就是复制过程把字节改坏了，这份东西不配叫备份。
    /// 老记录（version < 3）里是 None：按 `LegacyUnverified` 处理 —— 不自动复用、
    /// 恢复前必须用户确认，但绝不因为"证明不了"就删掉升级用户的旧备份。
    #[serde(default)]
    source_sha256: Option<String>,
    #[serde(default)]
    backup_sha256: Option<String>,
    /// §2.2 v4：快照当时源文件的文件系统身份。恢复与复用多一道"还是那个 inode"的门。
    #[serde(default)]
    source_device: Option<u64>,
    #[serde(default)]
    source_inode: Option<u64>,
}

fn meta_version_v1() -> u32 {
    1
}

/// 一份既有备份此刻的可信程度（五态，§2.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupVerification {
    /// 记录里的两份指纹互相咬合，副本字节也和记录一致。
    Verified,
    /// 老版本留下的备份：没有内容指纹，证明不了它是否完整。
    /// 不自动复用、恢复前必须由用户明确确认；旧备份绝不因此被删除。
    LegacyUnverified,
    /// 有指纹但对不上：副本内容已经不是记录的那份字节（或记录自相矛盾）。
    ContentMismatch,
    /// 备份文件不在磁盘上。
    Missing,
    /// 副本读不出来（权限、IO 故障）。
    Unreadable,
}

/// 一次既有备份的复用判定结果。
enum ReuseDecision {
    /// 内容指纹互相咬合（或 v3 记录 + 连压链）：按可信备份复用。
    Reuse,
    /// 老备份凭历史证据补写了可信来历记录后复用。
    Upgrade { source_sha256: String },
    /// 老备份证明不了内容，但当前字节是我们自己的产物：复用是
    /// 「恢复原图回到最初原图」的唯一途径；恢复时要求用户明确确认。
    ReuseLegacy,
    /// 不可复用。旧备份原样保留，另起新快照。
    Refuse(String),
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub removed_entries: usize,
    pub removed_backups: usize,
    pub kept_backups: usize,
    /// 本次启动从备份目录重建出的恢复入口数。
    pub recovered_entries: usize,
    /// 损坏的 `history.json` 被改名留下的现场（绝对路径）。
    pub quarantined_to: Option<String>,
    pub warnings: Vec<String>,
}

/// §4.1 恢复/删除前对压缩产物的验证结论（五态，与 Swift `HistoryConflict`
/// 同一契约）。不能再把「无法证明」当成「没有冲突」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictState {
    /// 产物就是记录里那份字节。
    NoConflict,
    /// 产物被用户改过：覆盖/删除需要 force。
    ContentChanged,
    /// 旧记录没有产物指纹：退回 size+mtime 弱验证。不是冲突，
    /// 但备份侧的五态验证会另外把 legacy 备份拦到用户确认为止。
    LegacyUnverified,
    /// 产物文件已不在。
    Missing,
    /// 产物读不出来（权限/IO）：证明不了它是谁，必须按冲突拦截。
    Unreadable,
}

#[derive(Debug)]
pub enum RestoreError {
    NotFound,
    /// 没有可恢复的备份（后缀/目录模式，原图本来就没被覆盖）
    NotRestorable,
    /// 原图确实被覆盖过，但那份备份已经不在了（「不保留」档退出时清理过）
    BackupGone,
    /// 压缩之后目标文件被外部修改过，需要用户确认
    Conflict,
    /// §44 压缩会话还在跑：此时恢复会改写正在被读的源文件，还会让进度倒退
    SessionBusy,
    /// §2.3 这份备份来自旧版本，没有内容指纹：用户明确确认后才能恢复
    LegacyBackup,
    Io(String),
    /// 沙盒下拿不到（也续不上）目标位置的访问授权
    AccessDenied,
}

/// §44 会话期间拒绝恢复时的原话。前端 app.js、对比窗口与 Swift AppState 用同一句，
/// tests/history-view.cjs 与 Swift 自检钉住三处逐字一致。
pub const RESTORE_BLOCKED_MESSAGE: &str = "压缩进行中，暂不能恢复；请先停止或等这一轮结束";

impl RestoreError {
    pub fn message(&self) -> &'static str {
        match self {
            RestoreError::NotFound => "找不到这条历史记录",
            RestoreError::NotRestorable => "原图未被覆盖，无需恢复",
            // 「不保留」档退出后就会走到这里：原图确实被覆盖过，只是备份没了。
            RestoreError::BackupGone => "原图备份已清理，无法恢复",
            RestoreError::Conflict => "这个文件在压缩后又被修改过",
            RestoreError::SessionBusy => RESTORE_BLOCKED_MESSAGE,
            RestoreError::LegacyBackup => "该备份来自旧版本，缺少内容指纹",
            RestoreError::Io(_) => "恢复失败",
            RestoreError::AccessDenied => "没有该文件夹的访问权限，请重新授权",
        }
    }
}

/// §46 §48 一次恢复的完整回答：哪些历史一起收敛、是不是本来就已经恢复、有没有告警。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub history_ids: Vec<String>,
    /// 当前文件已经是原图（上次恢复成功但状态没落盘时也能收敛）。
    pub already_restored: bool,
    /// 原图已写回，但压缩结果没能删掉（§48）：成功 + 必须展示的告警。
    pub warning: Option<String>,
}

/// 内存里的历史快照 + 可信度。落盘永远是整份覆盖（写 tmp → fsync → rename），
/// 所以"缓存与磁盘不一致"只可能发生在写失败之后 —— 那时缓存不更新，
/// 内存反映的仍是磁盘上那份可信内容。
#[derive(Debug, Default)]
struct Cache {
    entries: Vec<HistoryEntry>,
    loaded: bool,
    /// `history.json` 存在但读不出可信内容（半个文件 / 无法读）：
    /// 引用关系已经不可信，本次启动**禁止 sweep 备份**。
    untrusted: bool,
}

/// 进程内共享的历史存储。所有读-改-写都在同一把锁里完成，
/// 3 个并发 worker 同时追加不会互相覆盖。
///
/// 启动时读一次 `history.json` 进内存，之后每次追加只覆盖落盘，
/// 不再"读整份 → 解析整份 → 追加 → 序列化整份"。
pub struct HistoryStore {
    root: PathBuf,
    cache: Mutex<Cache>,
    /// 备份目录的读-改-写单独一把锁：与 `cache` 无嵌套，不会互锁。
    backup_lock: Mutex<()>,
    /// 历史是"读全量 → 改 → 写全量"，所以读改写的整个过程必须独占：
    /// 否则两个并发的压缩各自拿到同一份快照，后写的那份把前一份整条抹掉。
    commit_lock: Mutex<()>,
    /// 首次加载时对损坏现场做的处理，留给启动流程报告（只报一次）。
    pending_report: Mutex<Option<CleanupReport>>,
}

impl HistoryStore {
    pub fn new(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(root.join(BACKUPS_DIR)).map_err(|e| e.to_string())?;
        Ok(Self {
            root,
            cache: Mutex::new(Cache::default()),
            backup_lock: Mutex::new(()),
            commit_lock: Mutex::new(()),
            pending_report: Mutex::new(None),
        })
    }

    fn history_path(&self) -> PathBuf {
        self.root.join(HISTORY_FILE)
    }

    fn backups_dir(&self) -> PathBuf {
        self.root.join(BACKUPS_DIR)
    }

    // ─── 读写 ────────────────────────────────────────────────────

    /// 严格读取：**"没有历史"和"历史读不出来"是两回事**。
    ///
    /// 老实现 `serde_json::from_str(&raw).unwrap_or_default()` 把半个 JSON 当成空历史，
    /// 于是下一次清理看到"没有任何记录引用这些备份"，把用户所有原图备份全删了。
    /// Err 只会来自"文件存在但内容不可信"，调用方据此关掉清理。
    fn load_from_disk(&self) -> Result<Vec<HistoryEntry>, String> {
        let raw = match fs::read_to_string(self.history_path()) {
            Ok(raw) => raw,
            // 文件不存在 = 真的没有历史，这是正常状态。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("无法读取历史记录: {error}")),
        };
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        serde_json::from_str(&raw).map_err(|error| format!("历史记录已损坏: {error}"))
    }

    /// 取内存快照（首次调用时落盘加载）。调用方必须已持有 `cache` 锁。
    fn ensure_loaded(&self, cache: &mut Cache) {
        if cache.loaded {
            return;
        }
        cache.loaded = true;
        match self.load_from_disk() {
            Ok(entries) => cache.entries = entries,
            Err(error) => {
                cache.untrusted = true;
                let mut report = CleanupReport::default();
                report.warnings.push(error);
                // 先保住现场：改名留档，绝不能让后续清理把这份文件覆盖成 []。
                match self.quarantine_history_file() {
                    Ok(path) => report.quarantined_to = Some(path.to_string_lossy().into_owned()),
                    Err(eject) => report.warnings.push(eject),
                }
                // 备份还在，就还能恢复：按 backup-meta 重建恢复入口。
                let rebuilt = self.rebuild_from_backups();
                report.recovered_entries = rebuilt.len();
                cache.entries = rebuilt;
                // 重建出来的这份必须落盘，否则重启又看到空历史。
                if let Err(write_error) = self.write_raw(&cache.entries) {
                    report.warnings.push(write_error);
                }
                *self.pending_report.lock().unwrap() = Some(report);
            }
        }
    }

    fn quarantine_history_file(&self) -> Result<PathBuf, String> {
        let path = self.history_path();
        let stamped = self
            .root
            .join(format!("history.corrupt-{}.json", now_millis()));
        fs::rename(&path, &stamped).map_err(|e| e.to_string())?;
        Ok(stamped)
    }

    fn write_raw(&self, entries: &[HistoryEntry]) -> Result<(), String> {
        let json = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
        write_atomic(&self.history_path(), json.as_bytes())
    }

    /// 覆盖磁盘写入 + 内存快照：写失败时内存保持磁盘上那份可信内容。
    fn store(&self, entries: Vec<HistoryEntry>) -> Result<(), String> {
        self.write_raw(&entries)?;
        let mut cache = self.cache.lock().unwrap();
        self.ensure_loaded(&mut cache);
        cache.entries = entries;
        Ok(())
    }

    /// 本次启动是否禁止清理无人引用的备份。
    pub fn cleanup_is_locked(&self) -> bool {
        let mut cache = self.cache.lock().unwrap();
        self.ensure_loaded(&mut cache);
        cache.untrusted
    }

    /// 取出损坏现场的处理结果（只有一份，报告完就没了）。
    /// 这里主动触发一次加载：启动流程可能在任何历史读取之前就调它。
    pub fn take_startup_report(&self) -> Option<CleanupReport> {
        {
            let mut cache = self.cache.lock().unwrap();
            self.ensure_loaded(&mut cache);
        }
        self.pending_report.lock().unwrap().take()
    }

    fn decorate(&self, mut entries: Vec<HistoryEntry>) -> Vec<HistoryEntry> {
        for entry in &mut entries {
            entry.source_exists = Path::new(&entry.source_path).exists();
            entry.backup_exists = entry
                .backup_path
                .as_ref()
                .map(|p| Path::new(p).exists())
                .unwrap_or(false);
            entry.output_exists = entry
                .output_path
                .as_ref()
                .map(|p| Path::new(p).exists())
                .unwrap_or(false);
            if !entry.source_exists && entry.status == HistoryStatus::Compressed {
                entry.status = HistoryStatus::Missing;
            }
        }
        entries
    }

    fn snapshot(&self) -> Vec<HistoryEntry> {
        let mut cache = self.cache.lock().unwrap();
        self.ensure_loaded(&mut cache);
        cache.entries.clone()
    }

    /// 最新在前，历史页直接渲染。
    pub fn list(&self) -> Vec<HistoryEntry> {
        let mut entries = self.snapshot();
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
        self.decorate(entries)
    }

    pub fn find(&self, history_id: &str) -> Option<HistoryEntry> {
        self.decorate(self.snapshot())
            .into_iter()
            .find(|entry| entry.id == history_id)
    }

    /// 事务是否已完整提交：历史里有没有这条 id。crash recovery 的唯一判据。
    ///
    /// 「已恢复」也算提交过 —— 它证明当初那条链路写完了历史，只剩日志没销。
    /// 反过来按状态排除，会让一条残留日志在每次启动都尝试用早已删除的备份回滚。
    pub fn contains_committed(&self, history_id: &str) -> bool {
        self.snapshot().iter().any(|entry| entry.id == history_id)
    }

    /// 主队列的「恢复原图」不带 id 时，按源路径找最近一条还没恢复的记录。
    pub fn find_latest_for_source(&self, source_path: &str) -> Option<HistoryEntry> {
        self.decorate(self.snapshot())
            .into_iter()
            .filter(|entry| entry.source_path == source_path && !entry.status.is_settled())
            .max_by_key(|entry| entry.created_at)
    }

    pub fn add(&self, entry: HistoryEntry) -> Result<(), String> {
        let _commit = self.commit_lock.lock().unwrap();
        let mut entries = self.snapshot();
        entries.retain(|existing| existing.id != entry.id);
        entries.push(entry);
        // 硬上限：异常设置或时间问题都不许让历史无限膨胀。淘汰的是最老的记录，
        // 备份只在"没有任何幸存记录引用它"时才连带删。
        let evicted = self.trim_to_cap(&mut entries, MAX_HISTORY_ENTRIES);
        if !evicted.is_empty() {
            let survivors = entries.clone();
            self.store(entries)?;
            self.drop_backups_of(&evicted, &survivors);
            return Ok(());
        }
        self.store(entries)
    }

    /// 只保留最近 `cap` 条，返回被淘汰的那些。
    fn trim_to_cap(&self, entries: &mut Vec<HistoryEntry>, cap: usize) -> Vec<HistoryEntry> {
        if entries.len() <= cap {
            return Vec::new();
        }
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by(|a, b| {
            entries[*a]
                .created_at
                .cmp(&entries[*b].created_at)
                .then_with(|| entries[*a].id.cmp(&entries[*b].id))
        });
        let doomed: HashSet<usize> = order.into_iter().take(entries.len() - cap).collect();
        let mut evicted = Vec::with_capacity(doomed.len());
        let mut kept = Vec::new();
        for (index, entry) in entries.drain(..).enumerate() {
            if doomed.contains(&index) {
                evicted.push(entry);
            } else {
                kept.push(entry);
            }
        }
        *entries = kept;
        evicted
    }

    /// 删掉被淘汰记录独占的备份目录。`survivors` 是还留在历史里的那些 ——
    /// 同一张图重压多次时备份是共享的，只要还有一个引用就绝不能动。
    fn drop_backups_of(&self, evicted: &[HistoryEntry], survivors: &[HistoryEntry]) {
        for stale in evicted {
            let Some(dir) = stale
                .backup_path
                .as_deref()
                .and_then(|path| Self::backup_dir_of_path(path))
            else {
                continue;
            };
            let Some(key) = backup_key_of(stale.backup_path.as_deref()) else {
                continue;
            };
            let still_used = survivors.iter().any(|other| {
                other.status != HistoryStatus::Restored
                    && backup_key_of(other.backup_path.as_deref()).as_deref() == Some(key.as_str())
            });
            if !still_used {
                let _ = fs::remove_dir_all(dir);
            }
        }
    }

    pub fn clear(&self) -> Result<CleanupReport, String> {
        let _commit = self.commit_lock.lock().unwrap();
        let entries = self.snapshot();
        let referenced: HashSet<String> = entries
            .iter()
            .filter_map(|entry| backup_key_of(entry.backup_path.as_deref()))
            .collect();
        // 先让历史不再引用任何备份，再动手删：中间崩溃只是留下孤儿，下次退出扫掉。
        self.store(Vec::new())?;
        let mut report = CleanupReport {
            removed_entries: entries.len(),
            ..Default::default()
        };
        for key in referenced {
            match fs::remove_dir_all(self.backup_dir(&key)) {
                Ok(()) => report.removed_backups += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => report.warnings.push(format!("{key}: {error}")),
            }
        }
        Ok(report)
    }

    // ─── 原图备份 ────────────────────────────────────────────────

    /// 稳定哈希：FNV-1a 64 位。同一 sourcePath 在任意次启动、任意 rustc 版本下
    /// 都落进同一个备份目录，也和 Swift 线 `backupKey(forPath:)` 完全一致。
    ///
    /// 刻意不用 `DefaultHasher` —— 标准库从不把它的算法当作持久存储格式的保证，
    /// 一次升级就能让所有已有备份变成"无人引用"，进而被退出清理删掉。
    fn stable_hash(text: &str) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in text.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }

    pub fn backup_key(path: &Path) -> String {
        Self::stable_hash(&canonical_path(path))
    }

    /// 老版本 `DefaultHasher` 的 key：只用来认已有备份，不再产生新目录。
    fn legacy_backup_key(path: &Path) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        canonical_path(path).hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// security-scoped 书签文件名候选：新 key 优先，老 key 只用于续认已有授权。
    /// 只有沙盒线（`inproc-backends`）会读它，默认构建下不参与压缩。
    #[cfg_attr(not(feature = "inproc-backends"), allow(dead_code))]
    pub fn bookmark_keys_for(path: &Path) -> [String; 2] {
        [Self::backup_key(path), Self::legacy_backup_key(path)]
    }

    fn backup_dir(&self, key: &str) -> PathBuf {
        self.backups_dir().join(key)
    }

    fn read_meta(&self, key: &str) -> Option<BackupMeta> {
        let raw = fs::read_to_string(self.backup_dir(key).join(META_FILE)).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn existing_backup_file(&self, key: &str) -> Option<PathBuf> {
        let dir = self.backup_dir(key);
        let mut names: Vec<PathBuf> = fs::read_dir(&dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("original."))
                    .unwrap_or(false)
            })
            .collect();
        names.sort();
        names.pop()
    }

    /// 为一次 replace 压缩准备原图备份（无内容指纹的调用方入口）。
    ///
    /// Err = 这份备份不可信（备份文件在、来历记录不在的半份备份等同于没有：
    /// `history.json` 一旦损坏就再也认不出它属于谁）。调用方**必须放弃这次覆盖**。
    ///
    /// 关键不变量：**已有有效备份时绝不覆盖**。同一张图连压三次，备份里永远是
    /// 第一次压缩前的真正原图，否则「恢复原图」只会回到上一版压缩结果。
    pub fn ensure_backup(&self, source: &Path) -> Result<PathBuf, String> {
        // §2.1：覆盖流程的调用方必须有内容指纹；这里代算一份（测试与内部工具用）。
        let current = crate::source_identity::sha256_of(source)
            .map_err(|error| format!("无法读取原图指纹: {error}"))?;
        self.ensure_backup_verified(source, Some(&current))
    }

    /// 与 [Self::ensure_backup] 同一入口，但复用旧备份前先核对「这次要盖掉的原图」
    /// 的指纹：两轮压缩之间用户改过原图时，旧备份已经是更早一轮的字节，复用它
    /// 会让「恢复原图」静默回滚掉用户后来的编辑 —— 指纹对不上就往后挪一个槽位
    /// 给当前字节另写一份；旧备份原样留给引用它的历史条目，绝不覆盖。
    pub fn ensure_backup_verified(
        &self,
        source: &Path,
        current_sha256: Option<&str>,
    ) -> Result<PathBuf, String> {
        let _guard = self.backup_lock.lock().unwrap();
        let canonical = PathBuf::from(canonical_path(source));
        let base_key = Self::backup_key(source);
        let extension = canonical
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .filter(|ext| !ext.is_empty() && ext.len() <= 8)
            .unwrap_or_else(|| "bin".into());

        for index in 0..32u32 {
            let key = if index == 0 {
                base_key.clone()
            } else {
                format!("{base_key}-{index}")
            };
            let dir = self.backup_dir(&key);
            match self.read_meta(&key) {
                // 命中同一源路径：按五态判定这份旧备份还能不能代表这次要盖掉的原图。
                Some(meta) if meta.source_path == canonical.to_string_lossy() => {
                    if let Some(existing) = self.existing_backup_file(&key) {
                        match self.backup_reuse_decision(Some(&meta), &existing, &canonical, current_sha256) {
                            ReuseDecision::Reuse => return Ok(existing),
                            ReuseDecision::Upgrade { source_sha256 } => {
                                if self.upgrade_legacy_meta(&key, &canonical, &extension, &source_sha256) {
                                    return Ok(existing);
                                }
                                // 升级记录写失败：按 legacy 复用，恢复时要求用户确认。
                                return Ok(existing);
                            }
                            ReuseDecision::ReuseLegacy => return Ok(existing),
                            ReuseDecision::Refuse(reason) => {
                                // 旧备份原样保留（绝不删除），换下一个槽位。
                                log::warn!("跳过旧备份槽位 {key}: {reason}");
                                continue;
                            }
                        }
                    }
                    // 备份文件被外部删掉了，重新写一份真正原图。
                }
                // 哈希撞上了别的文件，往后挪一个槽位。
                Some(_) => continue,
                None => {
                    if dir.exists() {
                        continue;
                    }
                    // 升级前用老 key 备份过的图：认那一份，不另起炉灶，
                    // 否则第二次压缩会把压缩结果当成原图存进新目录。
                    // 老备份同样过五态判定：证明不了内容就不自动当可信备份。
                    if index == 0 {
                        if let Some(legacy) = self
                            .existing_backup_file(&Self::legacy_backup_key(source))
                        {
                            match self.backup_reuse_decision(None, &legacy, &canonical, current_sha256) {
                                ReuseDecision::Reuse => return Ok(legacy),
                                ReuseDecision::Upgrade { source_sha256 } => {
                                    let legacy_key = backup_key_of(legacy.to_str())
                                        .unwrap_or_else(|| Self::legacy_backup_key(source));
                                    if self.upgrade_legacy_meta(&legacy_key, &canonical, &extension, &source_sha256) {
                                        return Ok(legacy);
                                    }
                                    return Ok(legacy);
                                }
                                ReuseDecision::ReuseLegacy => return Ok(legacy),
                                ReuseDecision::Refuse(_) => {}
                            }
                        }
                    }
                }
            }

            if fs::create_dir_all(&dir).is_err() {
                return Err("无法创建原图备份目录".into());
            }
            let backup_path = dir.join(format!("original.{extension}"));
            return self.write_verified_snapshot(
                &key, &canonical, &backup_path, &extension, current_sha256,
            );
        }
        Err("原图备份槽位已满，无法为这张图准备备份".into())
    }

    /// 在一个空槽位上写出"验证过的快照"：采集源身份 → 复制 → 验副本 → 复制后复核源
    /// → 落 v4 来历记录。任何一步失败都不留下冒充备份的目录，也不允许覆盖发生。
    fn write_verified_snapshot(
        &self,
        key: &str,
        canonical: &Path,
        backup_path: &Path,
        extension: &str,
        baseline_sha256: Option<&str>,
    ) -> Result<PathBuf, String> {
        let dir = backup_path.parent().ok_or("备份目录路径不完整")?.to_path_buf();
        // §2.2 基线核对在采集**之前**：压缩流程在捕获基线指纹之后才走到这里，
        // 中间源文件被改过的话，快照留住的就不再是"这次压缩所依据的原图" ——
        // 提交复核只看得见"文件又变回了基线"，恢复却会交出中间那份字节。
        // 任何不一致都必须在覆盖发生前终止，绝不把冒名快照存成原图。
        let pre = crate::source_identity::SourceIdentity::read_with_hash(canonical)
            .map_err(|error| format!("无法读取原图指纹: {error}"))?;
        let source_sha256 = pre.sha256.clone().expect("read_with_hash 必有内容指纹");
        if let Some(baseline) = baseline_sha256 {
            if baseline != source_sha256 {
                let _ = fs::remove_dir_all(&dir);
                return Err(
                    "原图在采集基线之后被修改，快照与本次压缩所依据的字节不一致，已跳过覆盖"
                        .into(),
                );
            }
        }
        // 先写临时名再 rename：崩溃不许留下半个 original.xxx 冒充有效备份。
        // 临时名随机（§30）：两张图同时首次备份时，固定名会互相把对方的半成品盖掉。
        if let Err(error) =
            crate::output_transaction::StagedOutput::copy_from(canonical, backup_path)
        {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!("无法保存原图备份: {error}"));
        }
        // 备份目录项必须先于 meta 落盘：断电后"meta 在、备份不在"是冒名现场。
        sync_dir(&dir).map_err(|error| {
            let _ = fs::remove_dir_all(&dir);
            format!("原图备份目录未能落盘: {error}")
        })?;
        let backup_sha256 = match crate::source_identity::sha256_of(backup_path) {
            Ok(sha256) => sha256,
            Err(error) => {
                let _ = fs::remove_dir_all(&dir);
                return Err(format!("无法读取原图备份的指纹: {error}"));
            }
        };
        if backup_sha256 != source_sha256 {
            let _ = fs::remove_dir_all(&dir);
            return Err("原图备份的字节和源文件对不上，已跳过覆盖".into());
        }
        // §9.1 测试注入：复制之后、复核之前，源文件被并发改写。
        #[cfg(test)]
        if TEST_FAULT_EDIT_SOURCE_AFTER_COPY.with(std::cell::Cell::get) {
            fs::write(canonical, b"user-edited-during-backup").unwrap();
        }
        // §2.2 复制完成后把原图整个复核一遍：复制横着一场修改的话，快照和
        // "用户此刻的原图"就不是同一份字节，这次覆盖没有可信的依据。
        let post = crate::source_identity::SourceIdentity::read_with_hash(canonical).map_err(
            |error| {
                let _ = fs::remove_dir_all(&dir);
                format!("无法复核原图指纹: {error}")
            },
        )?;
        if !pre.same_fingerprint(&post) {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!(
                "原图在备份过程中被修改（stat 指纹变了：{:?} → {:?}），已跳过覆盖",
                (pre.size, pre.modified), (post.size, post.modified)
            ));
        }
        if post.sha256.as_deref() != Some(source_sha256.as_str()) {
            let _ = fs::remove_dir_all(&dir);
            return Err("原图在备份过程中被修改（内容哈希变了），已跳过覆盖".into());
        }
        let meta = BackupMeta {
            version: 4,
            source_path: canonical.to_string_lossy().into_owned(),
            created_at: now_millis(),
            original_size: file_size(backup_path),
            original_modified_at: file_mtime_millis(canonical),
            original_extension: extension.to_owned(),
            source_sha256: Some(source_sha256),
            backup_sha256: Some(backup_sha256),
            source_device: pre.identity.map(|(dev, _)| dev),
            source_inode: pre.identity.map(|(_, ino)| ino),
        };
        let json = serde_json::to_string(&meta)
            .map_err(|error| format!("原图备份的来历记录写不成功: {error}"))?;
        if let Err(error) = write_atomic(&dir.join(META_FILE), json.as_bytes()) {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!("原图备份的来历记录写不成功: {error}"));
        }
        // meta 目录项落盘：备份文件 + 来历记录一起扛断电。
        sync_dir(&dir).map_err(|error| {
            let _ = fs::remove_dir_all(&dir);
            format!("原图备份的来历记录目录未能落盘: {error}")
        })?;
        // 备份文件 + 来历记录**都在**才算备份成功，缺一处这次覆盖就不可恢复。
        if self.existing_backup_file(key).is_none() || self.read_meta(key).is_none() {
            let _ = fs::remove_dir_all(&dir);
            return Err("原图备份不完整，已跳过覆盖".into());
        }
        Ok(backup_path.to_path_buf())
    }

    /// 复用一份既有备份前的五态判定（§2.3）。
    ///
    /// 返回 `Refuse` 时旧备份**原样保留**（绝不删除），调用方往后挪一个槽位
    /// 给当前字节另写快照。
    fn backup_reuse_decision(
        &self,
        meta: Option<&BackupMeta>,
        backup: &Path,
        canonical: &Path,
        current_sha256: Option<&str>,
    ) -> ReuseDecision {
        // 覆盖模式的调用方必须带着当前内容指纹进来；算不出指纹就不许覆盖（§2.1）。
        let Some(current) = current_sha256 else {
            return ReuseDecision::Refuse("无法计算当前文件的内容指纹".into());
        };
        let actual = match crate::source_identity::sha256_of(backup) {
            Ok(hash) => hash,
            Err(error) => return ReuseDecision::Refuse(format!("备份读不出来: {error}")),
        };
        if let Some(meta) = meta {
            if let Some(recorded) = meta.backup_sha256.as_deref() {
                if recorded != actual {
                    return ReuseDecision::Refuse("备份内容与来历记录不符".into());
                }
            }
            if let Some(recorded) = meta.source_sha256.as_deref() {
                if recorded != actual {
                    return ReuseDecision::Refuse("备份的来历记录自相矛盾".into());
                }
                if recorded == current {
                    // 快照就是这次要盖掉的那份字节：最直接的复用。
                    return ReuseDecision::Reuse;
                }
            }
        }
        // 字节不等于"这次要盖掉的原图"：只有当前字节是 Sizqo 自己上一轮的产物
        // （A→B→C 连压链）才允许复用最早的备份；用户改过的字节必须有新快照。
        if !self.replaced_bytes_are_own_product(canonical, Some(current)) {
            return ReuseDecision::Refuse("两轮之间文件被修改过，旧备份不再代表这次要盖掉的原图".into());
        }
        match meta {
            Some(meta) if meta.source_sha256.is_some() => ReuseDecision::Reuse,
            _ => {
                // 老备份：历史里还留着"某一轮开始时的原图指纹"就能升级成可信记录；
                // 什么证据都没有就按 legacy 复用，恢复时要求用户明确确认。
                if self.history_records_this_original(canonical, &actual) {
                    ReuseDecision::Upgrade { source_sha256: actual }
                } else {
                    ReuseDecision::ReuseLegacy
                }
            }
        }
    }

    /// 历史里有没有一条记录声明过"这个哈希是我某轮开始时的原图"。
    /// 老备份凭这条证据可以升级成带指纹的可信备份。
    fn history_records_this_original(&self, canonical: &Path, sha256: &str) -> bool {
        let canonical = canonical.to_string_lossy();
        self.snapshot().iter().any(|entry| {
            entry.source_path == canonical && entry.source_sha256.as_deref() == Some(sha256)
        })
    }

    /// 给一份证明过内容的老备份补写 v4 来历记录。写失败只降级成 legacy 复用，
    /// 不阻塞压缩，也不删旧备份。
    fn upgrade_legacy_meta(
        &self,
        key: &str,
        canonical: &Path,
        extension: &str,
        source_sha256: &str,
    ) -> bool {
        let Some(existing) = self.existing_backup_file(key) else {
            return false;
        };
        let meta = BackupMeta {
            version: 4,
            source_path: canonical.to_string_lossy().into_owned(),
            created_at: now_millis(),
            original_size: file_size(&existing),
            original_modified_at: None,
            original_extension: extension.to_owned(),
            source_sha256: Some(source_sha256.to_owned()),
            backup_sha256: Some(source_sha256.to_owned()),
            source_device: None,
            source_inode: None,
        };
        match serde_json::to_string(&meta) {
            Ok(json) => write_atomic(&self.backup_dir(key).join(META_FILE), json.as_bytes()).is_ok(),
            Err(_) => false,
        }
    }

    /// 覆盖之后、记历史之前最后一次确认：这份备份还能当原图用吗（§27）。
    ///
    /// `Err` = 这份东西不能当原图用。它**不删备份**：留着才能让用户自己看清是谁动过，
    /// 而且事务日志必须留在磁盘上（AGENTS 规则 9：失败的回滚保留日志与备份）。
    ///
    /// 提交时刻的备份都是刚由 `ensure_backup_verified` 写出或刚判定过的：
    /// `LegacyUnverified` 在这里放行（那是升级用户 conscious 复用的老备份，
    /// 恢复侧会另外要求确认），`Missing`/`Unreadable`/`ContentMismatch` 一律拦下。
    pub fn backup_is_faithful(&self, backup: &Path) -> Result<(), String> {
        match self.backup_verification(backup) {
            Ok(BackupVerification::Verified) | Ok(BackupVerification::LegacyUnverified) => Ok(()),
            Ok(BackupVerification::Missing) => Err("原图备份已经不在磁盘上了".into()),
            Ok(BackupVerification::ContentMismatch) => Err("备份内容与来历记录不符".into()),
            Ok(BackupVerification::Unreadable) => Err("原图备份读不出来".into()),
            Err(detail) => Err(detail),
        }
    }

    /// 一份备份的五态验证（§2.3）。恢复入口按这个结果决定
    /// 直接恢复 / 要求用户确认 / 拒绝，绝不把「无法证明」当成「没有问题」。
    pub fn backup_verification(&self, backup: &Path) -> Result<BackupVerification, String> {
        if !backup.exists() {
            return Ok(BackupVerification::Missing);
        }
        let Some(key) = backup_key_of(backup.to_str()) else {
            // 备份目录结构认不出来：证明不了任何事，按 legacy 处理。
            return Ok(BackupVerification::LegacyUnverified);
        };
        let Some(meta) = self.read_meta(&key) else {
            return Ok(BackupVerification::LegacyUnverified);
        };
        let (Some(source_sha256), Some(recorded_sha256)) =
            (meta.source_sha256.as_deref(), meta.backup_sha256.as_deref())
        else {
            // 老记录（版本 ≤ 2）没有指纹：明确标记为未验证，不再静默当可信。
            return Ok(BackupVerification::LegacyUnverified);
        };
        if source_sha256 != recorded_sha256 {
            return Err(format!(
                "备份的来历记录自相矛盾（源 {source_sha256} ≠ 副本 {recorded_sha256}）"
            ));
        }
        match crate::source_identity::sha256_of(backup) {
            Ok(actual) if actual == recorded_sha256 => Ok(BackupVerification::Verified),
            Ok(actual) => Err(format!(
                "备份文件已经不是当初那份字节（{recorded_sha256} → {actual}）"
            )),
            Err(_) => Ok(BackupVerification::Unreadable),
        }
    }

    /// 事务凭证/提交前需要备份的内容指纹时用它：优先读来历记录，
    /// 老备份没有记录就现场算一份（覆盖模式多读一遍大文件是值得的）。
    pub fn recorded_backup_sha256(&self, backup: &Path) -> Option<String> {
        let key = backup_key_of(backup.to_str())?;
        if let Some(meta) = self.read_meta(&key) {
            if let Some(recorded) = meta.backup_sha256 {
                return Some(recorded);
            }
        }
        crate::source_identity::sha256_of(backup).ok()
    }

    /// `history.json` 损坏后按备份目录重建恢复入口。
    ///
    /// 只"把文件留着但历史页看不到"不叫数据恢复 —— 用户必须看到
    /// 「检测到可恢复的原图备份」并且能一键恢复。
    fn rebuild_from_backups(&self) -> Vec<HistoryEntry> {
        let Ok(dirs) = fs::read_dir(self.backups_dir()) else {
            return Vec::new();
        };
        let now = now_millis();
        let mut rebuilt = Vec::new();
        for entry in dirs.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(key) = dir.file_name().and_then(|name| name.to_str()).map(str::to_owned)
            else {
                continue;
            };
            let Some(backup) = self.existing_backup_file(&key) else {
                continue;
            };
            let meta = self.read_meta(&key);
            // 没有来历记录就不知道这份备份是谁的图，宁可不认也不许瞎猜。
            let Some(meta) = meta else { continue };
            let source = Path::new(&meta.source_path);
            rebuilt.push(HistoryEntry {
                media_kind: if crate::video::is_video_extension(source) { "video" } else { "image" }.into(),
                video_metadata: None,
                id: format!("recovery-{key}"),
                created_at: meta.created_at,
                expires_at: now,
                source_path: meta.source_path.clone(),
                // replace 模式覆盖的就是源文件本身。
                output_path: Some(meta.source_path.clone()),
                file_name: source
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "image".into()),
                output_mode: "replace".into(),
                original_size: if meta.original_size > 0 {
                    meta.original_size
                } else {
                    file_size(&backup)
                },
                compressed_size: file_size(source),
                savings: 0.0,
                out_type: source
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.to_lowercase())
                    .unwrap_or_else(|| "bin".into()),
                algorithm: RECOVERY_ALGORITHM.into(),
                backup_path: Some(backup.to_string_lossy().into_owned()),
                status: HistoryStatus::RecoveryAvailable,
                restored_at: None,
                // 明细已经无从得知，但"此刻源文件是什么样"是量得出来的：拿它的实时
                // mtime，冲突判定才不会在每次恢复前都误报一次「压缩后又被修改过」。
                output_modified_at: file_mtime_millis(source).or(meta.original_modified_at),
                // 版本 3 的来历记录里存着真正原图的指纹：幂等恢复能认出"已经写回过"。
                // 压缩产物的指纹当年没记，保持 None，冲突判定退回大小/mtime。
                source_sha256: meta.source_sha256.clone(),
                output_sha256: None,
                backup_sha256: meta.backup_sha256.clone(),
                source_exists: true,
                backup_exists: true,
                // 重建条目的"输出"就是源文件本身，它刚被 stat 过。
                output_exists: true,
            });
        }
        rebuilt
    }

    pub fn backup_dir_of_path(backup_path: &str) -> Option<PathBuf> {
        Path::new(backup_path).parent().map(Path::to_path_buf)
    }

    // ─── 退出清理（唯一的清理时机）────────────────────────────────

    /// 正常退出时按保留档位清一次：删掉的记录写回 `history.json`，再删已经没人引用的备份。
    /// 单个删除失败只 warn，剩下的孤儿下次退出继续扫。
    ///
    /// - 按天档位（1/3/7/14/30）：`created_at` 超出窗口的记录过期。
    /// - 「不保留」（0）：**本次运行**造出的记录过期。`run_started_at` 之前那条如果是
    ///   上次异常退出留下的、备份文件还在，就再留它一次（`previous_run_ended_cleanly`
    ///   为假时）—— 那份备份可能是被覆盖原图唯一的副本，而这一次会话是用户唯一看得见
    ///   也能一键恢复它的窗口。下一次干净退出收账，不留"说不保留却永久占着磁盘"的死角。
    pub fn apply_retention_on_exit(
        &self,
        retention_days: u32,
        run_started_at: i64,
        previous_run_ended_cleanly: bool,
        protected_backup_keys: &[String],
    ) -> CleanupReport {
        let mut report = CleanupReport::default();
        let _commit = self.commit_lock.lock().unwrap();
        if self.cleanup_is_locked() {
            // 引用关系不可信的时候一个备份都不许删：这是"history.json 损坏 →
            // 所有备份变成无人引用 → 全被清掉"那条链路唯一的断点。
            report.warnings.push(
                "历史记录文件已损坏，本次退出跳过清理，原图备份全部保留".into(),
            );
            return report;
        }
        let not_retained = retention_days == crate::app_settings::KEEP_UNTIL_QUIT;
        let cutoff = now_millis() - retention_days.max(1) as i64 * DAY_MILLIS;
        let all = self.snapshot();
        let kept: Vec<HistoryEntry> = all
            .iter()
            .filter(|entry| {
                let survives = if not_retained {
                    entry.created_at < run_started_at
                        && !previous_run_ended_cleanly
                        && backup_file_is_live(entry)
                } else {
                    entry.created_at >= cutoff
                };
                if !survives {
                    report.removed_entries += 1;
                }
                survives
            })
            .cloned()
            .collect();

        // 先落账再删文件：备份没了而 `history.json` 还说备份在，就是一个点开只会报错的
        // 恢复入口；写失败时一个文件都不许动。
        if kept.len() != all.len() {
            if let Err(error) = self.store(kept.clone()) {
                report.warnings.push(format!("history.json 写入失败: {error}"));
                return report;
            }
        }

        self.sweep_unreferenced_backups(&kept, protected_backup_keys, &mut report);
        report
    }

    /// 读取并抹掉「上次运行走完了退出清理」的记号，只在启动时取一次。
    /// 崩溃 / 强杀写不下这个文件，所以"没有记号"就是上次没清账的证据。
    pub fn take_clean_exit_marker(&self) -> bool {
        let path = self.root.join(CLEAN_EXIT_FILE);
        match fs::remove_file(&path) {
            Ok(()) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => {
                log::warn!("退出记号 {path:?} 读不了，按上次没清账处理: {error}");
                false
            }
        }
    }

    /// 只有退出清理真的跑完才留记号：跳过清理时不许留下"账已结清"的假证据。
    pub fn mark_clean_exit(&self) {
        let _ = fs::write(self.root.join(CLEAN_EXIT_FILE), now_millis().to_string());
    }

    /// 删掉没有任何存活引用的备份目录。引用集 = 历史条目（含退出后仍存活的）
    /// + 调用方额外提供的保护键（未完成事务引用的备份：§2.3 清理必须同时
    /// 检查历史引用与未完成事务引用，两者都清空才允许删）。
    fn sweep_unreferenced_backups(
        &self,
        kept: &[HistoryEntry],
        protected_keys: &[String],
        report: &mut CleanupReport,
    ) {
        if self.cleanup_is_locked() {
            report.warnings.push("历史记录不可信，跳过备份清理".into());
            return;
        }
        let mut referenced: HashSet<String> = kept
            .iter()
            .filter(|entry| !entry.status.is_settled())
            .filter_map(|entry| backup_key_of(entry.backup_path.as_deref()))
            .collect();
        referenced.extend(protected_keys.iter().cloned());

        let Ok(entries) = fs::read_dir(self.backups_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                let _ = fs::remove_file(&path);
                continue;
            }
            let Some(key) = path.file_name().and_then(|name| name.to_str()).map(str::to_owned)
            else {
                continue;
            };
            if referenced.contains(&key) {
                report.kept_backups += 1;
                continue;
            }
            match fs::remove_dir_all(&path) {
                Ok(()) => report.removed_backups += 1,
                Err(error) => report
                    .warnings
                    .push(format!("备份 {key} 删除失败，留待下次清理: {error}")),
            }
        }
    }

    // ─── 恢复 ────────────────────────────────────────────────────

    fn mark_restored(&self, ids: &[String], warning: bool) -> Result<(), String> {
        let _commit = self.commit_lock.lock().unwrap();
        let stamp = now_millis();
        let mut entries = self.snapshot();
        let mut touched = false;
        for entry in &mut entries {
            if ids.contains(&entry.id) && !entry.status.is_settled() {
                entry.status = if warning {
                    HistoryStatus::RestoredWithWarning
                } else {
                    HistoryStatus::Restored
                };
                entry.restored_at = Some(stamp);
                entry.backup_path = None;
                entry.backup_exists = false;
                touched = true;
            }
        }
        if touched {
            self.store(entries)?;
        }
        Ok(())
    }

    fn remove_ids(&self, ids: &[String]) -> Result<(), String> {
        let _commit = self.commit_lock.lock().unwrap();
        let before = self.snapshot();
        let after: Vec<HistoryEntry> = before
            .iter()
            .filter(|entry| !ids.contains(&entry.id))
            .cloned()
            .collect();
        if after.len() != before.len() {
            self.store(after)?;
        }
        Ok(())
    }

    /// 同一条备份可能被多条记录引用（同一张图重压 N 次）。恢复的是那份真正原图，
    /// 所有这些记录的「已压缩」状态同时失效，必须一起标 Restored。
    fn siblings_sharing_backup(&self, entry: &HistoryEntry) -> Vec<String> {
        let Some(key) = backup_key_of(entry.backup_path.as_deref()) else {
            return vec![entry.id.clone()];
        };
        self.snapshot()
            .into_iter()
            .filter(|other| {
                !other.status.is_settled()
                    && backup_key_of(other.backup_path.as_deref()).as_deref() == Some(key.as_str())
            })
            .map(|other| other.id)
            .collect()
    }

    /// 恢复/撤销前的安全检查：压缩结果被外部改过就不要静默覆盖或删除。
    ///
    /// - replace：产物就是源文件本身，冲突 = 恢复会覆盖用户改过的字节；
    /// - 后缀/目录：原图从未动过，撤销 = 删产物，冲突 = 产物被用户编辑过
    ///   （比如在 Photoshop 里修过 `foo_compressed.jpg`），删掉就等于丢编辑。
    ///   两种模式用的是同一份指纹，判断逻辑一致。
    ///
    /// 有内容指纹时按 SHA-256 判 —— 大小和 mtime 都相同的原地编辑躲不过这一道。
    /// **指纹读不出来（Unreadable）绝不放行**：旧逻辑会退回 size+mtime，
    /// "同大小同 mtime 且读不出来"的文件就被当成了没改过 —— 正是把
    /// 「无法证明」当成「没有冲突」的那条漏洞。老记录没有指纹，退回
    /// 大小/mtime 并明确标记为弱验证（升级用户的既有记录仍然能恢复）。
    pub fn conflict_state(entry: &HistoryEntry) -> ConflictState {
        let Some(target) = entry.output_path.as_deref().or(Some(entry.source_path.as_str())) else {
            return ConflictState::NoConflict;
        };
        let target = Path::new(target);
        if !target.exists() {
            return ConflictState::Missing;
        }
        if let Some(recorded) = entry.output_sha256.as_deref() {
            return match crate::source_identity::sha256_of(target) {
                Ok(current) if recorded == current => ConflictState::NoConflict,
                Ok(_) => ConflictState::ContentChanged,
                Err(_) => ConflictState::Unreadable,
            };
        }
        if fs::metadata(target).map(|m| m.len()).unwrap_or(0) != entry.compressed_size {
            return ConflictState::ContentChanged;
        }
        match (entry.output_modified_at, file_mtime_millis(target)) {
            (Some(recorded), Some(current)) if (current - recorded).abs() > MTIME_TOLERANCE_MILLIS => {
                ConflictState::ContentChanged
            }
            (Some(_), Some(_)) => ConflictState::LegacyUnverified,
            _ => ConflictState::LegacyUnverified,
        }
    }

    pub fn has_conflict(entry: &HistoryEntry) -> bool {
        matches!(
            Self::conflict_state(entry),
            ConflictState::ContentChanged | ConflictState::Unreadable
        )
    }

    /// 把一份备份原子地写回目标位置：同目录临时文件 → fsync → rename。
    ///
    /// `restore` 和事务回滚共用这一个实现 —— 恢复原图这件事绝不允许有两套写法。
    pub fn write_backup_back_to_source(backup: &Path, target: &Path) -> Result<(), String> {
        // §30 随机临时名：恢复全部 / 事务回滚可能同时在多个文件上写回原图。
        // 写坏时 StagedOutput 自己收走临时文件，覆盖失败绝不动用户当前的目标文件。
        crate::output_transaction::StagedOutput::copy_from(backup, target)
            .map(crate::output_transaction::StagedOutput::settle)?;
        // §2.2 恢复的目录项也要落盘：断电后"历史说已恢复、源位置还是产物"
        // 比恢复失败更糟。rename 的持久化以父目录 fsync 为准。
        sync_dir(target.parent().unwrap_or(Path::new("/")))
    }

    /// 跨格式 replace（PNG → JPG）时压缩结果是个新文件，恢复后不该留下孤儿。
    /// 同格式 replace 的输出就是源文件本身，绝不许删。
    ///
    /// §4.1 审查修复：删除前必须证明这个文件**还是本次生成的产物** ——
    /// 有产物指纹按内容对账；对不上（用户改过）或读不出来时保留文件并提示。
    /// 老记录没有指纹，只删大小正好等于承诺产出的文件（有界删除）。
    /// 这道门同时护住普通恢复分支与 `already_restored` 幂等分支 ——
    /// 后者此前会无条件删除，用户编辑过的跨格式产物可能被静默清掉。
    ///
    /// §48 删除失败不是"恢复失败"：原图已经写回去了，只是压缩产物还在。
    /// 这个事实必须由调用方原样报给用户，不能吞掉。
    fn remove_generated_output(entry: &HistoryEntry) -> Result<(), String> {
        let target = PathBuf::from(&entry.source_path);
        if let Some(output) = entry.output_path.as_deref() {
            let output = Path::new(output);
            if !same_file(output, &target) && output.exists() {
                let ours = match entry.output_sha256.as_deref() {
                    Some(expected) => match crate::source_identity::sha256_of(output) {
                        Ok(actual) if actual == expected => true,
                        Ok(_) => false,
                        Err(_) => false,
                    },
                    None => {
                        fs::metadata(output).map(|meta| meta.len()).unwrap_or(0)
                            == entry.compressed_size
                            && entry.compressed_size > 0
                    }
                };
                if !ours {
                    return Ok(());
                }
                fs::remove_file(output)
                    .map_err(|error| format!("压缩文件删除失败: {error}"))?;
            }
        }
        Ok(())
    }

    /// 统一恢复入口：`restore_original` / `restore_history_entry` / `restore_all`
    /// 都走这里，避免三套逻辑对历史状态的处理不一致。
    ///
    /// `access` 负责在沙盒下临时取得目标位置的访问权（Direct 版直通），
    /// 守卫必须覆盖整个文件操作，返回即释放授权。
    ///
    /// 提交顺序是**刻意**的：写回原图 → 历史落盘 → 才删压缩输出和备份目录。
    /// 反过来（先删备份再写历史）一旦历史写失败，历史会显示「已压缩」而备份已经没了 ——
    /// 用户看到一条永远恢复不了的记录。现在最坏只留下一个没人引用的孤儿目录。
    ///
    /// §46 幂等：如果当前文件已经等于原图指纹（上次写回成功、状态没落盘），
    /// 直接收敛历史状态并回答 `AlreadyRestored`，不再要求用户 force。
    pub fn restore(
        &self,
        entry: &HistoryEntry,
        force: bool,
        accept_legacy: bool,
        access: &dyn FileAccess,
        reauth: &mut dyn FnMut() -> bool,
    ) -> Result<RestoreReport, RestoreError> {
        if entry.output_mode == "replace" {
            let target = PathBuf::from(&entry.source_path);
            let parent = target
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/"));
            // §46 上次恢复是否已经写回：内容指纹说了算，状态字段只是记账。
            let already_restored = matches!(
                (entry.source_sha256.as_deref(), crate::source_identity::sha256_of(&target).ok()),
                (Some(recorded), Some(current)) if recorded == current
            );
            if already_restored {
                let ids = self.siblings_sharing_backup(entry);
                let ids = if ids.is_empty() { vec![entry.id.clone()] } else { ids };
                self.mark_restored(&ids, false)
                    .map_err(RestoreError::Io)?;
                // 上次的清理可能也没做完：孤儿产物/备份尽力收掉，失败不影响这次收敛。
                let _ = Self::remove_generated_output(entry);
                if let Some(dir) = entry
                    .backup_path
                    .as_deref()
                    .and_then(Self::backup_dir_of_path)
                {
                    let _ = fs::remove_dir_all(dir);
                }
                return Ok(RestoreReport {
                    history_ids: ids,
                    already_restored: true,
                    warning: None,
                });
            }
            if !force && Self::has_conflict(entry) {
                return Err(RestoreError::Conflict);
            }
            let backup = entry
                .backup_path
                .as_ref()
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .ok_or(RestoreError::BackupGone)?;
            // §2.3 备份在写回之前按五态验证。`force` 只代表用户确认覆盖被修改过的
            // 压缩产物，绝不绕过备份真实性验证；legacy 备份要的是另一句明确的确认。
            match self.backup_verification(&backup) {
                Ok(BackupVerification::Verified) => {}
                Ok(BackupVerification::LegacyUnverified) if accept_legacy => {}
                Ok(BackupVerification::LegacyUnverified) => return Err(RestoreError::LegacyBackup),
                Ok(state) => {
                    return Err(RestoreError::Io(match state {
                        BackupVerification::Missing => "原图备份已经不在磁盘上了".into(),
                        BackupVerification::ContentMismatch => {
                            "备份内容与来历记录不符，已停止恢复".into()
                        }
                        _ => "原图备份读不出来，已停止恢复".into(),
                    }))
                }
                Err(detail) => return Err(RestoreError::Io(detail)),
            }
            let ids = self.siblings_sharing_backup(entry);
            let backup_dir = entry
                .backup_path
                .as_deref()
                .and_then(Self::backup_dir_of_path);
            let _guard = access
                .acquire(&parent, reauth)
                .ok_or(RestoreError::AccessDenied)?;
            Self::write_backup_back_to_source(&backup, &target)
                .map_err(RestoreError::Io)?;
            // §48 产物清理失败 = 原图恢复成功 + 告警，不是失败。
            let warning = Self::remove_generated_output(entry).err();
            drop(_guard);
            if let Err(error) = self.mark_restored(&ids, warning.is_some()) {
                // 文件已经回到原图，但状态没落盘：备份必须留着，用户重试即可收敛。
                return Err(RestoreError::Io(format!(
                    "文件已恢复，但历史记录状态保存失败（{error}），原图备份已保留"
                )));
            }
            if let Some(dir) = backup_dir {
                // 备份在 AppData 内，不需要沙盒授权。删不掉只是孤儿，下次退出扫。
                let _ = fs::remove_dir_all(dir);
            }
            Ok(RestoreReport {
                history_ids: ids,
                already_restored: false,
                warning,
            })
        } else {
            // 后缀/目录模式原图从未被改动：撤销 = 删掉这次生成的压缩结果。
            // 删之前先看产物是不是被用户改过 —— 记录里的指纹对不上就要求 force，
            // 不许把用户的编辑静默删掉（与 replace 模式同一份判据）。
            if !force && Self::has_conflict(entry) {
                return Err(RestoreError::Conflict);
            }
            let mut already_restored = false;
            if let Some(output) = entry.output_path.as_deref() {
                let path = Path::new(output);
                if path.exists() {
                    let dir = path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("/"));
                    let _guard = access
                        .acquire(&dir, reauth)
                        .ok_or(RestoreError::AccessDenied)?;
                    fs::remove_file(path).map_err(|e| RestoreError::Io(e.to_string()))?;
                } else {
                    // §46 上次删除可能成功、只是记录没删掉：再点一次照样收敛。
                    already_restored = true;
                }
            }
            self.remove_ids(&[entry.id.clone()])
                .map_err(RestoreError::Io)?;
            Ok(RestoreReport {
                history_ids: vec![entry.id.clone()],
                already_restored,
                warning: None,
            })
        }
    }
}

fn canonical_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// 两个路径指的是不是同一个文件。
///
/// 记录里的 `source_path` 是规范路径，`output_path` 是当时那个字符串 —— macOS 上
/// `/var/…` 与 `/private/var/…`、任何软链目录都会让两者字面不等。同格式 replace 的
/// 压缩输出**就是**源文件本身，判成"另一个文件"会在恢复之后把刚写回的原图删掉。
/// 目录 fsync：rename / 新建目录项只有落了**父目录**才扛得住断电。
/// 文件内容的 fsync 不能代替它 —— "事务日志在、备份目录项没了"正是
/// 进程被杀与机器断电的差别所在。
///
/// Windows 上普通 `File::open` 打不开目录句柄，必须带
/// `FILE_FLAG_BACKUP_SEMANTICS`（`OpenOptionsExt::custom_flags`）；
/// 同步失败按真错误返回，绝不静默假装成功 —— 提交链路上的失败
/// 由调用方走回滚。
pub(crate) fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("{}: {error}", path.display()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE：目录是共享的，
        // 同步用的句柄不许把并发写入者挡在门外。
        const FILE_SHARE_ALL: u32 = 0x0000_0007;
        // §28 审查修复：FlushFileBuffers 要求句柄具有写权限 —— 只读句柄即使带着
        // FILE_FLAG_BACKUP_SEMANTICS 开成功，落盘那一步也必然返回 ACCESS_DENIED。
        // RC.3 的 Windows 分支就是这么写的，所以"Windows 编译通过"从没证明过
        // 目录 fsync 真的能成。这里按原生 API 的要求开：backup semantics + 写访问。
        fs::OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("{}: {error}", path.display()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        // 未知平台：没有可信的目录同步原语，宁可如实失败也不假装成功。
        Err(format!(
            "{}: directory sync unsupported on this platform",
            path.display()
        ))
    }
}

pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    a == b || canonical_path(a) == canonical_path(b)
}

/// §9.1 仅测试使用的故障注入点：快照复制完成后、复核源文件前，把源文件改掉，
/// 模拟"备份过程横着一场并发编辑"。正式产品里这个开关不存在。
/// **thread-local**：并行测试各有各的注入位，全局静态会把故障写进
/// 别的测试正在备份的文件（真实踩过的并行 flake）。
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_FAULT_EDIT_SOURCE_AFTER_COPY: std::cell::Cell<bool>
        = const { std::cell::Cell::new(false) };
}

/// 测试用：开关"复制后改写源文件"的故障注入（只影响当前测试线程）。
#[cfg(test)]
pub(crate) fn set_fault_edit_source_after_copy(active: bool) {
    TEST_FAULT_EDIT_SOURCE_AFTER_COPY.with(|flag| flag.set(active));
}

/// 备份 key = 备份目录名；目录名 = backups/<key>/original.<ext>。
pub(crate) fn backup_key_of(backup_path: Option<&str>) -> Option<String> {
    let dir = Path::new(backup_path?).parent()?;
    dir.file_name()?.to_str().map(str::to_owned)
}

/// 备份文件此刻是否还在磁盘上。清理时用它判断"这条记录还替用户守着一份原图吗"。
fn backup_file_is_live(entry: &HistoryEntry) -> bool {
    entry
        .backup_path
        .as_ref()
        .map(|path| Path::new(path).exists())
        .unwrap_or(false)
}

impl HistoryStore {
    /// 这次要盖掉的字节是不是 Sizqo 自己上一轮的产物（历史里记着它的指纹）。
    ///
    /// 是 → 复用最早的备份：同一张图连压三次，「恢复原图」永远给真正的原图
    /// （既有语义，不许因为重压就把上一版压缩结果当成"原图"存进新槽位）。
    /// 不是 → 说明两轮之间用户自己改过文件：那份字节必须有属于自己的一份快照，
    /// 否则 replace 一提交它就再也找不回来了（谁的 bytes 都不是）。
    fn replaced_bytes_are_own_product(
        &self,
        canonical: &Path,
        current_sha256: Option<&str>,
    ) -> bool {
        let canonical = canonical.to_string_lossy();
        let size = file_size(Path::new(canonical.as_ref()));
        let mtime = file_mtime_millis(Path::new(canonical.as_ref()));
        self.snapshot().iter().any(|entry| {
            entry.source_path == canonical
                && match (current_sha256, entry.output_sha256.as_deref()) {
                    (Some(current), Some(recorded)) => recorded == current,
                    _ => entry.compressed_size == size && entry.output_modified_at == mtime,
                }
        })
    }
}

/// 只有测试用：一条指向 `source`、覆盖模式为 replace 的历史记录。
#[cfg(test)]
pub(crate) fn sample_entry(source: &Path, backup: &Path, id: &str) -> HistoryEntry {
    let created_at = now_millis();
    HistoryEntry {
        media_kind: "image".into(),
        video_metadata: None,
        id: id.into(),
        created_at,
        expires_at: created_at + DAY_MILLIS * 3,
        source_path: source.to_string_lossy().into_owned(),
        output_path: Some(source.to_string_lossy().into_owned()),
        file_name: source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into()),
        output_mode: "replace".into(),
        original_size: 100,
        compressed_size: file_size(source),
        savings: 60.0,
        out_type: "png".into(),
        algorithm: "test".into(),
        backup_path: Some(backup.to_string_lossy().into_owned()),
        status: HistoryStatus::Compressed,
        restored_at: None,
        output_modified_at: file_mtime_millis(source),
        source_sha256: crate::source_identity::sha256_of(backup).ok(),
        output_sha256: crate::source_identity::sha256_of(source).ok(),
        backup_sha256: crate::source_identity::sha256_of(backup).ok(),
        source_exists: true,
        backup_exists: true,
        output_exists: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_in(dir: &Path) -> HistoryStore {
        HistoryStore::new(dir.join("history")).unwrap()
    }

    fn sample_source(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path.canonicalize().unwrap()
    }

    /// 备份文件所在目录 = backups/<key>；用它断言备份是否还在。
    fn dir_of_backup(backup: &Path) -> PathBuf {
        backup.parent().unwrap().to_path_buf()
    }

    fn entry_for(source: &Path, backup: &Path, created_at: i64) -> HistoryEntry {
        HistoryEntry {
            media_kind: "image".into(),
            video_metadata: None,
            id: format!(
                "{}-{}",
                created_at,
                dir_of_backup(backup).file_name().unwrap().to_string_lossy()
            ),
            created_at,
            expires_at: created_at + DAY_MILLIS * 3,
            source_path: source.to_string_lossy().into_owned(),
            output_path: Some(source.to_string_lossy().into_owned()),
            file_name: source.file_name().unwrap().to_string_lossy().into_owned(),
            output_mode: "replace".into(),
            original_size: 100,
            compressed_size: fs::metadata(source).map(|m| m.len()).unwrap_or(0),
            savings: 60.0,
            out_type: "png".into(),
            algorithm: "test".into(),
            backup_path: Some(backup.to_string_lossy().into_owned()),
            status: HistoryStatus::Compressed,
            restored_at: None,
            output_modified_at: file_mtime_millis(source),
            source_sha256: crate::source_identity::sha256_of(backup).ok(),
            output_sha256: crate::source_identity::sha256_of(source).ok(),
            backup_sha256: crate::source_identity::sha256_of(backup).ok(),
            source_exists: true,
            backup_exists: true,
            output_exists: true,
        }
    }

    fn open_access() -> std::sync::Arc<dyn crate::sandbox_access::FileAccess> {
        crate::sandbox_access::unrestricted()
    }

    #[test]
    fn replace_reuses_the_first_backup_so_the_true_original_survives_recompression() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"bytes1");

        let first = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed-once").unwrap();
        // 生产顺序：第一次压缩提交后先记历史（产物指纹入账），下一轮的
        // ensure_backup 才认得出「现在的字节是我们自己的产物」→ 复用最早的备份，
        // 「恢复原图」永远给真正的原图。
        store.add(entry_for(&source, &first, now_millis())).unwrap();
        let second = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed-twice").unwrap();

        assert_eq!(first, second);
        assert_eq!(fs::read(&second).unwrap(), b"bytes1");
    }

    /// 用户在两轮压缩之间自己改过文件：那份字节不是我们的产物，必须有属于
    /// 自己的一份快照 —— 否则 replace 一提交它就再也找不回来了。
    #[test]
    fn an_external_edit_between_rounds_gets_its_own_backup_slot() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"bytes1");

        let first = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed-once").unwrap();
        store.add(entry_for(&source, &first, now_millis())).unwrap();
        // 用户在第二轮之前自己改了图（不是我们的产物）
        fs::write(&source, b"user-edited-version").unwrap();
        let second = store.ensure_backup(&source).unwrap();

        assert_ne!(first, second, "外部编辑的字节必须有新的备份槽位");
        assert_eq!(fs::read(&first).unwrap(), b"bytes1");
        assert_eq!(fs::read(&second).unwrap(), b"user-edited-version");
        // 最早的备份不许被覆盖：它仍是第一轮「恢复原图」的依据。
        assert!(dir_of_backup(&first).exists());
    }

    /// §27 「恢复原图」这句话的全部依据，就是这份副本的字节真的等于当初的原图。
    #[test]
    fn a_backup_is_only_faithful_while_its_bytes_match_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");

        let backup = store.ensure_backup(&source).unwrap();
        // 刚写好的版本 3 快照：源与副本两份指纹都在，而且相等。
        assert_eq!(store.backup_is_faithful(&backup), Ok(()));

        // 副本被换成别的内容 —— 它就不再是任何人的原图了。
        fs::write(&backup, b"tampered-content").unwrap();
        let detail = store.backup_is_faithful(&backup).unwrap_err();
        assert!(detail.contains("已经不是当初那份字节"), "{detail}");

        // 副本整个不见了：连字节都读不到，谈不上恢复。
        fs::remove_file(&backup).unwrap();
        let detail = store.backup_is_faithful(&backup).unwrap_err();
        assert!(detail.contains("不在磁盘上"), "{detail}");
    }

    /// 来历记录自相矛盾（两份指纹不等）= 当年复制就没抄对，这份东西不配叫备份。
    #[test]
    fn a_self_contradictory_backup_record_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        let path = crate::source_identity::sha256_of(&backup).unwrap();
        fs::write(
            dir_of_backup(&backup).join(META_FILE),
            serde_json::json!({
                "version": 3,
                "sourcePath": "whatever",
                "createdAt": 1,
                "originalSize": 13,
                "originalModifiedAt": null,
                "originalExtension": "png",
                "sourceSha256": "0000000000000000000000000000000000000000000000000000000000000000",
                "backupSha256": path
            })
            .to_string(),
        )
        .unwrap();
        let detail = store.backup_is_faithful(&backup).unwrap_err();
        assert!(detail.contains("自相矛盾"), "{detail}");
    }

    /// 升级前留下的备份没有指纹记录：证明不了 ≠ 证明坏了，恢复入口照旧能用。
    #[test]
    fn a_backup_recorded_before_fingerprints_existed_is_still_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(
            dir_of_backup(&backup).join(META_FILE),
            r#"{"version":2,"sourcePath":"/tmp/a.png","createdAt":1,"originalSize":13,"originalModifiedAt":null,"originalExtension":"png"}"#,
        )
        .unwrap();
        assert_eq!(store.backup_is_faithful(&backup), Ok(()));
    }

    #[test]
    fn expired_entries_and_their_backups_are_dropped_when_quitting() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let old = sample_source(dir.path(), "old.png", b"old-original");
        let fresh = sample_source(dir.path(), "fresh.png", b"fresh-original");
        let now = now_millis();

        let old_backup = store.ensure_backup(&old).unwrap();
        let fresh_backup = store.ensure_backup(&fresh).unwrap();
        store
            .add(entry_for(&old, &old_backup, now - 4 * DAY_MILLIS))
            .unwrap();
        store
            .add(entry_for(&fresh, &fresh_backup, now - 2 * DAY_MILLIS))
            .unwrap();

        let report = store.apply_retention_on_exit(3, now, true, &[]);
        assert_eq!(report.removed_entries, 1);
        assert!(!dir_of_backup(&old_backup).exists());
        assert!(dir_of_backup(&fresh_backup).exists());
        assert_eq!(store.list().len(), 1);
        // 用户自己的文件永远不在清理范围内
        assert!(old.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn shared_backup_is_kept_until_the_last_reference_expires() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        let now = now_millis();

        // 生产顺序：压缩替换了源文件 → 记历史（产物指纹入账）→ 下一轮复用同一份备份。
        fs::write(&source, b"recompressed").unwrap();
        store
            .add(entry_for(&source, &backup, now - 4 * DAY_MILLIS))
            .unwrap();
        let second = store.ensure_backup(&source).unwrap();
        assert_eq!(backup, second, "自家产物 → 复用同一份共享备份");
        store
            .add(entry_for(&source, &second, now - 2 * DAY_MILLIS))
            .unwrap();

        store.apply_retention_on_exit(3, now, true, &[]);
        assert!(dir_of_backup(&backup).exists());
        assert_eq!(store.list().len(), 1);

        store.apply_retention_on_exit(1, now, true, &[]);
        assert!(!dir_of_backup(&backup).exists());
        assert!(store.list().is_empty());
        assert!(source.exists());
    }

    #[test]
    fn quitting_with_not_retained_clears_this_runs_rows_and_backups() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        let run_started_at = now_millis();
        store
            .add(entry_for(&source, &backup, run_started_at + 1_000))
            .unwrap();

        let report = store.apply_retention_on_exit(0, run_started_at, true, &[]);
        assert_eq!(report.removed_entries, 1);
        assert_eq!(report.removed_backups, 1);
        assert!(store.list().is_empty(), "「不保留」= 关掉应用后记录也没了");
        assert!(!dir_of_backup(&backup).exists());
        // 清的是 Sizqo 自己的账本和副本，用户的压缩结果一个字节都不动。
        assert!(source.exists());
        assert_eq!(fs::read(&source).unwrap(), b"compressed");

        // 再退一次不许炸：已经没有东西可清了。
        let again = store.apply_retention_on_exit(0, run_started_at, true, &[]);
        assert_eq!(again.removed_entries, 0);
        assert_eq!(again.removed_backups, 0);
    }

    #[test]
    fn a_row_whose_backup_vanished_says_so_instead_of_pretending_to_be_restorable() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        store
            .add(entry_for(&source, &backup, now_millis()))
            .unwrap();
        // 记录还活着、备份目录却已经没了（手动清过 / 磁盘出错）：这时只能说实话。
        fs::remove_dir_all(dir_of_backup(&backup)).unwrap();

        let rows = store.list();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].backup_exists);
        let error = store
            .restore(&rows[0], true, false, open_access().as_ref(), &mut || false)
            .unwrap_err();
        assert!(matches!(error, RestoreError::BackupGone));
        assert_eq!(error.message(), "原图备份已清理，无法恢复");
    }

    #[test]
    fn crash_leftovers_get_one_more_session_under_not_retained() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        // 上次被强杀/崩溃欠下的：那份备份可能是这个原图唯一还活着的副本。
        let run_started_at = now_millis();
        store
            .add(entry_for(&source, &backup, run_started_at - 60_000))
            .unwrap();

        // 本次退出：上一次没结清账 → 留着，用户在这一次会话里看得见、也恢复得了。
        let report = store.apply_retention_on_exit(0, run_started_at, false, &[]);
        assert_eq!(report.removed_entries, 0);
        assert_eq!(report.removed_backups, 0);
        assert!(dir_of_backup(&backup).exists());
        let rows = store.list();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].backup_exists);
        store
            .restore(&rows[0], true, false, open_access().as_ref(), &mut || false)
            .unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
    }

    #[test]
    fn not_retained_leftovers_are_collected_by_the_next_clean_quit() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        // 同一份遗留：这次的上一次运行是**正常退出**的，账已经结过，不该再享有豁免。
        store
            .add(entry_for(&source, &backup, now_millis() - 60_000))
            .unwrap();

        let report = store.apply_retention_on_exit(0, now_millis(), true, &[]);
        assert_eq!(report.removed_entries, 1);
        assert_eq!(report.removed_backups, 1);
        assert!(store.list().is_empty());
        assert!(!dir_of_backup(&backup).exists());
        assert!(source.exists());
    }

    #[test]
    fn the_clean_exit_marker_is_what_tells_a_crash_apart_from_a_quit() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        // 全新安装：没有记号 = 上次没结清（宁可多留一批备份，也不误删）。
        assert!(!store.take_clean_exit_marker());
        store.mark_clean_exit();
        // 取一次就抹掉：下次启动读到的必须是"本次运行"的结论，不是上上次的。
        assert!(store.take_clean_exit_marker());
        assert!(!store.take_clean_exit_marker());
    }

    #[test]
    fn restore_marks_every_entry_sharing_the_backup_and_deletes_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        let now = now_millis();

        fs::write(&source, b"compressed-once").unwrap();
        let first = entry_for(&source, &backup, now - 60_000);
        store.add(first.clone()).unwrap();
        let second = entry_for(&source, &backup, now);
        store.add(second.clone()).unwrap();

        let restored = store.restore(&second, false, false, open_access().as_ref(), &mut || false).unwrap();
        assert_eq!(restored.history_ids.len(), 2);
        assert!(!restored.already_restored);
        assert_eq!(restored.warning, None);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!dir_of_backup(&backup).exists());
        for entry in store.list() {
            assert_eq!(entry.status, HistoryStatus::Restored);
            assert!(entry.restored_at.is_some());
        }
    }

    #[test]
    fn cross_format_replace_removes_the_converted_output_when_restoring() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "photo.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        let output = dir.path().canonicalize().unwrap().join("photo.jpg");
        fs::write(&output, b"converted").unwrap();

        let mut entry = entry_for(&source, &backup, now_millis());
        entry.compressed_size = fs::metadata(&output).unwrap().len();
        entry.output_modified_at = file_mtime_millis(&output);
        entry.output_path = Some(output.to_string_lossy().into_owned());
        entry.output_sha256 = crate::source_identity::sha256_of(&output).ok();
        fs::write(&source, b"converted").unwrap();
        store.add(entry.clone()).unwrap();

        store.restore(&entry, false, false, open_access().as_ref(), &mut || false).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!output.exists());
    }

    #[test]
    fn externally_modified_output_requires_force() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        let mut entry = entry_for(&source, &backup, now_millis());
        entry.output_modified_at = Some(1_000);
        store.add(entry.clone()).unwrap();

        fs::write(&source, b"photoshop-saved-this").unwrap();
        assert!(HistoryStore::has_conflict(&entry));
        assert!(matches!(
            store.restore(&entry, false, false, open_access().as_ref(), &mut || false),
            Err(RestoreError::Conflict)
        ));
        // 备份没被动过，强制恢复仍可成功
        assert!(store.restore(&entry, true, false, open_access().as_ref(), &mut || false).is_ok());
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
    }

    #[test]
    fn suffix_entries_keep_no_backup_and_are_dropped_when_undone() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"untouched-original");
        let generated = dir.path().join("a_compressed.png");
        fs::write(&generated, b"compressed").unwrap();

        let mut entry = entry_for(&source, &generated, now_millis());
        entry.output_mode = "suffix".into();
        entry.backup_path = None;
        entry.output_path = Some(generated.to_string_lossy().into_owned());
        // 生产里 record 会把产物指纹写全；撤销判冲突靠的就是这份指纹。
        entry.compressed_size = fs::metadata(&generated).map(|m| m.len()).unwrap_or(0);
        entry.output_modified_at = file_mtime_millis(&generated);
        entry.output_sha256 = crate::source_identity::sha256_of(&generated).ok();
        store.add(entry.clone()).unwrap();

        // 后缀模式没有备份，恢复语义只能是"删掉生成文件"。
        store.restore(&entry, false, false, open_access().as_ref(), &mut || false).unwrap();
        assert!(!generated.exists());
        assert!(store.list().is_empty());
        assert_eq!(fs::read(&source).unwrap(), b"untouched-original");
    }

    #[test]
    fn concurrent_appends_do_not_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(store_in(dir.path()));
        let source = sample_source(dir.path(), "a.png", b"x");
        let backup = store.ensure_backup(&source).unwrap();

        let mut handles = Vec::new();
        for index in 0..12 {
            let store = store.clone();
            let source = source.clone();
            let backup = backup.clone();
            handles.push(std::thread::spawn(move || {
                store
                    .add(entry_for(&source, &backup, 1_000 + index))
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.list().len(), 12);
    }

    #[test]
    fn corrupt_history_never_deletes_backups() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        fs::write(store.history_path(), b"{ half-written").unwrap();

        // 退出清理第一刀就砍在损坏的历史上：它绝不能被当成"空历史"，否则所有备份
        // 看起来无人引用，一次不报错的退出就把原图全删了。
        let report = store.apply_retention_on_exit(3, now_millis(), true, &[]);
        assert_eq!(report.removed_entries, 0);
        assert_eq!(report.removed_backups, 0);
        assert!(!report.warnings.is_empty(), "跳过清理必须说给用户听");
        assert!(dir_of_backup(&backup).exists());
        assert_eq!(fs::read(&backup).unwrap(), b"true-original");
        assert!(store.list().iter().any(|entry| entry.status == HistoryStatus::RecoveryAvailable));
        // 损坏现场要留档，不能悄悄被 [] 覆盖掉。
        let quarantined = PathBuf::from(store.take_startup_report().unwrap().quarantined_to.unwrap());
        assert_eq!(fs::read(&quarantined).unwrap(), b"{ half-written");
    }

    #[test]
    fn corrupt_history_rebuilds_a_restore_entry_from_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        fs::write(store.history_path(), b"not json at all").unwrap();
        drop(store);

        // 换一个新实例 = 重启一次：读不到老历史，就从备份重建能一键恢复的入口。
        let store = store_in(dir.path());
        let rows = store.list();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].status, HistoryStatus::RecoveryAvailable);
        assert_eq!(rows[0].original_size, 13, "备份里那份才是真正原图");
        assert_eq!(rows[0].compressed_size, 10);
        assert!(rows[0].backup_exists);
        // 重建条目不该一上来就误报「压缩后又被改过」：不 force 也必须能恢复。
        assert!(!HistoryStore::has_conflict(&rows[0]));

        store
            .restore(&rows[0], false, false, open_access().as_ref(), &mut || false)
            .unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!dir_of_backup(&backup).exists());
        assert_eq!(store.list()[0].status, HistoryStatus::Restored);
    }

    #[test]
    fn a_missing_history_file_is_not_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"x");
        let backup = store.ensure_backup(&source).unwrap();
        assert!(!store.cleanup_is_locked());
        // 全新安装（没有任何历史）时，无人引用的孤儿备份照常被回收。
        let report = store.apply_retention_on_exit(3, now_millis(), true, &[]);
        assert_eq!(report.removed_backups, 1);
        assert!(!dir_of_backup(&backup).exists());
    }

    #[test]
    fn backup_keys_are_stable_across_store_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        fs::write(&path, b"x").unwrap();
        let first = HistoryStore::backup_key(&path);
        let second = HistoryStore::backup_key(&path.canonicalize().unwrap());
        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        // FNV-1a 64 的已知测试向量：换算法会在这里炸，而不是在用户升级之后。
        assert_eq!(HistoryStore::stable_hash(""), "cbf29ce484222325");
        assert_eq!(HistoryStore::stable_hash("a"), "af63dc4c8601ec8c");
        assert_ne!(first, HistoryStore::legacy_backup_key(&path));
    }

    #[test]
    fn a_backup_made_under_the_legacy_hash_key_is_still_reused() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        // 模拟升级前老 key 写的备份目录（只有 original，没有来历记录）。
        let legacy = HistoryStore::legacy_backup_key(&source);
        let legacy_dir = dir.path().join("history").join(BACKUPS_DIR).join(&legacy);
        fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_backup = legacy_dir.join("original.png");
        fs::write(&legacy_backup, b"true-original").unwrap();

        // 第二次压缩必须复用那份老备份，而不是把压缩结果当成原图存进新目录。
        // 真实升级现场的历史里有上一轮的记录（0.3.5 起带 source/output 指纹），
        // 老备份凭它升级成可信备份；什么证据都没有的老备份不再被静默采信（§2.3）。
        fs::write(&source, b"compressed").unwrap();
        store
            .add(entry_for(&source, &legacy_backup, now_millis()))
            .unwrap();
        assert_eq!(store.ensure_backup(&source).unwrap(), legacy_backup);
        assert_eq!(fs::read(&legacy_backup).unwrap(), b"true-original");
    }

    /// §2.3 没有任何证据的老备份：既不自动复用（当前字节不是我们的产物），
    /// 也绝不被当成垃圾清掉 —— 它留给旧历史条目和用户确认。
    #[test]
    fn a_legacy_backup_without_any_evidence_is_not_silently_reused() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"bytes1");
        let first = store.ensure_backup(&source).unwrap();
        // 把来历记录降级成 v1（没有指纹）：模拟更老版本的备份。
        let key = backup_key_of(first.to_str()).unwrap();
        let meta = BackupMeta {
            version: 1,
            source_path: canonical_path(&source),
            created_at: now_millis(),
            original_size: 0,
            original_modified_at: None,
            original_extension: "png".into(),
            source_sha256: None,
            backup_sha256: None,
            source_device: None,
            source_inode: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        write_atomic(&store.backup_dir(&key).join(META_FILE), json.as_bytes()).unwrap();

        // 用户改过文件：老备份证明不了它还代表这次要盖掉的原图 → 另起新槽位。
        fs::write(&source, b"user-edited").unwrap();
        let second = store.ensure_backup(&source).unwrap();
        assert_ne!(second, first, "无证据的老备份不允许被静默复用");
        assert_eq!(fs::read(&first).unwrap(), b"bytes1", "旧备份原样保留");
        assert_eq!(fs::read(&second).unwrap(), b"user-edited");
        // 复用判定看到的是 LegacyUnverified：恢复路径要求用户确认。
        assert_eq!(
            store.backup_verification(&first).unwrap(),
            BackupVerification::LegacyUnverified
        );
    }

    /// §2.3 内容对不上记录的备份：不可复用、恢复时拒绝，但绝不删除。
    #[test]
    fn a_backup_whose_content_no_longer_matches_its_record_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"bytes1");
        let first = store.ensure_backup(&source).unwrap();
        // 副本被外部换掉（保留大小不一致更真实）：记录还在，字节不是那份字节。
        fs::write(&first, b"tampered-bytes-longer").unwrap();

        fs::write(&source, b"user-edited").unwrap();
        let second = store.ensure_backup(&source).unwrap();
        assert_ne!(second, first, "内容对不上记录的备份不允许复用");
        assert_eq!(fs::read(&first).unwrap(), b"tampered-bytes-longer", "不删除");
        assert!(matches!(
            store.backup_verification(&first),
            Err(_) | Ok(BackupVerification::ContentMismatch)
        ));
    }

    /// §2.2 备份过程横着一场并发编辑：复核发现源文件已经不是采集时的那份字节，
    /// 快照作废、目录清掉、覆盖被拦下 —— 用户改过的内容原样留在磁盘上。
    #[test]
    fn a_snapshot_is_aborted_when_the_source_changes_during_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"bytes1");
        set_fault_edit_source_after_copy(true);
        let outcome = store.ensure_backup(&source);
        set_fault_edit_source_after_copy(false);
        let error = outcome.unwrap_err();
        assert!(
            error.contains("原图在备份过程中被修改"),
            "失败原因必须说清是备份期间发生了并发修改: {error}"
        );
        assert_eq!(
            fs::read(&source).unwrap(),
            b"user-edited-during-backup",
            "用户的并发编辑一个字节都不能动"
        );
        assert!(
            store
                .backups_dir()
                .read_dir()
                .map(|entries| entries.count())
                .unwrap_or(0)
                == 0,
            "作废的快照不许留下冒充备份的目录"
        );
    }

    /// §2.3 老备份恢复前必须由用户明确确认；确认也只能跳过"完整性未知"这一条，
    /// 备份字节对得上记录的（Verified）从来不需要确认。
    #[test]
    fn restore_requires_confirmation_for_legacy_backups() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        // 记录降级成 v1（无指纹）+ 历史条目指向它：真实升级现场。
        let key = backup_key_of(backup.to_str()).unwrap();
        let meta = BackupMeta {
            version: 1,
            source_path: canonical_path(&source),
            created_at: now_millis(),
            original_size: 0,
            original_modified_at: None,
            original_extension: "png".into(),
            source_sha256: None,
            backup_sha256: None,
            source_device: None,
            source_inode: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        write_atomic(&store.backup_dir(&key).join(META_FILE), json.as_bytes()).unwrap();
        fs::write(&source, b"compressed").unwrap();
        let entry = entry_for(&source, &backup, now_millis());
        store.add(entry.clone()).unwrap();

        // 未确认：明确要求用户先看清这份备份来自旧版本。
        match store.restore(&entry, false, false, open_access().as_ref(), &mut || false) {
            Err(RestoreError::LegacyBackup) => {}
            other => panic!("legacy 备份未确认必须报 LegacyBackup，实际: {other:?}"),
        }
        assert_eq!(fs::read(&source).unwrap(), b"compressed", "未确认绝不写回");

        // 确认后恢复成功，备份字节原样写回。
        let report = store
            .restore(&entry, false, true, open_access().as_ref(), &mut || false)
            .unwrap();
        assert!(!report.already_restored);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
    }

    #[test]
    fn the_history_cap_evicts_the_oldest_rows_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"x");
        let backup = store.ensure_backup(&source).unwrap();
        let mut entries = Vec::new();
        for index in 0..5 {
            let mut entry = entry_for(&source, &backup, 1_000 + index);
            entry.id = format!("row-{index}");
            entries.push(entry);
        }

        let evicted = store.trim_to_cap(&mut entries, 3);
        assert_eq!(entries.len(), 3);
        assert_eq!(evicted.len(), 2);
        assert!(entries.iter().all(|entry| entry.id.starts_with("row-")));
        assert!(evicted.iter().any(|entry| entry.id == "row-0"));
        assert!(evicted.iter().any(|entry| entry.id == "row-1"));
        // 还没超上限时一条都不动。
        assert!(store.trim_to_cap(&mut entries, 9).is_empty());
    }

    /// §46 恢复是幂等的：第二次点「恢复原图」不该被当成冲突，也不该要求 force。
    #[test]
    fn restoring_twice_reports_already_restored_instead_of_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        let entry = entry_for(&source, &backup, now_millis());
        store.add(entry.clone()).unwrap();

        let first = store
            .restore(&entry, false, false, open_access().as_ref(), &mut || false)
            .unwrap();
        assert_eq!(first.history_ids, vec![entry.id.clone()]);
        assert!(!first.already_restored);
        assert_eq!(first.warning, None);

        let row = store.find(&entry.id).unwrap();
        assert_eq!(row.status, HistoryStatus::Restored);
        assert!(row.backup_path.is_none());

        // 文件已经是原图字节：再看一眼指纹就够了，不需要备份、也不该报 Conflict。
        let second = store
            .restore(&row, false, false, open_access().as_ref(), &mut || false)
            .unwrap();
        assert!(second.already_restored);
        assert_eq!(second.warning, None);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        // 已恢复的条目不再出现在「按源路径找可恢复记录」的结果里。
        assert!(store
            .find_latest_for_source(&row.source_path)
            .is_none());
    }

    /// §47 大小与 mtime 都被伪装成记录时的样子，只有内容指纹能拦住这个冲突。
    /// §4.1 审查修复回归：有指纹、但当前文件**读不出来**时绝不放行 ——
    /// 旧逻辑退回 size+mtime，读不出来又对得上的文件被当成没改过。
    #[cfg(unix)]
    #[test]
    fn an_unreadable_output_is_a_conflict_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "locked.png", b"payload12");
        let mut entry = entry_for(&source, &source, now_millis());
        entry.output_modified_at = file_mtime_millis(&source);
        let mut mode = fs::metadata(&source).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        mode.set_mode(0o000);
        fs::set_permissions(&source, mode).unwrap();
        let state = HistoryStore::conflict_state(&entry);
        let conflicted = HistoryStore::has_conflict(&entry);
        let restore_outcome =
            store.restore(&entry, false, false, open_access().as_ref(), &mut || false);
        let _ = fs::remove_file(&source);
        assert_eq!(state, ConflictState::Unreadable,
            "读不出来 = 证明不了归属，必须拦");
        assert!(conflicted);
        assert!(
            matches!(restore_outcome, Err(RestoreError::Conflict) | Err(RestoreError::Io(_))),
            "未加 force 的恢复不得覆盖读不出来的文件"
        );
    }

    #[test]
    fn same_size_and_mtime_edit_is_still_a_conflict_thanks_to_the_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed-AA").unwrap();
        let mut entry = entry_for(&source, &backup, now_millis());
        // 记录写完后再原地改写：长度一样，mtime 也照当前值记，旧判据完全看不出来。
        fs::write(&source, b"compressed-BB").unwrap();
        entry.compressed_size = fs::metadata(&source).unwrap().len();
        entry.output_modified_at = file_mtime_millis(&source);
        store.add(entry.clone()).unwrap();

        assert!(HistoryStore::has_conflict(&entry));
        assert!(matches!(
            store.restore(&entry, false, false, open_access().as_ref(), &mut || false),
            Err(RestoreError::Conflict)
        ));
        // 用户明确选择覆盖后仍然拿得回真正原图。
        store
            .restore(&entry, true, false, open_access().as_ref(), &mut || false)
            .unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
    }

    /// §48 原图写回成功、产物删不掉：恢复是成功的，但告警必须让用户看见。
    /// §4.1 审查修复回归：原图已经恢复（幂等分支）之后，用户编辑过的
    /// 跨格式产物绝不能被"顺手"删掉 —— 删除前必须对得上产物指纹。
    #[test]
    fn already_restored_never_deletes_an_edited_cross_format_output() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "photo.png", b"true-original");
        let backup = dir.path().join("backups").join("k1").join("original.png");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"true-original").unwrap();
        let output = dir.path().join("photo.webp");
        // 产物指纹 = 用户编辑**之后**的字节不存在于历史里：历史记的是当初产物。
        fs::write(&output, b"original-product").unwrap();
        let mut entry = entry_for(&source, &backup, now_millis());
        entry.output_path = Some(output.to_string_lossy().into_owned());
        entry.output_mode = "replace".into();
        store.add(entry.clone()).unwrap();
        // 用户在历史落盘后编辑了跨格式产物。
        fs::write(&output, b"user-edited-webp").unwrap();
        // 原图已经写回 = already_restored 分支（不再要求 force）。
        assert_eq!(fs::read(&source).unwrap(), b"true-original".to_vec(),
            "前提检查：源位置就是原图字节");
        let report = store
            .restore(&entry, false, false, open_access().as_ref(), &mut || false)
            .unwrap_or_else(|error| panic!("幂等恢复不该失败: {error:?}"));
        assert!(report.already_restored, "应走幂等分支");
        assert_eq!(
            fs::read(&output).unwrap(),
            b"user-edited-webp".to_vec(),
            "用户编辑过的跨格式产物必须保留"
        );
    }

    /// 「产物删不掉」这段现场的两个平台各造各的，期望的实话完全一样：
    /// 原图写回照常成立，历史记成"带警告恢复"，失败必须回给用户而不是吞掉。
    fn output_cleanup_failure_scene(
        dir: &Path,
    ) -> (HistoryStore, HistoryEntry, PathBuf, PathBuf, PathBuf) {
        let store = store_in(dir);
        let src_dir = dir.join("src");
        let out_dir = dir.join("out");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&out_dir).unwrap();
        let source = src_dir.join("photo.png");
        fs::write(&source, b"true-original").unwrap();
        let source = source.canonicalize().unwrap();
        let backup = store.ensure_backup(&source).unwrap();
        let output = out_dir.join("photo.jpg");
        fs::write(&output, b"converted").unwrap();

        let mut entry = entry_for(&source, &backup, now_millis());
        entry.output_path = Some(output.to_string_lossy().into_owned());
        entry.compressed_size = fs::metadata(&output).unwrap().len();
        entry.output_modified_at = file_mtime_millis(&output);
        entry.output_sha256 = crate::source_identity::sha256_of(&output).ok();
        fs::write(&source, b"converted").unwrap();
        store.add(entry.clone()).unwrap();
        (store, entry, source, output, out_dir)
    }

    fn restore_survives_a_failed_output_cleanup(
        report: &RestoreReport,
        row: &HistoryEntry,
        source: &Path,
        output: &Path,
    ) {
        assert_eq!(fs::read(source).unwrap(), b"true-original");
        assert!(output.exists());
        let warning = report.warning.as_deref().expect("清理失败必须回给用户");
        assert!(warning.contains("压缩文件删除失败"), "{warning}");
        assert_eq!(row.status, HistoryStatus::RestoredWithWarning);
        assert!(row.restored_at.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_output_cleanup_keeps_the_restore_but_reports_a_warning() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (store, entry, source, output, out_dir) = output_cleanup_failure_scene(dir.path());

        // 目录只读：产物删不掉，但原图写回完全不受影响。
        fs::set_permissions(&out_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let report = store
            .restore(&entry, false, false, open_access().as_ref(), &mut || false)
            .unwrap();
        fs::set_permissions(&out_dir, fs::Permissions::from_mode(0o755)).unwrap();

        let row = store.find(&entry.id).unwrap();
        restore_survives_a_failed_output_cleanup(&report, &row, &source, &output);
        assert!(store.find_latest_for_source(&entry.source_path).is_none());
    }

    /// Windows 上「删不掉」的自然形态：另一个句柄还开着且**不给共享删除位**，
    /// `remove_file` 直接 sharing violation。权限位在那边不管用，所以这一支单独造现场，
    /// 断言的是同一句实话 —— 平台不同，契约不能不同。
    ///
    /// 共享位必须精确到 `READ | WRITE`，两个方向都会把这条测试变成假绿：
    /// - std 的 `File::open` 默认带 `FILE_SHARE_DELETE`，那种"打开中"拦不住删除
    ///   （只是延后到句柄关闭），"删不掉"被悄悄测成"删得掉"；
    /// - `share_mode(0)` 又过头：恢复要先读产物指纹做归属判定，独占会把这步一起打死，
    ///   于是走的是"证明不了归属所以不删"，跟"删的时候失败"根本不是同一条分支。
    #[cfg(windows)]
    #[test]
    fn an_output_held_open_by_another_handle_fails_cleanup_but_not_the_restore() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        let dir = tempfile::tempdir().unwrap();
        let (store, entry, source, output, _) = output_cleanup_failure_scene(dir.path());
        let blocker = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&output)
            .unwrap();
        let report = store
            .restore(&entry, false, false, open_access().as_ref(), &mut || false)
            .unwrap();
        drop(blocker);

        let row = store.find(&entry.id).unwrap();
        restore_survives_a_failed_output_cleanup(&report, &row, &source, &output);
        assert!(output.exists(), "句柄释放后产物仍在原位：恢复从不靠删掉它");
        assert!(store.find_latest_for_source(&entry.source_path).is_none());
    }

    /// §76 产物在访达里被删掉不等于记录坏了：原图备份还在，恢复照常成立。
    /// 这时目标位置根本不存在，has_conflict 没有可冲突的对象，也就不该要求 force。
    #[test]
    fn an_externally_deleted_output_still_restores_from_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        let entry = entry_for(&source, &backup, now_millis());
        store.add(entry.clone()).unwrap();
        // 用户在访达里把压缩产物删掉：源位置和产物是同一个文件。
        fs::remove_file(&source).unwrap();
        let row = store.find(&entry.id).unwrap();
        assert!(!row.output_exists && !row.source_exists);
        assert!(row.backup_exists, "产物没了，备份不能跟着消失");

        let report = store
            .restore(&entry, false, false, open_access().as_ref(), &mut || false)
            .unwrap();
        assert_eq!(report.history_ids, vec![entry.id.clone()]);
        assert!(!report.already_restored);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert_eq!(
            store.find(&entry.id).unwrap().status,
            HistoryStatus::Restored
        );
    }

    /// §76 产物和备份都不在 = 记录不完整。恢复必须明确失败，
    /// 绝不能凭一条路径在目标位置写出一份空文件。
    #[test]
    fn a_missing_backup_with_a_missing_output_reports_an_incomplete_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"true-original");
        let backup = store.ensure_backup(&source).unwrap();
        fs::write(&source, b"compressed").unwrap();
        let entry = entry_for(&source, &backup, now_millis());
        store.add(entry.clone()).unwrap();

        fs::remove_file(&source).unwrap();
        fs::remove_dir_all(dir_of_backup(&backup)).unwrap();
        let row = store.find(&entry.id).unwrap();
        assert!(!row.output_exists && !row.backup_exists);

        assert!(matches!(
            store.restore(&entry, false, false, open_access().as_ref(), &mut || false),
            Err(RestoreError::BackupGone)
        ));
        assert!(!source.exists(), "恢复失败不许在目标位置留下任何东西");
    }

    #[test]
    fn a_backup_shared_with_a_surviving_row_survives_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        let source = sample_source(dir.path(), "a.png", b"x");
        let backup = store.ensure_backup(&source).unwrap();
        let mut entries = Vec::new();
        for index in 0..2 {
            let mut entry = entry_for(&source, &backup, 1_000 + index);
            entry.id = format!("row-{index}");
            entries.push(entry);
        }
        // 两条记录共用同一份备份：淘汰最老的那条也不能把备份删了。
        let evicted = store.trim_to_cap(&mut entries, 1);
        store.drop_backups_of(&evicted, &entries);
        assert!(dir_of_backup(&backup).exists());
    }
}
