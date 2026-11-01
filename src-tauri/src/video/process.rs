//! Direct only. Typed argv, bounded reads, deadlines, child reaping.
use std::{path::{Path,PathBuf}, process::{Command,Stdio}, io::Read, time::{Duration,Instant}};
pub fn tool(name: &str) -> Result<PathBuf,String> {
    // Windows 上带扩展名的才是那个文件：只查 "ffmpeg" 会永远找不到，
    // 于是把装好的内置后端报成"不可用"（图片线的 find_tool 早就处理了这件事）。
    let file_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let exe = std::env::current_exe().map_err(|e|e.to_string())?;
    if let Some(macos) = exe.parent() {
        let bundled = macos.join("../Resources/video/bin").join(&file_name);
        // 内置候选必须先是"这台机器跑得动"的格式：仓库里躺着的是 macOS 产物，
        // 在别的平台上把它交出去，能力矩阵就已经在说谎，编码时才炸 Exec format error。
        if bundled.is_file() && crate::binfmt::runnable_here(&bundled) { return Ok(bundled); }
        // Bundled applications must never silently depend on the host's PATH.
        if macos.file_name().and_then(|s|s.to_str()) == Some("MacOS") {
            return Err(format!("Sizqo bundle has no runnable video/bin/{name}"));
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/video/bin").join(&file_name);
    if dev.is_file() && crate::binfmt::runnable_here(&dev) { return Ok(dev); }
    let paths = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&paths).map(|p|p.join(&file_name))
        .find(|p|p.is_file() && crate::binfmt::runnable_here(p))
        .ok_or_else(||format!("{name} unavailable; build the bundled video runtime"))
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
pub fn capture(mut cmd: Command, timeout: Duration, limit: usize) -> Result<Vec<u8>,String> {
    let mut child=cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e|e.to_string())?;
    let stdout=child.stdout.take().ok_or("stdout unavailable")?;
    let stderr=child.stderr.take().ok_or("stderr unavailable")?;
    let out=std::thread::spawn(move||drain(stdout,limit));
    let err=std::thread::spawn(move||drain(stderr,65536));
    let start=Instant::now();
    let status=loop {
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
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_macos_bundled_engine_is_still_the_resolution() {
        let dev=Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/video/bin/ffmpeg");
        if dev.is_file() {
            assert_eq!(tool("ffmpeg").ok().as_deref(),Some(dev.as_path()),"macOS 上内置 ffmpeg 必须仍是解析结果");
        }
    }
}
