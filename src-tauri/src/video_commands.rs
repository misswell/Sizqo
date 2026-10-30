//! Media IPC uses one execution session and one frozen input snapshot.
use std::{collections::{HashMap,HashSet},path::{Path,PathBuf},sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}}};
use serde::{Serialize,Deserialize};
use tauri::{AppHandle,State,Manager,Emitter};
use crate::{commands::{AppState,AcquireOutcome},video::{self,VideoOptions,VideoResult,probe::VideoProbe,progress::VideoProgress}};
#[derive(Default)]pub struct VideoRuntime {pub cancellations:Mutex<HashMap<(String,String),Arc<AtomicBool>>>}
#[derive(Debug,Clone,Deserialize)]#[serde(rename_all="camelCase")]
pub struct VideoTask {pub task_id:String,pub path:String}
#[derive(Serialize)]#[serde(rename_all="camelCase")]
pub struct ImportedMedia {path:String,name:String,size:u64,media_kind:String,video_probe:Option<VideoProbe>,warning:Option<String>,source_root:String,relative_path:String}
/// 内容级图片识别（魔数嗅探）：导入分类（probe_media）和会话收集
/// （commands::collect_image_files）共用这一份判据，不许两套标准。
pub(crate) fn image_header(path:&Path)->bool {
    use std::io::Read;let mut b=[0u8;64];let n=std::fs::File::open(path).and_then(|mut f|f.read(&mut b)).unwrap_or(0);let b=&b[..n];
    image::guess_format(b).is_ok() || b.starts_with(&[0xff,0x0a]) || b.starts_with(b"\0\0\0\x0cJXL ") ||
        (b.get(4..8)==Some(b"ftyp")&&b.get(8..12).map(|s|[b"avif" as &[u8],b"avis",b"heic",b"heix",b"mif1"].contains(&s)).unwrap_or(false))
}
/// 导入时就把「这个文件属于哪个来源根、相对位置是什么」定死（§39 §40）。
///
/// `root` 是这次导入的一个来源根：目录导入就是那个目录，单文件导入就是它的父目录。
/// 之后队列项、重试、续跑、导出都只读这两个值，不再每批重新猜。
fn collect(root:&Path,path:&Path,seen:&mut HashSet<PathBuf>,out:&mut Vec<(PathBuf,PathBuf,PathBuf)>){
    // 根和文件都走同一次 canonicalize：macOS 的 /var → /private/var 这类软链
    // 会让"明明是子路径"的两个字符串对不上，相对路径就退化成文件名。
    let Ok(root)=root.canonicalize()else{return};
    let Ok(real)=path.canonicalize()else{return};if !seen.insert(real.clone()){return;}
    // §8.1 默认不跟随指向根目录之外的符号链接：软链指去别的盘/别的目录时，
    // 导入的就成了链接目标而不是用户点开的那份树。目录循环本身由 seen 集合
    // （规范路径去重）兜住，这里再补一道"不许越界"。
    if real.starts_with(&root)==false {return;}
    if real.is_dir(){if let Ok(children)=std::fs::read_dir(&real){let mut children:Vec<_>=children.flatten().map(|e|e.path()).collect();children.sort();for p in children{collect(&root,&p,seen,out);}}}else if real.is_file(){
        let rel=real.strip_prefix(&root).map(Path::to_path_buf).unwrap_or_else(|_|PathBuf::from(real.file_name().unwrap_or_default()));
        out.push((real,root,rel));
    }
}
/// 低成本嗅探"这可能是视频"：常见容器的魔数。扩展名之外多认这一层，
/// 无扩展名但内容有效的视频仍然进得了队列；明显不是媒体的文件则不再为它们起 ffprobe。
fn plausible_video_header(path:&Path)->bool{
    use std::io::Read;let mut b=[0u8;376];let n=std::fs::File::open(path).and_then(|mut f|f.read(&mut b)).unwrap_or(0);let b=&b[..n];
    b.starts_with(&[0x1a,0x45,0xdf,0xa3])                                       // EBML（mkv/webm/mka）
        || b.get(4..8)==Some(b"ftyp" as &[u8])                                   // mp4/mov/m4v/3gp…
        || (b.starts_with(b"RIFF")&&b.get(8..12)==Some(&b"AVI "[..]))             // AVI
        || b.starts_with(b"OggS")                                                // ogv/ogm
        || b.starts_with(b"FLV")                                                 // Flash Video
        || b.starts_with(&[0x30,0x26,0xb2,0x75,0x8e,0x66,0xcf,0x11])             // ASF（wmv/asf）
        || b.starts_with(&[0x00,0x00,0x01,0xba])                                 // MPEG-PS（mpg/vob）
        || (b.first()==Some(&0x47)&&b.get(188)==Some(&0x47))                     // MPEG-TS：188 字节固定包
}
/// 是否值得为它起一次 ffprobe：明确的视频扩展名，或嗅探出了媒体容器特征。
fn probe_candidate(path:&Path)->bool{video::is_video_extension(path)||plausible_video_header(path)}
#[tauri::command]
pub async fn probe_media(paths:Vec<String>,state:State<'_,AppState>)->Result<Vec<ImportedMedia>,String>{
    state.access.remember_all(&paths);let access=state.access.clone();
    tauri::async_runtime::spawn_blocking(move||{
        let mut guards=vec![];for path in &paths{guards.push(access.acquire(Path::new(path),&mut||false).ok_or_else(||format!("Select the folder again to grant access: {path}"))?);}
        let mut files=vec![];let mut seen=HashSet::new();
        for p in paths {
            let Ok(real)=Path::new(&p).canonicalize()else{continue};
            let root=if real.is_dir(){real.clone()}else{real.parent().map(Path::to_path_buf).unwrap_or_else(||real.clone())};
            collect(&root,Path::new(&p),&mut seen,&mut files);
        }
        // §6.4：图片当场分类（魔数嗅探只读 64 字节，很便宜）；视频候选按槽位记录、
        // 有界并发探测；其余文件既不是图片也没有任何媒体特征，不再为它们白付 ffprobe。
        #[derive(Clone)]
        struct Candidate{index:usize,path:PathBuf,name:String,source_root:String,relative:String,size:u64}
        fn probe_video(index:usize,path:&Path,name:String,source_root:String,relative:String,size:u64)->(usize,Option<ImportedMedia>){
            let entry=|video_probe:Option<VideoProbe>,warning:Option<String>|(index,Some(ImportedMedia{path:path.to_string_lossy().into(),name,size,media_kind:"video".into(),video_probe,warning,source_root,relative_path:relative}));
            match video::probe::probe(path){
                Ok(probe)=>entry(Some(probe),None),
                Err(error)=>{
                    // 扩展名说它是视频但探测失败：仍进队列，带着原因让用户看得见。
                    if video::is_video_extension(path){entry(None,Some(error))}else{(index,None)}
                }
            }
        }
        let mut slots:Vec<Option<ImportedMedia>>=Vec::with_capacity(files.len());
        let mut candidates:Vec<Candidate>=vec![];
        for (index,(path,root,relative)) in files.iter().enumerate(){
            let size=std::fs::metadata(path).map(|m|m.len()).unwrap_or(0);
            let name=path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let source_root=root.to_string_lossy().into_owned();
            let relative=relative.to_string_lossy().into_owned();
            if image_header(path){slots.push(Some(ImportedMedia{path:path.to_string_lossy().into(),name,size,media_kind:"image".into(),video_probe:None,warning:None,source_root,relative_path:relative}));}
            else if probe_candidate(path){slots.push(None);candidates.push(Candidate{index,path:path.clone(),name,source_root,relative,size});}
            else{slots.push(None);}
        }
        let workers=std::thread::available_parallelism().map(|n|n.get()).unwrap_or(2).min(4).max(1);
        if candidates.len()>1&&workers>1{
            let chunk_size=candidates.len().div_ceil(workers);
            // §8.1 线程 panic 不许静默吞掉整块结果：worker 炸了就把那一块的
            // 每个文件标成失败原因"import worker panicked"，其余块照常回收。
            let filled:Vec<Vec<(usize,Option<ImportedMedia>)>>=std::thread::scope(|scope|{
                let handles:Vec<_>=candidates.chunks(chunk_size).map(|chunk|{let chunk:Vec<Candidate>=chunk.to_vec();scope.spawn(move||{
                    // §8.1 panic 隔离：探测闭包只**借用**块数据，panic 之后块还在
                    // 外层手里 —— 把这一块的每个文件按"探测失败"入队（带原因），
                    // 其余块照常回收。整块静默消失是被禁止的。
                    let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
                        chunk.iter().map(|c|probe_video(c.index,&c.path,c.name.clone(),c.source_root.clone(),c.relative.clone(),c.size)).collect::<Vec<(usize,Option<ImportedMedia>)>>()
                    }));
                    match result{
                        Ok(entries)=>entries,
                        Err(_)=>{
                            log::error!("导入探测 worker panic，本块 {} 个文件按探测失败处理",chunk.len());
                            chunk.iter().map(|c|(c.index,Some(ImportedMedia{
                                path:c.path.to_string_lossy().into(),name:c.name.clone(),size:c.size,
                                media_kind:"video".into(),video_probe:None,
                                warning:Some("import worker panicked".into()),
                                source_root:c.source_root.clone(),relative_path:c.relative.clone()}))).collect()
                        }
                    }
                })}).collect();
                handles.into_iter().filter_map(|handle|handle.join().ok()).collect()
            });
            for batch in filled{for (index,entry) in batch{slots[index]=entry;}}
        }else{
            for candidate in candidates{let (index,entry)=probe_video(candidate.index,&candidate.path,candidate.name,candidate.source_root,candidate.relative,candidate.size);slots[index]=entry;}
        }
        Ok::<_,String>(slots.into_iter().flatten().collect())
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
pub async fn get_video_capabilities()->Result<video::capabilities::CapabilityMatrix,String>{tauri::async_runtime::spawn_blocking(video::capabilities::capabilities).await.map_err(|e|e.to_string())}
#[tauri::command]
pub async fn get_video_presets(app:AppHandle)->Result<Vec<video::presets::VideoPreset>,String>{
    let c=get_video_capabilities().await?;let mut out=video::presets::presets(&c);
    let path=app.path().app_data_dir().map_err(|e|e.to_string())?.join("video-presets.json");
    if path.exists() {
        let entries:HashMap<String,VideoOptions>=serde_json::from_slice(&std::fs::read(path).map_err(|e|e.to_string())?).map_err(|e|format!("Preset file is damaged; preserved unchanged: {e}"))?;
        for (name,options) in entries {let available=c.available&&(options.encoder=="auto"||c.encoder(&options.encoder).is_some());
            out.push(video::presets::VideoPreset{id:format!("user:{name}"),name,description:"Saved custom preset; options are validated against each source".into(),available,reason:if available{None}else{Some("Encoder unavailable in this edition".into())},options});
        }
    }Ok(out)
}
#[tauri::command]
pub async fn validate_video_options(options:VideoOptions,probe:VideoProbe,state:State<'_,AppState>)->Result<video::plan::EncodePlan,String>{let c=get_video_capabilities().await?;video::plan::build(&options,&probe,&c,state.compression.max_parallelism())}

/// §7.2 单个视频的预检结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoPreflightResult {
    pub path: String,
    pub valid: bool,
    pub reason: Option<String>,
    pub warnings: Vec<String>,
}

