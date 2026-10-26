import Foundation
import CryptoKit

/// §24–§27 源文件身份：一次压缩到底基于哪一份字节。
///
/// 为什么要有这个文件：覆盖模式会**盖掉用户的文件**。以前它只在覆盖前那一刻才备份，
/// 于是"编码期间用户改了这张图"没人能发现 —— 盖上去的候选是按旧字节算的，备份里存的
/// 却是改完之后的新内容，「恢复原图」从此恢复不出任何用户认得出的东西。
///
/// 现在的顺序（§25）：编码前读指纹 + 留快照 → 编码 → 覆盖前复核 → 一致才 commit。
/// 图片线只有这一份实现，不许在 Call Center 与历史线各判一次（§54）。
struct SourceIdentity: Equatable {
    var size: Int64
    /// mtime，纳秒计数。用 Int64 而不是 Date：Date 的 Double 存 ns 会丢尾数，
    /// 而"时间戳没动"正是这道门要判的事，比不得能比错。
    var modified: Int64
    /// `(st_dev, st_ino)` 拆成两个字段存：元组不 conforms to Equatable，合成 `==` 会失败。
    /// 含义仍是一个整体 —— 改名、软链换指向、整个文件被换成另一个 inode 都躲不过这组。
    /// Swift 线只跑 macOS，`fstat()` 必定读得到，所以不做成可选。
    var deviceID: Int64
    var inode: UInt64
    /// §26 的内容指纹。刻意**不参与**快速比较（`sameFingerprint`）：改名前那道门只 stat，
    /// 两边一个有值一个为 nil，按字段整个比就永远判成"文件变了"。
    var sha256: String?

    /// 读源文件指纹失败的原因。
    enum ReadError: Equatable, Error {
        /// 文件已经不在这个位置上了。
        case missing
        /// 还在，但读不出来（权限、拔盘、IO 故障）。
        case unreadable(String)

        var message: String {
            switch self {
            case .missing: return "源文件已经不在这个位置"
            case .unreadable(let detail): return "源文件读不出来（\(detail)）"
            }
        }
    }

    /// 覆盖前复核的回答。
    enum Check: Equatable {
        /// 还是这次压缩所依据的那份字节。
        case unchanged
        /// 不是了，附人话说明。§25：此时**不许覆盖**，候选删掉，用户当前文件原样留着。
        case changed(String)
    }

    /// 快速指纹：打开文件问一次 `fstat()`，不读内容。
    ///
    /// 这里刻意不用 `stat()`：Swift 的 Darwin 叠加层里同名的 `stat` 结构体永远压过同名
    /// 函数（`Darwin.stat(path, &raw)` 会编成「调了一个不接受参数的调用」），只能用
    /// `open` + `fstat`。多出来的一次 open 不改判据——它和 Rust 侧的 `fs::metadata` 一样
    /// 跟着软链走，看的是同一份字节；差别只在"没读权限"的文件上：那一侧 Rust 能 stat、
    /// 这一侧 open 就失败，于是判成"读不出来"。这个方向只会**拒绝覆盖**，不会误盖。
    static func read(_ path: String) -> Result<SourceIdentity, ReadError> {
        let fd = Darwin.open(path, O_RDONLY)
        guard fd >= 0 else {
            return .failure(errno == ENOENT ? .missing : .unreadable(errorText()))
        }
        defer { Darwin.close(fd) }
        var raw = stat()
        guard Darwin.fstat(fd, &raw) == 0 else {
            return .failure(.unreadable(errorText()))
        }
        let seconds = Int64(raw.st_mtimespec.tv_sec)
        let nanos = Int64(raw.st_mtimespec.tv_nsec)
        return .success(Self(
            size: Int64(max(0, raw.st_size)),
            modified: seconds * 1_000_000_000 + nanos,
            deviceID: Int64(raw.st_dev),
            inode: raw.st_ino,
            sha256: nil
        ))
    }

    /// 快速指纹 + 内容指纹：§25 流程第一步，覆盖模式编码前钉下的那份依据。
    static func readWithHash(_ path: String) -> Result<SourceIdentity, ReadError> {
        switch read(path) {
        case .failure(let error): return .failure(error)
        case .success(var identity):
            switch sha256OfFile(path) {
            case .failure(let error): return .failure(error)
            case .success(let sha256):
                identity.sha256 = sha256
                return .success(identity)
            }
        }
    }

    /// §25 流程里那个"比较"：只看一次 `fstat()` 读得到的几项。
    func sameFingerprint(as other: SourceIdentity) -> Bool {
        size == other.size && modified == other.modified
            && deviceID == other.deviceID && inode == other.inode
    }

    /// 改名前最后一道快速门（§31 的 source identity check）：只 stat，不再读一遍内容。
    /// 读不出来 = 证明不了"还是原来那份"，一样不许盖，原因照实带出去。
    func fingerprintIntact(at path: String) -> Check {
        switch SourceIdentity.read(path) {
        case .failure(let error): return .changed(error.message)
        case .success(let current) where sameFingerprint(as: current): return .unchanged
        case .success(let current):
            return .changed("大小 \(size) → \(current.size) 字节，或修改时间/inode 变了")
        }
    }

    /// §25 + §26 的完整复核：先比 stat 三项，再比内容哈希。
    ///
    /// 顺序刻意是先便宜后贵；哈希那一轮只在算过指纹的任务上跑（`sha256 == nil` 的
    /// 调用方走到这里就等于"快速指纹一致"）。
    func verify(at path: String) -> Check {
        if case .changed(let detail) = fingerprintIntact(at: path) { return .changed(detail) }
        guard let expected = sha256 else { return .unchanged }
        switch sha256OfFile(path) {
        case .failure(let error): return .changed(error.message)
        case .success(let actual) where actual == expected: return .unchanged
        case .success(let actual):
            return .changed("字节内容已经不是同一份（\(expected) → \(actual)）")
        }
    }
}

/// 内容指纹。流式读：一张 200 MB 的 TIFF 不许整份进内存才算得出它是谁。
func sha256OfFile(_ path: String) -> Result<String, SourceIdentity.ReadError> {
    let fd = Darwin.open(path, O_RDONLY)
    if fd < 0 {
        return .failure(errno == ENOENT ? .missing : .unreadable(errorText()))
    }
    defer { Darwin.close(fd) }
    let capacity = 256 * 1024
    let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: capacity)
    defer { buffer.deallocate() }
    var hasher = SHA256()
    while true {
        let read = Darwin.read(fd, buffer, capacity)
        if read < 0 {
            // 信号打断（沙盒 / 休眠唤醒都会撞 EINTR）不是故障，接着读。
            if errno == EINTR { continue }
            return .failure(.unreadable(errorText()))
        }
        if read == 0 { break }
        hasher.update(data: Data(bytes: buffer, count: read))
    }
    return .success(hexString(Array(hasher.finalize())))
}

func errorText(domain: String = "") -> String {
    let code = errno
    let base = String(cString: strerror(code))
    return domain.isEmpty ? "\(base)（errno \(code)）" : "\(domain): \(base)（errno \(code)）"
}

func hexString(_ bytes: [UInt8]) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}

/// 内存字节的指纹：产物还没落盘就能先记进事务日志，
/// 启动恢复时靠它证明"源位置上这份字节是我们写出去的"。
func sha256OfData(_ data: Data) -> String {
    hexString(Array(SHA256.hash(data: data)))
}
