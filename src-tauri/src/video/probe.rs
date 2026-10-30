use serde::{Deserialize,Serialize};
use serde_json::Value;
use std::path::Path;
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct VideoStream {
    pub index: u32, pub codec_type: String, pub codec_name: String,
    pub width: u32, pub height: u32, pub pixel_format: String,
    pub bit_rate: Option<u64>, pub channels: u32,
    /// §56 采样率：音频转换后必须与原轨一致，不能只看声道数。
    #[serde(default)] pub sample_rate: u32,
    pub frame_rate: Option<f64>,
    pub time_base: String, pub color_transfer: String, pub color_primaries: String,
    pub color_space: String, pub field_order: String, pub tags: Value,
    pub disposition: Value, pub side_data: Value, pub raw: Value,
}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct VideoProbe {
    pub duration_us: Option<u64>, pub streams: Vec<VideoStream>, pub format: String,
    /// §55 全局 metadata：校验 StripPrivate / StripAll 是否真的去了该去的东西。
    #[serde(default)] pub format_tags: Value,
    pub size_bytes: u64, pub rotation: i32, pub hdr_kind: String,
    pub color_metadata: Value, pub chapters: Vec<Value>, pub warnings: Vec<String>,
    pub width: u32, pub height: u32, pub frame_rate: Option<f64>, pub bit_rate: Option<u64>,
    pub variable_frame_rate: bool,
    /// §6 program 级 metadata 的载体。校验隐私清理时必须遍历到这里。
    #[serde(default)] pub programs: Vec<Value>,
}
fn text(v:&Value,k:&str)->String {v[k].as_str().unwrap_or("").into()}
fn uint(v:&Value,k:&str)->u64 { v[k].as_u64().or_else(||v[k].as_str()?.parse().ok()).unwrap_or(0) }
pub fn rational(s:&str)->Option<f64> { let (a,b)=s.split_once('/')?; let n=a.parse::<f64>().ok()?;let d=b.parse::<f64>().ok()?;
    let r=n/d; if r.is_finite()&&r>0.0 {Some(r)}else{None} }
