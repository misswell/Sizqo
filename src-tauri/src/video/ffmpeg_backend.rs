//! Direct FFmpeg engine: streaming IO, bounded diagnostics, independent cancellation.
use std::{fs,io::{Read,Write,BufRead,BufReader},path::Path,sync::{Arc,atomic::{AtomicBool,Ordering}},time::{Instant,Duration},process::Stdio};
use super::{VideoOptions,VideoResult,capabilities::CapabilityMatrix,probe,plan,progress::{VideoProgress,ProgressParser},transaction};
use crate::{history::HistoryStore,output_transaction::TransactionStore};
use crate::result_model::{ReasonCode,ResultOutcome};
pub fn encode(task_id:&str,source:&Path,options:&VideoOptions,caps:&CapabilityMatrix,session:&str,revision:u64,roots:&[String],history:&HistoryStore,transactions:&TransactionStore,retention:u32,budget:usize,logs:&Path,cancel:Arc<AtomicBool>,emit:&(dyn Fn(VideoProgress)+Sync))->VideoResult{
    let started=Instant::now();let mut result=VideoResult::pending(task_id,source,options);
    let outcome=(||->Result<(),String>{
        let source=source.canonicalize().map_err(|e|e.to_string())?;
        // §3.2 覆盖模式必须先算出内容指纹：事务凭证、崩溃恢复、备份复用
        // 全都靠它，stat 三项在"同大小同时间戳的原地改动"面前什么也证明不了。
        // 新建模式保持只 stat（§2.1：普通输出不必为每个源文件额外算哈希）。
        let identity=if options.output_mode=="replace"{
            transaction::SourceIdentity::read_with_hash(&source).map_err(|e|e.to_string())?
        }else{
            transaction::SourceIdentity::read(&source).map_err(|e|e.to_string())?
        };
        let source_probe=probe::probe(&source)?;
        let plan=plan::build(options,&source_probe,caps,budget)?;
        result.encoder_actual=plan.encoder.clone();result.warnings=plan.warnings.clone();
        let target=transaction::destination(&source,options,roots)?;
        let parent=target.parent().ok_or("Output directory missing")?;
        let estimated=plan.bitrate_kbps.zip(source_probe.duration_us).map(|(b,t)|b.saturating_mul(1000).saturating_mul(t/1000000)/8).unwrap_or(source_probe.size_bytes);
        let needed=estimated.max(source_probe.size_bytes/4).saturating_mul(2).saturating_add(64*1024*1024);
        if transaction::available_space(parent)?<needed{return Err(format!("Insufficient output disk space: reserve at least {} bytes",needed));}
        // §3.2 覆盖模式在**编码之前**备好原图备份（§2.2 顺序：身份 → 备份 → 编码）。
        // 编码期间用户改了文件，commit 的两次完整复核会拦住；但备份必须先落盘，
        // 否则"备份失败"这种最需要停止的情况会等到烧完几个小时 CPU 才被发现。
        let backup=if options.output_mode=="replace"{
            let backup_root=logs.parent().ok_or("Application data directory missing")?;
            let mut needed_backup=source_probe.size_bytes.saturating_add(64*1024*1024);
            #[cfg(unix)]{use std::os::unix::fs::MetadataExt;if fs::metadata(parent).map_err(|e|e.to_string())?.dev()==fs::metadata(backup_root).map_err(|e|e.to_string())?.dev(){needed_backup=needed_backup.saturating_add(needed);}}
            // Windows 上"是不是同一块盘"看卷号；查不到就按同一盘处理（多要空间是安全方向）。
            #[cfg(windows)]{let same=match (crate::win32_fs::volume_id_of_path(parent),crate::win32_fs::volume_id_of_path(backup_root)){(Some(a),Some(b))=>a==b,_=>true};if same{needed_backup=needed_backup.saturating_add(needed);}}
            if transaction::available_space(backup_root)?<needed_backup{return Err("Insufficient disk space for the persistent original backup and encoded output".into());}
            result.warnings.push(format!("Replace keeps a persistent original backup of {} bytes",source_probe.size_bytes));
            Some(history.ensure_backup_verified(&source,identity.sha256.as_deref()).map_err(|e|e.to_string())?)
        }else{None};
        fs::create_dir_all(logs).map_err(|e|e.to_string())?;
        let id=crate::history::HistoryEntry::new_id(&source);
        let log_path=logs.join(format!("{id}.log"));
        result.diagnostics_log_path=Some(log_path.to_string_lossy().into());
        let job=tempfile::Builder::new().prefix(&format!(".sizqo-video-{id}-")).tempdir_in(parent).map_err(|e|e.to_string())?;
        let partial=job.path().join(format!("output.partial.{}",plan.container));
        let stats=job.path().join("pass-stats");
        for pass in 1..=plan.passes {
            if cancel.load(Ordering::Acquire){return Err("Cancelled by user".into());}
            let args=plan.argv(&source,&partial,pass,&stats);
            run(&args,&log_path,&cancel,|parser|{
                let mut event=VideoProgress::new(task_id,session,revision,"encoding");
                parser.decorate(&mut event,pass,plan.passes,source_probe.duration_us);emit(event);
            })?;
        }
        if cancel.load(Ordering::Acquire){return Err("Cancelled by user".into());}
        // 产物复探给足时间（编码刚完成，同一份大文件慢卷上不止 45 秒）。
        let encoded=probe::probe_with_timeout(&partial,Duration::from_secs(300))?;
        super::validation::validate(&source_probe,&encoded,&plan)?;
        // Decode the entire output to null. Exit 0 and a progress=end record alone are insufficient.
        let mut verify=super::process::command("ffmpeg")?;
        verify.args(["-hide_banner","-v","error","-xerror","-nostdin","-protocol_whitelist","file,pipe","-threads"]).arg(plan.threads.to_string())
            .arg("-i").arg(&partial).args(["-map","0:v","-map","0:a?","-threads"]).arg(plan.threads.to_string()).args(["-f","null","-"]);
        // Full read validation uses the same cancellation-aware process pump.
        run_command(verify,&log_path,&cancel,|_|{})?;
        if cancel.load(Ordering::Acquire){return Err("Cancelled by user".into());}
        result.output_bytes=Some(encoded.size_bytes);result.savings=Some((1.0-encoded.size_bytes as f64/source_probe.size_bytes as f64)*100.0);
        if encoded.size_bytes>=source_probe.size_bytes&&!options.allow_larger_output{
            // §14 §70 有效但没变小 = 跳过，不是成功，也不是编码失败。判据说清是哪一种，
            // 但只有结论这一份：这一轮到此跑完（stage=completed），"没产物"由 outcome/reason_code
            // 回答。从前的 skippedLarger 旗标既当状态又当原因，§139 第三阶段把它删掉。
            result.stage="completed".into();result.reason=Some("Valid output is not smaller; original left unchanged".into());result.outcome=Some(ResultOutcome::SkippedNoBenefit);result.reason_code=Some(if encoded.size_bytes>source_probe.size_bytes{ReasonCode::OutputLargerThanSource}else{ReasonCode::NoSizeReduction});return Ok(());
        }
        if encoded.size_bytes>=source_probe.size_bytes{result.warnings.push("Larger output retained by explicit professional-mode choice".into());}
        result.elapsed_ms=started.elapsed().as_millis()as u64;
        transaction::commit(&source,&partial,&target,options,&source_probe,&mut result,history,transactions,retention,&identity,backup.as_deref())?;
        result.success=true;result.stage="completed".into();result.outcome=Some(ResultOutcome::Committed);Ok(())
    })();
    result.elapsed_ms=started.elapsed().as_millis()as u64;
    if let Err(error)=outcome {let cancelled=cancel.load(Ordering::Acquire);result.stage=if cancelled{"cancelled"}else{"failed"}.into();result.outcome=Some(if cancelled{ResultOutcome::Cancelled}else{ResultOutcome::Failed});result.reason_code=Some(if cancelled{ReasonCode::Cancelled}else{ReasonCode::Unknown});result.reason=Some(error);}
    result
}
fn run(args:&[String],log:&Path,cancel:&AtomicBool,emit:impl Fn(&ProgressParser)+Sync)->Result<(),String>{let mut cmd=super::process::command("ffmpeg")?;cmd.args(args);run_command(cmd,log,cancel,emit)}
fn run_command(mut cmd:std::process::Command,log:&Path,cancel:&AtomicBool,emit:impl Fn(&ProgressParser)+Sync)->Result<(),String>{
    let mut child=cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e|e.to_string())?;
    let stdout=child.stdout.take().ok_or("Missing progress pipe")?;
    let stderr=child.stderr.take().ok_or("Missing diagnostic pipe")?;
    std::thread::scope(|scope|{
        let progress=scope.spawn(||{let mut parser=ProgressParser::default();let mut reader=BufReader::new(stdout);let mut line=String::new();loop{line.clear();if reader.read_line(&mut line).unwrap_or(0)==0{break;}if line.len()<=4096&&parser.feed(&line){emit(&parser);}}});
        let diagnostics=scope.spawn(||{
            let mut reader=stderr;let mut buffer=[0u8;8192];let mut ring=std::collections::VecDeque::with_capacity(65536);
            let mut log=fs::OpenOptions::new().create(true).append(true).open(log).ok();
            let mut written=log.as_ref().and_then(|f|f.metadata().ok()).map(|m|m.len()).unwrap_or(0);
            loop{let n=reader.read(&mut buffer).unwrap_or(0);if n==0{break;}
                if let Some(file)=log.as_mut(){let take=(1024*1024u64).saturating_sub(written).min(n as u64)as usize;if take>0{let _=file.write_all(&buffer[..take]);written+=take as u64;}}
                for b in &buffer[..n]{if ring.len()==65536{ring.pop_front();}ring.push_back(*b);}
            }if let Some(file)=log{let _=file.sync_all();}ring.into_iter().collect::<Vec<_>>()
        });
        let status=loop{
            if cancel.load(Ordering::Acquire){
                #[cfg(unix)]unsafe{libc::kill(child.id()as i32,libc::SIGTERM);}
                #[cfg(not(unix))]let _=child.kill();
                let deadline=Instant::now()+Duration::from_secs(2);
                loop{match child.try_wait(){Ok(Some(_))=>break,_=>{}}if Instant::now()>deadline{let _=child.kill();let _=child.wait();break;}std::thread::sleep(Duration::from_millis(30));}
                break Err("Cancelled by user".into());
            }
            match child.try_wait(){Ok(Some(s))=>break Ok(s),Ok(None)=>{},Err(e)=>{let _=child.kill();let _=child.wait();break Err(e.to_string());}}
            std::thread::sleep(Duration::from_millis(50));
        };
        progress.join().map_err(|_|"Progress reader failed")?;
        let errors=diagnostics.join().map_err(|_|"Diagnostic reader failed")?;
        if !status?.success(){return Err(format!("FFmpeg failed: {}",String::from_utf8_lossy(&errors).chars().take(8192).collect::<String>()));}Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::video::{capabilities,probe,validation};

    fn fixture(dir:&Path,extension:&str)->PathBuf{
        let meta=dir.join(format!("meta-{extension}.txt"));
        fs::write(&meta,";FFMETADATA1\ntitle=Sizqo Metadata Test\nlocation=+37.7749-122.4194/\nmake=SizqoCam\nmodel=FixtureOne\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=Intro\n").unwrap();
        let source=dir.join(format!("source.{extension}"));
        let mut cmd=super::super::process::command("ffmpeg").unwrap();
        cmd.args(["-hide_banner","-v","error","-nostdin","-y",
            "-f","lavfi","-i","testsrc2=size=160x120:rate=24:duration=2",
            "-f","lavfi","-i","sine=frequency=440:duration=2",
            "-i"]).arg(&meta)
            .args(["-map","0:v","-map","1:a","-map_chapters","2","-map_metadata","2",
                "-c:v","libx264","-preset","ultrafast","-pix_fmt","yuv420p","-c:a","aac","-shortest"])
            .arg(&source);
        super::super::process::capture(cmd,Duration::from_secs(120),16*1024*1024).unwrap();
        source
    }

    fn encode_policy(dir:&Path,source:&Path,source_probe:&super::super::probe::VideoProbe,caps:&CapabilityMatrix,policy:&str,container:&str)->super::super::probe::VideoProbe{
        let options=VideoOptions{encoder:"libx264".into(),mode:"crf".into(),crf:Some(23.0),preset:"ultrafast".into(),audio_policy:"copy".into(),metadata_policy:policy.into(),container:container.into(),threads:2,..Default::default()};
        let plan=plan::build(&options,source_probe,caps,2).unwrap();
        let out=dir.join(format!("{policy}.{container}"));
        let cancel=AtomicBool::new(false);
        let log=dir.join(format!("{policy}-{container}.log"));
        for pass in 1..=plan.passes{
            let args=plan.argv(source,&out,pass,&dir.join(format!("{policy}-{container}-stats")));
            run(&args,&log,&cancel,|_|{}).unwrap();
        }
        let encoded=probe::probe(&out).unwrap();
        validation::validate(source_probe,&encoded,&plan).expect("输出必须通过 §56 全量校验");
        encoded
    }

    /// §55 §56 真实夹具：元数据策略必须在实际编码产物里被探到，而不是只在 argv 里写过。
    /// 需要内置 ffmpeg/ffprobe，显式运行：`cargo test --lib metadata_policies_hold -- --ignored`
    #[test]
    #[ignore="requires bundled ffmpeg/ffprobe; run explicitly for release validation"]
    fn metadata_policies_hold_on_real_encoded_fixtures(){
        assert!(super::super::process::tool("ffmpeg").is_ok()&&super::super::process::tool("ffprobe").is_ok(),"需要内置 ffmpeg/ffprobe");
        let dir=tempfile::tempdir().unwrap();
        let source=fixture(dir.path(),"mkv");
        let source_probe=probe::probe(&source).unwrap();
        assert_eq!(source_probe.chapters.len(),1,"夹具必须真的带章节");
        assert_eq!(source_probe.format_tags["title"],"Sizqo Metadata Test");
        assert_eq!(source_probe.format_tags["LOCATION"],"+37.7749-122.4194/");
        assert_eq!(source_probe.format_tags["MAKE"],"SizqoCam");
        let caps=capabilities::capabilities();
        assert!(caps.encoder("libx264").is_some(),"内置 ffmpeg 必须通过 libx264 运行时试用");

        let kept=encode_policy(dir.path(),&source,&source_probe,&caps,"preserve","mkv");
        assert_eq!(kept.format_tags["title"],"Sizqo Metadata Test");
        assert_eq!(kept.format_tags["LOCATION"],"+37.7749-122.4194/");
        assert_eq!(kept.format_tags["MAKE"],"SizqoCam");
        assert_eq!(kept.chapters.len(),1,"保留策略丢了章节");

        let private=encode_policy(dir.path(),&source,&source_probe,&caps,"stripPrivate","mkv");
        assert_eq!(private.format_tags["title"],"Sizqo Metadata Test","隐私模式不应清掉普通标题");
        for key in ["LOCATION","MAKE","MODEL"]{
            assert!(private.format_tags.get(key).is_none(),"隐私模式仍留下 {key}");
        }
        assert_eq!(private.chapters.len(),1,"章节不是隐私数据");

        let stripped=encode_policy(dir.path(),&source,&source_probe,&caps,"strip","mkv");
        assert!(stripped.format_tags.get("title").is_none(),"全部移除仍留下标题");
        assert_eq!(stripped.chapters.len(),0,"全部移除仍留下章节");

        // MP4 的章节轨道是 data/text 流：真实文件必须能进 plan，章节必须能活过编码。
        let mp4=fixture(dir.path(),"mp4");
        let mp4_probe=probe::probe(&mp4).unwrap();
        assert!(mp4_probe.streams.iter().any(|s|s.is_chapter_carrier()),"夹具应含 MP4 章节载体");
        assert_eq!(mp4_probe.chapters.len(),1);
        let mp4_kept=encode_policy(dir.path(),&mp4,&mp4_probe,&caps,"preserve","mp4");
        assert_eq!(mp4_kept.chapters.len(),1,"MP4 保留策略丢了章节");
    }

    fn encode_with(dir:&Path,source:&Path,options:&VideoOptions)->super::super::probe::VideoProbe{
        let caps=capabilities::capabilities();
        let source_probe=probe::probe(source).unwrap();
        let plan=plan::build(options,&source_probe,&caps,2).unwrap();
        let out=dir.join(format!("out-{}.{}",std::process::id(),options.container));
        let cancel=AtomicBool::new(false);
        for pass in 1..=plan.passes{
            let args=plan.argv(source,&out,pass,&dir.join(format!("stats-{}-{}",std::process::id(),pass)));
            run(&args,&dir.join(format!("matrix-{}.log",std::process::id())),&cancel,|_|{}).unwrap();
        }
        let encoded=probe::probe(&out).unwrap();
        validation::validate(&source_probe,&encoded,&plan).expect("输出必须通过 §56 全量校验");
        encoded
    }

    fn make_fixture(root:&Path,name:&str,args:&[&str])->PathBuf{
        let path=root.join(name);
        let mut cmd=super::super::process::command("ffmpeg").unwrap();
        cmd.args(["-hide_banner","-v","error","-nostdin","-y"]).args(args).arg(&path);
        super::super::process::capture(cmd,Duration::from_secs(120),16*1024*1024)
            .unwrap_or_else(|e|panic!("夹具 {name} 生成失败: {e}"));
        path
    }

    /// §9.3 §11 真实媒体矩阵：拒绝与接受路径都由真实文件、真实编码产物背书，
    /// 不再只用合成的 probe 值说话。需要内置 ffmpeg/ffprobe，显式运行：
    /// `cargo test --lib s11_real_media_matrix -- --ignored`
    #[test]
    #[ignore="requires bundled ffmpeg/ffprobe; run explicitly for release validation"]
    fn s11_real_media_matrix(){
        assert!(super::super::process::tool("ffmpeg").is_ok()&&super::super::process::tool("ffprobe").is_ok(),"需要内置 ffmpeg/ffprobe");
        let dir=tempfile::tempdir().unwrap();
        let root=dir.path();
        let caps=capabilities::capabilities();
        assert!(caps.encoder("libx264").is_some()&&caps.encoder("libx265").is_some(),"内置 ffmpeg 必须有 libx264/libx265");
        let base=VideoOptions{encoder:"libx264".into(),mode:"crf".into(),crf:Some(23.0),preset:"ultrafast".into(),audio_policy:"copy".into(),metadata_policy:"preserve".into(),container:"mkv".into(),threads:2,..Default::default()};

        // ① SDR 双音轨 + 字幕 + 章节：copy 策略下轨道全部活着，输出过全量校验。
        let meta=root.join("matrix-meta.txt");
        fs::write(&meta,";FFMETADATA1\ntitle=Matrix\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1500\ntitle=Intro\n").unwrap();
        let subs=root.join("matrix.srt");
        fs::write(&subs,"1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nmatrix\n").unwrap();
        let multi=make_fixture(root,"multi.mkv",&[
            "-f","lavfi","-i","testsrc2=size=160x120:rate=24:duration=2",
            "-f","lavfi","-i","sine=frequency=440:duration=2",
            "-f","lavfi","-i","sine=frequency=880:duration=2",
            "-i",meta.to_str().unwrap(),"-i",subs.to_str().unwrap(),
            "-map","0:v","-map","1:a","-map","2:a","-map","4:s",
            "-c:v","libx264","-preset","ultrafast","-pix_fmt","yuv420p",
            "-c:a","aac","-c:s","webvtt",
            "-map_chapters","3","-map_metadata","3","-shortest"]);
        let multi_probe=probe::probe(&multi).unwrap();
        assert_eq!(multi_probe.audio().count(),2,"夹具必须真的有双音轨");
        assert_eq!(multi_probe.subtitles().count(),1,"夹具必须真的有字幕");
        assert_eq!(multi_probe.chapters.len(),1,"夹具必须真的有章节");
        let multi_encoded=encode_with(root,&multi,&base);
        assert_eq!(multi_encoded.audio().count(),2,"双音轨丢了");
        assert_eq!(multi_encoded.subtitles().count(),1,"字幕丢了");
        assert_eq!(multi_encoded.chapters.len(),1,"章节丢了");

        // ② VFR 源：§5.3 默认 passthrough —— 保留源 PTS，不重排时间轴。
        // 用 mp4：mkv 会把丢帧后的时间戳归一化，探不出 r≠avg。
        let vfr=make_fixture(root,"vfr.mp4",&[
            "-f","lavfi","-i","testsrc2=size=160x120:rate=30:duration=2",
            "-vf","select='not(mod(n\\,5))'","-fps_mode","passthrough",
            "-c:v","libx264","-preset","ultrafast"]);
        let vfr_probe=probe::probe(&vfr).unwrap();
        assert!(vfr_probe.variable_frame_rate,"夹具必须真的是 VFR（r≠avg）");
        let mut vfr_options=base.clone();vfr_options.audio_policy="none".into();vfr_options.container="mp4".into();
        let vfr_plan=plan::build(&vfr_options,&vfr_probe,&caps,2).unwrap();
        let vfr_argv=vfr_plan.argv(&vfr,&root.join("vfr-out.mp4"),1,&root.join("stats"));
        assert!(vfr_argv.windows(2).any(|w|w==["-fps_mode","passthrough"]),"VFR 源默认保留原始时间戳");
        assert!(!vfr_argv.windows(2).any(|w|w==["-fps_mode","cfr"]),"不许再自动归一化成 CFR");
        let vfr_encoded=encode_with(root,&vfr,&vfr_options);
        // passthrough 的意义就是时间轴原样：时长漂移不超过帧间隔容差（§5.4），
        // 产物平均帧率等于源平均帧率（时间戳没被重排的证据）。
        let src_secs=vfr_probe.duration_us.unwrap() as f64/1e6;
        let out_secs=vfr_encoded.duration_us.unwrap() as f64/1e6;
        let fps=vfr_probe.frame_rate.unwrap_or(30.0);
        let tolerance=((1e6/fps)*2.0/1e6).max(0.25);
        assert!((src_secs-out_secs).abs()<=tolerance,"VFR 产物时长漂移超容差：{src_secs} → {out_secs}");
        let expected=vfr_probe.frame_rate.unwrap();
        // passthrough 保留的是时间戳本身；容器写回的 avg_frame_rate 有取整，
        // 2% / 0.5fps 内即视为时间轴未被重排。
        assert!((vfr_encoded.frame_rate.unwrap()-expected).abs()<(expected*0.02).max(0.5),"产物平均帧率应约等于源平均帧率");

        // ③ 带透明通道：argb/qtrle 必须在编码前被拒，绝不静默去掉 alpha。
        let alpha=make_fixture(root,"alpha.mov",&[
            "-f","lavfi","-i","testsrc2=size=160x120:rate=10:duration=1",
            "-vf","format=argb","-c:v","qtrle"]);
        let alpha_probe=probe::probe(&alpha).unwrap();
        let alpha_err=plan::build(&base,&alpha_probe,&caps,2).unwrap_err();
        assert!(alpha_err.contains("Alpha"),"alpha 输入必须被拒：{alpha_err}");

        // ④ 10bit + HDR10 VUI：高位深与 HDR 输入在编码前被拒。
        let hdr=make_fixture(root,"hdr.mp4",&[
            "-f","lavfi","-i","testsrc2=size=160x120:rate=24:duration=1",
            "-c:v","libx265","-preset","ultrafast","-pix_fmt","yuv420p10le",
            "-x265-params","colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc"]);
        let hdr_probe=probe::probe(&hdr).unwrap();
        assert!(hdr_probe.hdr_kind!="sdr","夹具必须被探成 HDR（实际 {}）",hdr_probe.hdr_kind);
        assert!(plan::build(&base,&hdr_probe,&caps,2).is_err(),"HDR/高位深输入必须被拒");

        // ⑤ 隔行扫描：编码前被拒。
        let interlaced=make_fixture(root,"interlaced.mkv",&[
            "-f","lavfi","-i","testsrc2=size=160x120:rate=24:duration=1",
            "-vf","tinterlace=interleave_top","-c:v","libx264","-preset","ultrafast",
            "-flags","+ilme+ildct"]);
        let inter_probe=probe::probe(&interlaced).unwrap();
        assert!(plan::build(&base,&inter_probe,&caps,2).is_err(),"隔行输入必须被拒");

        // ⑥ 损坏文件：probe 干净地失败，不 panic、不冒充成功。
        let good=fixture(root,"mkv");
        let corrupt=root.join("corrupt.mkv");
        let mut bytes=fs::read(&good).unwrap();
        for b in &mut bytes[..40] { *b=b'X'; }
        fs::write(&corrupt,&bytes).unwrap();
        assert!(probe::probe(&corrupt).is_err(),"被覆盖魔数的文件必须被探出来");

        // ⑦ 目标体积不可达：对真实文件在 plan 阶段给出明确失败。
        let mut tiny=base.clone();tiny.mode="targetSize".into();tiny.audio_policy="none".into();tiny.container="mp4".into();tiny.target_mi_b=Some(0.0001);
        let sdr_probe=probe::probe(&good).unwrap();
        assert!(plan::build(&tiny,&sdr_probe,&caps,2).is_err(),"不可达的目标体积必须被拒");
    }
}