/// §7.2 批量预检的入参：路径 + 导入时缓存的探测结果。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct VideoPreflightItem {
    pub path: String,
    pub probe: VideoProbe,
}

/// §7.2 批量预检：**一次 IPC** 校验整批视频，前端不再对 100 个视频连发 100 次
/// `validate_video_options`。探测在导入时已完成并随队列项缓存，这里只跑纯 CPU
/// 的计划构建；单个失败不阻塞其他文件，逐文件独立作答。
#[tauri::command]
pub async fn validate_video_batch(
    items: Vec<VideoPreflightItem>,
    options: VideoOptions,
    state: State<'_, AppState>,
) -> Result<Vec<VideoPreflightResult>, String> {
    let caps = get_video_capabilities().await?;
    let budget = state.compression.max_parallelism();
    // plan::build 是纯 CPU（微秒级、无 IO），顺序跑完即可；真正的开销本来就在
    // 每文件一次的 IPC 往返上，批量接口把它从 N 次压成 1 次。
    let results = items
        .into_iter()
        .map(|item| match video::plan::build(&options, &item.probe, &caps, budget) {
            Ok(plan) => VideoPreflightResult {
                path: item.path,
                valid: true,
                reason: None,
                warnings: plan.warnings,
            },
            Err(reason) => VideoPreflightResult {
                path: item.path,
                valid: false,
                reason: Some(reason),
                warnings: vec![],
            },
        })
        .collect();
    Ok(results)
}
#[tauri::command]
pub fn save_video_preset(name:String,options:VideoOptions,app:AppHandle)->Result<(),String>{
    // Names never become filesystem paths; all presets share one atomic store.
    if name.trim().is_empty()||name.chars().count()>80{return Err("Preset name must contain 1…80 characters".into());}
    let path=app.path().app_data_dir().map_err(|e|e.to_string())?.join("video-presets.json");
    static LOCK:Mutex<()>=Mutex::new(());let _lock=LOCK.lock().map_err(|e|e.to_string())?;
    let mut entries:HashMap<String,VideoOptions>=match std::fs::read(&path){Ok(b)=>serde_json::from_slice(&b).map_err(|e|format!("Preset file is damaged; preserved unchanged: {e}"))?,Err(e)if e.kind()==std::io::ErrorKind::NotFound=>HashMap::new(),Err(e)=>return Err(e.to_string())};
    if entries.len()>=100&&!entries.contains_key(&name){return Err("Preset limit reached".into());}entries.insert(name,options);crate::history::write_atomic(&path,&serde_json::to_vec_pretty(&entries).map_err(|e|e.to_string())?)
}
#[tauri::command]
pub fn cancel_video_task(task_id:String,session_id:String,app:AppHandle,state:State<'_,AppState>)->bool{
    let runtime=app.state::<VideoRuntime>();let tokens=runtime.cancellations.lock().unwrap();
    if let Some(token)=tokens.get(&(session_id,task_id)){token.store(true,Ordering::Release);state.compression.wake_waiters();true}else{false}
}
async fn video_batch(app:&AppHandle,state:&AppState,tasks:Vec<VideoTask>,options:VideoOptions,session:String,revision:u64,roots:Vec<String>)->Result<Vec<VideoResult>,String>{
    // 任何早退（canonicalize 失败、能力获取失败、app_data_dir 失败……）都不许把
    // 已登记的取消令牌留在账上：令牌按 (session,taskId) 记账，滞留的旧令牌
    // 会话号一旦复用就会误伤下一轮同 id 的任务。
    let outcome=video_batch_inner(app,state,tasks,options,&session,revision,roots).await;
    if outcome.is_err(){app.state::<VideoRuntime>().cancellations.lock().unwrap().retain(|(sid,_),_|sid!=&session);}
    outcome
}
/// §5.3 单文件失效只拖垮它自己：路径解析（canonicalize）失败的条目就地记成 failed，
/// 绝不用 `?` 把整批拖下水 —— 导入之后文件被移动/删除是常态，一张坏卡不能没收走
/// 整批有效视频的执行机会。同一批里重复出现的路径只执行第一次，与原行为一致。
fn resolve_video_tasks(tasks: Vec<VideoTask>, options: &VideoOptions) -> (Vec<(VideoTask, PathBuf)>, Vec<VideoResult>) {
    let mut seen = HashSet::new();
    let mut ready = vec![];
    let mut failed = vec![];
    for task in tasks {
        let path = match PathBuf::from(&task.path).canonicalize() {
            Ok(path) => path,
            Err(error) => {
                let mut result = VideoResult::pending(&task.task_id, Path::new(&task.path), options);
                result.outcome = Some(crate::result_model::ResultOutcome::Failed);
                result.reason_code = Some(crate::result_model::ReasonCode::SourceNotFound);
                result.reason = Some(format!("Input file is no longer available: {error}"));
                failed.push(result);
                continue;
            }
        };
        if !seen.insert(path.clone()) { continue; }
        ready.push((task, path));
    }
    (ready, failed)
}
async fn video_batch_inner(app:&AppHandle,state:&AppState,tasks:Vec<VideoTask>,options:VideoOptions,session:&str,revision:u64,roots:Vec<String>)->Result<Vec<VideoResult>,String>{
    let runtime=app.state::<VideoRuntime>();
    let (ready,mut results)=resolve_video_tasks(tasks,&options);
    let mut tokens=vec![];
    for (task,path) in ready{let token=Arc::new(AtomicBool::new(false));
        runtime.cancellations.lock().unwrap().insert((session.to_string(),task.task_id.clone()),token.clone());tokens.push((task,path,token));}
    // 就地失败的条目当场把终态播出去：前端不必等会话返回就能落定这一行。
    for result in &results{let mut event=VideoProgress::new(&result.task_id,session,revision,"failed");event.result=Some(result.clone());let _=app.emit("video-progress",event);}
    let caps=get_video_capabilities().await?;
    for (task,path,cancel) in tokens {
        let emit=|event|{let _=app.emit("video-progress",event);};
        emit(VideoProgress::new(&task.task_id,session,revision,"queued"));
        let permit=match state.compression.acquire(||cancel.load(Ordering::Acquire)||state.cancel_queue.lock().unwrap().contains(&task.path)).await{
            AcquireOutcome::Acquired(p)=>p,
            AcquireOutcome::Stopped=>{emit(VideoProgress::new(&task.task_id,session,revision,"deferred"));runtime.cancellations.lock().unwrap().remove(&(session.to_string(),task.task_id.clone()));continue;},
            AcquireOutcome::Cancelled=>{emit(VideoProgress::new(&task.task_id,session,revision,"cancelled"));runtime.cancellations.lock().unwrap().remove(&(session.to_string(),task.task_id.clone()));continue;},
        };
        emit(VideoProgress::new(&task.task_id,session,revision,"starting"));
        #[cfg(not(feature="inproc-backends"))]
        let result={
            let history=state.history_store.clone();let transactions=state.transactions.clone();let retention=state.settings_store.load().original_retention_days;let budget=state.compression.max_parallelism();
            let logs=app.path().app_data_dir().map_err(|e|e.to_string())?.join("video-logs");let options=options.clone();let caps=caps.clone();let session=session.to_string();let roots=roots.clone();let app=app.clone();let task_id=task.task_id.clone();let access=state.access.clone();
            tauri::async_runtime::spawn_blocking(move||{
                // 守卫拿不到授权就立刻失败：把拒绝前置，而不是让 ffmpeg 在半路
                // 以 IO 错误的姿势撞上沙盒（AGENTS 规则 12：守卫必须覆盖编码全程）。
                let _scope=match access.acquire(&path,&mut||false){
                    Some(scope)=>scope,
                    None=>{
                        let mut r=VideoResult::pending(&task_id,&path,&options);
                        r.stage="failed".into();
                        r.outcome=Some(crate::result_model::ResultOutcome::Failed);
                        r.reason_code=Some(crate::result_model::ReasonCode::PermissionDenied);
                        r.reason=Some("File authorization expired; select the file again".into());
                        return r;
                    }
                };
                video::ffmpeg_backend::encode(&task_id,&path,&options,&caps,&session,revision,&roots,&history,&transactions,retention,budget,&logs,cancel,&|event|{let _=app.emit("video-progress",event);})
            }).await.map_err(|e|e.to_string())?
        };
        #[cfg(feature="inproc-backends")]
        let result={let _=(&caps,&roots);let mut r=VideoResult::pending(&task.task_id,&path,&options);r.backend_actual="unavailable".into();r.reason=Some(video::capabilities::NATIVE_UNAVAILABLE.into());r.outcome=Some(crate::result_model::ResultOutcome::Failed);r.reason_code=Some(crate::result_model::ReasonCode::UnsupportedFormat);r};
        drop(permit);let mut event=VideoProgress::new(&task.task_id,session,revision,&result.stage);event.fraction=if result.success{Some(1.0)}else{None};event.result=Some(result.clone());emit(event);
        results.push(result);runtime.cancellations.lock().unwrap().remove(&(session.to_string(),task.task_id));
    }Ok(results)
}
/// §37 §38：视频不另有一套会话凭证。图片、视频、混合三条入口共用
/// `commands::SessionGuard`，同一时刻只有一个 compression session，
/// 暂停/停止/取消的清理也只有一份实现。
use crate::commands::SessionGuard;
#[tauri::command]
pub async fn start_video_session(app:AppHandle,state:State<'_,AppState>,tasks:Vec<VideoTask>,options:VideoOptions,session_id:String,queue_revision:u64,source_roots:Vec<String>)->Result<Vec<VideoResult>,String>{let _session=SessionGuard::begin(&app,state.inner())?;video_batch(&app,state.inner(),tasks,options,session_id,queue_revision,source_roots).await}
#[derive(Serialize)]#[serde(rename_all="camelCase")]
pub struct MediaSessionResult {session_id:String,image_results:Vec<crate::engine::CompressResult>,video_results:Vec<VideoResult>}
#[tauri::command]
pub async fn start_media_session(app:AppHandle,state:State<'_,AppState>,images:Vec<String>,video_tasks:Vec<VideoTask>,image_options:crate::engine::CompressOptions,video_options:VideoOptions,session_id:String,queue_revision:u64,source_roots:Vec<String>,use_smart:bool)->Result<MediaSessionResult,String>{
    let _access=crate::commands::prepare_compression_access(&app,state.inner(),&images,&image_options)?;
    let session=SessionGuard::begin(&app,state.inner())?;
    let image_results=if images.is_empty(){vec![]}else{crate::commands::compress_batch(&app,state.inner(),&session,session_id.clone(),queue_revision,images,image_options,use_smart).await.results};
    let video_results=video_batch(&app,state.inner(),video_tasks,video_options,session_id.clone(),queue_revision,source_roots).await?;
    Ok(MediaSessionResult{session_id,image_results,video_results})
}
#[tauri::command]
pub async fn open_media_file(path:String,app:AppHandle,state:State<'_,AppState>)->Result<(),String>{use tauri_plugin_opener::OpenerExt;
    let path=Path::new(&path).canonicalize().map_err(|e|e.to_string())?;let _scope=state.access.acquire(&path,&mut||false).ok_or("File authorization expired; select it again")?;
    if !path.is_file(){return Err("Media file no longer exists".into());}app.opener().open_path(path.to_string_lossy(),None::<&str>).map_err(|e|e.to_string())
}
#[tauri::command]
pub async fn preview_video_frame(path:String,time_us:u64,state:State<'_,AppState>)->Result<String,String>{
    let access=state.access.clone();tauri::async_runtime::spawn_blocking(move||frame(&path,time_us,access.as_ref())).await.map_err(|e|e.to_string())?
}
#[cfg(not(feature="inproc-backends"))]
fn frame(path:&str,time_us:u64,access:&dyn crate::sandbox_access::FileAccess)->Result<String,String>{
    use base64::Engine;let path=Path::new(path).canonicalize().map_err(|e|e.to_string())?;let _scope=access.acquire(&path,&mut||false).ok_or("Video access denied")?;
    let p=video::probe::probe(&path)?;let t=time_us.min(p.duration_us.map(|d|d.saturating_sub(1000)).unwrap_or(time_us));
    let mut cmd=video::process::command("ffmpeg")?;
    cmd.args(["-hide_banner","-v","error","-nostdin","-protocol_whitelist","file,pipe","-threads","1","-ss"]).arg(format!("{:.6}",t as f64/1e6)).arg("-i").arg(path)
        .args(["-map","0:v:0","-frames:v","1","-filter_threads","1","-vf","scale=1280:1280:force_original_aspect_ratio=decrease","-threads","1","-f","image2pipe","-vcodec","mjpeg","pipe:1"]);
    let b=video::process::capture(cmd,std::time::Duration::from_secs(30),4*1024*1024)?;if b.is_empty(){return Err("Preview frame unavailable".into());}Ok(format!("data:image/jpeg;base64,{}",base64::engine::general_purpose::STANDARD.encode(b)))
}
#[cfg(feature="inproc-backends")]
fn frame(_path:&str,_time:u64,_access:&dyn crate::sandbox_access::FileAccess)->Result<String,String>{Err(video::capabilities::NATIVE_UNAVAILABLE.into())}
#[tauri::command]
pub async fn compare_video_frames(source_path:String,output_path:String,time_us:u64,state:State<'_,AppState>)->Result<serde_json::Value,String>{
    let access=state.access.clone();tauri::async_runtime::spawn_blocking(move||{
        let a=video::probe::probe(Path::new(&source_path))?;let b=video::probe::probe(Path::new(&output_path))?;
        let time=time_us.min(a.duration_us.unwrap_or(time_us).min(b.duration_us.unwrap_or(time_us)).saturating_sub(1000));
        let source_frame=frame(&source_path,time,access.as_ref())?;let output_frame=frame(&output_path,time,access.as_ref())?;
        Ok::<_,String>(serde_json::json!({"sourceFrame":source_frame,"outputFrame":output_frame,"timeUs":time}))
    }).await.map_err(|e|e.to_string())?
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use std::fs;

    /// §39 §40：来源根与相对路径在导入时定死，目录导入保住层级，单文件导入落在父目录。
    #[test]
    fn an_imported_file_carries_its_root_and_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        let nested = root.join("2026");
        fs::create_dir_all(&nested).unwrap();
        let photo = nested.join("a.jpg");
        fs::write(&photo, b"\xff\xd8\xff\xe0not-really-a-jpeg").unwrap();
        let loose = temp.path().join("loose.jpg");
        fs::write(&loose, b"\xff\xd8\xff\xe0loose").unwrap();

        let mut seen = HashSet::new();
        let mut files = vec![];
        collect(&root, &root, &mut seen, &mut files);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, root.canonicalize().unwrap(), "目录导入的来源根就是被选中的目录");
        assert_eq!(files[0].2, PathBuf::from("2026/a.jpg"), "相对路径保住目录层级");

        let mut seen = HashSet::new();
        let mut files = vec![];
        collect(temp.path(), &loose, &mut seen, &mut files);
        assert_eq!(files[0].1, temp.path().canonicalize().unwrap(), "单文件导入的来源根是它的父目录");
        assert_eq!(files[0].2, PathBuf::from("loose.jpg"));
    }

    /// 同一次导入里父目录和子目录同时被选：同一个文件只收一次，根各自归各自。
    #[test]
    fn overlapping_import_roots_never_duplicate_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        let nested = root.join("2026");
        fs::create_dir_all(&nested).unwrap();
        let photo = nested.join("a.jpg");
        fs::write(&photo, b"\xff\xd8\xff\xe0x").unwrap();

        let mut seen = HashSet::new();
        let mut files = vec![];
        collect(&root, &root, &mut seen, &mut files);
        collect(&nested, &nested, &mut seen, &mut files);
        assert_eq!(files.len(), 1, "同一个文件不能在队列里出现两次");
        assert_eq!(files[0].1, root.canonicalize().unwrap(), "先被哪个根收下，就按哪个根算相对路径");
        assert_eq!(files[0].2, PathBuf::from("2026/a.jpg"));
    }

    /// §5.3 单文件错误隔离：一个失效路径只产生一条 failed 结果，绝不连坐整批；
    /// 同批重复路径只保留第一次；有效任务的顺序保持不变。
    #[test]
    fn a_missing_video_fails_alone_and_never_sinks_the_batch() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good.mp4");
        fs::write(&good, b"not-really-a-video").unwrap();
        let tasks = vec![
            VideoTask { task_id: "t1".into(), path: good.to_string_lossy().into_owned() },
            VideoTask { task_id: "t2".into(), path: temp.path().join("missing.mp4").to_string_lossy().into_owned() },
            VideoTask { task_id: "t3".into(), path: good.to_string_lossy().into_owned() },
        ];
        let (ready, failed) = resolve_video_tasks(tasks, &VideoOptions::default());
        assert_eq!(ready.len(), 1, "有效任务必须照常进入执行名单");
        assert_eq!(ready[0].0.task_id, "t1");
        assert_eq!(failed.len(), 1, "失效文件就地失败，不拖垮别人");
        assert_eq!(failed[0].task_id, "t2");
        assert_eq!(failed[0].reason_code, Some(crate::result_model::ReasonCode::SourceNotFound));
        assert_eq!(failed[0].outcome, Some(crate::result_model::ResultOutcome::Failed));
        assert!(!failed[0].success, "失效文件不许伪装成功");
    }

    /// §6.4 低成本嗅探：常见容器魔数要认得出来，无扩展名但内容有效的视频仍进得了队列；
    /// 文本之类明显不是媒体的文件不再各自起一次 ffprobe。
    #[test]
    fn video_header_sniffing_recognizes_containers_and_ignores_documents() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).unwrap();
            path
        };
        let mp4 = write("clip.mp4", b"\x00\x00\x00\x18ftypisom\xff\xd8garbage");
        let mkv = write("movie.mkv", &[0x1a, 0x45, 0xdf, 0xa3, 0x01, 0x00]);
        let avi = write("old.avi", b"RIFF\x00\x00\x00\x00AVI LIST");
        let webm = write("raw", &[0x1a, 0x45, 0xdf, 0xa3, 0x99]);
        let ts = write("stream.ts", &{
            let mut bytes = vec![0x47u8];
            bytes.extend(std::iter::repeat(0u8).take(187));
            bytes.push(0x47);
            bytes
        });
        let text = write("notes.txt", b"just some meeting notes, nothing media about them");

        for path in [&mp4, &mkv, &avi, &webm, &ts] {
            assert!(plausible_video_header(path), "{} 应被认作媒体候选", path.display());
        }
        assert!(!plausible_video_header(&text), "文本文件不该被送去 ffprobe");
        assert!(probe_candidate(&mp4));
        assert!(probe_candidate(&webm), "无扩展名但内容是 EBML 的文件仍要探测");
        assert!(!probe_candidate(&text), "文本文件在候选筛选这一步就该被跳过");
    }
}
