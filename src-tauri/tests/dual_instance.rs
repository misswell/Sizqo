//! T21（0.3.6-rc.11 外部审查）：两个**真实进程**同时争抢同一目录的输出名。
//!
//! 故障注入测试在本进程内伪造现场，代替不了两个独立进程在同一目录里同时
//! `O_EXCL` 认领、同时写入的真实竞争 —— inode 复用、TOCTOU、文件系统缓存
//! 这些只在跨进程时才成立。本文件用「自拉起」模式：
//! 父测试 spawn 本测试二进制两次（`SIZQO_DUAL_INSTANCE_ROLE` 选择角色），
//! 两个子进程各自独占认领候选名 → 写入自己的标记内容 → 校验最终磁盘状态：
//! 两份产物都在、内容各自完整、谁也没把对方的字节盖掉。

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static ROLE_MARKER: &str = "SIZQO_DUAL_INSTANCE_ROLE";
/// 每份产物的字节数：几 MB 让两个写入窗口真正重叠。
const PATTERN_LEN: usize = 4 * 1024 * 1024;

fn pattern(role: &str, len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    let tag = format!("[{role}]");
    while bytes.len() < len {
        let seed = bytes.len();
        bytes.extend_from_slice(&seed.to_le_bytes());
        bytes.extend_from_slice(tag.as_bytes());
    }
    bytes.truncate(len);
    bytes
}

/// 子进程入口：认领 → 写入 → fsync。角色决定往内容里写的标记。
fn worker(role: &str, directory: &std::path::Path) {
    use sizqo_lib::output_transaction::{OutputNameAllocator, OutputNaming};
    let naming = OutputNaming::WithSuffix {
        stem: "race".into(),
        suffix: "_out".into(),
        extension: "bin".into(),
    };
    let claimed = OutputNameAllocator::claim(directory, &naming)
        .unwrap_or_else(|error| panic!("{role}: 认领失败（O_EXCL 竞争不允许任何一方靠覆盖取胜）: {error}"));
    let payload = pattern(role, PATTERN_LEN);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(claimed.path())
        .expect("占位是我们 O_EXCL 建的，必定可写");
    file.write_all(&payload).unwrap();
    file.sync_all().unwrap();
    drop(file);
    // 路径交给父进程核对；ClaimedOutput 生命周期结束（句柄关闭），文件保留。
    let path_file = directory.join(format!(".sizqo-dual-{role}.path"));
    std::fs::write(&path_file, claimed.path().to_string_lossy().as_bytes()).unwrap();
}

#[ignore = "worker entrypoint: 由 two_instances_racing 通过环境变量点名将本二进制自拉起"]
#[test]
fn dual_instance_worker_entrypoint() {
    // 只在父进程以环境变量点名的身份下运行：普通 cargo test 里直接收敛。
    let Ok(role) = std::env::var(ROLE_MARKER) else {
        return;
    };
    let directory = std::env::var("SIZQO_DUAL_INSTANCE_DIR")
        .map(PathBuf::from)
        .expect("worker 需要 SIZQO_DUAL_INSTANCE_DIR");
    worker(&role, &directory);
}

#[test]
fn two_instances_racing_on_the_same_directory_never_clobber_each_other() {
    // 集成测试跑在 tests/ 下：库以 sizqo_lib 名字链接。
    let directory = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let started = AtomicUsize::new(0);
    let mut children = Vec::new();
    for role in ["alpha", "beta"] {
        let mut command = Command::new(&exe);
        command
            .arg("dual_instance_worker_entrypoint")
            .arg("--exact")
            .arg("--ignored")
            .arg("--nocapture")
            .env(ROLE_MARKER, role)
            .env("SIZQO_DUAL_INSTANCE_DIR", directory.path());
        children.push((role, command));
        started.fetch_add(1, Ordering::SeqCst);
    }
    // 同时拉起：两个子进程几乎同时开始 O_EXCL 竞争。
    let mut handles = Vec::new();
    for (role, mut command) in children {
        handles.push((role, std::thread::spawn(move || command.status())));
    }
    for (role, handle) in handles {
        let status = handle
            .join()
            .expect("spawn 线程不该 panic")
            .expect("子进程启动失败");
        assert!(status.success(), "{role} worker 退出码非零: {status}");
    }
    assert_eq!(started.load(Ordering::SeqCst), 2);

    // 两个角色各自留下路径凭证。
    let mut seen_paths = Vec::new();
    for role in ["alpha", "beta"] {
        let path_file = directory.path().join(format!(".sizqo-dual-{role}.path"));
        let recorded = std::fs::read_to_string(&path_file)
            .unwrap_or_else(|error| panic!("{role} 没有留下产物路径凭证: {error}"));
        seen_paths.push((role, PathBuf::from(recorded.trim().to_string())));
    }
    // 独占认领：两份产物必须在不同候选名上。
    assert_ne!(
        seen_paths[0].1, seen_paths[1].1,
        "两个实例各自 O_EXCL 认领，绝不允许落在同一个名字上"
    );
    // 各自内容完整：谁也没把对方的字节盖掉。
    for (role, path) in &seen_paths {
        let expected = pattern(role, PATTERN_LEN);
        let actual = std::fs::read(path).unwrap_or_else(|error| panic!("{role} 的产物读不出来: {error}"));
        assert_eq!(
            actual, expected,
            "{role} 的产物内容被破坏 —— 跨进程竞争出现了覆盖"
        );
    }
    // 目录里没有第三份来历不明的产物。
    let outputs: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| name.starts_with("race_out"))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(outputs.len(), 2, "目录里只有两份产物，没有多余的覆盖残骸");
}
