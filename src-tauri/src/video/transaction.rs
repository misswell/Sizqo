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
/// REC-01：SIGKILL / 断电不会执行 tempfile 的 Drop —— `.sizqo-video-{id}-` 作业目录
/// 会留在用户目录里。清扫只认**内容自证**的目录：目录名带作业前缀，且里面只有
/// 本管线自己产出的东西（`output.partial.*` 候选、`pass-stats*` 两遍编码统计）。
/// 出现任何其他名字 —— 尤其子目录 —— 整目录保留并记录日志：禁止按前缀批量删除，
/// 用户的文件哪怕恰好躺在同前缀目录里也分文不动。返回清扫掉的目录数。
pub fn sweep_stale_video_job_dirs(parent:&Path)->usize{
    let Ok(entries)=fs::read_dir(parent) else {return 0;};
    let mut swept=0usize;
    for entry in entries.flatten(){
        let path=entry.path();
        let Some(name)=path.file_name().and_then(|n|n.to_str()) else {continue;};
        if !name.starts_with(".sizqo-video-"){continue;}
        if !path.is_dir(){continue;}
        let only_ours=fs::read_dir(&path).map(|children|{
            children.flatten().all(|child|{
                let Some(child_name)=child.file_name().to_str().map(str::to_owned) else {return false};
                child.file_type().map(|t|t.is_file()).unwrap_or(false)
                    &&(child_name.starts_with("output.partial.")||child_name.starts_with("pass-stats"))
            })
        }).unwrap_or(false);
        if only_ours{
            if fs::remove_dir_all(&path).is_ok(){
                log::info!("清扫上次崩溃遗留的视频作业目录: {}",path.display());
                swept+=1;
            }else{
                log::warn!("视频作业目录清扫失败，保留现场: {}",path.display());
            }
        }else{
            log::warn!("视频作业目录 {} 含未知内容，保留现场请人工确认",path.display());
        }
    }
    swept
}

