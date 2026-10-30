//! Closed parameter contract: every selectable field is validated before argv generation.
use super::{VideoOptions,MetadataPolicy,capabilities::CapabilityMatrix,format_classifier,probe::VideoProbe};
use serde::Serialize;
use std::path::Path;
#[derive(Debug,Clone,Serialize)]#[serde(rename_all="camelCase")]
pub struct EncodePlan {pub encoder:String,pub container:String,pub bitrate_kbps:Option<u64>,pub crf:Option<f64>,pub width:u32,pub height:u32,pub passes:u32,pub threads:usize,pub warnings:Vec<String>,
    /// §55 已解析的策略；序列化给前端时仍看 `options.metadataPolicy`，这里不额外暴露一份。
    #[serde(skip)] pub metadata_policy:MetadataPolicy,
    pub options:VideoOptions}
fn member(value:&str,allowed:&[&str],what:&str)->Result<(),String>{if allowed.contains(&value){Ok(())}else{Err(format!("Invalid {what}: {value}"))}}
pub fn build(options:&VideoOptions,probe:&VideoProbe,caps:&CapabilityMatrix,budget:usize)->Result<EncodePlan,String>{
    if !caps.available{return Err(caps.reason.clone().unwrap_or("Video unavailable".into()));}
    if !caps.muxers.contains(&options.container){return Err("Output container unavailable".into());}
    member(&options.output_mode,&["suffix","directory","folder","replace"],"output mode")?;
    member(&options.audio_policy,&["copy","aac","opus","mp3","flac","none"],"audio policy")?;
    member(&options.subtitle_policy,&["copy","drop"],"subtitle policy")?;
    let metadata_policy=MetadataPolicy::parse(&options.metadata_policy).ok_or_else(||format!("Invalid metadata policy: {}",options.metadata_policy))?;
    if options.output_mode=="directory"||options.output_mode=="folder" {if options.output_dir.as_deref().filter(|s|!s.is_empty()).is_none(){return Err("Select an output directory".into());}}
    let video=probe.video().ok_or("No video stream")?;
    // §5.1 §5.2 位深 / Alpha / 浮点 / HDR / 隔行 / BT.2020-SDR 全部由能力分类器
    // 按白名单与传递函数判定 —— 不再靠子串猜。拒绝原因照实说明，绝不静默转换。
    format_classifier::classify(
        &video.pixel_format,
        video.color_transfer.as_str(),
        video.color_primaries.as_str(),
        &probe.hdr_kind,
        video.field_order.as_str(),
    )
    .support_level
    .map_err(|reason| format!("{reason} ({})", video.pixel_format))?;
    if probe.streams.iter().filter(|s|s.codec_type=="video"&&s.disposition["attached_pic"].as_u64()!=Some(1)).count()!=1{return Err("Multiple video streams cannot be silently discarded".into());}
    if probe.streams.iter().any(|s|s.codec_type=="video"&&s.disposition["attached_pic"].as_u64()==Some(1)){return Err("Attached picture preservation is not yet validated".into());}
    if probe.audio().count()>16{return Err("More than 16 audio tracks require manual review".into());}
    if probe.streams.iter().any(|s|s.codec_type=="data"&&!s.is_chapter_carrier()){return Err("Input contains data tracks whose preservation is not validated".into());}
    if probe.streams.iter().any(|s|s.codec_type=="attachment")&&options.container!="mkv"{return Err("Attachments require MKV; no silent discarding".into());}
    if !probe.chapters.is_empty()&&metadata_policy!=MetadataPolicy::StripAll&&["webm","ts"].contains(&options.container.as_str()){return Err("Chapter preservation in WebM/TS is unavailable; choose MKV".into());}
    let mut warnings=probe.warnings.clone();
    // §5.3 帧时间轴的默认行为：保留源 PTS（passthrough）。`r_frame_rate != avg_frame_rate`
    // 不能严格判定 VFR，更不是把时间轴重排成平均帧率的理由；只有用户明确指定
    // 帧率时才执行 CFR 转换。
    if probe.variable_frame_rate && options.frame_rate.is_none(){
        warnings.push("Variable-frame-rate source: original timestamps are preserved (passthrough)".into());
    }
    if metadata_policy==MetadataPolicy::StripPrivate { warnings.push("Privacy mode removes GPS, location and device-model tags; ordinary metadata and chapters are preserved".into()); }
    let encoder=if options.encoder=="auto" {
        let preference=if options.container=="webm"{vec!["libsvtav1","libvpx-vp9"]}else if options.mode=="twoPass"||options.mode=="targetSize"{vec!["libx264"]}else if options.mode=="crf"{vec!["libx265","libx264","libvpx-vp9"]}else{vec!["hevc_videotoolbox","libx265","h264_videotoolbox","libx264"]};
        let selected=preference.iter().find(|id|caps.encoder(id).is_some()).ok_or("No suitable encoder available")?;
        if *selected!=preference[0]{warnings.push(format!("Automatic encoder fallback: {} → {}",preference[0],selected));}
        (*selected).to_string()
    }else{options.encoder.clone()};
    let cap=caps.encoder(&encoder).ok_or_else(||format!("{encoder} failed capability detection"))?;
    if !cap.modes.contains(&options.mode){return Err(format!("{} does not support {} (hardware CRF and unverified two-pass modes are disabled)",encoder,options.mode));}
    if options.container=="webm"&&encoder!="libsvtav1"&&encoder!="libvpx-vp9"{return Err("WebM requires AV1 or VP9 video in this build".into());}
    if options.container=="ts"&&encoder=="libsvtav1"{return Err("AV1 in MPEG-TS is not a verified combination; use MP4, MKV, or WebM".into());}
    if encoder=="libvpx-vp9"&&["mp4","mov","ts"].contains(&options.container.as_str()){return Err("VP9 output requires WebM or MKV in this build; MP4/MOV VP9 compatibility is not verified".into());}
    let hardware=encoder.ends_with("videotoolbox");
    if !hardware {member(&options.preset,&["ultrafast","veryfast","medium","slow"],"encoder preset")?;}
    if options.pixel_format.as_deref().is_some_and(|s|s!="yuv420p"){return Err("Only verified yuv420p SDR output is supported".into());}
    let crf=if options.mode=="crf" {let (default,max)=if encoder=="libx265"{(28.0,51.0)}else if encoder=="libsvtav1"{(35.0,63.0)}else if encoder=="libvpx-vp9"{(31.0,63.0)}else{(23.0,51.0)};
        let c=options.crf.unwrap_or(default);if !c.is_finite()||c<0.0||c>max {return Err(format!("CRF range for {encoder} is 0…{max}"));}Some(c)}else{None};
    if options.audio_bitrate_kbps<32||options.audio_bitrate_kbps>1536{return Err("Audio bitrate range is 32…1536 kbps".into());}
    if options.audio_policy=="aac"&&(options.container=="webm"||!caps.audio_encoders.contains(&"aac".into())){return Err("AAC unavailable for this container/runtime".into());}
    if options.audio_policy=="opus"&&(!["mkv","webm"].contains(&options.container.as_str())||!caps.audio_encoders.contains(&"libopus".into())){return Err("Opus requires MKV/WebM and libopus".into());}
    if options.audio_policy=="mp3"&&(!["mp4","mov","mkv","ts"].contains(&options.container.as_str())||!caps.audio_encoders.contains(&"libmp3lame".into())){return Err("MP3 audio requires libmp3lame".into());}
    if options.audio_policy=="flac"&&(options.container!="mkv"||!caps.audio_encoders.contains(&"flac".into())){return Err("FLAC audio requires MKV (no silent container hacks)".into());}
    for a in probe.audio(){
        if options.audio_policy=="copy" {
            let compatible=match options.container.as_str(){"mp4"=>["aac","mp3","ac3","eac3","alac"].contains(&a.codec_name.as_str()),"mov"=>["aac","mp3","ac3","alac","pcm_s16le","pcm_s24le"].contains(&a.codec_name.as_str()),"webm"=>["opus","vorbis"].contains(&a.codec_name.as_str()),"ts"=>["aac","mp3","ac3","eac3","mp2"].contains(&a.codec_name.as_str()),"mkv"=>true,_=>false};
            if !compatible{return Err(format!("{} audio cannot be copied into {}; choose an audio policy or MKV",a.codec_name,options.container));}
        }else if options.audio_policy!="none"&&a.channels>6{return Err("Audio conversion with more than six channels is not verified".into());}
    }
    if options.subtitle_policy=="copy" {for s in probe.subtitles(){let okay=match options.container.as_str(){"mp4"|"mov"=>s.codec_name=="mov_text","webm"=>s.codec_name=="webvtt","mkv"=>true,_=>false};if !okay{return Err(format!("{} subtitles cannot be copied to {}; choose MKV or explicitly drop subtitles",s.codec_name,options.container));}}}else if options.subtitle_policy=="drop"&&options.container=="ts"&&probe.subtitles().count()>0{warnings.push("MPEG-TS has no verified subtitle path; subtitles are removed by your explicit choice".into());}
    if options.audio_policy=="none"&&probe.audio().count()>0{warnings.push("Audio removed by explicit user choice".into());}
    if options.subtitle_policy=="drop"&&probe.subtitles().count()>0{warnings.push("Subtitles removed by explicit user choice".into());}
    if let Some(fps)=options.frame_rate {if !fps.is_finite()||![24.0,25.0,30.0,60.0].contains(&fps){return Err("Frame rate must be 24/25/30/60 or preserve source".into());}warnings.push("Changing frame rate may drop or duplicate frames".into());}
    if options.resolution>16384{return Err("Resolution limit exceeded".into());}
    let (sw,sh)=if probe.rotation.rem_euclid(180)!=0{(probe.height,probe.width)}else{(probe.width,probe.height)};
    let scale=if options.resolution==0{1.0}else{(options.resolution as f64/sw.max(sh) as f64).min(1.0)};
    let width=((sw as f64*scale) as u32/2*2).max(2);let height=((sh as f64*scale) as u32/2*2).max(2);
    // 用户没要求缩放却发生了 1px 取整（yuv420p 只接受偶数）时必须说明，不许静默变形。
    if options.resolution==0&&(sw%2!=0||sh%2!=0){warnings.push(format!("Source dimensions {sw}×{sh} are not even; yuv420p requires even numbers, output rounded to {width}×{height}"));}
    let audio_bits=if options.audio_policy=="none"{0}else if options.audio_policy=="copy"{probe.audio().try_fold(0u64,|n,s|s.bit_rate.and_then(|b|n.checked_add(b))).unwrap_or(0)}else{options.audio_bitrate_kbps*1000*probe.audio().count() as u64};
    let bitrate_kbps=if options.mode=="targetSize" {
        if options.audio_policy=="copy"&&probe.audio().any(|s|s.bit_rate.is_none()){return Err("Target size needs known audio bitrate; choose AAC/Opus".into());}
        let seconds=probe.duration_us.filter(|v|*v>0).ok_or("Target size requires known duration")? as f64/1e6;
        let target=options.target_mi_b.ok_or("Set target MiB")?;if !target.is_finite()||target<=0.0||target>1e7{return Err("Invalid target size".into());}
        let available=target*1024.0*1024.0*8.0/seconds*0.97-audio_bits as f64;
        if available<32000.0{return Err("Target is smaller than audio and container overhead plus minimum video bitrate".into());}
        // 推导出的码率同样受 32…500000 kbps 契约约束：目标过大 / 时长过短会推出
        // 离谱的码率，ffmpeg 会照单全收，这里必须像 abr/twoPass 分支一样拦下。
        let kbps=(available/1000.0) as u64;
        if !(32..=500000).contains(&kbps){return Err("Target size implies a video bitrate outside 32…500000 kbps; raise the target or pick a longer source".into());}
        warnings.push("Target size is an estimate; audio, muxing and rate control cause deviations".into());Some(kbps)
    }else if options.mode=="abr"||options.mode=="twoPass"{
        let estimated=((width as f64*height as f64*probe.frame_rate.unwrap_or(30.0)*0.07)/1000.0) as u64;
        let source=video.bit_rate.or(probe.bit_rate).unwrap_or(estimated*1000)/1000;
        let b=options.bitrate_kbps.unwrap_or(estimated.min(source*3/4).max(200));if !(32..=500000).contains(&b){return Err("Video bitrate range is 32…500000 kbps".into());}Some(b)
    }else{None};
    Ok(EncodePlan {encoder,container:options.container.clone(),bitrate_kbps,crf,width,height,passes:if ["twoPass","targetSize"].contains(&options.mode.as_str()){2}else{1},threads:if options.threads==0{budget.max(1)}else{options.threads.max(1).min(budget.max(1))},warnings,metadata_policy,options:options.clone()})
}
impl EncodePlan {
    pub fn argv(&self,input:&Path,output:&Path,pass:u32,passlog:&Path)->Vec<String>{
        // §5.5 输入严格解码：`-err_detect explode`（在 -i 之前 = 输入侧）让损坏的
        // 输入在解码时报错而不是被容错吞掉；`-xerror` 让任何一步出错都以非零退出。
        // 两者一起保证"损坏视频伪装成压缩成功"在编码与验证两端都被拦下。
        let mut a=vec!["-hide_banner".into(),"-nostdin".into(),"-xerror".into(),"-err_detect".into(),"explode".into(),"-n".into(),"-protocol_whitelist".into(),"file,pipe".into(),"-threads".into(),self.threads.to_string(),"-i".into(),input.to_string_lossy().into(),"-map".into(),"0:v:0".into(),"-c:v".into(),self.encoder.clone(),"-threads".into(),self.threads.to_string(),"-filter_threads".into(),self.threads.to_string(),"-pix_fmt".into(),"yuv420p".into(),"-vf".into(),format!("scale={}:{}",self.width,self.height),"-metadata:s:v:0".into(),"rotate=0".into()];
        let mut add=|k:&str,v:String|{a.push(k.into());a.push(v);};
        if self.encoder.ends_with("videotoolbox"){add("-allow_sw","0".into());}else if self.encoder=="libsvtav1"{let speed=match self.options.preset.as_str(){"ultrafast"=>12,"veryfast"=>10,"slow"=>4,_=>8};add("-preset",speed.to_string());add("-svtav1-params",format!("lp={}",self.threads));}else if self.encoder=="libvpx-vp9"{let cpu_used=match self.options.preset.as_str(){"ultrafast"=>"5","veryfast"=>"4","slow"=>"1",_=>"2"};add("-deadline","good".into());add("-cpu-used",cpu_used.into());add("-row-mt","1".into());}else{add("-preset",self.options.preset.clone());if self.encoder=="libx265"{add("-x265-params",format!("pools={}:frame-threads=1",self.threads));}}
        if let Some(c)=self.crf{add("-crf",c.to_string());if self.encoder=="libvpx-vp9"{add("-b:v","0".into());}}if let Some(b)=self.bitrate_kbps{add("-b:v",format!("{b}k"));}
        // §5.3 未指定帧率 = 保留源 PTS（passthrough）；只有用户明确指定才 CFR。
        if let Some(f)=self.options.frame_rate{add("-r",f.to_string());}else{add("-fps_mode","passthrough".into());}
        if self.passes==2{add("-pass",pass.to_string());add("-passlogfile",passlog.to_string_lossy().into());}
        if self.encoder.contains("hevc")||self.encoder=="libx265"{if self.container=="mp4"||self.container=="mov"{add("-tag:v","hvc1".into());}}
        if self.passes==2&&pass==1{a.extend(["-an".into(),"-sn".into(),"-f".into(),"null".into(),"-progress".into(),"pipe:1".into(),"-nostats".into(),"-".into()]);return a;}
        if self.options.audio_policy=="none"{a.push("-an".into());}else{a.extend(["-map".into(),"0:a?".into(),"-c:a".into(),match self.options.audio_policy.as_str(){"copy"=>"copy","opus"=>"libopus","mp3"=>"libmp3lame","flac"=>"flac",_=>"aac"}.into()]);if self.options.audio_policy!="copy"&&self.options.audio_policy!="flac"{a.extend(["-b:a".into(),format!("{}k",self.options.audio_bitrate_kbps)]);}}
        if self.options.subtitle_policy=="copy"{a.extend(["-map".into(),"0:s?".into(),"-c:s".into(),"copy".into()]);}else{a.push("-sn".into());}
        if self.container=="mkv"{a.extend(["-map".into(),"0:t?".into(),"-c:t".into(),"copy".into()]);}
        match self.metadata_policy {
            // §6 只有"全部移除"才配 -map_metadata -1 / -map_chapters -1。
            // 全局 -1 只清全局作用域：流级、章节级、program 级的 metadata 必须各自
            // 清空，否则手机写的流标签会原样跟过去 —— 隐私承诺不能只覆盖一半。
            MetadataPolicy::StripAll=>{a.extend(["-map_metadata".into(),"-1".into(),"-map_metadata:s".into(),"-1".into(),"-map_metadata:c".into(),"-1".into(),"-map_metadata:p".into(),"-1".into(),"-map_chapters".into(),"-1".into()]);}
            MetadataPolicy::KeepAll=>{a.extend(["-map_metadata".into(),"0".into(),"-map_chapters".into(),"0".into()]);}
            MetadataPolicy::StripPrivate=>{
                // 普通 metadata 与章节照常保留；只把隐私键清空。作用域要覆盖全局、
                // 视频、音频、字幕四层 —— 手机拍的文件经常把位置写进流标签。
                a.extend(["-map_metadata".into(),"0".into(),"-map_chapters".into(),"0".into()]);
                for key in crate::video::PRIVATE_METADATA_KEYS {
                    for scope in ["",":s:v",":s:a",":s:s"] {
                        a.extend([format!("-metadata{scope}"),format!("{key}=")]);
                    }
                }
            }
        }
        if self.container=="mp4"||self.container=="mov"{a.extend(["-movflags".into(),"+faststart".into()]);}
        a.extend(["-progress".into(),"pipe:1".into(),"-nostats".into(),"-f".into(),match self.container.as_str(){"mkv"=>"matroska","ts"=>"mpegts",other=>other}.into(),output.to_string_lossy().into()]);a
    }
}
#[cfg(test)]mod tests {
    use super::*; use crate::video::capabilities::EncoderCapability;
    pub fn caps()->CapabilityMatrix{CapabilityMatrix {backend:"ffmpeg".into(),available:true,reason:None,encoders:["libx264","libx265","libsvtav1","libvpx-vp9","hevc_videotoolbox"].iter().map(|id|EncoderCapability{id:id.to_string(),label:id.to_string(),modes:if id.ends_with("videotoolbox"){vec!["abr".into()]}else if *id=="libx264"||*id=="libvpx-vp9"{vec!["crf".into(),"abr".into(),"twoPass".into(),"targetSize".into()]}else{vec!["crf".into(),"abr".into()]},presets:vec![]}).collect(),muxers:vec!["mp4".into(),"mkv".into(),"webm".into(),"ts".into()],audio_encoders:vec!["aac".into(),"libopus".into(),"libmp3lame".into(),"flac".into()],diagnostics:vec![]}}
    pub fn probe()->VideoProbe{VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":720,"height":1280,"codec_name":"h264","pix_fmt":"yuv420p","avg_frame_rate":"30/1"}],"format":{"duration":"10","format_name":"mp4"}}),1234).unwrap()}
    pub fn vfr_probe()->VideoProbe{VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":3840,"height":2160,"codec_name":"hevc","pix_fmt":"yuv420p","avg_frame_rate":"130500/9191","r_frame_rate":"89/6"}],"format":{"duration":"30.636667","format_name":"mov"}}),1234).unwrap()}
    #[test]fn combinations_are_checked(){let mut o=VideoOptions::default();o.encoder="libx264".into();o.container="webm".into();assert!(build(&o,&probe(),&caps(),3).is_err());o.container="mp4".into();o.encoder="hevc_videotoolbox".into();o.mode="crf".into();assert!(build(&o,&probe(),&caps(),3).is_err());}
    #[test]fn resolution_never_upscales_and_even(){let mut o=VideoOptions::default();o.resolution=1920;let p=build(&o,&probe(),&caps(),2).unwrap();assert_eq!((p.width,p.height),(720,1280));o.resolution=720;let p=build(&o,&probe(),&caps(),2).unwrap();assert_eq!(p.height,720);assert_eq!(p.width%2,0);assert_eq!(p.threads,2);}
    #[test]fn hdr_and_size_lower_bound_are_blocked(){let mut p=probe();p.hdr_kind="hdr".into();assert!(build(&VideoOptions::default(),&p,&caps(),3).is_err());let mut o=VideoOptions::default();o.encoder="libx264".into();o.mode="targetSize".into();o.target_mi_b=Some(0.0001);assert!(build(&o,&probe(),&caps(),3).is_err());}
    #[test]fn argv_preserves_paths_without_shell(){let mut o=VideoOptions::default();o.encoder="libx264".into();o.mode="crf".into();let p=build(&o,&probe(),&caps(),1).unwrap();let a=p.argv(Path::new("/tmp/a ' $().mov"),Path::new("/tmp/result.mp4"),1,Path::new("/tmp/stats"));assert!(a.contains(&"/tmp/a ' $().mov".into()));assert!(a.windows(2).any(|w|w==["-fps_mode","passthrough"]));assert!(!a.contains(&"-r".into()));}
    /// §5.3 VFR 源的默认行为：保留源 PTS（passthrough），不再重排成平均帧率；
    /// 只有用户明确指定帧率时才 CFR。
    #[test]fn vfr_source_keeps_its_timestamps_unless_fps_is_explicit(){
        let mut o=VideoOptions::default();o.encoder="libx265".into();
        let p=build(&o,&vfr_probe(),&caps(),3).unwrap();
        assert!(p.warnings.iter().any(|w|w.contains("passthrough")),"VFR 源必须说明保留了原始时间戳");
        let a=p.argv(Path::new("in.mov"),Path::new("out.mp4"),1,Path::new("stats"));
        assert!(a.windows(2).any(|w|w==["-fps_mode","passthrough"]),"VFR 源默认 passthrough");
        assert!(!a.windows(2).any(|w|w==["-fps_mode","cfr"]),"不许再自动归一化成 CFR");
        assert!(!a.contains(&"-r".into()));
        // CFR 源同样是 passthrough。
        let cfr=build(&o,&probe(),&caps(),3).unwrap();
        assert!(cfr.argv(Path::new("in.mp4"),Path::new("out.mp4"),1,Path::new("stats")).windows(2).any(|w|w==["-fps_mode","passthrough"]));
        // 用户明确指定帧率才执行 CFR 转换。
        o.frame_rate=Some(24.0);
        let explicit=build(&o,&vfr_probe(),&caps(),3).unwrap();
        let a=explicit.argv(Path::new("in.mov"),Path::new("out.mp4"),1,Path::new("stats"));
        assert!(a.windows(2).any(|w|w==["-r","24"]));assert!(!a.contains(&"passthrough".into()));
    }

    /// §5.1 输入门由能力分类器判定：白名单外的像素格式给不出精确理由就拒绝。
    #[test]fn pixel_format_whitelist_gates_the_pipeline(){
        let mut root=serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":720,"height":1280,"codec_name":"h264","pix_fmt":"yuv410p9le","avg_frame_rate":"30/1"}],"format":{"duration":"10","format_name":"mp4"}});
        let p=VideoProbe::parse(root.clone(),1234).unwrap();
        assert!(build(&VideoOptions::default(),&p,&caps(),3).is_err(),"9-bit 子串匹配抓不到，白名单必须抓到");
        root["streams"][0]["pix_fmt"]=serde_json::json!("bgra");
        let p=VideoProbe::parse(root.clone(),1234).unwrap();
        assert!(build(&VideoOptions::default(),&p,&caps(),3).is_err(),"packed alpha 必须拒绝");
        root["streams"][0]["pix_fmt"]=serde_json::json!("grayf32le");
        let p=VideoProbe::parse(root,1234).unwrap();
        assert!(build(&VideoOptions::default(),&p,&caps(),3).is_err(),"浮点格式必须拒绝");
    }

    /// §5.2 BT.2020 原色 ≠ HDR，但不能按普通 BT.709 静默转换。
    #[test]fn bt2020_sdr_is_rejected_with_a_color_reason(){
        let mut root=serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":720,"height":1280,"codec_name":"h264","pix_fmt":"yuv420p","avg_frame_rate":"30/1","color_primaries":"bt2020","color_transfer":"bt709"}],"format":{"duration":"10","format_name":"mp4"}});
        let p=VideoProbe::parse(root,1234).unwrap();
        let error=build(&VideoOptions::default(),&p,&caps(),3).unwrap_err();
        assert!(error.contains("color-management"),"拒绝理由必须说明是色彩空间问题: {error}");
    }

    /// §5.5 编码参数必须在输入侧声明严格解码：损坏的输入不许被容错吞掉。
    #[test]fn argv_declares_strict_input_decoding(){
        let p=build(&VideoOptions::default(),&probe(),&caps(),3).unwrap();
        let a=p.argv(Path::new("in.mp4"),Path::new("out.mp4"),1,Path::new("stats"));
        let input_position=a.iter().position(|x|x=="-i").unwrap();
        let err_detect=a.iter().position(|x|x=="-err_detect").unwrap();
        assert!(err_detect<input_position,"-err_detect 必须在 -i 之前才是输入侧选项");
        assert_eq!(a.get(err_detect+1).map(String::as_str),Some("explode"));
        assert!(a.contains(&"-xerror".into()));
    }

    /// §6 StripAll 覆盖全部作用域：流级、章节级、program 级 metadata 一并清空。
    #[test]fn strip_all_clears_every_metadata_scope(){
        let mut o=VideoOptions::default();
        o.encoder="libx264".into();o.mode="crf".into();o.metadata_policy="strip".into();
        let p=build(&o,&probe(),&caps(),3).unwrap();
        let a=p.argv(Path::new("in.mp4"),Path::new("out.mp4"),1,Path::new("st"));
        for flag in ["-map_metadata:s","-map_metadata:c","-map_metadata:p"] {
            assert!(a.windows(2).any(|w|w[0]==flag&&w[1]=="-1"),"StripAll 缺少 {flag} -1");
        }
        assert!(a.windows(2).any(|w|w==["-map_metadata","-1"]));
        assert!(a.windows(2).any(|w|w==["-map_chapters","-1"]));
    }
    #[test]fn target_mib_contract(){let v=serde_json::to_value(VideoOptions::default()).unwrap();assert!(v.get("targetMiB").is_some());}
    #[test]fn zero_threads_means_auto_budget(){let mut o=VideoOptions::default();o.threads=0;let p=build(&o,&probe(),&caps(),5).unwrap();assert_eq!(p.threads,5);o.threads=99;let p=build(&o,&probe(),&caps(),5).unwrap();assert_eq!(p.threads,5);}
    #[test]fn vp9_webm_and_ts_contract(){let mut o=VideoOptions::default();o.encoder="libvpx-vp9".into();o.container="webm".into();o.mode="crf".into();o.crf=Some(31.0);o.audio_policy="opus".into();let p=build(&o,&probe(),&caps(),3).unwrap();let a=p.argv(Path::new("in.mp4"),Path::new("out.webm"),1,Path::new("st"));assert!(a.windows(2).any(|w|w==["-b:v","0"]));assert!(a.windows(2).any(|w|w==["-cpu-used","2"]));assert!(a.windows(2).any(|w|w==["-row-mt","1"]));
        let mut o2=VideoOptions::default();o2.encoder="libvpx-vp9".into();o2.container="mp4".into();assert!(build(&o2,&probe(),&caps(),3).is_err());
        let mut o3=VideoOptions::default();o3.container="ts".into();o3.encoder="libsvtav1".into();assert!(build(&o3,&probe(),&caps(),3).is_err());
        let mut o4=VideoOptions::default();o4.container="ts".into();o4.encoder="libx264".into();o4.audio_policy="flac".into();assert!(build(&o4,&probe(),&caps(),3).is_err());
        let mut o5=VideoOptions::default();o5.container="mkv".into();o5.encoder="libx264".into();o5.audio_policy="flac".into();let p5=build(&o5,&probe(),&caps(),3).unwrap();let a5=p5.argv(Path::new("in.mp4"),Path::new("out.mkv"),1,Path::new("st"));assert!(a5.windows(2).any(|w|w==["-c:a","flac"]));assert!(!a5.contains(&"-b:a".into()));
        let mut o6=VideoOptions::default();o6.container="ts".into();o6.encoder="libx264".into();o6.audio_policy="mp3".into();let p6=build(&o6,&probe(),&caps(),3).unwrap();let a6=p6.argv(Path::new("in.mp4"),Path::new("out.ts"),1,Path::new("st"));assert!(a6.windows(2).any(|w|w==["-c:a","libmp3lame"]));assert!(a6.windows(2).any(|w|w==["-f","mpegts"]));
        let mut o7=VideoOptions::default();o7.container="mp4".into();o7.encoder="libx264".into();o7.audio_policy="mp3".into();assert!(build(&o7,&probe(),&caps(),3).is_ok());}

    /// §55 隐私模式不是"全部删掉"：章节和普通 metadata 留下，只清隐私键。
    #[test]
    fn privacy_mode_keeps_metadata_and_chapters_but_clears_private_tags() {
        let mut o=VideoOptions::default();
        o.encoder="libx264".into();o.mode="crf".into();o.metadata_policy="stripPrivate".into();
        let p=build(&o,&probe(),&caps(),3).unwrap();
        assert_eq!(p.metadata_policy,MetadataPolicy::StripPrivate);
        let a=p.argv(Path::new("in.mp4"),Path::new("out.mp4"),1,Path::new("st"));
        assert!(a.windows(2).any(|w|w==["-map_metadata","0"]),"普通 metadata 必须保留");
        assert!(a.windows(2).any(|w|w==["-map_chapters","0"]),"章节不是隐私数据");
        assert!(!a.windows(2).any(|w|w==["-map_metadata","-1"]),"隐私模式不等于全部删除");
        assert!(!a.windows(2).any(|w|w==["-map_chapters","-1"]));
        for key in ["location","make","model"] {
            assert!(a.windows(2).any(|w|w[0]=="-metadata"&&w[1]==format!("{key}=")),"缺少全局清除 {key}");
            assert!(a.windows(2).any(|w|w[0]=="-metadata:s:v"&&w[1]==format!("{key}=")),"缺少视频流清除 {key}");
            assert!(a.windows(2).any(|w|w[0]=="-metadata:s:a"&&w[1]==format!("{key}=")),"缺少音频流清除 {key}");
        }
    }

    #[test]
    fn strip_all_is_the_only_policy_that_drops_chapters_and_all_metadata() {
        let mut o=VideoOptions::default();
        o.encoder="libx264".into();o.mode="crf".into();o.metadata_policy="strip".into();
        let p=build(&o,&probe(),&caps(),3).unwrap();
        let a=p.argv(Path::new("in.mp4"),Path::new("out.mp4"),1,Path::new("st"));
        assert!(a.windows(2).any(|w|w==["-map_metadata","-1"]));
        assert!(a.windows(2).any(|w|w==["-map_chapters","-1"]));
        assert!(!a.windows(2).any(|w|w==["-map_metadata","0"]));
    }

    #[test]
    fn preserve_is_the_default_and_keeps_both_metadata_and_chapters() {
        let mut o=VideoOptions::default();
        o.encoder="libx264".into();o.mode="crf".into();
        let p=build(&o,&probe(),&caps(),3).unwrap();
        assert_eq!(p.metadata_policy,MetadataPolicy::KeepAll);
        let a=p.argv(Path::new("in.mp4"),Path::new("out.mp4"),1,Path::new("st"));
        assert!(a.windows(2).any(|w|w==["-map_metadata","0"]));
        assert!(a.windows(2).any(|w|w==["-map_chapters","0"]));
    }

    /// MP4 的章节轨道在 ffprobe 里就是一条 `data` 流（样本项 `text`），
    /// 不能因为"数据轨未验证"把带章节的普通 MP4 全部拒之门外；时间码轨仍然拒绝。
    #[test]
    fn mp4_chapter_carrier_is_accepted_but_other_data_tracks_are_not() {
        let mut root=serde_json::json!({
            "streams":[
                {"index":0,"codec_type":"video","width":720,"height":1280,"codec_name":"h264","pix_fmt":"yuv420p","avg_frame_rate":"30/1"},
                {"index":1,"codec_type":"data","codec_name":"bin_data","codec_tag_string":"text","tags":{"handler_name":"SubtitleHandler"}}
            ],
            "format":{"duration":"10","format_name":"mp4"},
            "chapters":[{"id":0,"tags":{"title":"Intro"}}]
        });
        let p=VideoProbe::parse(root.clone(),1234).unwrap();
        assert!(p.streams[1].is_chapter_carrier());
        assert!(build(&VideoOptions::default(),&p,&caps(),3).is_ok(),"带章节的 MP4 必须可以处理");
        root["streams"][1]["codec_tag_string"]=serde_json::json!("tmcd");
        let p=VideoProbe::parse(root,1234).unwrap();
        assert!(!p.streams[1].is_chapter_carrier());
        assert!(build(&VideoOptions::default(),&p,&caps(),3).is_err(),"时间码数据轨仍要拒绝");
    }
}
