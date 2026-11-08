//! Streaming, atomic output commit; journal/backup/restore use the image safety service.
use std::{fs,path::{Path,PathBuf}};
use super::{VideoOptions,VideoResult,probe::VideoProbe};
use crate::{history::{HistoryStore,HistoryEntry,now_millis},output_transaction::{TransactionStore,ReplaceTransaction,TransactionMode,OutputNaming},engine::CompressResult};
// §24 源文件指纹只有 `source_identity` 那一份实现：视频线和图片线复核的是同一件事，
// 各自定义一份就等于允许两边判得不一样。
pub use crate::source_identity::{SourceCheck, SourceIdentity};
pub fn available_space(path:&Path)->Result<u64,String>{
    #[cfg(unix)]{use std::{ffi::CString,os::unix::ffi::OsStrExt};
        let c=CString::new(path.as_os_str().as_bytes()).map_err(|e|e.to_string())?;
        let mut stats=std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe{libc::statvfs(c.as_ptr(),stats.as_mut_ptr())}!=0{return Err(std::io::Error::last_os_error().to_string());}
        let stats=unsafe{stats.assume_init()};return Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64));
    }
    #[cfg(windows)]{return crate::win32_fs::available_space(path);}
    #[cfg(not(any(unix,windows)))]{let _=path;Err("Disk-space verification is unavailable on this platform".into())}
}
/// 每任务冻结的 relativePath 必须真是相对路径：绝对路径、`..` 上跳、空串一律
/// 拒用（防目录逃逸），宁可退化到来源根解析也不照单全收。
pub fn frozen_relative_is_safe(relative:&str)->bool{
    let p=Path::new(relative);
    !relative.is_empty()&&p.is_relative()&&!p.components().any(|c|matches!(c,std::path::Component::ParentDir))
}
pub fn destination(source:&Path,o:&VideoOptions,roots:&[String],per_task_relative:Option<&str>)->Result<PathBuf,String>{
    if o.output_mode=="replace"{
        let ext=source.extension().and_then(|s|s.to_str()).unwrap_or("").to_lowercase();
        if ext!=o.container && !(ext=="m4v"&&o.container=="mp4"){return Err("Safe replace currently requires the source container extension; use suffix for format conversion".into());}
        return Ok(source.into());
    }
    let stem=source.file_stem().ok_or("Source has no file name")?.to_string_lossy().into_owned();
    let suffix:String=o.output_suffix.trim().chars().map(|c|if c=='/'||c=='\\'||c.is_control(){'_'}else{c}).take(64).collect();
    let suffix=if suffix.is_empty(){"_compressed".to_owned()}else{suffix};
    let parent=if o.output_mode=="directory"||o.output_mode=="folder" {
        let root=PathBuf::from(o.output_dir.as_deref().ok_or("Select output directory")?);
        // §39 §40：来源根在导入时定死，这里和图片线共用同一份解析，不再按"这一批"重猜。
        // 本轮加固（P2-11 / T18）：每任务冻结的 relativePath 优先（合法性照验），
        // 批级"最深根胜出"猜测只做缺省兜底。
        let rel=per_task_relative.filter(|rel|frozen_relative_is_safe(rel)).map(PathBuf::from)
            .or_else(||crate::source_root::relative_to_roots(source,roots));
        root.join(rel.and_then(|r|r.parent().map(Path::to_path_buf)).unwrap_or_default())
    }else{source.parent().ok_or("Source has no parent")?.to_owned()};
    fs::create_dir_all(&parent).map_err(|e|e.to_string())?;
    // §54 命名序列只有 `OutputNameAllocator` 那一份：这里编码前规划，不能提前占住名字，
    // 所以按候选序探测空位；真正的"绝不覆盖"由 commit 里的原子占名兑现。
    let naming=OutputNaming::WithSuffix{stem,suffix,extension:o.container.clone()};
    for n in 0..crate::output_transaction::MAX_NAME_CONFLICTS {let p=parent.join(naming.candidate(n));if !p.exists(){return Ok(p);}}
    Err("Too many output filename conflicts".into())
}
/// 命名序列（stem + 后缀 + 容器扩展名）只有这一处派生：commit 的换号重试必须
/// 与 `destination` 的候选序完全一致，各算各的就会换到错误的名字上。
fn output_naming(source:&Path,o:&VideoOptions)->Result<OutputNaming,String>{
    let stem=source.file_stem().ok_or("Source has no file name")?.to_string_lossy().into_owned();
    let suffix:String=o.output_suffix.trim().chars().map(|c|if c=='/'||c=='\\'||c.is_control(){'_'}else{c}).take(64).collect();
    let suffix=if suffix.is_empty(){"_compressed".to_owned()}else{suffix};
    Ok(OutputNaming::WithSuffix{stem,suffix,extension:o.container.clone()})
}
/// 编码前独占认领输出名（本轮加固 P2-12 / T17）：`O_EXCL` 0 字节占位从这一刻起
/// 锁住候选名 —— 句柄与 `(dev,ino)` 凭证都由 `ClaimedOutput` 持有，长编码期间
/// 外部程序"看见名字 → 建同名文件"的窗口被从根上关掉。replace 模式没有占名
/// 这一步 —— 目标就是源文件本身，覆盖由带身份复核的原子交换完成 —— 返回 None。
pub fn claim_output(source:&Path,o:&VideoOptions,roots:&[String],per_task_relative:Option<&str>)->Result<Option<crate::output_transaction::ClaimedOutput>,String>{
    if o.output_mode=="replace"{return Ok(None);}
    let planned=destination(source,o,roots,per_task_relative)?;
    let parent=planned.parent().ok_or("Output directory missing")?;
    let naming=output_naming(source,o)?;
    Ok(Some(crate::output_transaction::OutputNameAllocator::claim(parent,&naming)?))
}
/// 目录项落盘：只有 `history::sync_dir` 一处按平台要求开句柄（Windows 需要 backup
/// semantics + 写访问），视频线不许自带一份只读句柄的实现。
pub fn sync_dir(path:&Path)->Result<(),String>{crate::history::sync_dir(path)}
#[allow(clippy::too_many_arguments)]
pub fn commit(source:&Path,partial:&Path,target:&Path,o:&VideoOptions,probe:&VideoProbe,result:&mut VideoResult,history:&HistoryStore,transactions:&TransactionStore,retention:u32,identity:&SourceIdentity,backup:Option<&Path>,claimed:&mut Option<crate::output_transaction::ClaimedOutput>)->Result<(),String>{
    // §25 覆盖前复核：这次编码所依据的那份字节，现在还是不是它。
    // replace 的 identity 带内容指纹（§3.2）：同大小同时间戳的原地改动也查得出。
    if let SourceCheck::Changed(detail)=identity.verify(source){return Err(format!("Source changed during encoding; original was not overwritten ({detail})"));}
    let size=fs::metadata(partial).map_err(|e|e.to_string())?.len();
    fs::File::open(partial).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;
    let replace=o.output_mode=="replace";
    // §3.2 备份在编码之前就已备好（ffmpeg_backend）；一场长编码之后它必须重新过验：
    // 备份没了或字节变了，这次覆盖就没有可信的恢复依据。
    if replace&&backup.is_none(){return Err("Replace mode lost its pre-encode original backup".into());}
    if let Some(backup)=backup {
        fs::File::open(backup).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;
        sync_dir(backup.parent().ok_or("Backup parent missing")?)?;
        if let Err(detail)=history.backup_is_faithful(backup){return Err(format!("Original backup failed verification before commit: {detail}"));}
    }
    let id=HistoryEntry::new_id(source);
    // §2.5：产物哈希在记账前算好 —— 恢复时要靠它证明"源位置上的字节是我们写出去的"。
    // 指纹算不出来就必须在覆盖之前终止：没有恢复依据的覆盖等于裸奔（§5 审查意见）。
    let output_sha256=Some(crate::source_identity::sha256_of(partial).map_err(|e|e.to_string())?);
    // §3.1 备份凭证：来历记录里有就抄，老备份没有记录就现场算一份。
    let backup_sha256=backup.and_then(|b|history.recorded_backup_sha256(b));
    let backup_legacy_unverified=backup.is_some_and(|b|matches!(history.backup_verification(b),Ok(crate::history::BackupVerification::LegacyUnverified)));
    let txn=Some(ReplaceTransaction{id:id.clone(),history_id:id.clone(),source_path:source.to_string_lossy().into(),output_path:target.to_string_lossy().into(),backup_path:backup.map(|p|p.to_string_lossy().into()).unwrap_or_default(),temp_output_path:Some(partial.to_string_lossy().into()),original_size:probe.size_bytes,expected_output_size:size,original_sha256:identity.sha256.clone(),output_sha256,backup_sha256,placeholder_identity:None,temp_identity:crate::output_transaction::file_identity(partial),created_at:now_millis(),cross_format:false,mode:if replace{TransactionMode::ReplaceExisting}else{TransactionMode::CreateNew},schema_version:crate::output_transaction::SCHEMA_VERSION,backup_legacy_unverified});
    if replace{transactions.prepare_durable(txn.as_ref().unwrap())?;}else{transactions.prepare(txn.as_ref().unwrap())?;}
    let mut claimed_output=false;
    // 实际输出位：replace 时就是源文件位；新建/目录模式可能因换号重试与计划位不同。
    let mut final_target:PathBuf=target.to_path_buf();
    let install=(||->Result<(),(String,bool)>{
        // 备份可能拷了很久：提交前再核一次（§25）。
        if let SourceCheck::Changed(detail)=identity.verify(source){return Err((format!("Source changed while saving backup ({detail})"),false));}
        if replace {
            // §3.3 原子交换提交：先交换、再验证被换出来的字节与落进目标的候选
            // （本轮加固 P1-1 双侧对账）。
            // swapped=true 表示覆盖已发生且换回失败：必须走回滚保留现场。
            let candidate_sha=Some(crate::source_identity::sha256_of(partial).map_err(|e|(e.to_string(),false))?);
            match crate::output_transaction::publish_replace_if_identity_matches(partial,target,identity,candidate_sha.as_deref()){
                Ok(_outcome)=>{}
                Err(error)=>return Err((error.to_string(),error.swapped)),
            }
        }else{
            // 本轮加固（P2-12 / T17）：编码前占住的名字可能被外部在长编码期间抢走。
            // 发布绝不覆盖、绝不重新编码：先收回**仍属于我们的**占位（归属门在
            // release 里：被外部偷换的路径分文不动），再按 `OutputNaming` 候选序用
            // 不覆盖原语装进去；装不进就换下一个候选名 —— partial 一字节不动，
            // 名字换到装得下为止。最终实际位置写回事务日志与历史。
            if let Some(claimed)=claimed.take(){claimed.release();}
            let parent_dir=target.parent().ok_or_else(||(String::from("Output parent missing"),false))?;
            let naming=output_naming(source,o).map_err(|e|(e,false))?;
            let mut installed:Option<PathBuf>=None;
            let mut last_error=String::new();
            for n in 0..crate::output_transaction::MAX_NAME_CONFLICTS {
                let candidate=parent_dir.join(naming.candidate(n));
                match crate::output_transaction::install_at_free_path(partial,&candidate){
                    Ok(())=>{installed=Some(candidate);break;}
                    Err(detail)=>{last_error=detail;}
                }
            }
            let Some(installed_at)=installed else {
                return Err((format!("Cannot atomically claim an output path: {last_error}"),false));
            };
            // 记账里的 outputPath 换成实际位置：崩溃恢复据此认领产物（指纹门照旧）。
            if installed_at!=target{
                if let Err(detail)=transactions.update_output_path(&id,&installed_at){
                    return Err((format!("实际输出位与记账不一致且日志更新失败: {detail}"),false));
                }
            }
            final_target=installed_at;
            claimed_output=true;
            sync_dir(parent_dir).map_err(|e|(e,false))?;
        }
        // §13 Publish：走到这里文件已经原子地占了目标位置、目录也 fsync 了，
        // 才允许对历史承诺"这次有产物"。
        let image_contract=CompressResult{file:source.to_string_lossy().into(),original_size:probe.size_bytes,compressed_size:size,savings:result.savings.unwrap_or(0.0),original_size_formatted:crate::engine::format_bytes(probe.size_bytes),compressed_size_formatted:crate::engine::format_bytes(size),out_type:o.container.clone(),algorithm:result.encoder_actual.clone(),error:None,output_path:Some(final_target.to_string_lossy().into()),backup_path:backup.map(|p|p.to_string_lossy().into()),output_mode:Some(if o.output_mode=="directory"{"folder".into()}else{o.output_mode.clone()}),outcome:Some(crate::result_model::ResultOutcome::Committed),reason_code:None,..Default::default()};
        let mut entry=HistoryEntry::record_with_id(id.clone(),source,&image_contract,&final_target,backup.as_deref(),retention);
        entry.media_kind="video".into();entry.video_metadata=Some(serde_json::json!({"probe":probe,"encoderActual":result.encoder_actual,"warnings":result.warnings,"elapsedMs":result.elapsed_ms,"diagnosticsLogPath":result.diagnostics_log_path}));
        history.add(entry).map_err(|e|(e,false))?;
        Ok(())
    })();
    if let Err((error,replaced))=install {
        if let Some(t)=txn.as_ref(){
            // 覆盖真的发生（占名成功，或交换后换回失败）才回滚：
            // 编码中途源文件变了、交换被换回原状的情况，用户文件一个字节都没动，不回滚。
            if claimed_output||replaced {
                if let Err(rollback)=crate::output_transaction::rollback(t){return Err(format!("{error}; rollback failed, backup retained: {rollback}"));}
            }
            transactions.finish(&id);
        }else if claimed_output{
            // 产物已经占了目标位置，但历史没写成。删不掉就必须说清楚，
            // 不能一边报失败一边在用户目录里留一个没人认领的文件。
            if let Err(cleanup)=fs::remove_file(&final_target){return Err(format!("{error}; uncommitted output {} could not be removed: {cleanup}",final_target.display()));}
        }
        return Err(error);
    }
    if let Some(t)=txn{transactions.finish(&t.id);}
    result.output_path=Some(final_target.to_string_lossy().into());result.backup_path=backup.map(|p|p.to_string_lossy().into());result.history_id=Some(id);Ok(())
}
#[cfg(test)]mod tests{use super::*;
    /// T17（本轮加固 P2-12）：编码期间输出名被外部抢占 → 换下一个候选名发布，
    /// partial 复用不重新编码，外部文件分文不动，实际路径写进历史与结果。
    /// 现场按跨平台语义构造：计划位被外部内容占住、占位凭证失效（claimed=None，
    /// 等价于"认领已被偷换"）。Windows 上持有句柄的占位本就抢不走名字 ——
    /// 那正是句柄占名的意义；unix 的真实偷换路径由 t03 覆盖。
    #[test]fn an_output_name_stolen_during_encoding_moves_to_the_next_candidate(){
        let dir=tempfile::tempdir().unwrap();
        let source=dir.path().join("clip.mp4");fs::write(&source,b"source-bytes").unwrap();
        let o=VideoOptions::default();
        let planned=dir.path().join("clip_compressed.mp4");
        // 外部在长编码期间把计划位占住。
        fs::write(&planned,b"external-file").unwrap();
        let partial=dir.path().join("partial.mp4");fs::write(&partial,b"encoded-output-bytes").unwrap();
        let history=HistoryStore::new(dir.path().join("history")).unwrap();
        let transactions=TransactionStore::new(dir.path().join("transactions"));
        let identity=SourceIdentity::read(&source).unwrap();
        let probe=VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":16,"height":16,"codec_name":"h264","pix_fmt":"yuv420p","avg_frame_rate":"30/1"}],"format":{"duration":"10","format_name":"mp4"}}),13).unwrap();
        let mut result=VideoResult::pending("t1",&source,&o);
        let mut claimed_slot=None;
        commit(&source,&partial,&planned,&o,&probe,&mut result,&history,&transactions,3,&identity,None,&mut claimed_slot).unwrap();
        // 计划位上的外部文件分文不动。
        assert_eq!(fs::read(&planned).unwrap(),b"external-file");
        // 产物落在下一个候选名上，partial 一字节没少。
        let next=dir.path().join("clip_compressed_1.mp4");
        assert_eq!(fs::read(&next).unwrap(),b"encoded-output-bytes");
        assert_eq!(result.output_path.as_deref(),Some(next.to_string_lossy().as_ref()));
        assert!(history.list().iter().any(|e|e.output_path.as_deref()==Some(next.to_string_lossy().as_ref())),"实际输出路径必须写进历史");
    }

    /// 认领仍然有效时的默认路径：commit 收回仍属于我们的占位，产物落在计划位。
    #[test]fn an_intact_claim_is_released_and_the_product_lands_on_the_planned_slot(){
        let dir=tempfile::tempdir().unwrap();
        let source=dir.path().join("clip.mp4");fs::write(&source,b"source-bytes").unwrap();
        let o=VideoOptions::default();
        let mut claimed=claim_output(&source,&o,&[],None).unwrap().unwrap();
        let planned=claimed.path().to_path_buf();
        let partial=dir.path().join("partial.mp4");fs::write(&partial,b"encoded-output-bytes").unwrap();
        let history=HistoryStore::new(dir.path().join("history")).unwrap();
        let transactions=TransactionStore::new(dir.path().join("transactions"));
        let identity=SourceIdentity::read(&source).unwrap();
        let probe=VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":16,"height":16,"codec_name":"h264","pix_fmt":"yuv420p","avg_frame_rate":"30/1"}],"format":{"duration":"10","format_name":"mp4"}}),13).unwrap();
        let mut result=VideoResult::pending("t1",&source,&o);
        let mut claimed_slot=Some(claimed);
        commit(&source,&partial,&planned,&o,&probe,&mut result,&history,&transactions,3,&identity,None,&mut claimed_slot).unwrap();
        assert_eq!(fs::read(&planned).unwrap(),b"encoded-output-bytes");
        assert_eq!(result.output_path.as_deref(),Some(planned.to_string_lossy().as_ref()));
    }
    #[test]fn suffix_conflicts_never_claim_source(){let d=tempfile::tempdir().unwrap();let s=d.path().join("clip.mp4");fs::write(&s,b"source").unwrap();fs::write(d.path().join("clip_compressed.mp4"),b"existing").unwrap();let p=destination(&s,&VideoOptions::default(),&[],None).unwrap();assert_eq!(p.file_name().unwrap(),"clip_compressed_1.mp4");assert_eq!(fs::read(s).unwrap(),b"source");}
    #[test]fn source_identity_detects_changes(){let d=tempfile::tempdir().unwrap();let s=d.path().join("clip");fs::write(&s,b"original").unwrap();let id=SourceIdentity::read(&s).unwrap();fs::write(&s,b"changed and longer").unwrap();assert_ne!(id,SourceIdentity::read(&s).unwrap());}

    /// 编码前的第一个检查点就是这一步：`Err` 在非 unix 上曾无条件返回，Windows 视频轨
    /// 因此**还没开始编码就整条失败**。这一条要在 Windows runner 上真的问出数字，
    /// 而不是靠 `cargo check` 通过就当作运行时成立（P0-3 的教训）。
    #[test]fn available_space_answers_for_a_real_directory(){
        let d=tempfile::tempdir().unwrap();
        match available_space(d.path()){
            Ok(free)=>assert!(free>0,"能回答就必须给出正数，拿到 0 等于没读到"),
            Err(e)=>assert!(cfg!(not(any(unix,windows))),"{e}：只有 unix/windows 之外才允许答不出来"),
        }
    }
}
