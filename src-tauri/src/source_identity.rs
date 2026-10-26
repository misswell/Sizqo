//! §24–§27 源文件身份：一次压缩到底基于哪一份字节。
//!
//! 为什么要有这个模块：replace 模式会**盖掉用户的文件**。以前它只在覆盖前那一刻才备份，
//! 于是"编码期间用户改了这张图"没人能发现 —— 盖上去的候选是按旧字节算的，备份里存的
//! 却是改完之后的新内容，「恢复原图」从此恢复不出任何用户认得出的东西。
//!
//! 现在的顺序（§25）：编码前读指纹 + 留快照 → 编码 → 覆盖前复核指纹 → 一致才 commit。
//! 图片线与视频线共用这一份实现，不许各判一次。

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

/// 一次 `stat()` 就能拿到的源文件指纹。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// `(dev, ino)`：改名、软链换指向、整个文件被换成另一个 inode 都躲不过这组。
    pub identity: Option<(u64, u64)>,
    /// §26 的内容指纹，§24 里那个 optionalHash。刻意**不参与**快速比较
    /// （`same_fingerprint`）：改名前那道快速门只 stat 不重算哈希，两边一个 `Some`
    /// 一个 `None`，按字段整个比就永远判成"文件变了"。
    pub sha256: Option<String>,
}

/// 读源文件指纹失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// 文件已经不在这个位置上了。
    Missing,
    /// 还在，但读不出来（权限、拔盘、IO 故障）。
    Unreadable(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceError::Missing => write!(formatter, "源文件已经不在这个位置"),
            SourceError::Unreadable(detail) => write!(formatter, "源文件读不出来（{detail}）"),
        }
    }
}

/// 覆盖前复核的回答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceCheck {
    /// 还是这次压缩所依据的那份字节。
    Unchanged,
    /// 不是了，附人话说明。§25：此时**不许覆盖**，候选删掉，用户当前文件原样留着。
    Changed(String),
}

impl SourceIdentity {
    /// 快速指纹：一次 `stat()`，不读内容。
    pub fn read(path: &Path) -> Result<Self, SourceError> {
        let meta = fs::metadata(path).map_err(metadata_error)?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Some((meta.dev(), meta.ino()))
        };
        #[cfg(not(unix))]
        let identity = None;
        Ok(Self {
            size: meta.len(),
            modified: meta.modified().ok(),
            identity,
            sha256: None,
        })
    }

    /// 快速指纹 + 内容指纹：§25 流程第一步，replace 模式编码前钉下的那份依据。
    pub fn read_with_hash(path: &Path) -> Result<Self, SourceError> {
        let mut identity = Self::read(path)?;
        identity.sha256 = Some(sha256_of(path)?);
        Ok(identity)
    }

    /// §25 流程里那个"比较"：只看一次 `stat()` 读得到的三项。
    pub fn same_fingerprint(&self, other: &SourceIdentity) -> bool {
        self.size == other.size
            && self.modified == other.modified
            && self.identity == other.identity
    }

    /// 改名前最后一道快速门（§31 的 source identity check）：只 stat，不再读一遍内容。
    /// 读不出来 = 证明不了"还是原来那份"，一样不许盖，原因照实带出去。
    pub fn fingerprint_intact(&self, path: &Path) -> SourceCheck {
        match SourceIdentity::read(path) {
            Ok(current) if self.same_fingerprint(&current) => SourceCheck::Unchanged,
            Ok(current) => SourceCheck::Changed(format!(
                "大小 {} → {} 字节，或修改时间/inode 变了",
                self.size, current.size
            )),
            Err(error) => SourceCheck::Changed(error.to_string()),
        }
    }

    /// §25 + §26 的完整复核：先比 stat 三项，再比内容哈希。
    ///
    /// 顺序刻意是先便宜后贵；哈希那一轮只在算过指纹的任务上跑（`sha256 == None`
    /// 的调用方，比如视频线，走到这里就等于"快速指纹一致"）。
    pub fn verify(&self, path: &Path) -> SourceCheck {
        if let SourceCheck::Changed(detail) = self.fingerprint_intact(path) {
            return SourceCheck::Changed(detail);
        }
        let Some(expected) = self.sha256.as_deref() else {
            return SourceCheck::Unchanged;
        };
        match sha256_of(path) {
            // 时间戳能伪造，字节不能：同大小同 mtime 的原地改动只有这一轮看得出来。
            Ok(actual) if actual == expected => SourceCheck::Unchanged,
            Ok(actual) => SourceCheck::Changed(format!(
                "字节内容已经不是同一份（{expected} → {actual}）"
            )),
            Err(error) => SourceCheck::Changed(error.to_string()),
        }
    }
}