/// fsync 一个文件。**必须带写权限打开**：Windows 的 `FlushFileBuffers` 对只读句柄
/// 一律返回 ACCESS_DENIED（与 `history::sync_dir` 修过的是同一类坑）——视频线此前
/// 从没在 Windows 上真的跑过 commit，只读句柄的 fsync 一直是每条视频任务的
/// 必败点，直到 T17 的跨平台回归第一次把它跑进 CI。
pub fn fsync_file(path:&Path)->Result<(),String>{
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_ALL:u32=0x0000_0007;
        fs::OpenOptions::new().read(true).write(true).share_mode(FILE_SHARE_ALL)
            .open(path).and_then(|f|f.sync_all()).map_err(|e|format!("{}: {e}",path.display()))
    }
    #[cfg(not(windows))]
    {
        fs::File::open(path).and_then(|f|f.sync_all()).map_err(|e|format!("{}: {e}",path.display()))
    }
}
/// 事务状态机的一步：journal 推进 + 本地镜像同步。日志推进失败时**不**推进
/// 本地镜像 —— stage 必须以落盘的事实为准（TX-01：布尔变量不再是唯一依据）。
fn advance(transactions:&TransactionStore,txn:&mut ReplaceTransaction,stage:crate::output_transaction::TransactionStage)->Result<(),String>{
    transactions.update_stage(&txn.id,stage)?;
    txn.stage=stage;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn commit(source:&Path,partial:&Path,target:&Path,o:&VideoOptions,probe:&VideoProbe,result:&mut VideoResult,history:&HistoryStore,transactions:&TransactionStore,retention:u32,identity:&SourceIdentity,backup:Option<&Path>,claimed:&mut Option<crate::output_transaction::ClaimedOutput>)->Result<(),String>{
    // §25 覆盖前复核：这次编码所依据的那份字节，现在还是不是它。
    // replace 的 identity 带内容指纹（§3.2）：同大小同时间戳的原地改动也查得出。
    if let SourceCheck::Changed(detail)=identity.verify(source){return Err(format!("Source changed during encoding; original was not overwritten ({detail})"));}
    let size=fs::metadata(partial).map_err(|e|e.to_string())?.len();
    fsync_file(partial)?;
    let replace=o.output_mode=="replace";
    // §3.2 备份在编码之前就已备好（ffmpeg_backend）；一场长编码之后它必须重新过验：
    // 备份没了或字节变了，这次覆盖就没有可信的恢复依据。
    // 状态机角度：journal 还不存在，PREPARING 阶段的失败发生在任何覆盖之前 ——
    // 备份凭证凑不齐就不许落账（validate_for_prepare 强制），也就不存在要回滚的东西。
    if replace&&backup.is_none(){return Err("Replace mode lost its pre-encode original backup".into());}
    if let Some(backup)=backup {
        fsync_file(backup)?;
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
    // TX-01：journal 一落地就带状态机；TX-03：非覆盖模式的占位身份一并入账，
    // 崩溃恢复才能凭 (dev,ino) 收走遗留的 0 字节占位。
    let placeholder_identity=claimed.as_ref().and_then(|c|c.identity());
    let mut txn=ReplaceTransaction{id:id.clone(),history_id:id.clone(),source_path:source.to_string_lossy().into(),output_path:target.to_string_lossy().into(),backup_path:backup.map(|p|p.to_string_lossy().into()).unwrap_or_default(),temp_output_path:Some(partial.to_string_lossy().into()),original_size:probe.size_bytes,expected_output_size:size,original_sha256:identity.sha256.clone(),output_sha256,backup_sha256,placeholder_identity,temp_identity:crate::output_transaction::file_identity(partial),created_at:now_millis(),cross_format:false,mode:if replace{TransactionMode::ReplaceExisting}else{TransactionMode::CreateNew},schema_version:crate::output_transaction::SCHEMA_VERSION,backup_legacy_unverified,stage:if replace{crate::output_transaction::TransactionStage::BackupVerified}else{crate::output_transaction::TransactionStage::Encoded},retired_original_path:None};
    if replace{transactions.prepare_durable(&txn)?;}else{transactions.prepare(&txn)?;}
    // 覆盖是否已经发生：交换成功（replace）或产物已装进目标位（新建）。
    // 错误分支据此决定回滚/收走产物，journal 的 stage 只是辅助证据。
    let mut destructive=false;
    // 实际输出位：replace 时就是源文件位；新建/目录模式可能因换号重试与计划位不同。
    let mut final_target:PathBuf=target.to_path_buf();
    let mut claimed_output=false;
    let install=(||->Result<(),String>{
        // 备份可能拷了很久：提交前再核一次（§25）。
        if let SourceCheck::Changed(detail)=identity.verify(source){return Err(format!("Source changed while saving backup ({detail})"));}
        advance(transactions,&mut txn,crate::output_transaction::TransactionStage::OutputValidated).map_err(|e|format!("事务状态推进失败（覆盖尚未发生，安全失败）: {e}"))?;
        if replace {
            advance(transactions,&mut txn,crate::output_transaction::TransactionStage::Committing)?;
            // §3.3 原子交换提交：先交换、再验证被换出来的字节与落进目标的候选
            // （本轮加固 P1-1 双侧对账）。
            let candidate_sha=Some(crate::source_identity::sha256_of(partial).map_err(|e|e.to_string())?);
            match crate::output_transaction::publish_replace_if_identity_matches(partial,target,identity,candidate_sha.as_deref()){
                Ok(outcome)=>{
                    // TX-02：交换换出的原图保留路径必须当场入账 —— journal、
                    // 本地镜像、历史三方一致，崩溃恢复才能认领它。
                    if let Some(retained)=&outcome.swapped_out_retained_at{
                        transactions.update_retired_path(&id,retained)?;
                        txn.retired_original_path=Some(retained.to_string_lossy().into_owned());
                    }
                    destructive=true;
                    advance(transactions,&mut txn,crate::output_transaction::TransactionStage::OutputPublished)?;
                }
                Err(error)=>return Err(error.to_string()),
            }
        }else{
            // 本轮加固（P2-12 / T17）：编码前占住的名字可能被外部在长编码期间抢走。
            // 发布绝不覆盖、绝不重新编码：先收回**仍属于我们的**占位（归属门在
            // release 里：被外部偷换的路径分文不动），再按 `OutputNaming` 候选序用
            // 不覆盖原语装进去；装不进就换下一个候选名 —— partial 一字节不动，
            // 名字换到装得下为止。最终实际位置写回事务日志与历史。
            if let Some(claimed)=claimed.take(){claimed.release();}
            let parent_dir=target.parent().ok_or_else(||String::from("Output parent missing"))?;
            let naming=output_naming(source,o)?;
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
                return Err(format!("Cannot atomically claim an output path: {last_error}"));
            };
            // TX-03 顺序修正：安装成功 = 磁盘上已经多了一份我们的产物，这一事实
            // 必须**先**记下来，再去更新记账路径。旧顺序（先 update_output_path
            // 再置 claimed_output）在日志更新失败时会一边留着已安装的产物、
            // 一边按"未占用"收尾 —— 孤儿就是这么来的。
            final_target=installed_at.clone();
            claimed_output=true;
            destructive=true;
            advance(transactions,&mut txn,crate::output_transaction::TransactionStage::OutputPublished)?;
            // 记账里的 outputPath 换成实际位置：崩溃恢复据此认领产物（指纹门照旧）。
            if installed_at!=target{
                transactions.update_output_path(&id,&installed_at)?;
                txn.output_path=installed_at.to_string_lossy().into_owned();
            }
            sync_dir(parent_dir)?;
        }
        // §13 Publish：走到这里文件已经原子地占了目标位置、目录也 fsync 了，
        // 才允许对历史承诺"这次有产物"。结果协议（RES-01）：targetSize 未达成
        // 是 TargetUnmet —— 产物有效、已提交、可保存可恢复，绝不是普通 Committed。
        let target_unmet=result.target_met==Some(false);
        let image_contract=CompressResult{file:source.to_string_lossy().into(),original_size:probe.size_bytes,compressed_size:size,savings:result.savings.unwrap_or(0.0),original_size_formatted:crate::engine::format_bytes(probe.size_bytes),compressed_size_formatted:crate::engine::format_bytes(size),out_type:o.container.clone(),algorithm:result.encoder_actual.clone(),error:None,output_path:Some(final_target.to_string_lossy().into()),backup_path:backup.map(|p|p.to_string_lossy().into()),output_mode:Some(if o.output_mode=="directory"{"folder".into()}else{o.output_mode.clone()}),outcome:Some(if target_unmet{crate::result_model::ResultOutcome::TargetUnmet}else{crate::result_model::ResultOutcome::Committed}),reason_code:if target_unmet{Some(crate::result_model::ReasonCode::TargetUnmet)}else{None},..Default::default()};
        let mut entry=HistoryEntry::record_with_id(id.clone(),source,&image_contract,&final_target,backup.as_deref(),retention);
        entry.media_kind="video".into();
        entry.retired_original_path=txn.retired_original_path.clone();
        // RES-02：历史保存**实际执行**的任务快照 —— 请求参数、目标对账、
        // 最终结论都在，重启后历史详情能还原真实的压缩结果。
        entry.video_metadata=Some(serde_json::json!({
            "probe":probe,
            "encoderActual":result.encoder_actual,
            "warnings":result.warnings,
            "elapsedMs":result.elapsed_ms,
            "diagnosticsLogPath":result.diagnostics_log_path,
            "options":o,
            "targetBytes":result.target_bytes,
            "targetMet":result.target_met,
            "targetDeviation":result.target_deviation,
            "outputBytes":size,
            "outcome":image_contract.outcome,
            "reasonCode":image_contract.reason_code,
            "outputProbe":result.output_probe,
        }));
        history.add(entry)?;
        // 第 9 步：历史"写成"必须以**读回来**为准 —— 写调用成功但断电丢缓存、
        // 或写入的是损坏文件时，contains_committed 的答案才是恢复判定的同一份事实。
        if !history.contains_committed(&id){return Err("History entry not readable after write; treating as history failure".into());}
        advance(transactions,&mut txn,crate::output_transaction::TransactionStage::HistoryPersisted)?;
        Ok(())
    })();
    if let Err(error)=install {
        let overwrite=destructive||txn.stage>=crate::output_transaction::TransactionStage::OutputPublished;
        if overwrite&&replace {
            // TX-01：覆盖已发生而收尾失败（典型：历史写入失败）—— 必须回滚原图。
            // 回滚失败时保留事务日志与备份（rollback 内部保证），绝不销账了事。
            if let Err(rollback)=crate::output_transaction::rollback(&txn){return Err(format!("{error}; rollback failed, backup & journal retained for recovery: {rollback}"));}
            transactions.finish(&txn.id);
            return Err(format!("{error}; original was restored from the verified backup"));
        }
        if overwrite {
            // 非覆盖模式产物已装进目标位但历史没写成：按指纹门收走自己的产物；
            // 收不走（归属证明不了）就保留事务现场 —— 可恢复事务，不是孤儿。
            let mut cleanup=txn.clone();
            cleanup.output_path=final_target.to_string_lossy().into_owned();
            crate::output_transaction::remove_generated_output(&cleanup);
            if Path::new(&cleanup.output_path).exists(){
                return Err(format!("{error}; installed output kept with its transaction journal for recovery (ownership could not be proven for removal)"));
            }
            transactions.finish(&txn.id);
            return Err(error);
        }
        // 覆盖尚未发生（源变了、装不进、状态推进失败）：现场无破坏，销账即可。
        transactions.finish(&txn.id);
        return Err(error);
    }
    transactions.finish(&txn.id);
    // RES-01：结果状态在与历史同一个知情点回填 —— encode 侧只是镜像这一份事实，
    // "历史记 TargetUnmet、结果却报 Committed"的分叉从结构上不可能出现。
    let target_unmet=result.target_met==Some(false);
    result.outcome=Some(if target_unmet{crate::result_model::ResultOutcome::TargetUnmet}else{crate::result_model::ResultOutcome::Committed});
    result.reason_code=if target_unmet{Some(crate::result_model::ReasonCode::TargetUnmet)}else{None};
    result.output_path=Some(final_target.to_string_lossy().into());result.backup_path=backup.map(|p|p.to_string_lossy().into());result.history_id=Some(id);Ok(())
}
#[cfg(test)]mod tests{use super::*;
    use crate::output_transaction::TransactionStage;

    const ORIGINAL:&[u8]=b"true-original-video-bytes";
    const ENCODED:&[u8]=b"compressed-output-bytes";
    fn probe()->VideoProbe{VideoProbe::parse(serde_json::json!({"streams":[{"index":0,"codec_type":"video","width":16,"height":16,"codec_name":"h264","pix_fmt":"yuv420p","avg_frame_rate":"30/1","nb_read_frames":12}],"format":{"duration":"10","format_name":"mkv"}}),ORIGINAL.len() as u64).unwrap()}
    /// replace 模式的公共现场：源文件 + 已过验的真实备份 + 候选产物。
    fn replace_stage(dir:&Path)->(PathBuf,VideoOptions,HistoryStore,TransactionStore,SourceIdentity,PathBuf){
        let source=dir.join("clip.mkv");fs::write(&source,ORIGINAL).unwrap();
        let o=VideoOptions{output_mode:"replace".into(),container:"mkv".into(),..Default::default()};
        let history=HistoryStore::new(dir.join("history")).unwrap();
        let transactions=TransactionStore::new(dir.join("history").join("transactions"));
        let identity=SourceIdentity::read_with_hash(&source).unwrap();
        let backup=history.ensure_backup_verified(&source,identity.sha256.as_deref()).unwrap();
        let partial=dir.join("partial.mkv");fs::write(&partial,ENCODED).unwrap();
        (source,o,history,transactions,identity,backup)
    }

    /// TX-01：覆盖成功后历史写入失败 —— 必须回滚原图、销账、收走换出的保留文件。
    /// 历史写入失败由 TEST_FAULT_FAIL_NEXT_HISTORY_STORE 注入（磁盘满/权限被撤的替身）。
    #[test]
    fn a_history_write_failure_after_replace_restores_the_original(){
        let dir=tempfile::tempdir().unwrap();
        let (source,o,history,transactions,identity,backup)=replace_stage(dir.path());
        let mut result=VideoResult::pending("t1",&source,&o);
        let mut claimed_slot=None;
        crate::history::set_fault_fail_next_history_store(1);
        let error=commit(&source,dir.path().join("partial.mkv").as_path(),&source,&o,&probe(),&mut result,&history,&transactions,3,&identity,Some(&backup),&mut claimed_slot).unwrap_err();
        assert!(error.contains("original was restored"),"必须如实报告已回滚: {error}");
        // 原图字节回来了 —— 数据安全是第一原则。
        assert_eq!(fs::read(&source).unwrap(),ORIGINAL);
        // 历史里没有假记录，事务销账（回滚成功才允许销账）。
        assert!(history.list().is_empty(),"失败的提交不许留下半条历史");
        assert!(!transactions.has_pending(),"回滚成功后事务必须销账");
        // 备份仍在：它由历史/保留策略管理，回滚不许顺手删。
        assert!(backup.exists(),"备份不许被回滚顺手删掉");
        // 换出的原图保留文件（.sizqo-retire-*.keep）已按内容指纹收走 ——
        // 它与备份内容重复，回滚成功后没有存在价值；指纹对不上就绝不会删。
        // 换出的原图保留副本按内容指纹收走（与备份内容重复，没有存在价值）。
        // 回滚自己的原子交换会把源位上的产物字节换出来并按安全策略保留 ——
        // "宁可留下可恢复的副本"：留下的 .keep 只能是产物字节，绝不许是原图。
        let keeps:Vec<PathBuf>=fs::read_dir(dir.path()).unwrap().flatten()
            .map(|e|e.path())
            .filter(|p|p.file_name().map(|n|n.to_string_lossy().contains(".sizqo-retire-")).unwrap_or(false))
            .collect();
        for keep in &keeps{
            assert_ne!(fs::read(keep).unwrap(),ORIGINAL,"原图的保留副本必须被收走: {}",keep.display());
        }
        assert!(keeps.iter().all(|p|fs::read(p).unwrap()==ENCODED),"留下的 .keep 只能是产物副本");
    }

    /// TX-01：回滚失败时保留全部恢复现场 —— 事务日志、备份、换出的原图保留
    /// 文件一个都不许删；随后启动恢复（故障清除）能接着把原图收回来。
    #[test]
    fn a_failed_rollback_keeps_the_full_recovery_scene(){
        let dir=tempfile::tempdir().unwrap();
        let (source,o,history,transactions,identity,backup)=replace_stage(dir.path());
        let mut result=VideoResult::pending("t1",&source,&o);
        let mut claimed_slot=None;
        crate::output_transaction::set_fault_rollback_write_back(true);
        crate::history::set_fault_fail_next_history_store(1);
        let error=commit(&source,dir.path().join("partial.mkv").as_path(),&source,&o,&probe(),&mut result,&history,&transactions,3,&identity,Some(&backup),&mut claimed_slot).unwrap_err();
        assert!(error.contains("rollback failed"),"必须如实报告回滚失败: {error}");
        // 覆盖已发生且回滚失败：源位置是产物，绝不谎称"已恢复"。
        assert_eq!(fs::read(&source).unwrap(),ENCODED);
        // 全部恢复现场原样：日志（带状态机与保留路径）、备份、换出文件。
        assert!(transactions.has_pending(),"回滚失败绝不允许销账");
        let txn=&transactions.list()[0];
        assert_eq!(txn.stage,TransactionStage::OutputPublished,"journal 必须记录覆盖已发生");
        let retired=txn.retired_original_path.as_ref().expect("TX-02：换出路径必须已入账");
        assert!(Path::new(retired).exists(),"回滚失败时换出的原图保留文件是唯一原件副本，绝不清理");
        assert_eq!(fs::read(Path::new(retired)).unwrap(),ORIGINAL);
        assert!(backup.exists(),"备份原样保留");
        // 故障清除后，启动恢复按同一本账把原图收回来 —— 恢复可继续、幂等。
        crate::output_transaction::set_fault_rollback_write_back(false);
        let report=transactions.recover(&history);
        assert_eq!(report.rolled_back,1);
        assert_eq!(fs::read(&source).unwrap(),ORIGINAL,"启动恢复必须把原图写回");
        assert!(!transactions.has_pending());
        let second=transactions.recover(&history);
        assert_eq!(second.rolled_back,0,"二次恢复无事可做（幂等）");
    }

    /// TX-02：交换换出的原图保留文件必须进历史管理（retiredOriginalPath），
    /// 清空历史时按文件身份安全收走。
    #[test]
    fn the_swapped_out_original_is_recorded_for_history_management(){
        let dir=tempfile::tempdir().unwrap();
        let (source,o,history,transactions,identity,backup)=replace_stage(dir.path());
        let mut result=VideoResult::pending("t1",&source,&o);
        let mut claimed_slot=None;
        commit(&source,dir.path().join("partial.mkv").as_path(),&source,&o,&probe(),&mut result,&history,&transactions,3,&identity,Some(&backup),&mut claimed_slot).unwrap();
        let id=result.history_id.clone().expect("提交成功必须回填 history_id");
        let entry=history.list().into_iter().find(|e|e.id==id).expect("历史里必须有这条提交");
        let retired=entry.retired_original_path.clone().expect("换出的原图必须写进历史");
        let retired_path=PathBuf::from(&retired);
        assert!(retired_path.exists(),"保留文件必须在磁盘上");
        assert_eq!(fs::read(&retired_path).unwrap(),ORIGINAL,"保留文件就是被换出的原图字节");
        assert_eq!(entry.output_path.as_deref(),Some(source.to_string_lossy().as_ref()));
        // 清空历史按身份收走保留文件（history.rs 的清理路径消费同一字段）。
        let _=history.clear();
        assert!(!retired_path.exists(),"清空历史必须按身份收走换出的原图副本");
        assert!(backup.is_file()||!backup.exists(),"备份清理走它自己的保留策略");
    }

    /// RES-01：targetSize 未达成 —— 历史必须记 TargetUnmet（不是 Committed，
    /// 也不是 Failed），产物照常提交、可保存可恢复。
    #[test]
    fn a_missed_target_is_committed_as_target_unmet(){
        let dir=tempfile::tempdir().unwrap();let root=dir.path();
        let source=root.join("clip.mp4");fs::write(&source,b"source-bytes").unwrap();
        let o=VideoOptions::default();
        let planned=root.join("clip_compressed.mp4");
        let partial=root.join("partial.mp4");fs::write(&partial,ENCODED).unwrap();
        let history=HistoryStore::new(root.join("history")).unwrap();
        let transactions=TransactionStore::new(root.join("transactions"));
        let identity=SourceIdentity::read(&source).unwrap();
        let mut result=VideoResult::pending("t1",&source,&o);
        // 预算对账已经判出未达成（encode 里 targetSize 分支的职责）。
        result.target_bytes=Some(1.0);result.target_met=Some(false);result.target_deviation=Some(9999.0);
        let mut claimed_slot=None;
        commit(&source,&partial,&planned,&o,&probe(),&mut result,&history,&transactions,3,&identity,None,&mut claimed_slot).unwrap();
        assert_eq!(result.outcome,Some(crate::result_model::ResultOutcome::TargetUnmet));
        assert_eq!(result.reason_code,Some(crate::result_model::ReasonCode::TargetUnmet));
        let id=result.history_id.clone().unwrap();
        let entry=history.list().into_iter().find(|e|e.id==id).expect("历史里有这条");
        let meta=entry.video_metadata.expect("视频历史必须带快照");
        assert_eq!(meta["outcome"],"target_unmet","历史 outcome 必须是 target_unmet");
        assert_eq!(meta["reasonCode"],"target_unmet");
        assert_eq!(meta["targetMet"],serde_json::json!(false),"目标对账必须进历史");
        assert!(meta["options"].is_object(),"RES-02：实际请求参数快照必须进历史");
        assert!(meta["outputProbe"].is_null()||meta["outputProbe"].is_object());
        // 未达成仍然是有效提交：产物在、历史状态是 Compressed（可恢复）。
        assert!(planned.exists());
        assert_eq!(entry.status,crate::history::HistoryStatus::Compressed);
    }

    /// TX-03：换号重试后日志路径更新失败 —— 已安装的产物必须按指纹门收走，
    /// 外部文件分文不动，事务销账，绝不留孤儿。
    #[test]
    fn a_journal_update_failure_never_orphans_the_output(){
        let dir=tempfile::tempdir().unwrap();let root=dir.path();
        let source=root.join("clip.mp4");fs::write(&source,b"source-bytes").unwrap();
        let o=VideoOptions::default();
        let planned=root.join("clip_compressed.mp4");
        fs::write(&planned,b"external-file").unwrap();
        let partial=root.join("partial.mp4");fs::write(&partial,ENCODED).unwrap();
        let history=HistoryStore::new(dir.path().join("history")).unwrap();
        let transactions=TransactionStore::new(dir.path().join("transactions"));
        let identity=SourceIdentity::read(&source).unwrap();
        let mut result=VideoResult::pending("t1",&source,&o);
        let mut claimed_slot=None;
        crate::output_transaction::set_fault_update_output_path(true);
        let error=commit(&source,&partial,&planned,&o,&probe(),&mut result,&history,&transactions,3,&identity,None,&mut claimed_slot).unwrap_err();
        crate::output_transaction::set_fault_update_output_path(false);
        assert!(error.contains("journal"),"错误必须指向日志更新失败: {error}");
        assert_eq!(fs::read(&planned).unwrap(),b"external-file","外部文件分文不动");
        let next=root.join("clip_compressed_1.mp4");
        assert!(!next.exists(),"日志更新失败后已安装的产物必须被安全收走，不留孤儿");
        assert!(!history.contains_committed(&result.history_id.clone().unwrap_or_default()));
        assert!(!transactions.has_pending(),"非覆盖失败收场必须销账");
    }

    /// TX-03 同款：非覆盖模式历史写入失败 —— 产物收走、销账、无孤儿。
    #[test]
    fn a_history_failure_in_suffix_mode_removes_the_uncommitted_output(){
        let dir=tempfile::tempdir().unwrap();let root=dir.path();
        let source=root.join("clip.mp4");fs::write(&source,b"source-bytes").unwrap();
        let o=VideoOptions::default();
        let planned=root.join("clip_compressed.mp4");
        let partial=root.join("partial.mp4");fs::write(&partial,ENCODED).unwrap();
        let history=HistoryStore::new(dir.path().join("history")).unwrap();
        let transactions=TransactionStore::new(dir.path().join("transactions"));
        let identity=SourceIdentity::read(&source).unwrap();
        let mut result=VideoResult::pending("t1",&source,&o);
        let mut claimed_slot=None;
        crate::history::set_fault_fail_next_history_store(1);
        let error=commit(&source,&partial,&planned,&o,&probe(),&mut result,&history,&transactions,3,&identity,None,&mut claimed_slot).unwrap_err();
        assert!(error.contains("injected history write failure"),"{error}");
        assert!(!planned.exists(),"未提交的产物必须被收走");
        assert!(history.list().is_empty());
        assert!(!transactions.has_pending());
    }

    /// REC-01：作业目录清扫只认内容自证的目录；含未知内容的目录保留，
    /// 用户自己的东西分文不动。
    #[test]
    fn the_job_dir_sweep_only_takes_directories_it_can_prove(){
        let dir=tempfile::tempdir().unwrap();let root=dir.path();
        // 可证明的：只有 output.partial / pass-stats 的作业目录。
        let stale=root.join(".sizqo-video-old-1-");
        std::fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("output.partial.mkv"),b"leftover").unwrap();
        fs::write(stale.join("pass-stats-0.log"),b"stats").unwrap();
        // 不可证明的：里面混进了别的文件 —— 整目录保留。
        let foreign=root.join(".sizqo-video-weird-2-");
        std::fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("output.partial.mkv"),b"leftover").unwrap();
        fs::write(foreign.join("user-note.txt"),b"keep me").unwrap();
        // 用户自己的目录：前缀无关，根本不进候选。
        let user=root.join("my-vacation");std::fs::create_dir_all(&user).unwrap();
        fs::write(user.join("clip.mkv"),b"user").unwrap();
        let swept=sweep_stale_video_job_dirs(root);
        assert_eq!(swept,1,"只清扫能证明归属的作业目录");
        assert!(!stale.exists());
        assert!(foreign.exists(),"含未知内容的作业目录必须保留");
        assert!(foreign.join("user-note.txt").exists());
        assert!(user.exists(),"用户目录分文不动");
    }

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
