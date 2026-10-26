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
pub fn commit(source:&Path,partial:&Path,target:&Path,o:&VideoOptions,probe:&VideoProbe,result:&mut VideoResult,history:&HistoryStore,transactions:&TransactionStore,retention:u32,identity:&SourceIdentity)->Result<(),String>{
    // §25 覆盖前复核：这次编码所依据的那份字节，现在还是不是它。
    if let SourceCheck::Changed(detail)=identity.verify(source){return Err(format!("Source changed during encoding; original was not overwritten ({detail})"));}
    let size=fs::metadata(partial).map_err(|e|e.to_string())?.len();
    fs::File::open(partial).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;
    let replace=o.output_mode=="replace";
    let backup=if replace{Some(history.ensure_backup_verified(source,identity.sha256.as_deref())?)}else{None};
    if let Some(ref backup)=backup {fs::File::open(backup).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;sync_dir(backup.parent().ok_or("Backup parent missing")?)?;}
    let id=HistoryEntry::new_id(source);
    // §2.5：产物哈希在记账前算好 —— 恢复时要靠它证明"源位置上的字节是我们写出去的"。
    let output_sha256=crate::source_identity::sha256_of(partial).ok();
    let txn=backup.as_ref().map(|b|ReplaceTransaction{id:id.clone(),history_id:id.clone(),source_path:source.to_string_lossy().into(),output_path:target.to_string_lossy().into(),backup_path:b.to_string_lossy().into(),temp_output_path:Some(partial.to_string_lossy().into()),original_size:probe.size_bytes,expected_output_size:size,original_sha256:identity.sha256.clone(),output_sha256,created_at:now_millis(),cross_format:false,mode:TransactionMode::ReplaceExisting});
    if let Some(t)=txn.as_ref(){transactions.prepare_durable(t)?;}
    let mut claimed=false;
    let install=(||{
        // 备份可能拷了很久：改名前再核一次（§25）。
        if let SourceCheck::Changed(detail)=identity.verify(source){return Err(format!("Source changed while saving backup ({detail})"));}
        if replace {fs::rename(partial,target).map_err(|e|e.to_string())?;}else{
            // Hard-link is atomic and fails if an external actor claimed the target.
            fs::hard_link(partial,target).map_err(|e|format!("Cannot atomically claim output path: {e}"))?;
            // 产物字节已经通过硬链接完整落在 target；partial 还在本次专属的临时目录里，
            // TempDir 回收时会再删一次。这里删不掉不许把已成立的占名判成失败 ——
            // 那会在用户目录里留下"报了失败却真实存在"的孤儿产物。
            if let Err(e)=fs::remove_file(partial){log::warn!("临时产物交由临时目录兜底回收: {e}");}
        }
        claimed=true;
        sync_dir(target.parent().ok_or("Output parent missing")?)?;
        // §13 Publish：走到这里文件已经原子地占了目标位置、目录也 fsync 了，
        // 才允许对历史承诺"这次有产物"。
        let image_contract=CompressResult{file:source.to_string_lossy().into(),original_size:probe.size_bytes,compressed_size:size,savings:result.savings.unwrap_or(0.0),original_size_formatted:crate::engine::format_bytes(probe.size_bytes),compressed_size_formatted:crate::engine::format_bytes(size),out_type:o.container.clone(),algorithm:result.encoder_actual.clone(),error:None,output_path:Some(target.to_string_lossy().into()),backup_path:backup.as_ref().map(|p|p.to_string_lossy().into()),output_mode:Some(if o.output_mode=="directory"{"folder".into()}else{o.output_mode.clone()}),outcome:Some(crate::result_model::ResultOutcome::Committed),reason_code:None,..Default::default()};
        let mut entry=HistoryEntry::record_with_id(id.clone(),source,&image_contract,target,backup.as_deref(),retention);
        entry.media_kind="video".into();entry.video_metadata=Some(serde_json::json!({"probe":probe,"encoderActual":result.encoder_actual,"warnings":result.warnings,"elapsedMs":result.elapsed_ms,"diagnosticsLogPath":result.diagnostics_log_path}));
        history.add(entry)?;
        Ok::<(),String>(())
    })();
    if let Err(error)=install {
        if let Some(t)=txn.as_ref(){
            // Only roll back if replacement actually happened. A changed source before rename must not be clobbered.
            if claimed {if let Err(rollback)=crate::output_transaction::rollback(t){return Err(format!("{error}; rollback failed, backup retained: {rollback}"));}}
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
