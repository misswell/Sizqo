//! Direct only. Typed argv, bounded reads, deadlines, child reaping.
use std::{path::{Path,PathBuf}, process::{Command,Stdio}, io::Read, time::{Duration,Instant}};
pub fn tool(name: &str) -> Result<PathBuf,String> {
    // Windows 上带扩展名的才是那个文件：只查 "ffmpeg" 会永远找不到，
    // 于是把装好的内置后端报成"不可用"（图片线的 find_tool 早就处理了这件事）。
    let file_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    // 「随包了但本机跑不动」和「根本没随包」是两条不同的结论，必须分开说：
    // 前者要的是重新投递对应切片，后者要的是补打包步骤。合成一句会把排查带偏。
    let mut present_but_unrunnable = false;
    let exe = std::env::current_exe().map_err(|e|e.to_string())?;
    if let Some(macos) = exe.parent() {
        let bundled = macos.join("../Resources/video/bin").join(&file_name);
        // 内置候选必须先是"这台机器跑得动"的格式：仓库里躺着的是 macOS 产物，
        // 在别的平台上把它交出去，能力矩阵就已经在说谎，编码时才炸 Exec format error。
        if bundled.is_file() {
            if crate::binfmt::runnable_here(&bundled) { return Ok(bundled); }
            present_but_unrunnable = true;
        }
        // Bundled applications must never silently depend on the host's PATH.
        if macos.file_name().and_then(|s|s.to_str()) == Some("MacOS") {
            return Err(if present_but_unrunnable {
                format!("Sizqo bundle's video/bin/{name} cannot run on this machine (platform or CPU slice mismatch)")
            } else {
                format!("Sizqo bundle has no runnable video/bin/{name}")
            });
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/video/bin").join(&file_name);
    if dev.is_file() {
        if crate::binfmt::runnable_here(&dev) { return Ok(dev); }
        present_but_unrunnable = true;
    }
    let paths = std::env::var_os("PATH").unwrap_or_default();
    crate::binfmt::system_tool_in(std::env::split_paths(&paths), &file_name)
        .ok_or_else(||if present_but_unrunnable {
            format!("{name} unavailable: the bundled engine does not match this platform or CPU; ship the matching slice")
        } else {
            format!("{name} unavailable; build the bundled video runtime")
        })
}
pub fn command(name: &str) -> Result<Command,String> {
    let path=tool(name)?;
    let mut cmd=Command::new(&path);
    if let Some(bin)=path.parent() {
        let lib=bin.join("../lib");
        if lib.is_dir() { cmd.env("DYLD_FALLBACK_LIBRARY_PATH",lib); }
    }
    cmd.stdin(Stdio::null()); Ok(cmd)
}
pub fn drain(mut input: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut saved=Vec::new(); let mut chunk=[0u8;8192];
    loop { let n=input.read(&mut chunk)?; if n==0 { break; }
        let take=n.min(limit.saturating_sub(saved.len())); saved.extend_from_slice(&chunk[..take]); }
    Ok(saved)
}
/// 旧签名兼容包装：无取消令牌的探测（启动恢复等后台维护用）。
pub fn capture(mut cmd: Command, timeout: Duration, limit: usize) -> Result<Vec<u8>,String> {
    capture_cancellable(cmd,timeout,limit,None)
}
/// 带**取消令牌**的探测（本轮加固 P2-10 / T16）：ffprobe 的 45s/300s 窗口里
/// 用户点取消必须立即生效 —— 轮询令牌、SIGTERM 先礼后兵、两秒后强杀回收，
/// 与编码用的 `run_command` 同一条纪律。stdout/stderr 读取线程随 join 回收。
pub fn capture_cancellable(mut cmd: Command, timeout: Duration, limit: usize, cancel: Option<&std::sync::atomic::AtomicBool>) -> Result<Vec<u8>,String> {
    use std::sync::atomic::Ordering;
    let mut child=cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e|e.to_string())?;
    let stdout=child.stdout.take().ok_or("stdout unavailable")?;
    let stderr=child.stderr.take().ok_or("stderr unavailable")?;
    let out=std::thread::spawn(move||drain(stdout,limit));
    let err=std::thread::spawn(move||drain(stderr,65536));
    let start=Instant::now();
    let status=loop {
        if let Some(token)=cancel {
            if token.load(Ordering::Acquire) {
                // 先 SIGTERM 给 ffprobe 一个体面收场，两秒内不退再强杀 ——
                // 与 run_command 的取消路径一致，不留僵尸子进程。
                #[cfg(unix)]
                unsafe{libc::kill(child.id() as i32,libc::SIGTERM);}
                #[cfg(not(unix))]
                { let _=child.kill(); }
                let deadline=Instant::now()+Duration::from_secs(2);
                loop {
                    match child.try_wait() { Ok(Some(_))=>break, Ok(None)=>{}, Err(_)=>break }
                    if Instant::now()>deadline { let _=child.kill(); let _=child.wait(); break; }
                    std::thread::sleep(Duration::from_millis(30));
                }
                let _=child.wait();
                break Err("Cancelled by user".into());
            }
        }
        match child.try_wait() {
            Ok(Some(s))=>break Ok(s), Ok(None)=>{}, Err(e)=>{ let _=child.kill(); let _=child.wait(); break Err(e.to_string()); }
        }
        if start.elapsed()>timeout { let _=child.kill(); let _=child.wait(); break Err("Media operation timed out".into()); }
        std::thread::sleep(Duration::from_millis(30));
    };
    let output=out.join().map_err(|_|"stdout reader failed")?.map_err(|e|e.to_string())?;
    let errors=err.join().map_err(|_|"stderr reader failed")?.map_err(|e|e.to_string())?;
    if !status?.success() { return Err(String::from_utf8_lossy(&errors).chars().take(4096).collect()); }
    if output.len()>=limit { return Err("Media metadata exceeds safety limit".into()); }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// T16（本轮加固 P2-10）：ffprobe/ffmpeg 探测阶段点取消 → 子进程立即终止回收，
    /// 返回明确的取消错误，绝不等到超时。
    #[test]
    fn capture_cancellable_terminates_the_child_on_cancel() {
        let token=std::sync::Arc::new(AtomicBool::new(false));
        let token_for_thread=token.clone();
        let killer=std::thread::spawn(move||{
            std::thread::sleep(Duration::from_millis(150));
            token_for_thread.store(true,Ordering::Release);
        });
        let mut cmd=if cfg!(windows){
            let mut c=Command::new("ping");c.args(["-n","30","127.0.0.1"]);c
        }else{
            let mut c=Command::new("sleep");c.arg("30");c
        };
        let started=Instant::now();
        let outcome=capture_cancellable(cmd,Duration::from_secs(60),1024*1024,Some(&token));
        killer.join().unwrap();
        assert!(started.elapsed()<Duration::from_secs(10),"取消必须立即生效，不能等满超时");
        let error=outcome.expect_err("取消是一次明确的收场");
        assert!(error.contains("Cancelled"),"{error}");
    }

    /// 解析器不许把"本机跑不动的内置产物"当后端交出去。Linux/Windows 上这一条就是
    /// `Exec format error (os error 8)` 的正面防线：内置那一步必须被跳过。
    #[test]
    fn a_bundled_engine_this_os_cannot_exec_is_never_offered() {
        let dev=Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/video/bin");
        for name in ["ffmpeg","ffprobe"] {
            let file_name=if cfg!(windows){format!("{name}.exe")}else{name.to_string()};
            let candidate=dev.join(&file_name);
            if !(candidate.is_file() && !crate::binfmt::runnable_here(&candidate)) { continue }
            if let Ok(resolved)=tool(name) {
                assert_ne!(resolved,candidate,"{name}: 本机跑不了的内置候选被当成了后端");
                assert!(crate::binfmt::runnable_here(&resolved),"{name}: 解析结果本机也跑不了：{}",resolved.display());
            }
        }
    }

    /// 上面那条在 macOS 上是空跑的（内置产物本来就跑得动），所以正向结论必须有
    /// 这条独立的门背书：格式闸门不能把自家内置 ffmpeg 一起拒掉 ——
    /// 那会让视频线在 Direct 主平台上静默变成"引擎不可用"，而单元测试照样全绿。
    /// 只问"这台机器跑得动的内置产物有没有被端出去"；跑不动的那一半由上面那条管，
    /// 两条合起来在任何平台都不空判。
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_macos_bundled_engine_is_still_the_resolution() {
        let dev=Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/video/bin/ffmpeg");
        if dev.is_file() && crate::binfmt::runnable_here(&dev) {
            assert_eq!(tool("ffmpeg").ok().as_deref(),Some(dev.as_path()),"macOS 上跑得动的内置 ffmpeg 必须仍是解析结果");
        }
    }

    /// 「随包了但本机跑不动」要说清是切片/平台不匹配，而不是含糊的"build the bundled
    /// runtime"：Intel mac 拿到 arm64 单切片时，前一句指向上游投递问题，后一句会把人
    /// 带去重跑打包 —— 两条结论的排查方向完全不同。
    #[test]
    fn an_incompatible_bundled_engine_names_the_half_that_failed() {
        let dev=Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/video/bin")
            .join(if cfg!(windows) {"ffmpeg.exe"} else {"ffmpeg"});
        if !dev.is_file() { return } // 没随包（打包前的工作区）时这条对照不适用
        match tool("ffmpeg") {
            Ok(resolved)=>assert!(crate::binfmt::runnable_here(&resolved),"解析结果本机跑不了：{}",resolved.display()),
            Err(reason)=>{
                assert!(!crate::binfmt::runnable_here(&dev),"内置候选跑得动却报错：{reason}");
                assert!(reason.contains("does not match"),"跑不动的内置产物必须说明是平台/切片不匹配: {reason}");
            }
        }
    }
}