/// 内容指纹。流式读：一张 200 MB 的 TIFF 不许整份进内存才算得出它是谁。
pub fn sha256_of(path: &Path) -> Result<String, SourceError> {
    use sha2::Digest;
    use std::io::ErrorKind;
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        // 文件在这一步已经不原位 = 它变了。这不该和真正的 IO 故障混成一句话，
        // 因为给用户看的处置不一样（前者是"你改了文件"，后者是"盘/权限有问题"）。
        Err(error) if error.kind() == ErrorKind::NotFound => return Err(SourceError::Missing),
        Err(error) => return Err(SourceError::Unreadable(format!("{}: {error}", path.display()))),
    };
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| SourceError::Unreadable(format!("{}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn metadata_error(error: std::io::Error) -> SourceError {
    if error.kind() == std::io::ErrorKind::NotFound {
        SourceError::Missing
    } else {
        SourceError::Unreadable(error.to_string())
    }
}

/// 内存字节的指纹：图片线的产物在落盘之前就能先记进事务日志，
/// 启动恢复时靠它证明"源位置上这份字节是我们写出去的"。
pub fn sha256_bytes(data: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 测试用：把 `path` 的 atime/mtime 钉到 `moment`，模拟"原地换内容、时间戳没动"。
#[cfg(test)]
impl SourceIdentity {
    pub(crate) fn freeze_mtime_to(path: &Path, moment: SystemTime) -> Result<(), SourceError> {
        Self::set_times(path, moment, moment)
    }

    #[cfg(unix)]
    fn set_times(path: &Path, accessed: SystemTime, modified: SystemTime) -> Result<(), SourceError> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::time::{Duration, UNIX_EPOCH};
        let to_timespec = |time: SystemTime| -> libc::timespec {
            let since: Duration = time
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::from_secs(0));
            libc::timespec {
                tv_sec: since.as_secs() as libc::time_t,
                tv_nsec: since.subsec_nanos() as libc::c_long,
            }
        };
        let times = [to_timespec(accessed), to_timespec(modified)];
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|error| SourceError::Unreadable(error.to_string()))?;
        if unsafe { libc::utimensat(libc::AT_FDCWD, name.as_ptr(), times.as_ptr(), 0) } != 0 {
            return Err(SourceError::Unreadable(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn set_times(_: &Path, _: SystemTime, _: SystemTime) -> Result<(), SourceError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_content_hash_catches_an_edit_that_left_size_and_mtime_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        fs::write(&path, b"aaaa").unwrap();
        let before = SourceIdentity::read_with_hash(&path).unwrap();

        // 原地换成别的字节，再把时间戳钉回去：这正是"只 stat"查不出来的那一支，
        // §26 要内容指纹就是为了它。
        fs::write(&path, b"bbbb").unwrap();
        let modified = before.modified.expect("刚 stat 过的文件必有 mtime");
        SourceIdentity::freeze_mtime_to(&path, modified).unwrap();

        let current = SourceIdentity::read(&path).unwrap();
        assert!(
            before.same_fingerprint(&current),
            "快速门本该看不出同大小同时间戳的改动，否则这条测试没测到点上"
        );
        assert_eq!(
            before.fingerprint_intact(&path),
            SourceCheck::Unchanged,
            "改名前的快速门放行是设计，不是漏判 —— 所以动用户文件之前的那道必须是 verify"
        );
        match before.verify(&path) {
            SourceCheck::Changed(detail) => assert!(
                detail.contains("字节内容已经不是同一份"),
                "内容指纹看出的理由不对：{detail}"
            ),
            SourceCheck::Unchanged => panic!("字节都换了还判成没变：内容哈希没生效"),
        }
    }

    #[test]
    fn a_rewritten_file_fails_the_fast_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        fs::write(&path, b"original").unwrap();
        let before = SourceIdentity::read(&path).unwrap();
        fs::write(&path, b"changed and longer").unwrap();
        assert!(
            matches!(before.fingerprint_intact(&path), SourceCheck::Changed(_)),
            "改过大小/时间戳要能被快速门拦下"
        );
        assert!(matches!(before.verify(&path), SourceCheck::Changed(_)));
    }

    #[test]
    fn an_untouched_file_passes_both_gates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        fs::write(&path, b"unchanged").unwrap();
        let before = SourceIdentity::read_with_hash(&path).unwrap();
        assert_eq!(before.fingerprint_intact(&path), SourceCheck::Unchanged);
        assert_eq!(before.verify(&path), SourceCheck::Unchanged);
    }

    #[test]
    fn a_gone_source_is_reported_missing_and_never_verified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        fs::write(&path, b"unchanged").unwrap();
        let before = SourceIdentity::read_with_hash(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(sha256_of(&path).unwrap_err(), SourceError::Missing);
        assert!(
            matches!(before.fingerprint_intact(&path), SourceCheck::Changed(_)),
            "文件没了绝不能算'没变'"
        );
        assert!(matches!(before.verify(&path), SourceCheck::Changed(_)));
    }

    #[test]
    fn the_hash_is_content_addressed_not_path_addressed() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, b"same bytes").unwrap();
        fs::write(&b, b"same bytes").unwrap();
        assert_eq!(sha256_of(&a).unwrap(), sha256_of(&b).unwrap());
        fs::write(&b, b"same bytes!").unwrap();
        assert_ne!(sha256_of(&a).unwrap(), sha256_of(&b).unwrap());
    }

    #[test]
    fn a_fast_only_identity_skips_the_content_round() {
        // 视频线只 stat 不哈希：verify 必须照样放行，否则每个视频都会被判成"源文件变了"。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        fs::write(&path, b"clip-bytes").unwrap();
        let before = SourceIdentity::read(&path).unwrap();
        assert_eq!(before.verify(&path), SourceCheck::Unchanged);
    }
}
