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
    /// 老日志没有这两个字段：按 legacy 启发式（体积对账）处理。
    #[serde(default)]
    pub output_sha256: Option<String>,
    pub created_at: i64,
    /// PNG → JPG 这类跨格式 replace：压缩结果是个新文件，回滚时要一并删掉。
    pub cross_format: bool,
    /// §29 老日志里没有这个字段：它们只可能来自覆盖模式，按覆盖模式读。
    #[serde(default = "default_mode")]
    pub mode: TransactionMode,
}

fn default_mode() -> TransactionMode {
    TransactionMode::ReplaceExisting
}

pub const TRANSACTIONS_DIR: &str = "transactions";

#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// 历史已记录，只是删日志前崩溃：补删日志。
    pub committed: usize,
    /// 回滚成真正原图的事务数。
    pub rolled_back: usize,
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
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let json = serde_json::to_string(txn).map_err(|e| e.to_string())?;
        write_atomic(&self.path_of(&txn.id), json.as_bytes())
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

    pub fn has_pending(&self) -> bool {
        !self.list().is_empty()
    }

    /// 启动时第一件事：把上次没走完的事务结清。
    ///
    /// 判定只依赖一条事实 —— 历史里有没有这条 `history_id`。有则整条链路都走完了，
    /// 只欠删日志；没有则证明不了提交完成，按模式各自的下场收拾：ReplaceExisting 一律
    /// 回滚成真正原图（宁可丢掉一次压缩结果，也不能丢掉原图），CreateNew 只删这次新建的文件。
    pub fn recover(&self, history: &HistoryStore) -> RecoveryReport {
        let mut report = RecoveryReport::default();
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
                Err(error) => report
                    .warnings
                    .push(format!("{} 回滚失败，原图仍在备份里: {error}", txn.source_path)),
            }
        }
        report
    }
}

/// 用备份把真正原图写回源位置，再清掉这次生成的压缩结果。
///
/// 备份本身**不删**：回滚半途失败时它是唯一的原图副本，留给历史页/下次启动处理。
///
/// §2.5：动手之前必须先确认源位置上的字节归属。能证明它是本次事务写出的产物，
/// 才允许用备份盖回去；证明它还是没动过的原图，就什么都不用盖；证明不了归属
/// （外部在崩溃后改过它）就保留现场 —— 备份与日志都留着，绝不自动覆盖。
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
                // 源文件已经不在原位：按"提交没完成"对待，把原图写回去。
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
                // 源位置上就是这次写出的字节：安全回滚。
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
                // 老日志没有身份记录：维持升级前的恢复契约 —— 一律回滚，产物照原样清。
                // 这些现场只可能来自旧版本，行为跟着旧版本走，不另造启发式。
                HistoryStore::write_backup_back_to_source(&backup, &source)?;
                remove_generated_output(txn);
                return Ok(());
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

/// 删掉这次压缩产生的新文件与临时文件。同格式 replace 的输出就是源文件本身，绝不删。
///
/// 跨格式的输出文件也只删"能证明是这次生成的"：内容指纹命中，或老日志体积对上；
/// 其它体积/内容说明这位置已经被别人的内容占了，宁可留下也不误删。
fn remove_generated_output(txn: &ReplaceTransaction) {
    let source = PathBuf::from(&txn.source_path);
    if txn.cross_format || !crate::history::same_file(&PathBuf::from(&txn.output_path), &source) {
        let output = Path::new(&txn.output_path);
        let ours = match txn.output_sha256.as_deref() {
            // 新日志只删内容指纹对得上的产物；别人占的位置不动。
            Some(expected) => crate::source_identity::sha256_of(output).ok().as_deref() == Some(expected),
            // 老日志没有指纹：维持升级前契约，未提交事务的产物照原样清。
            None => true,
        };
        if ours {
            let _ = fs::remove_file(output);
        }
    }
    remove_temp(txn);
}

/// 新建模式清账：只删**能证明是这次生成的**那个文件。
///
/// 名字是独占申请来的，申请那一刻它不存在，所以空占位和"正好是我们承诺的体积"这两种
/// 都归这次事务；其它体积说明这位置已经被别人的内容占了，宁可留个来历不明的文件，
/// 也不许拿它当自己的半成品删（删用户文件永远不是恢复的选项）。
fn retire_generated_output(txn: &ReplaceTransaction) {
    let output = Path::new(&txn.output_path);
    match fs::metadata(output) {
        Ok(meta) if meta.len() == 0 || meta.len() == txn.expected_output_size => {
            let _ = fs::remove_file(output);
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
}

impl ClaimedOutput {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn into_path(self) -> PathBuf {
        self.path
    }

    /// 后端名字归调用方持有，但落盘前的某一步没做成：撤掉占位，别给用户留空文件。
    pub fn release(self) {
        let _ = fs::remove_file(&self.path);
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
                Ok(_) => return Ok(ClaimedOutput { path }),
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
            created_at: now_millis(),
            cross_format: false,
            mode: TransactionMode::ReplaceExisting,
        }
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
        fs::write(&source, b"compressed").unwrap();
        store.prepare(&txn(&source, &backup, "missing-from-history")).unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1);
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!store.has_pending());
        // 回滚靠的是备份，所以备份不能顺手删掉（回滚到一半失败时它是唯一副本）。
        assert!(backup.exists());
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

        let mut record = txn(&source, &backup, "gone");
        record.output_path = output.to_string_lossy().into_owned();
        record.cross_format = true;
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
            created_at: now_millis(),
            cross_format: false,
            mode: TransactionMode::CreateNew,
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
        store.prepare(&record).unwrap();

        store.recover(&history);
        assert!(!output.exists(), "空占位是这次申请的，除了它没人在乎");
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
            .prepare_durable(&txn(&source, &backup, "legacy"))
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
    fn legacy_logs_keep_the_pre_upgrade_recovery_contract() {
        let dir = tempfile::tempdir().unwrap();
        let (history, store) = stores(dir.path());
        let source = dir.path().join("photo.jpg");
        let backup = dir.path().join("backup").join("photo.jpg");
        fs::create_dir_all(backup.parent().unwrap()).unwrap();
        fs::write(&backup, b"true-original").unwrap();
        // 现场源文件的内容既不是原图也不是承诺的产物体积：老契约照样回滚。
        fs::write(&source, b"user-edited-content").unwrap();
        let record = txn(&source, &backup, "legacy-contract");
        store.prepare(&record).unwrap();

        let report = store.recover(&history);
        assert_eq!(report.rolled_back, 1, "老日志维持升级前契约：一律回滚");
        assert_eq!(fs::read(&source).unwrap(), b"true-original");
        assert!(!store.has_pending());

        // 跨格式的老日志：产物也照原样清（升级前它就是无条件删的）。
        let output = dir.path().join("photo.webp");
        fs::write(&output, b"someone-elses-file").unwrap();
        let mut record = txn(&source, &backup, "legacy-cross");
        record.output_path = output.to_string_lossy().into_owned();
        record.cross_format = true;
        store.prepare(&record).unwrap();
        store.recover(&history);
        assert!(!output.exists(), "老日志的产物清理维持升级前契约");
    }
}