impl VideoStream {
    /// MP4/MOV 的章节轨道在 ffprobe 里是 `data` 流，样本项标签是 `text`。
    /// 它是 `-map_chapters` 的载体，不是需要独立保留的数据轨；时间码（`tmcd`）等其它 data 流仍要拒绝。
    pub fn is_chapter_carrier(&self)->bool {
        self.codec_type=="data"&&self.raw["codec_tag_string"].as_str().is_some_and(|tag|tag.eq_ignore_ascii_case("text"))
    }
}
impl VideoProbe {
    pub fn video(&self)->Option<&VideoStream> { self.streams.iter().find(|s|s.codec_type=="video"&&s.disposition["attached_pic"].as_u64()!=Some(1)) }
    pub fn audio(&self)->impl Iterator<Item=&VideoStream> {self.streams.iter().filter(|s|s.codec_type=="audio")}
    pub fn subtitles(&self)->impl Iterator<Item=&VideoStream> {self.streams.iter().filter(|s|s.codec_type=="subtitle")}
    pub fn parse(root:Value,size:u64)->Result<Self,String> {
        let mut streams=Vec::new();
        for s in root["streams"].as_array().ok_or("No streams in media")? {
            streams.push(VideoStream {index:uint(s,"index") as u32,codec_type:text(s,"codec_type"),codec_name:text(s,"codec_name"),
                width:uint(s,"width") as u32,height:uint(s,"height") as u32,pixel_format:text(s,"pix_fmt"),
                bit_rate:Some(uint(s,"bit_rate")).filter(|n|*n>0),channels:uint(s,"channels") as u32,
                sample_rate:uint(s,"sample_rate") as u32,
                frame_rate:rational(&text(s,"avg_frame_rate")),time_base:text(s,"time_base"),color_transfer:text(s,"color_transfer"),
                color_primaries:text(s,"color_primaries"),color_space:text(s,"color_space"),field_order:text(s,"field_order"),
                tags:s["tags"].clone(),disposition:s["disposition"].clone(),side_data:s["side_data_list"].clone(),raw:s.clone() });
        }
        let video=streams.iter().find(|s|s.codec_type=="video"&&s.disposition["attached_pic"].as_u64()!=Some(1)).ok_or("No playable video stream")?;
        if video.width==0||video.height==0 {return Err("Invalid video dimensions".into());}
        let duration=root["format"]["duration"].as_str().and_then(|s|s.parse::<f64>().ok())
            .or_else(||video.raw["duration"].as_str()?.parse::<f64>().ok());
        let duration_us=duration.filter(|s|s.is_finite()&&*s>0.0&&*s<(u64::MAX as f64/1e6)).map(|s|(s*1e6) as u64);
        let side=video.side_data.to_string().to_lowercase();
        // §5.2 HDR 由传递函数与动态元数据判定：PQ / HLG / Dolby Vision / HDR10+。
        // BT.2020 原色只表示色彩原色 —— 把它当 HDR 是误判，把它当 BT.709 静默转换
        // 是错误；它由能力分类器按"色彩空间未验证"单独拒绝。
        let hdr_kind=if side.contains("dovi")||side.contains("dolby") {"dolbyVision"}
            else if side.contains("hdr10+")||side.contains("smpte2094") {"hdr10Plus"}
            else if video.color_transfer=="smpte2084" {"hdr10"}
            else if video.color_transfer=="arib-std-b67" {"hlg"}
            else if side.contains("mastering display") {"hdr"}
            else {"sdr"};
        let rotation=video.side_data.as_array().and_then(|a|a.iter().find_map(|d|d["rotation"].as_i64()))
            .or_else(||video.tags["rotate"].as_str()?.parse().ok()).unwrap_or(0) as i32;
        let mut warnings=vec![];
        let variable_frame_rate=matches!((rational(&text(&video.raw,"r_frame_rate")),video.frame_rate),(Some(r),Some(a)) if r!=a);
        if variable_frame_rate { warnings.push("Variable frame rate detected; original timestamps are preserved (passthrough) unless a frame rate is selected".into()); }
        let color_metadata=serde_json::json!({"transfer":video.color_transfer,"primaries":video.color_primaries,"space":video.color_space,"pixelFormat":video.pixel_format});
        let (width,height,frame_rate)=(video.width,video.height,video.frame_rate);
        Ok(Self {duration_us,streams,format:text(&root["format"],"format_name"),format_tags:root["format"]["tags"].clone(),size_bytes:size,rotation,hdr_kind:hdr_kind.into(),color_metadata,
            chapters:root["chapters"].as_array().cloned().unwrap_or_default(),
            programs:root["programs"].as_array().cloned().unwrap_or_default(),
            warnings,width,height,frame_rate,
            bit_rate:Some(uint(&root["format"],"bit_rate")).filter(|n|*n>0),variable_frame_rate})
    }
}
#[cfg(not(feature="inproc-backends"))]
#[cfg(not(feature="inproc-backends"))]
pub fn probe(path:&Path)->Result<VideoProbe,String>{probe_with_timeout(path,std::time::Duration::from_secs(45))}
/// 编码产物的复探走这里：编码本身可能刚跑了几分钟，慢卷上对同一份大文件再探一遍
/// 不该被导入档的 45 秒掐死 —— 超时会把一次完整成功的编码整体作废。
#[cfg(not(feature="inproc-backends"))]
pub fn probe_with_timeout(path:&Path,timeout:std::time::Duration)->Result<VideoProbe,String>{
    let path=path.canonicalize().map_err(|e|e.to_string())?;
    let size=std::fs::metadata(&path).map_err(|e|e.to_string())?.len();
    if size==0 {return Err("Empty file".into());}
    let mut cmd=super::process::command("ffprobe")?;
    // §6 program 也要采：隐私校验必须遍历得到 program 级 metadata。
    cmd.args(["-v","error","-protocol_whitelist","file,pipe","-show_format","-show_streams","-show_chapters","-show_programs","-of","json"]).arg(path);
    let out=super::process::capture(cmd,timeout,8*1024*1024)?;
    VideoProbe::parse(serde_json::from_slice(&out).map_err(|e|e.to_string())?,size)
}
#[cfg(feature="inproc-backends")]
pub fn probe(_path:&Path)->Result<VideoProbe,String> {Err(super::capabilities::NATIVE_UNAVAILABLE.into())}
#[cfg(feature="inproc-backends")]
pub fn probe_with_timeout(_path:&Path,_timeout:std::time::Duration)->Result<VideoProbe,String> {Err(super::capabilities::NATIVE_UNAVAILABLE.into())}
#[cfg(test)] mod tests { use super::*;
    #[test] fn hdr_rotation_tracks_are_not_lost() { let p=VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":1080,"height":1920,"pix_fmt":"yuv420p10le","color_transfer":"smpte2084","side_data_list":[{"rotation":90}]},{"index":1,"codec_type":"audio","codec_name":"aac","channels":6}],"format":{"duration":"4.5","format_name":"mov"}}),123).unwrap();assert_eq!(p.duration_us,Some(4500000));assert_eq!(p.hdr_kind,"hdr10");assert_eq!(p.rotation,90);assert_eq!(p.audio().count(),1); }
    #[test] fn invalid_video_is_rejected() {assert!(VideoProbe::parse(serde_json::json!({"streams":[]}),0).is_err());assert_eq!(rational("0/0"),None);}
    #[test] fn variable_frame_rate_is_detected() {
        let p=VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":720,"height":1280,"pix_fmt":"yuv420p","avg_frame_rate":"130500/9191","r_frame_rate":"89/6"}],"format":{"duration":"30.636667","format_name":"mov"}}),1).unwrap();
        assert!(p.variable_frame_rate);assert_eq!(p.frame_rate,rational("130500/9191"));
        let cfr=VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":720,"height":1280,"pix_fmt":"yuv420p","avg_frame_rate":"30/1","r_frame_rate":"30/1"}],"format":{"duration":"10","format_name":"mp4"}}),1).unwrap();
        assert!(!cfr.variable_frame_rate);
    }

    /// §5.2 BT.2020 原色只表示色彩原色，不再单独认定 HDR —— 归类交给能力分类器。
    #[test] fn bt2020_primaries_alone_are_not_hdr() {
        let p=VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":720,"height":1280,"pix_fmt":"yuv420p","avg_frame_rate":"30/1","color_primaries":"bt2020","color_transfer":"bt709"}],"format":{"duration":"10","format_name":"mp4"}}),1).unwrap();
        assert_eq!(p.hdr_kind,"sdr","BT.2020 SDR 不许被当成 HDR");
    }

    /// §6 program 采集：-show_programs 的结果必须落进 probe。
    #[test] fn programs_are_captured() {
        let p=VideoProbe::parse(serde_json::json!({
            "streams":[{"index":0,"codec_type":"video","width":720,"height":1280,"pix_fmt":"yuv420p","avg_frame_rate":"30/1"}],
            "format":{"duration":"10","format_name":"mp4"},
            "programs":[{"program_id":1,"program_num":1,"tags":{"service_name":"News"}}]}),1).unwrap();
        assert_eq!(p.programs.len(),1);
        assert_eq!(p.programs[0]["tags"]["service_name"],"News");
    }
}
