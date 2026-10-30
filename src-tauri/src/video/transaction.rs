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
    #[cfg(not(unix))]{let _=path;Err("Disk-space verification is unavailable on this platform".into())}
}
pub fn destination(source:&Path,o:&VideoOptions,roots:&[String])->Result<PathBuf,String>{
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
        let rel=crate::source_root::relative_to_roots(source,roots);
        root.join(rel.and_then(|r|r.parent().map(Path::to_path_buf)).unwrap_or_default())
    }else{source.parent().ok_or("Source has no parent")?.to_owned()};
    fs::create_dir_all(&parent).map_err(|e|e.to_string())?;
    // §54 命名序列只有 `OutputNameAllocator` 那一份：这里编码前规划，不能提前占住名字，
    // 所以按候选序探测空位；真正的"绝不覆盖"由 commit 里的原子占名兑现。
    let naming=OutputNaming::WithSuffix{stem,suffix,extension:o.container.clone()};
    for n in 0..crate::output_transaction::MAX_NAME_CONFLICTS {let p=parent.join(naming.candidate(n));if !p.exists(){return Ok(p);}}
    Err("Too many output filename conflicts".into())
}
pub fn sync_dir(path:&Path)->Result<(),String>{fs::File::open(path).and_then(|f|f.sync_all()).map_err(|e|e.to_string())}
#[allow(clippy::too_many_arguments)]
pub fn commit(source:&Path,partial:&Path,target:&Path,o:&VideoOptions,probe:&VideoProbe,result:&mut VideoResult,history:&HistoryStore,transactions:&TransactionStore,retention:u32,identity:&SourceIdentity,backup:Option<&Path>)->Result<(),String>{
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
    let txn=Some(ReplaceTransaction{id:id.clone(),history_id:id.clone(),source_path:source.to_string_lossy().into(),output_path:target.to_string_lossy().into(),backup_path:backup.map(|p|p.to_string_lossy().into()).unwrap_or_default(),temp_output_path:Some(partial.to_string_lossy().into()),original_size:probe.size_bytes,expected_output_size:size,original_sha256:identity.sha256.clone(),output_sha256,backup_sha256,placeholder_identity:None,created_at:now_millis(),cross_format:false,mode:if replace{TransactionMode::ReplaceExisting}else{TransactionMode::CreateNew},schema_version:crate::output_transaction::SCHEMA_VERSION});
    if replace{transactions.prepare_durable(txn.as_ref().unwrap())?;}else{transactions.prepare(txn.as_ref().unwrap())?;}
    let mut claimed=false;
    let install=(||->Result<(),(String,bool)>{
        // 备份可能拷了很久：提交前再核一次（§25）。
        if let SourceCheck::Changed(detail)=identity.verify(source){return Err((format!("Source changed while saving backup ({detail})"),false));}
        if replace {
            // §3.3 原子交换提交：先交换、再验证被换出来的字节。
            // swapped=true 表示覆盖已发生且换回失败：必须走回滚保留现场。
            match crate::output_transaction::publish_replace_if_identity_matches(partial,target,identity){
                Ok(())=>{}
                Err(error)=>return Err((error.to_string(),error.swapped)),
            }
        }else{
            // Hard-link is atomic and fails if an external actor claimed the target.
            if let Err(e)=fs::hard_link(partial,target){return Err((format!("Cannot atomically claim output path: {e}"),false));}
            // 产物字节已经通过硬链接完整落在 target；partial 还在本次专属的临时目录里，
            // TempDir 回收时会再删一次。这里删不掉不许把已成立的占名判成失败 ——
            // 那会在用户目录里留下"报了失败却真实存在"的孤儿产物。
            if let Err(e)=fs::remove_file(partial){log::warn!("临时产物交由临时目录兜底回收: {e}");}
        }
        claimed=true;
        let parent_dir=target.parent().ok_or_else(||(String::from("Output parent missing"),false))?;
        sync_dir(parent_dir).map_err(|e|(e,false))?;
        // §13 Publish：走到这里文件已经原子地占了目标位置、目录也 fsync 了，
        // 才允许对历史承诺"这次有产物"。
        let image_contract=CompressResult{file:source.to_string_lossy().into(),original_size:probe.size_bytes,compressed_size:size,savings:result.savings.unwrap_or(0.0),original_size_formatted:crate::engine::format_bytes(probe.size_bytes),compressed_size_formatted:crate::engine::format_bytes(size),out_type:o.container.clone(),algorithm:result.encoder_actual.clone(),error:None,output_path:Some(target.to_string_lossy().into()),backup_path:backup.map(|p|p.to_string_lossy().into()),output_mode:Some(if o.output_mode=="directory"{"folder".into()}else{o.output_mode.clone()}),outcome:Some(crate::result_model::ResultOutcome::Committed),reason_code:None,..Default::default()};
        let mut entry=HistoryEntry::record_with_id(id.clone(),source,&image_contract,target,backup.as_deref(),retention);
        entry.media_kind="video".into();entry.video_metadata=Some(serde_json::json!({"probe":probe,"encoderActual":result.encoder_actual,"warnings":result.warnings,"elapsedMs":result.elapsed_ms,"diagnosticsLogPath":result.diagnostics_log_path}));
        history.add(entry).map_err(|e|(e,false))?;
        Ok(())
    })();
    if let Err((error,replaced))=install {
        if let Some(t)=txn.as_ref(){
            // 覆盖真的发生（占名成功，或交换后换回失败）才回滚：
            // 编码中途源文件变了、交换被换回原状的情况，用户文件一个字节都没动，不回滚。
            if claimed||replaced {
                if let Err(rollback)=crate::output_transaction::rollback(t){return Err(format!("{error}; rollback failed, backup retained: {rollback}"));}
            }
            transactions.finish(&id);
        }else if claimed{
            // 产物已经占了目标位置，但历史没写成。删不掉就必须说清楚，
            // 不能一边报失败一边在用户目录里留一个没人认领的文件。
            if let Err(cleanup)=fs::remove_file(target){return Err(format!("{error}; uncommitted output {} could not be removed: {cleanup}",target.display()));}
        }
        return Err(error);
    }
    if let Some(t)=txn{transactions.finish(&t.id);}
    result.output_path=Some(target.to_string_lossy().into());result.backup_path=backup.map(|p|p.to_string_lossy().into());result.history_id=Some(id);Ok(())
}
#[cfg(test)]mod tests{use super::*;
    #[test]fn suffix_conflicts_never_claim_source(){let d=tempfile::tempdir().unwrap();let s=d.path().join("clip.mp4");fs::write(&s,b"source").unwrap();fs::write(d.path().join("clip_compressed.mp4"),b"existing").unwrap();let p=destination(&s,&VideoOptions::default(),&[]).unwrap();assert_eq!(p.file_name().unwrap(),"clip_compressed_1.mp4");assert_eq!(fs::read(s).unwrap(),b"source");}
    #[test]fn source_identity_detects_changes(){let d=tempfile::tempdir().unwrap();let s=d.path().join("clip");fs::write(&s,b"original").unwrap();let id=SourceIdentity::read(&s).unwrap();fs::write(&s,b"changed and longer").unwrap();assert_ne!(id,SourceIdentity::read(&s).unwrap());}
}
