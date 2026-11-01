use serde::{Deserialize,Serialize};
pub const NATIVE_UNAVAILABLE:&str="Sizqo App Store video encoding is unavailable in this build. No third-party CLI is launched in the sandbox. Use the Direct edition; native video support requires validation before enabling.";
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct EncoderCapability {pub id:String,pub label:String,pub modes:Vec<String>,pub presets:Vec<String>}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct CapabilityMatrix {pub backend:String,pub available:bool,pub reason:Option<String>,pub encoders:Vec<EncoderCapability>,pub muxers:Vec<String>,pub audio_encoders:Vec<String>,pub diagnostics:Vec<String>,
    /// 「保留源时间戳」这个开关在 FFmpeg 5.0 改过名：新版只认 `-fps_mode`，发行版里的
    /// 4.4 只认 `-vsync`，而 FFmpeg 9 已经把 `-vsync` **删掉**了（实测 `Unrecognized
    /// option 'vsync'`）。上一轮 Linux CI 就是被硬编码的 `-fps_mode` 拦在编码中途。
    /// 名字由引擎自己回答 —— 一次真试用，不猜版本号。空串 = 两种都不认，
    /// 那时 §5.3 的 passthrough 无法兑现，必须在编码前拒绝而不是默默换别的语义。
    #[serde(default)] pub frame_timing_flag:String}
impl CapabilityMatrix {
    pub fn encoder(&self,id:&str)->Option<&EncoderCapability>{self.encoders.iter().find(|e|e.id==id)}
    pub fn unavailable(reason:String)->Self{Self {backend:"unavailable".into(),available:false,reason:Some(reason),encoders:vec![],muxers:vec![],audio_encoders:vec![],diagnostics:vec![],frame_timing_flag:String::new()}}
}
#[cfg(feature="inproc-backends")]
pub fn capabilities()->CapabilityMatrix{CapabilityMatrix::unavailable(NATIVE_UNAVAILABLE.into())}
#[cfg(not(feature="inproc-backends"))]
pub fn capabilities()->CapabilityMatrix{
    // 只固化成功的探测结果；失败的（启动时磁盘抖动 / bundle 尚未就绪）不留终身缓存，
    // 下次调用重试 —— 一次性失败把视频判死一整个会话，比多跑一次探测更伤。
    // 判定方向不变：探测失败只会得到 unavailable，绝不会被判成"支持"。
    static CACHE:std::sync::Mutex<Option<CapabilityMatrix>>=std::sync::Mutex::new(None);
    let mut slot=CACHE.lock().unwrap_or_else(|poisoned|poisoned.into_inner());
    if let Some(cached)=slot.as_ref(){if cached.available{return cached.clone();}}
    let discovered=discover().unwrap_or_else(CapabilityMatrix::unavailable);
    if discovered.available{*slot=Some(discovered.clone());}
    discovered
}
#[cfg(not(feature="inproc-backends"))]
fn discover()->Result<CapabilityMatrix,String>{
    use super::process::{command,capture};use std::time::Duration;
    let mut cmd=command("ffmpeg")?;cmd.args(["-hide_banner","-encoders"]);
    let enc=String::from_utf8_lossy(&capture(cmd,Duration::from_secs(20),2*1024*1024)?).into_owned();
    let mut cmd=command("ffmpeg")?;cmd.args(["-hide_banner","-muxers"]);
    let mux=String::from_utf8_lossy(&capture(cmd,Duration::from_secs(20),1024*1024)?).into_owned();
    let exists=|name:&str|enc.lines().any(|s|s.split_whitespace().nth(1)==Some(name));
    let mut encoders=vec![];let mut diagnostics=vec![];
    for (id,label) in [("h264_videotoolbox","H.264 · Apple hardware"),("hevc_videotoolbox","HEVC · Apple hardware"),("libx264","H.264 · software"),("libx265","HEVC · software"),("libsvtav1","AV1 · software"),("libvpx-vp9","VP9 · software")] {
        if !exists(id) {diagnostics.push(format!("{id}: not compiled into FFmpeg"));continue;}
        let mut trial=command("ffmpeg")?;
        trial.args(["-hide_banner","-v","error","-nostdin","-f","lavfi","-i","color=c=black:s=128x128:r=1","-frames:v","1","-threads","1","-c:v",id]);
        if id.ends_with("videotoolbox"){trial.args(["-allow_sw","0","-b:v","500k"]);}
        else if id=="libsvtav1"{trial.args(["-preset","12","-svtav1-params","lp=1"]);}
        else if id=="libvpx-vp9"{trial.args(["-deadline","good","-cpu-used","5","-crf","30","-b:v","0","-row-mt","1"]);}
        else if id=="libx265"{trial.args(["-preset","ultrafast","-x265-params","pools=1:frame-threads=1"]);}
        trial.args(["-pix_fmt","yuv420p","-f","null","-"]);
        match capture(trial,Duration::from_secs(30),65536){
            Ok(_)=>{let hardware=id.ends_with("videotoolbox");let two_pass=id=="libx264"||id=="libvpx-vp9";
                let mut modes=vec!["abr".into()];if !hardware {modes.push("crf".into());} if two_pass {modes.extend(["twoPass".into(),"targetSize".into()]);}
                encoders.push(EncoderCapability{id:id.into(),label:label.into(),modes,presets:if hardware {vec![]} else {vec!["ultrafast".into(),"veryfast".into(),"medium".into(),"slow".into()]}});},
            Err(e)=>diagnostics.push(format!("{id}: trial failed: {e}")),
        }
    }
    let muxers=[("mp4","mp4"),("mov","mov"),("mkv","matroska"),("webm","webm"),("ts","mpegts")].iter().filter(|(_,real)|mux.lines().any(|s|s.split_whitespace().nth(1)==Some(*real))).map(|(ui,_)|ui.to_string()).collect();
    let audio_encoders=["aac","libopus","libmp3lame","flac"].iter().filter(|s|exists(s)).map(|s|s.to_string()).collect();
    let frame_timing_flag=probe_frame_timing_flag();
    if frame_timing_flag.is_empty(){diagnostics.push("frame timing: neither -fps_mode nor -vsync is honored".into());}
    Ok(CapabilityMatrix{backend:"ffmpeg".into(),available:!encoders.is_empty(),reason:if encoders.is_empty(){Some("No encoder passed the runtime trial".into())}else{None},encoders,muxers,audio_encoders,diagnostics,frame_timing_flag})
}

/// 一次真试用决定"保留源时间戳"的开关叫什么：lavfi 黑场 → `rawvideo` → `-f null`。
/// 选 `rawvideo` 是因为它在任何 FFmpeg 构建里都在，这条探测因此不会被编码器差异污染；
/// 顺序先新后旧（FFmpeg 9 已经没有 `-vsync`）。两种都失败时返回空串，由 `plan` 在
/// 编码前拒绝 —— 语义换成"重排时间轴"不是可用的兜底，那是另一件事。
#[cfg(not(feature="inproc-backends"))]
fn probe_frame_timing_flag()->String{
    use super::process::{command,capture};use std::time::Duration;
    for flag in ["-fps_mode","-vsync"] {
        if let Ok(mut cmd)=command("ffmpeg"){
            cmd.args(["-hide_banner","-v","error","-nostdin","-f","lavfi","-i","color=c=black:s=128x128:r=1","-frames:v","1","-threads","1","-c:v","rawvideo",flag,"passthrough","-f","null","-"]);
            if capture(cmd,Duration::from_secs(30),65536).is_ok(){return flag.to_string();}
        }
    }
    String::new()
}
