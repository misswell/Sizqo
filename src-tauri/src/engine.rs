// Sizqo - Rust compression engine
// Replaces the Node.js engine (sharp/squoosh) with CLI tools + the `image` crate.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;
use serde::{Deserialize, Serialize};
use crate::result_model::{EncodeVerdict, ReasonCode, ResultOutcome};

#[cfg(feature = "inproc-backends")]
#[path = "engine_inproc.rs"]
mod engine_inproc;

/// 全局存储应用资源目录路径（在 setup 时初始化）
static RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 初始化资源目录路径（由 lib.rs setup 调用）
pub fn set_resource_dir(path: PathBuf) {
    let _ = RESOURCE_DIR.set(path);
}

/// 获取库目录路径（resources/lib）
fn get_lib_dir() -> Option<PathBuf> {
    if let Some(res_dir) = RESOURCE_DIR.get() {
        let lib_dir = res_dir.join("resources").join("lib");
        if lib_dir.exists() {
            return Some(lib_dir);
        }
        let lib_dir = res_dir.join("lib");
        if lib_dir.exists() {
            return Some(lib_dir);
        }
    }
    let dev_lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join("lib");
    if dev_lib.exists() {
        return Some(dev_lib);
    }
    None
}

/// 第一阶段的基础策略：一个文件任务只占 1 份 CPU 预算，编码器内部也只开 1 个 worker。
///
/// 于是"上限 = 4"真的是"约 4 份并行 CPU 工作"，而不是"3 个文件 × 每个吃满全核"。
/// 将来要做动态分配（把预算分给单文件的多线程）只改这一个函数。
pub fn per_task_threads() -> usize {
    1
}

/// 工具自身的 CPU 开关。集中在这里，PNG/JPG/WebP/AVIF/GIF 各函数不再各写一套。
fn cpu_flags(tool: &str, threads: usize) -> Vec<String> {
    match tool {
        // avifenc 的 --jobs 默认值在不同发行版本里不一致（有版本是 all），必须显式给。
        "avifenc" => vec!["--jobs".into(), threads.to_string()],
        // oxipng 内部是 rayon 多线程，显式 --threads 比只靠环境变量更确定。
        "oxipng" => vec!["--threads".into(), threads.to_string()],
        // cwebp 的 -mt 只能"开"或"不开"，没法指定几线程 —— 单 worker 就别开。
        "cwebp" if threads > 1 => vec!["-mt".into()],
        // cjxl 的 --num_threads 默认 0 = 按硬件线程数全开，必须显式钉住，
        // 否则"上限 3"变成"3 个文件 × 每个用满全部核心"。
        "cjxl" => vec![format!("--num_threads={threads}")],
        _ => Vec::new(),
    }
}

/// 从可执行文件路径取纯工具名。Windows 的 `.exe` 后缀不能影响判断。
fn tool_name(tool: &Path) -> String {
    tool.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

/// 子进程的统一线程类环境变量。
///
/// 只对认这些变量的 runtime 有效，不能假设所有第三方二进制都遵守 ——
/// 所以真正的多线程工具另有命令行开关（见 `cpu_flags`）。
fn cpu_env() -> Vec<(&'static str, String)> {
    let threads = per_task_threads().to_string();
    vec![
        ("OMP_NUM_THREADS", threads.clone()),
        ("RAYON_NUM_THREADS", threads),
    ]
}

/// 给子进程套上 CPU 预算。
fn configure_cpu_limits(cmd: &mut Command, tool: &Path) {
    for (key, value) in cpu_env() {
        cmd.env(key, value);
    }
    for flag in cpu_flags(&tool_name(tool), per_task_threads()) {
        cmd.arg(flag);
    }
}

/// 创建带有正确环境变量的 Command（自动设置 DYLD_FALLBACK_LIBRARY_PATH）
fn make_command(tool: &Path) -> Command {
    let mut cmd = Command::new(tool);
    cmd.kill_on_drop(true);
    #[cfg(target_os = "windows")]
    {
        // CREATE_NO_WINDOW: 避免每个 CLI 工具弹出控制台黑框
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if cfg!(target_os = "macos") {
        if let Some(lib_dir) = get_lib_dir() {
        // macOS: 设置 DYLD_FALLBACK_LIBRARY_PATH 让工具找到内置动态库
        // 这样无需修改二进制文件的依赖路径，保持原始签名有效
        let current = std::env::var("DYLD_FALLBACK_LIBRARY_PATH").unwrap_or_default();
        let combined = if current.is_empty() {
            lib_dir.to_string_lossy().into_owned()
        } else {
            format!("{}:{}", lib_dir.to_string_lossy(), current)
        };
        cmd.env("DYLD_FALLBACK_LIBRARY_PATH", combined);
        }
    }
    configure_cpu_limits(&mut cmd, tool);
    cmd
}

/// 压缩请求里**每一个字段都必须被某条引擎路径真正读到**（AGENTS.md：不许有假控件）。
///
/// `backend` / `effort` / `convertToWebp` / `lossless` / `smartMode` 已删除：没有任何
/// Rust 引擎路径读过它们。smart 路由的真实载体是命令参数 `use_smart` 与
/// `compress_smart`，不是这里的一个布尔字段。
/// Serde 默认忽略未知字段，所以老的历史 payload / localStorage 里带着这些键仍然能反序列化。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct CompressOptions {
    #[serde(default = "default_processing_mode")]
    pub processing_mode: String,
    #[serde(default = "default_system_image_size")]
    pub system_image_size: String,
    #[serde(default = "default_true")]
    pub preserve_metadata: bool,
    #[serde(default = "default_quality", deserialize_with = "deserialize_quality")]
    pub quality: u32,
    #[serde(default = "default_format")]
    pub output_format: String,
    #[serde(default = "default_mode")]
    pub output_mode: String,
    #[serde(default = "default_output_suffix")]
    pub output_suffix: String,
    #[serde(default)]
    pub output_dir: Option<String>,
    /// Original file/folder selections used to preserve relative paths when
    /// the frontend sends an already-expanded queue snapshot.
    #[serde(default)]
    pub source_roots: Vec<String>,
    /// 最长边上限（像素）。**只缩不放大**；缺省 / 0 / None = 不加尺寸约束。
    /// Direct 走 CLI 编码器之前、App Store 走进程内编码器之前，都在同一个
    /// `prepare_working_file()` 里兑现它（见下方）。
    #[serde(default)]
    pub max_dimension_px: Option<u32>,
    /// 目标体积（字节）。有值时走 `solve_to_budget()` 的有界阶梯搜索（最多 6 次编码），
    /// 命中即停；没命中时返回**全程最小的有效候选**并把结论说清楚，绝不报成成功。
    #[serde(default)]
    pub max_output_bytes: Option<u64>,
}

fn default_quality() -> u32 { 75 }

/// IPC 边界就把 quality 拦在 0..=100：放进来再 `as u8` 截断（300 → 44），
/// 用户得到的是一份从未请求过的质量，还没有任何报错。
fn deserialize_quality<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = u32::deserialize(deserializer)?;
    if value > 100 {
        return Err(serde::de::Error::custom(format!(
            "quality must be within 0..=100, got {value}"
        )));
    }
    Ok(value)
}
fn default_processing_mode() -> String { "advanced".into() }
fn default_system_image_size() -> String { "actual".into() }
fn default_true() -> bool { true }
fn default_format() -> String { "original".into() }
fn default_mode() -> String { "suffix".into() }
fn default_output_suffix() -> String { "_compressed".into() }

impl Default for CompressOptions {
    fn default() -> Self {
        Self {
            processing_mode: "advanced".into(), system_image_size: "actual".into(),
            preserve_metadata: true,
            quality: 75, output_format: "original".into(),
            output_mode: "suffix".into(), output_suffix: "_compressed".into(),
            output_dir: None, source_roots: Vec::new(),
            max_dimension_px: None, max_output_bytes: None,
        }
    }
}

/// Internal result carrying the compressed bytes.
#[derive(Debug, Clone, Default)]
pub struct EngineResult {
    pub success: bool,
    pub compressed: Vec<u8>,
    pub out_type: String,
    pub algorithm: String,
    pub error: Option<String>,
    /// 用户可见的告警（规则 11：元数据、alpha、未达成的目标都不许静默丢）。
    pub warnings: Vec<String>,
    /// 本次请求的体积目标（来自 `max_output_bytes`）；没有请求时为 None。
    pub target_bytes: Option<u64>,
    /// 预算搜索的真实结论：命中 true / 阶梯用尽 false / 未请求 None。
    /// 与 `success` 是两件事 —— "压出了有效文件"和"达到了你要的体积"不是同一个承诺。
    pub target_met: Option<bool>,
    /// §10 Encode 阶段只回答这一件事：交回的是候选、原图字节，还是什么都没交出来。
    pub encode: EncodeVerdict,
    /// 引擎这一轮失败或跳过的判据。由知道事实的那个 helper 赋值，调用方不再猜。
    pub reason: ReasonCode,
}

impl EngineResult {
    /// 追加一条告警。重复内容由调用方负责（同类告警只在一个地方产生）。
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }
}

/// Result returned to the frontend (no raw bytes).
///
/// 新增字段一律 `Option` + `#[serde(default)]`（AGENTS.md 规则 10）：已经落盘的
/// 历史记录里没有这些键，读取时必须仍然能反序列化。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompressResult {
    pub file: String,
    pub original_size: u64,
    pub compressed_size: u64,
    pub savings: f64,
    pub original_size_formatted: String,
    pub compressed_size_formatted: String,
    #[serde(rename = "type")]
    pub out_type: String,
    pub algorithm: String,
    pub error: Option<String>,
    pub output_path: Option<String>,
    pub backup_path: Option<String>,
    pub output_mode: Option<String>,
    /// 用户原图的像素尺寸；无法识别的容器（HEIC/AVIF 等）为 None。
    #[serde(default)]
    pub source_width: Option<u32>,
    #[serde(default)]
    pub source_height: Option<u32>,
    /// 输出字节流的像素尺寸。
    #[serde(default)]
    pub output_width: Option<u32>,
    #[serde(default)]
    pub output_height: Option<u32>,
    /// 这一次文件级编码墙上耗时（毫秒），由调用方测得后填入。
    #[serde(default)]
    pub elapsed_ms: Option<u64>,
    #[serde(default)]
    pub target_bytes: Option<u64>,
    #[serde(default)]
    pub target_met: Option<bool>,
    /// 空告警序列化为 None，前端可以据此区分"确实没有告警"和"这一版引擎没上报"。
    #[serde(default)]
    pub warnings: Option<Vec<String>>,
    /// §3 这次任务对用户的最终结论。§139 第四阶段后它是唯一结论，
    /// 不再有 `success` 旗标与它并存。
    #[serde(default)]
    pub outcome: Option<ResultOutcome>,
    /// §4 结论的理由。前端只读它，不再解析 `error` 文案（§59）。
    #[serde(default)]
    pub reason_code: Option<ReasonCode>,
    /// §73 §77 这次提交对应的历史记录。恢复全部 / 导出历史产物优先按它对齐，
    /// 不再靠源路径去猜"这条结果对应哪次提交"。
    #[serde(default)]
    pub history_id: Option<String>,
}

/// §6.1 §68 「这一项在磁盘上到底有没有产物」只由结论回答一次。
///
/// `output_path` 只是路径字符串：拿它判"有没有文件"，就会出现引擎压出了字节却没落盘、
/// 事务回滚把产物清掉但路径还在这一类假象。恢复、导出、对比都从这里取判据，
/// 前端 `result_state.js` 与 Swift `ResultContract` 是同一份规则。
impl CompressResult {
    pub fn has_product(&self) -> bool {
        match self.outcome {
            Some(outcome) => outcome.carries_output(),
            // §139 第四阶段：success 已删除，没有结论就不许拿路径猜一个产物。
            None => false,
        }
    }
}

// ─── Utilities ──────────────────────────────────────────────────

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{:.1}B", bytes as f64)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn get_file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// 只读文件头 4 KiB 判定格式。过去这里 `fs::read` 整张图，仅仅为了看前 12 字节：
/// 一张 200 MB 的 TIFF 会因此被完整读进内存，而结论和 12 字节时一模一样（§63 §64）。
pub fn detect_image_type(path: &Path) -> String {
    use std::io::Read;
    let mut head = [0u8; 4096];
    let read = match fs::File::open(path).and_then(|mut file| file.read(&mut head)) {
        Ok(read) => read,
        Err(_) => return "unknown".into(),
    };
    detect_image_format(&head[..read]).into()
}

/// 字节层面的格式判定。编码结果落盘前也要过这一道（§20）：扩展名和 `out_type`
/// 都是我们自己写的字符串，只有文件头才是字节自己说的话。
pub fn detect_image_format(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return "png";
    }
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return "jpg";
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return "gif";
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "webp";
    }
    if bytes.len() >= 2 && bytes[0] == b'B' && bytes[1] == b'M' {
        return "bmp";
    }
    if bytes.len() >= 4 && (&bytes[..4] == b"II*\0" || &bytes[..4] == b"MM\0*") {
        return "tiff";
    }
    if is_jxl_bytes(bytes) {
        return "jxl";
    }
    if let Some(family) = isobmff_family(bytes) {
        return family;
    }
    "unknown"
}

fn is_jxl_bytes(bytes: &[u8]) -> bool {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0x0A {
        return true;
    }
    const CONTAINER: [u8; 12] = [0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A];
    bytes.len() >= 12 && bytes[..12] == CONTAINER
}

/// AVIF 与 HEIC 都住在 `ftyp` 盒子里，只看主品牌会把带 avif 兼容品牌的 HEIF 判成 HEIC。
/// 先看主品牌，再扫全部兼容品牌；`mif1` 这种通用主品牌按 HEIC 处理（历史行为）。
fn isobmff_family(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    let box_size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = box_size.clamp(12, bytes.len());
    let mut brands: Vec<&[u8]> = vec![&bytes[8..12]];
    brands.extend(bytes[16..end].chunks_exact(4));
    let has = |ids: &[&[u8]]| brands.iter().any(|brand| ids.contains(brand));
    if has(&[b"avif", b"avis"]) {
        return Some("avif");
    }
    if has(&[b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1"]) {
        return Some("heic");
    }
    None
}

fn analyze_quality(path: &Path) -> u32 {
    let size = get_file_size(path);
    if size > 5 * 1024 * 1024 { 60 }
    else if size > 2 * 1024 * 1024 { 65 }
    else if size > 1024 * 1024 { 70 }
    else if size > 500 * 1024 { 75 }
    else { 80 }
}

/// 能力矩阵要如实报告「这台机器此刻有没有这个编码器」。
pub(crate) fn tool_available(name: &str) -> bool {
    find_tool(name).is_some()
}

/// Locate a CLI tool: first from bundled resources, then system PATH.
fn find_tool(name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(target_os = "windows") {
        format!("{}.exe", name)
    } else {
        name.to_string()
    };

    // 1. Tauri bundled: <res>/resources/bin/<tool>
    if let Some(res_dir) = RESOURCE_DIR.get() {
        let bundled = res_dir.join("resources").join("bin").join(&exe_name);
        if bundled.exists() {
            return Some(bundled);
        }
        // package.sh style: <res>/bin/<tool>
        let bundled = res_dir.join("bin").join(&exe_name);
        if bundled.exists() {
            return Some(bundled);
        }
    }
    // 2. Dev mode: src-tauri/resources/bin/<tool>
    let dev_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join("bin").join(&exe_name);
    if dev_path.exists() {
        return Some(dev_path);
    }
    // 3. System PATH
    if cfg!(target_os = "windows") {
        if let Ok(path_var) = std::env::var("PATH") {
            for dir in path_var.split(';') {
                let p = Path::new(dir).join(&exe_name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
        let mut where_cmd = std::process::Command::new("where");
        where_cmd.arg(&exe_name);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            where_cmd.creation_flags(0x08000000);
        }
        if let Ok(out) = where_cmd.output() {
            if out.status.success() {
                if let Some(line) = String::from_utf8_lossy(&out.stdout).lines().next() {
                    let s = line.trim();
                    if !s.is_empty() {
                        return Some(PathBuf::from(s));
                    }
                }
            }
        }
    } else {
        let extra = [
            "/opt/homebrew/opt/mozjpeg/bin",
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/opt/local/bin",
        ];
        for dir in extra {
            let p = Path::new(dir).join(name);
            if p.exists() {
                return Some(p);
            }
        }
        if let Ok(out) = std::process::Command::new("which").arg(name).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(PathBuf::from(s));
                }
            }
        }
    }
    None
}

/// 一次编码想要的结果规则。
///
/// §18.1 压缩：只有比源文件小才算赢；§18.2 转换：只要产出的是目标格式就提交。
/// 同一个差异必须写在**编码器内部**，不能交给上层猜 —— 编码器此前自己把大结果丢掉，
/// 上层再怎么判断都拿不到那份字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputRule {
    /// 只交回比源文件小的候选；否则交回原图字节（KeptOriginal）。
    BenefitOnly,
    /// 显式转换：交回目标格式的候选，不再问大小。
    Convert,
}

/// 读源文件失败时，绝不许退化成 `Vec::new()`（§58）。
///
/// 空字节会让 `original_size = 0`，于是任何输出都"不比原图小"、甚至"已达标"，
/// 把一次读盘失败伪装成一次成功的判断。这里返回的失败带精确 reasonCode（§59）。
fn read_source(file: &Path, out_type: &str) -> Result<Vec<u8>, EngineResult> {
    fs::read(file).map_err(|error| {
        let reason = match error.kind() {
            std::io::ErrorKind::NotFound => ReasonCode::SourceNotFound,
            std::io::ErrorKind::PermissionDenied => ReasonCode::PermissionDenied,
            _ => ReasonCode::SourceReadFailed,
        };
        let mut result = rejected(
            Vec::new(),
            out_type,
            "none",
            reason,
            format!("读不到源文件：{error}"),
        );
        result.encode = EncodeVerdict::Failed;
        result.success = false;
        result
    })
}

/// 显式转换拿不到目标格式时的失败。它不能落回 `KeptOriginal`：
/// "没换成"是一条失败，不是"没收益"，更不是"转换成功"。
fn conversion_failed(original: Vec<u8>, out_type: &str, detail: impl Into<String>) -> EngineResult {
    let mut result = rejected(
        original,
        out_type,
        "none",
        ReasonCode::EncodeFailed,
        detail,
    );
    result.encode = EncodeVerdict::Failed;
    result
}

fn make_engine_result(
    _original_size: u64,
    data: Vec<u8>,
    out_type: &str,
    algorithm: &str,
) -> EngineResult {
    let _compressed_size = data.len() as u64;
    EngineResult {
        success: true,
        compressed: data,
        out_type: out_type.into(),
        algorithm: algorithm.into(),
        error: None,
        encode: EncodeVerdict::Candidate,
        ..Default::default()
    }
}

/// 这一轮**没有交出行得过的候选**：格式不支持、输入认不出来。
/// 它和"编码时坏了"（`EncodeVerdict::Failed`，见 `system_image::fail`）是两件事：
/// 前者是"这件事我们不做"，后者是"做的时候坏了"。
fn rejected(original: Vec<u8>, out_type: &str, algorithm: &str, reason: ReasonCode, error: impl Into<String>) -> EngineResult {
    EngineResult {
        success: false,
        compressed: original,
        out_type: out_type.into(),
        algorithm: algorithm.into(),
        error: Some(error.into()),
        encode: EncodeVerdict::Rejected,
        reason,
        ..Default::default()
    }
}

/// 压缩后体积未减小：返回原始数据，标记为"无优化空间"（不算失败）
fn no_improvement(original: Vec<u8>, out_type: &str, algorithm: &str, orig_size: u64, comp_size: u64) -> EngineResult {
    let msg = if comp_size >= orig_size {
        format!("压缩后 {} > 原始 {}，原图已是最优压缩", format_bytes(comp_size), format_bytes(orig_size))
    } else {
        "压缩后体积未减小".into()
    };
    EngineResult {
        // 「没有优化空间」从来不是压缩成功：交回的是原图字节，判据必须说清是哪一种。
        // `success` 迁移期保持 true（旧调用方拿它当"没坏"读），最终结论只看 outcome。
        success: true,
        compressed: original,
        out_type: out_type.into(),
        algorithm: algorithm.into(),
        error: Some(msg),
        encode: EncodeVerdict::KeptOriginal,
        reason: if comp_size > orig_size {
            ReasonCode::OutputLargerThanSource
        } else {
            ReasonCode::NoSizeReduction
        },
        ..Default::default()
    }
}

/// 「保留原图」这一支交回的字节就是**源文件本身**，所以 out_type 必须写源格式。
/// 写目标格式会让落盘按 `.avif` 之类的扩展名存下 PNG 字节 —— 文件打不开，历史也救不回来。
fn kept_original(original: Vec<u8>, algorithm: &str, orig_size: u64, comp_size: u64) -> EngineResult {
    // 标签只能跟着**交回的字节**走。working file 可能是 PNG 中间文件，按路径猜格式
    // 会让一张缩放过的 WebP 被标成 PNG，落盘时扩展名跟着错。
    let source_type = detect_image_format(&original);
    no_improvement(original, source_type, algorithm, orig_size, comp_size)
}

/// 解码失败的明确结论：文件读得出来但任何解码器都解不开 = 数据损坏，
/// 不是"没有优化空间"。§8：把损坏吞成"已是最优"会让它在界面上永远显示
/// 为跳过而不是失败，用户也就永远不会知道这张图是坏的。
///
/// 只用于 image crate 能解的格式（png/jpg/gif/webp）；heic/avif/jxl 的解码
/// 在外部 CLI 里，CLI 失败与"压不小"无法在这里区分，维持原语义。
fn corrupted_source(original: &[u8], source_type: &str) -> Option<EngineResult> {
    if image::load_from_memory(original).is_err() {
        return Some(rejected(
            original.to_vec(),
            source_type,
            "none",
            ReasonCode::DecodeFailed,
            "无法解码：文件可能已损坏或不完整",
        ));
    }
    None
}

// ─── 动画保护（§23） ────────────────────────────────────────────

/// 源文件是否是动画，以及是哪一种。`None` = 可以按单帧处理。
fn animated_source(bytes: &[u8], source_type: &str) -> Option<&'static str> {
    match source_type {
        "gif" => {
            // 解析失败时按动画处理：宁可拒绝一次转换，也不静默丢掉后面的帧。
            if gif_frame_count(bytes).map(|count| count > 1).unwrap_or(true) {
                Some("GIF 动画")
            } else {
                None
            }
        }
        "png" => {
            if apng_is_animated(bytes) {
                Some("APNG 动画")
            } else {
                None
            }
        }
        "webp" => {
            if webp_is_animated(bytes) {
                Some("动画 WebP")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// 静态目标格式遇到动画输入时的拒收原话。调用方负责把原图字节原样交回。
fn animation_rejection(bytes: &[u8], target_label: &str) -> Option<String> {
    let source_type = detect_image_format(bytes);
    animated_source(bytes, source_type).map(|kind| {
        format!(
            "{kind} 不能输出为 {target_label}（只能保留第一帧）。已拒绝这次降级，原文件未改动"
        )
    })
}

fn apng_is_animated(bytes: &[u8]) -> bool {
    let Some(actl) = bytes.windows(4).position(|window| window == b"acTL") else {
        return false;
    };
    let idat = bytes
        .windows(4)
        .position(|window| window == b"IDAT")
        .unwrap_or(bytes.len());
    if actl >= idat || actl + 12 > bytes.len() {
        return false;
    }
    let frames = u32::from_be_bytes([
        bytes[actl + 4],
        bytes[actl + 5],
        bytes[actl + 6],
        bytes[actl + 7],
    ]);
    frames > 1
}

fn webp_is_animated(bytes: &[u8]) -> bool {
    if bytes.len() >= 21 && &bytes[12..16] == b"VP8X" && (bytes[20] & 0x02) != 0 {
        return true;
    }
    let keep = bytes.len().min(1024 * 1024);
    bytes[..keep]
        .windows(4)
        .any(|tag| tag == b"ANIM" || tag == b"ANMF")
}

/// 只解析块结构、不解码像素：GIF 的帧数在文件头里，没必要为它解一张大图。
fn gif_frame_count(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 13 || &bytes[..3] != b"GIF" {
        return None;
    }
    let mut pos = 13usize;
    let global_flags = bytes[10];
    if global_flags & 0x80 != 0 {
        pos = pos.checked_add(3 * (1usize << ((global_flags & 0x07) + 1)))?;
    }
    let skip_sub_blocks = |mut cursor: usize| -> Option<usize> {
        loop {
            let size = *bytes.get(cursor)? as usize;
            cursor = cursor.checked_add(1)?;
            if size == 0 {
                return Some(cursor);
            }
            cursor = cursor.checked_add(size)?;
        }
    };
    let mut frames = 0usize;
    while pos < bytes.len() {
        match bytes[pos] {
            0x3B => return Some(frames),
            0x21 => {
                // extension: block label + sub-blocks
                pos = skip_sub_blocks(pos.checked_add(2)?)?;
            }
            0x2C => {
                frames += 1;
                if pos + 10 > bytes.len() {
                    return None;
                }
                let local_flags = bytes[pos + 9];
                pos += 10;
                if local_flags & 0x80 != 0 {
                    pos = pos.checked_add(3 * (1usize << ((local_flags & 0x07) + 1)))?;
                }
                pos = pos.checked_add(1)?; // LZW minimum code size
                pos = skip_sub_blocks(pos)?;
            }
            _ => return None,
        }
    }
    None
}

/// Run a CLI tool that writes to an output file, then read the output.
async fn cli_to_file(
    tool: &Path,
    args: &[String],
    output_path: &Path,
) -> Option<Vec<u8>> {
    let result = make_command(tool)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !result.status.success() {
        return None;
    }
    fs::read(output_path).ok().filter(|d| !d.is_empty())
}

// ─── PNG ────────────────────────────────────────────────────────

async fn compress_png(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "png") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "PNG") {
        return rejected(original, "png", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;
    let acceptable = |size: usize| rule == OutputRule::Convert || (size as u64) < original_size;

    // 1. pngquant (lossy)
    if let Some(tool) = find_tool("pngquant") {
        let tmp = tempfile::tempdir().ok();
        if let Some(ref td) = tmp {
            let out = td.path().join("c.png");
            let q_low = quality.saturating_sub(10).max(10);
            let q_high = quality.min(100);
            let args = vec![
                format!("--quality={}-{}", q_low, q_high),
                "--speed=3".into(),
                "--strip".into(),
                "--output".into(),
                out.to_string_lossy().into(),
                "--".into(),
                file.to_string_lossy().into(),
            ];
            if let Some(data) = cli_to_file(&tool, &args, &out).await {
                if acceptable(data.len()) {
                    return make_engine_result(original_size, data, "png", "pngquant");
                }
            }
        }
    }

    // 2. oxipng (lossless)
    if let Some(tool) = find_tool("oxipng") {
        let tmp = tempfile::tempdir().ok();
        if let Some(ref td) = tmp {
            let out = td.path().join("c.png");
            let _ = fs::copy(file, &out);
            let level = (quality / 20).min(6).max(1);
            let args = vec![
                format!("-o{}", level),
                "--strip".into(),
                "safe".into(),
                out.to_string_lossy().into(),
            ];
            let _ = make_command(&tool).args(&args).stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()).status().await;
            if let Ok(data) = fs::read(&out) {
                if !data.is_empty() && acceptable(data.len()) {
                    return make_engine_result(original_size, data, "png", "oxipng");
                }
            }
        }
    }

    // 3. image crate fallback (lossless re-encode, best compression)
    if let Some(data) = compress_png_with_image(file) {
        if acceptable(data.len()) {
            return make_engine_result(original_size, data, "png", "image-png");
        }
    }

    match rule {
        OutputRule::BenefitOnly => {
            if let Some(failure) = corrupted_source(&original, "png") { return failure; }
            kept_original(original, "pngquant", original_size, original_size)
        },
        OutputRule::Convert => {
            conversion_failed(original, "png", "没能产出 PNG：所有编码器都没有交回有效字节")
        }
    }
}

fn compress_png_with_image(file: &Path) -> Option<Vec<u8>> {
    let img = image::open(file).ok()?;
    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut buf,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    );
    use image::ImageEncoder;
    let rgba = img.to_rgba8();
    encoder
        .write_image(&rgba, img.width(), img.height(), image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(buf)
}

// ─── JPEG ───────────────────────────────────────────────────────

async fn compress_jpg(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "jpg") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "JPEG") {
        return rejected(original, "jpg", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;
    let acceptable = |size: usize| rule == OutputRule::Convert || (size as u64) < original_size;

    // 1. cjpeg (mozjpeg) via PPM pipe: decode with image crate, encode with mozjpeg
    if let Some(tool) = find_tool("cjpeg") {
        // Decode image and convert to PPM for cjpeg input
        if let Ok(img) = image::open(file) {
            let rgb = img.to_rgb8();
            let (w, h) = (rgb.width(), rgb.height());
            // Build PPM (P6) header + raw RGB data
            let mut ppm = Vec::with_capacity(15 + (w * h * 3) as usize);
            ppm.extend_from_slice(format!("P6\n{} {}\n255\n", w, h).as_bytes());
            ppm.extend_from_slice(&rgb);
            // Pipe PPM to cjpeg stdin
            if let Ok(mut child) = make_command(&tool)
                .args([
                    "-quality", &quality.to_string(),
                    "-optimize", "-progressive",
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                use tokio::io::AsyncWriteExt;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(&ppm).await;
                }
                if let Ok(output) = child.wait_with_output().await {
                    if !output.stdout.is_empty() && acceptable(output.stdout.len()) {
                        return make_engine_result(original_size, output.stdout, "jpg", "mozjpeg");
                    }
                }
            }
        }
    }

    // 2. image crate fallback (decode + re-encode)
    if let Some(data) = compress_jpg_with_image(file, quality as u8) {
        if acceptable(data.len()) {
            return make_engine_result(original_size, data, "jpg", "image-jpeg");
        }
    }

    match rule {
        OutputRule::BenefitOnly => {
            if let Some(failure) = corrupted_source(&original, "jpg") { return failure; }
            kept_original(original, "mozjpeg", original_size, original_size)
        },
        OutputRule::Convert => {
            conversion_failed(original, "jpg", "没能产出 JPEG：所有编码器都没有交回有效字节")
        }
    }
}

fn compress_jpg_with_image(file: &Path, quality: u8) -> Option<Vec<u8>> {
    let img = image::open(file).ok()?;
    let rgb = img.to_rgb8();
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    use image::ImageEncoder;
    encoder
        .write_image(&rgb, img.width(), img.height(), image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(buf)
}

// ─── GIF ────────────────────────────────────────────────────────

async fn compress_gif(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "gif") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    let quality = options.quality;
    let acceptable = |size: usize| rule == OutputRule::Convert || (size as u64) < original_size;

    if let Some(tool) = find_tool("gifsicle") {
        let tmp = tempfile::tempdir().ok();
        if let Some(ref td) = tmp {
            let out = td.path().join("c.gif");
            let colors = ((quality as f64 / 100.0) * 256.0).floor().max(32.0) as u32;
            let args = vec![
                format!("--optimize=3"),
                format!("--colors={}", colors),
                "--no-comments".into(),
                "--output".into(),
                out.to_string_lossy().into(),
                file.to_string_lossy().into(),
            ];
            if let Some(data) = cli_to_file(&tool, &args, &out).await {
                if acceptable(data.len()) {
                    return make_engine_result(original_size, data, "gif", "gifsicle");
                }
            }
        }
    }

    // 没有 gifsicle 时进程内重编码只会取第一帧；这里宁可保留原图，
    // 也不产出一个"看起来成功、其实动画没了"的 GIF（§23）。
    match rule {
        OutputRule::BenefitOnly => {
            if let Some(failure) = corrupted_source(&original, "gif") { return failure; }
            kept_original(original, "gifsicle", original_size, original_size)
        },
        OutputRule::Convert => {
            conversion_failed(original, "gif", "没有可用的 GIF 编码器，无法在保留动画的前提下转换")
        }
    }
}

// ─── WebP ───────────────────────────────────────────────────────

async fn compress_to_webp(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "webp") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "WebP") {
        return rejected(original, "webp", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;
    let acceptable = |size: usize| rule == OutputRule::Convert || (size as u64) < original_size;

    if let Some(tool) = find_tool("cwebp") {
        let tmp = tempfile::tempdir().ok();
        if let Some(ref td) = tmp {
            let out = td.path().join("c.webp");
            let args = vec![
                format!("-q"), quality.to_string(),
                "-m".into(), "6".into(),
                "-pass".into(), "10".into(),
                "-o".into(),
                out.to_string_lossy().into(),
                file.to_string_lossy().into(),
            ];
            if let Some(data) = cli_to_file(&tool, &args, &out).await {
                if acceptable(data.len()) {
                    return make_engine_result(original_size, data, "webp", "cwebp");
                }
            }
        }
    }

    match rule {
        OutputRule::BenefitOnly => {
            if let Some(failure) = corrupted_source(&original, "webp") { return failure; }
            kept_original(original, "cwebp", original_size, original_size)
        },
        OutputRule::Convert => {
            conversion_failed(original, "webp", "没能产出 WebP：cwebp 没有交回有效字节")
        }
    }
}

// ─── AVIF ───────────────────────────────────────────────────────

async fn compress_to_avif(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "avif") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "AVIF") {
        return rejected(original, "avif", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;
    let acceptable = |size: usize| rule == OutputRule::Convert || (size as u64) < original_size;

    if let Some(tool) = find_tool("avifenc") {
        let tmp = tempfile::tempdir().ok();
        if let Some(ref td) = tmp {
            let out = td.path().join("c.avif");
            let args = vec![
                "--speed".into(), "6".into(),
                "--min".into(), "0".into(),
                "--max".into(), quality.to_string(),
                "-o".into(),
                out.to_string_lossy().into(),
                file.to_string_lossy().into(),
            ];
            if let Some(data) = cli_to_file(&tool, &args, &out).await {
                if acceptable(data.len()) {
                    return make_engine_result(original_size, data, "avif", "avifenc");
                }
            }
        }
    }

    match rule {
        OutputRule::BenefitOnly => kept_original(original, "avifenc", original_size, original_size),
        OutputRule::Convert => {
            conversion_failed(original, "avif", "没能产出 AVIF：avifenc 没有交回有效字节")
        }
    }
}

// ─── HEIC ───────────────────────────────────────────────────────

/// 用 macOS 自带的 sips 直接写 HEIC。
///
/// 旧实现先把 HEIF 转成 JPEG 再"当作压缩结果"，于是用户请求 HEIF 却拿到一张 JPEG：
/// 扩展名写着 .heic、字节是 JFIF —— 两边都打不开（§19）。sips 能写 `public.heic`，
/// 这里只接受**字节真的是 HEIC** 的结果，JPEG 冒充一律不认。
async fn sips_heic_bytes(file: &Path, quality: u32) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        let tmp = tempfile::tempdir().ok()?;
        let out = tmp.path().join("out.heic");
        let quality = quality.to_string();
        let output = tokio::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "heic", "-s", "formatOptions", &quality])
            .arg(file)
            .arg("--out")
            .arg(&out)
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let data = fs::read(&out).ok().filter(|bytes| !bytes.is_empty())?;
        (detect_image_format(&data) == "heic").then_some(data)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (file, quality);
        None
    }
}

async fn compress_heic(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "heic") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "HEIC") {
        return rejected(original, "heic", "none", ReasonCode::UnsupportedFormat, message);
    }
    // quality 0 是"由引擎推测"的旧约定；HEIC 编码器没有推测档，给一个稳妥的 75。
    let quality = if options.quality == 0 { 75 } else { options.quality.min(100) };
    let acceptable = |size: usize| rule == OutputRule::Convert || (size as u64) < original_size;

    if let Some(data) = sips_heic_bytes(file, quality).await {
        if acceptable(data.len()) {
            return make_engine_result(original_size, data, "heic", "sips-heic");
        }
    }

    match rule {
        OutputRule::BenefitOnly => kept_original(original, "sips-heic", original_size, original_size),
        OutputRule::Convert => {
            conversion_failed(original, "heic", "没能产出 HEIC：sips 没有交回有效的 HEIC 字节")
        }
    }
}

// ─── JPEG XL ────────────────────────────────────────────────────

async fn compress_to_jxl(file: &Path, options: &CompressOptions, rule: OutputRule) -> EngineResult {
    let original = match read_source(file, "jxl") {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;
    if let Some(message) = animation_rejection(&original, "JPEG XL") {
        return rejected(original, "jxl", "none", ReasonCode::UnsupportedFormat, message);
    }
    let quality = options.quality;
    let acceptable = |size: usize| rule == OutputRule::Convert || (size as u64) < original_size;

    if let Some(tool) = find_tool("cjxl") {
        let tmp = tempfile::tempdir().ok();
        if let Some(ref td) = tmp {
            let out = td.path().join("c.jxl");
            let distance = ((100 - quality) as f64 / 7.0).max(0.1).min(15.0);
            let args = vec![
                "--lossless_jpeg=0".into(),
                "--distance".into(),
                format!("{:.2}", distance),
                "--effort".into(), "7".into(),
                file.to_string_lossy().into(),
                out.to_string_lossy().into(),
            ];
            if let Some(data) = cli_to_file(&tool, &args, &out).await {
                if acceptable(data.len()) {
                    return make_engine_result(original_size, data, "jxl", "cjxl");
                }
            }
        }
    }

    match rule {
        OutputRule::BenefitOnly => kept_original(original, "cjxl", original_size, original_size),
        OutputRule::Convert => {
            conversion_failed(original, "jxl", "没能产出 JPEG XL：cjxl 没有交回有效字节")
        }
    }
}

// ─── Dimension cap (maxDimensionPx) ─────────────────────────────

/// 预算搜索最多几次编码（AGENTS.md 的 CPU 契约：一个文件任务只占 1 份预算，
/// 所以搜索必须是有界的，不能变成"压到满意为止"的无底洞）。
pub(crate) const BUDGET_ATTEMPT_LIMIT: usize = 6;

/// 为兑现尺寸上限而写出的 JPEG 工作文件的质量。
///
/// 这是**中间文件**的质量，不是输出质量：往下游编码器还会再按用户/阶梯的 quality 编一次。
/// 95 是"人眼看不出多编了一道"与"中间文件不至于膨胀到 10MB"之间的取值。
const WORKING_JPEG_QUALITY: u8 = 95;

/// 重编码像素必然丢掉 EXIF/GPS —— 必须说出去，不能悄悄丢（规则 11）。
const METADATA_WARNING: &str =
    "本次重新编码了像素，EXIF/GPS 等元数据不会写入输出（需要保留拍摄信息与方位请用「系统转换」）";

/// 最长边上限的纯计算部分：**只缩不放大**，取整后至少 1px。
/// 无需缩放时返回 None —— 调用方据此避免"为确认而白白重编码一次"。
pub(crate) fn capped_dims(width: u32, height: u32, cap: u32) -> Option<(u32, u32)> {
    if cap == 0 {
        return None;
    }
    let longest = width.max(height);
    if longest <= cap {
        return None; // 含"cap 比原图还大"：那是放大请求，本字段语义上明确拒绝。
    }
    let scale = cap as f64 / longest as f64;
    let next = |edge: u32| ((edge as f64 * scale).round() as u32).clamp(1, edge);
    Some((next(width), next(height)))
}

/// 缩放后的工作文件用什么格式落盘。
///
/// 下游编码器都只认"一个路径"，所以尺寸上限是靠**先写一个工作文件**兑现的：
/// 输出是 JPEG 时中间文件也必须是 JPEG（否则把照片存成几十 MB 的 PNG，白花一次时间），
/// 其余情况一律 PNG —— 它无损、保留 alpha，而且所有下游解码器都读得动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkingFormat {
    Png,
    Jpeg,
}

pub(crate) fn working_format_for(source_type: &str, requested: &str) -> WorkingFormat {
    let target = if requested == "original" || requested.is_empty() { source_type } else { requested };
    match target {
        "jpg" | "jpeg" => WorkingFormat::Jpeg,
        _ => WorkingFormat::Png,
    }
}

/// 下游编码器实际读取的文件 + 它的临时目录守卫 + 必须转告用户的说明。
///
/// `path` 要么就是用户原图（未缩放），要么是本模块自己新建目录里的缩放结果。
/// `_dir` 是守卫：函数从任何路径退出（含提前 return、错误与 panic）它都会被 drop，
/// 临时目录随之删除，原图永远不动、也从不在原图目录里落笔。
pub(crate) struct WorkingFile {
    path: PathBuf,
    _dir: Option<tempfile::TempDir>,
    resized: bool,
    warnings: Vec<String>,
}

impl WorkingFile {
    fn unchanged(file: &Path) -> Self {
        Self { path: file.to_path_buf(), _dir: None, resized: false, warnings: Vec::new() }
    }

    /// 上限没能生效时的出口：继续按原尺寸压缩，但把"没做到"写成告警。
    fn unchanged_with_warning(file: &Path, warning: String) -> Self {
        Self { path: file.to_path_buf(), _dir: None, resized: false, warnings: vec![warning] }
    }
}

/// 会被"解码成单帧"吃掉内容的源格式。规则 11 要求先说明再动手，不能悄悄丢帧。
fn multi_frame_source(file: &Path, img_type: &str) -> Option<String> {
    let reason = |what: &str| {
        Some(format!("尺寸上限暂不作用于{what}：进程内解码只会取第一帧，其余帧会全部丢失；本次按原始尺寸处理"))
    };
    match img_type {
        "gif" => reason("动图 GIF"),
        "png" => {
            let is_apng = image::codecs::png::PngDecoder::new(std::io::BufReader::new(
                fs::File::open(file).ok()?,
            ))
            .ok()
            .and_then(|decoder| decoder.is_apng().ok())
            .unwrap_or(false);
            if is_apng { reason("APNG 动画 PNG") } else { None }
        }
        "webp" => {
            // 动画 WebP 的 ANIM/ANMF chunk 紧跟在 VP8X 之后，扫文件头 1MB 足够判定。
            let head = fs::read(file).ok().map(|bytes| {
                let keep = bytes.len().min(1024 * 1024);
                bytes[..keep].to_vec()
            })?;
            if head.windows(4).any(|tag| tag == b"ANIM" || tag == b"ANMF") { reason("动画 WebP") } else { None }
        }
        _ => None,
    }
}

/// 把 `maxDimensionPx` 落地成"一个可以直接交给下游编码器的工作文件"。
///
/// 两条产物线共用这一份实现（Direct 的 CLI 编码器与 App Store 的进程内编码器都只是
/// `&Path` 消费者），所以不存在"某条线忘了缩放"的分裂实现。任何一步失败都退回原文件
/// 并留下告警 —— 绝不在没缩放的情况下假装上限已生效。
pub(crate) fn prepare_working_file(file: &Path, options: &CompressOptions) -> WorkingFile {
    let Some(cap) = options.max_dimension_px.filter(|edge| *edge > 0) else {
        return WorkingFile::unchanged(file);
    };
    let img_type = detect_image_type(file);
    if let Some(reason) = multi_frame_source(file, &img_type) {
        return WorkingFile::unchanged_with_warning(file, reason);
    }
    let Ok((width, height)) = image::image_dimensions(file) else {
        return WorkingFile::unchanged_with_warning(
            file,
            format!("{}px 尺寸上限未能应用：读不到这张图的尺寸（{}），本次按原始尺寸处理", cap, img_type),
        );
    };
    let Some((target_w, target_h)) = capped_dims(width, height, cap) else {
        return WorkingFile::unchanged(file);
    };
    let decoded = match image::open(file) {
        Ok(image) => image,
        Err(error) => {
            return WorkingFile::unchanged_with_warning(
                file,
                format!("{}px 尺寸上限未能应用：进程内解码失败（{error}），本次按原始尺寸处理", cap),
            );
        }
    };

    let mut warnings: Vec<String> = Vec::new();
    if matches!(
        decoded.color(),
        image::ColorType::L16
            | image::ColorType::La16
            | image::ColorType::Rgb16
            | image::ColorType::Rgba16
            | image::ColorType::Rgb32F
            | image::ColorType::Rgba32F
    ) {
        warnings.push(format!(
            "源图是高位深/浮点色域，为满足 {}px 尺寸上限按 8-bit 重新采样（引擎当前不支持高位深缩放）",
            cap
        ));
    }

    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            return WorkingFile::unchanged_with_warning(
                file,
                format!("{}px 尺寸上限未能应用：临时目录创建失败（{error}）", cap),
            );
        }
    };
    let filter = image::imageops::FilterType::Lanczos3;
    let outcome: Result<PathBuf, String> = match working_format_for(&img_type, &options.output_format) {
        WorkingFormat::Png => {
            let small = image::imageops::resize(&decoded.to_rgba8(), target_w, target_h, filter);
            let path = dir.path().join("source.png");
            small.save(&path).map(|_| path).map_err(|error| error.to_string())
        }
        WorkingFormat::Jpeg => {
            let rgba = decoded.to_rgba8();
            let base = if rgba.pixels().any(|p| p.0[3] != 255) {
                warnings.push(
                    "输出是 JPEG：透明通道已按白底合成后丢弃（JPEG 不支持 alpha，不做假透明）".to_string(),
                );
                flatten_onto_white(&rgba)
            } else {
                without_alpha(&rgba)
            };
            let small = image::imageops::resize(&base, target_w, target_h, filter);
            let mut buf = Vec::new();
            use image::ImageEncoder;
            let encoded = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, WORKING_JPEG_QUALITY)
                .write_image(small.as_raw(), small.width(), small.height(), image::ExtendedColorType::Rgb8)
                .map_err(|error| error.to_string());
            match encoded {
                Ok(()) => {
                    let path = dir.path().join("source.jpg");
                    fs::write(&path, &buf).map(|_| path).map_err(|error| error.to_string())
                }
                Err(error) => Err(error),
            }
        }
    };
    match outcome {
        Ok(path) => {
            warnings.push(format!(
                "尺寸上限 {}px 生效：{}×{} → {}×{}（Lanczos3，只缩不放大）",
                cap, width, height, target_w, target_h
            ));
            WorkingFile { path, _dir: Some(dir), resized: true, warnings }
        }
        Err(error) => {
            // dir 在这里 drop：失败的中间文件不会留在磁盘上，也绝不会写到原图目录。
            drop(dir);
            WorkingFile::unchanged_with_warning(
                file,
                format!("{}px 尺寸上限未能应用：缩放/写临时文件失败（{error}），本次按原始尺寸处理", cap),
            )
        }
    }
}

/// 丢掉一个"其实完全不透明"的 alpha 通道，不改变任何可见像素。
fn without_alpha(rgba: &image::RgbaImage) -> image::RgbImage {
    image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        image::Rgb([p[0], p[1], p[2]])
    })
}

/// alpha → 白底合成，与浏览器/Finder 的默认行为一致。
fn flatten_onto_white(rgba: &image::RgbaImage) -> image::RgbImage {
    image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = p[3] as f32 / 255.0;
        let mix = |c: u8| (c as f32 * a + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    })
}

/// 同一格式的不同写法（jpeg/jpg、heif/heic）不能被判成"格式转换"。
fn canonical_format(s: &str) -> &str {
    match s {
        "jpeg" => "jpg",
        "heif" => "heic",
        other => other,
    }
}

pub(crate) fn same_kind(a: &str, b: &str) -> bool {
    canonical_format(a) == canonical_format(b)
}

/// 用户明确要求的容器（`original` 解析成本次源文件的格式）。
pub(crate) fn requested_format<'a>(source_type: &'a str, options: &'a CompressOptions) -> &'a str {
    if options.output_format == "original" || options.output_format.is_empty() {
        source_type
    } else {
        canonical_format(&options.output_format)
    }
}

/// 用户是否明确要求换一个容器 —— 只有非转换才适用"不比原图小就不是赢"（规则 7）。
pub(crate) fn is_format_conversion(file: &Path, options: &CompressOptions) -> bool {
    options.output_format != "original" && !same_kind(&options.output_format, &detect_image_type(file))
}

/// 把缩放过的中间结果收成对用户有意义的结论。
///
/// 关键约束：**"有没有变小"永远拿用户原图比**，不是拿我的工作文件比 ——
/// 否则一张缩放后的 PNG 会让下游"比输入小"的判据变得毫无意义。
fn settle_working_file(
    original_file: &Path,
    options: &CompressOptions,
    working: WorkingFile,
    mut result: EngineResult,
) -> EngineResult {
    for warning in working.warnings {
        result.warnings.push(warning);
    }
    if working.resized {
        let encoded_size = result.compressed.len() as u64;
        let conversion = is_format_conversion(original_file, options);
        // §140：编码器明确失败或不接这个格式时，失败必须原样上报。把它改写成
        // "已保留原图"就是把一次真实的坏掉上报成一次成功的处理。
        if matches!(
            result.encode,
            EncodeVerdict::Rejected | EncodeVerdict::Failed | EncodeVerdict::Cancelled
        ) {
            return result;
        }
        // 判据读的是引擎交回的字节属于哪一种事实，不是读 error 文案有没有值 ——
        // 后者会让"一句提示"和"一次失败"在同一个 if 里长得一模一样。
        let not_a_win = result.encode == EncodeVerdict::KeptOriginal
            || (!conversion && encoded_size >= get_file_size(original_file));
        if not_a_win {
            // 编码器没产出有意义的结果时，交回的是**用户原图**，不是我那个缩放中间文件。
            // §58：读不到原图就是读不到，绝不能拿"0 字节的原图"往下判"没收益"。
            let original = match fs::read(original_file) {
                Ok(bytes) => bytes,
                Err(error) => {
                    result.encode = EncodeVerdict::Failed;
                    result.reason = ReasonCode::SourceReadFailed;
                    result.success = false;
                    result.compressed = Vec::new();
                    result.error = Some(format!("保留原图时读不到源文件：{error}"));
                    return result;
                }
            };
            let original_size = original.len() as u64;
            let cap = options.max_dimension_px.unwrap_or_default();
            let algorithm = std::mem::take(&mut result.algorithm);
            let mut warnings = std::mem::take(&mut result.warnings);
            warnings.push(format!(
                "{}px 尺寸上限没有换来比原图更小的有效文件：已保留原图，输出仍是原始尺寸", cap
            ));
            // 这里交回的是**用户原图**，所以 out_type 说源格式：目标格式写上去，
            // 落盘就会把 PNG 字节存成 .avif 之类的名字（文件打不开，历史也救不回来）。
            result = no_improvement(original, &detect_image_type(original_file), &algorithm, original_size, encoded_size);
            result.warnings = warnings;
        }
    }
    // 规则 11：重编码像素的通路必须说明元数据去向。系统转换是唯一保留元数据的路径。
    if options.preserve_metadata
        && options.processing_mode != "system"
        && result.encode == EncodeVerdict::Candidate
    {
        result.warnings.push(METADATA_WARNING.to_string());
    }
    result
}

// ─── Byte budget solver (maxOutputBytes) ────────────────────────

/// 阶梯：由轻到重，最多 `BUDGET_ATTEMPT_LIMIT` 次编码。
///
/// 顺序是"先只在质量上做文章，动像素尺寸是后话"，因为重采样比换质量档更难挽回。
/// 用户自己要求的尺寸上限会和每一档取 min —— 搜索永远不能把图放大到用户之上。
pub(crate) fn budget_ladder(user_cap: Option<u32>) -> Vec<(Option<u32>, u32)> {
    const RAW: [(Option<u32>, u32); 6] = [
        (None, 80),
        (None, 65),
        (None, 50),
        (Some(2560), 65),
        (Some(1920), 60),
        (Some(1280), 55),
    ];
    let mut rungs: Vec<(Option<u32>, u32)> = Vec::with_capacity(RAW.len());
    for (cap, quality) in RAW {
        let merged = (merge_cap(user_cap, cap), quality);
        // §60：用户上限把不同档位压成同一组参数后，只编码一次。
        // 去重按**完整参数对**——同一尺寸下 65/60/55 仍是三次不同的尝试。
        if !rungs.contains(&merged) {
            rungs.push(merged);
        }
    }
    rungs
}

fn merge_cap(user: Option<u32>, rung: Option<u32>) -> Option<u32> {
    match (user, rung) {
        (Some(u), Some(r)) => Some(u.min(r)),
        (Some(u), None) => Some(u),
        (None, r) => r,
    }
}

fn cancelled_result(out_type: &str) -> EngineResult {
    EngineResult {
        success: false,
        compressed: Vec::new(),
        out_type: canonical_format(out_type).into(),
        algorithm: "指定体积".into(),
        error: Some("指定体积搜索在两次编码之间被取消，没有交回任何候选".into()),
        encode: EncodeVerdict::Cancelled,
        reason: ReasonCode::Cancelled,
        ..Default::default()
    }
}

/// 有界的指定体积搜索。
///
/// 三条硬规则：**压缩时原图已在预算内就不重编码**；**命中即停**；
/// **没命中时交回全程最小的有效候选 + targetMet=false + 把实际达到的体积和预算都说出去**。
///
/// 显式转换（`OutputRule::Convert`）不适用规则 7：用户要的是容器，即使更大也提交；
/// 但预算仍然是一份真承诺，没达到就如实标 `target_met=false`。
///
/// §61 §62：失败不再伪装成"没收益"；取消不再是"最接近的结果"——
/// 两次编码之间一看到取消就丢下所有候选返回 `Cancelled`，一个字节都不许落盘。
async fn solve_to_budget(
    file: &Path,
    options: &CompressOptions,
    budget: u64,
    rule: OutputRule,
    source_type: &str,
    cancel: &(dyn Fn() -> bool + Send + Sync),
) -> EngineResult {
    let original = match read_source(file, source_type) {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    let original_size = original.len() as u64;

    if rule == OutputRule::BenefitOnly && original_size <= budget {
        let mut result = no_improvement(original, source_type, "指定体积", original_size, original_size);
        // 这一支压根没进编码：原图已经在用户要的体积之内。它不是"压完没收益"。
        result.reason = ReasonCode::AlreadyUnderTarget;
        result.target_bytes = Some(budget);
        result.target_met = Some(true);
        result.warnings.push(format!(
            "原图 {} 已在目标 {} 以内，未做无谓的重编码", format_bytes(original_size), format_bytes(budget)
        ));
        return result;
    }

    let rungs = budget_ladder(options.max_dimension_px);
    let mut attempts = 0usize;
    let mut best: Option<EngineResult> = None;
    // 引擎明确失败/拒收时留住最后一次事实：全部失败不能降级成"没有收益"（§61）。
    let mut last_failure: Option<EngineResult> = None;
    let mut produced_unusable = false;

    for (cap, quality) in rungs.into_iter().take(BUDGET_ATTEMPT_LIMIT) {
        // 取消/停止落在两次编码之间：已经开跑的那一次让它做完（中途 kill 会留下
        // 半成品临时文件和悬空事务），但绝不再启动新的 —— 搜索因此仍可中断。
        if cancel() {
            return cancelled_result(source_type);
        }
        let mut rung = options.clone();
        rung.max_output_bytes = None; // 递归闸门：搜索内部只许走一次编码
        rung.quality = quality;
        rung.max_dimension_px = cap;
        attempts += 1;

        let candidate = compress_core(file, &rung, rule, source_type).await;
        // 候选之间也要问一次：编码可能跑了几十秒，期间用户完全可能已经取消。
        if cancel() {
            return cancelled_result(source_type);
        }
        match candidate.encode {
            EncodeVerdict::Candidate if !candidate.compressed.is_empty() => {}
            EncodeVerdict::Rejected | EncodeVerdict::Failed => {
                last_failure = Some(candidate);
                continue;
            }
            _ => {
                produced_unusable = true;
                continue;
            }
        }
        let size = candidate.compressed.len() as u64;
        if rule == OutputRule::BenefitOnly && size >= original_size {
            // 规则 7：比原图大的输出不是赢。但"产出过、只是不划算"和"根本没产出"不同。
            produced_unusable = true;
            continue;
        }
        let is_improvement = best
            .as_ref()
            .map(|current| size < current.compressed.len() as u64)
            .unwrap_or(true);
        if is_improvement {
            best = Some(candidate);
        }
        if size <= budget {
            break; // 命中即停，不做无谓的第二次编码
        }
    }

    let mut result = match best {
        Some(candidate) => candidate,
        None => match last_failure {
            // 没有任何一档交回字节：把引擎的真实失败原样上报，不写成"已保留原图"。
            Some(mut failure) => {
                failure.target_bytes = Some(budget);
                failure.target_met = Some(false);
                failure.warnings.push(format!(
                    "指定体积搜索的 {attempts} 次尝试全部失败，没有可以交回的文件"
                ));
                failure
            }
            None => {
                let mut kept = no_improvement(original, source_type, "指定体积", original_size, original_size);
                kept.warnings.push(format!(
                    "指定体积 {want} 未达到：{attempts} 次尝试里没有任何一档产出比原图 {original} 更小、且可用的文件，已保留原图",
                    want = format_bytes(budget), original = format_bytes(original_size)
                ));
                if produced_unusable {
                    kept.warnings.push(
                        "这些尝试确实产出过文件，但都不比原图小（或没有交回有效字节），因此没有提交".into(),
                    );
                }
                kept
            }
        },
    };
    let achieved = result.compressed.len() as u64;
    result.target_bytes = Some(budget);
    result.target_met = Some(achieved <= budget);
    if achieved > budget {
        result.warnings.push(format!(
            "指定体积 {want} 未达到：最激进的一档也只压到 {got}（已给出这次真正产出的最小文件，不能当作达成目标）",
            want = format_bytes(budget), got = format_bytes(achieved)
        ));
    }
    result
}

// ─── Dispatcher ─────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn convert_with_system(file: &Path, options: &CompressOptions, target: &str) -> EngineResult {
    let mut system_options = options.clone();
    system_options.output_format = target.into();
    crate::system_image::convert(file, &system_options)
}

#[cfg(not(target_os = "macos"))]
fn convert_with_system(file: &Path, _options: &CompressOptions, target: &str) -> EngineResult {
    // 读源失败要报 source_read_failed，不能把空字节当成"格式不支持"的证据。
    let original = match read_source(file, target) {
        Ok(bytes) => bytes,
        Err(failure) => return failure,
    };
    rejected(original, target, "system", ReasonCode::UnsupportedFormat, "系统转换仅支持 macOS")
}

/// 系统转换（macOS ImageIO）路径。
///
/// 它自带尺寸档（1280/640/320），`maxDimensionPx` 通过把 ImageIO 的最长边再夹一次来兑现
/// （见 `system_image::convert`），两者取更严的那个，不会出现"上限被静默忽略"。
/// `maxOutputBytes` 这条路径**做不到**：ImageIO 没有质量档可搜。所以这里只把事实说明白，
/// 不假装跑过搜索、也不把结论报成达成。
fn system_mode_result(file: &Path, options: &CompressOptions) -> EngineResult {
    let mut result = convert_with_system(file, options, &options.output_format);
    if let Some(budget) = options.max_output_bytes.filter(|value| *value > 0) {
        result.target_bytes = Some(budget);
        let achieved = result.compressed.len() as u64;
        result.target_met = Some(achieved <= budget);
        if result.success {
            result.warnings.push(format!(
                "系统转换不做指定体积搜索（ImageIO 这条路径没有质量档）：实际 {got}，目标 {want}",
                got = format_bytes(achieved), want = format_bytes(budget)
            ));
        }
    }
    result
}

/// 预算只在"高级/进程内编码"这条线上成立；系统模式另有上面的说明。
fn budget_request(options: &CompressOptions) -> Option<u64> {
    if options.processing_mode == "system" {
        None
    } else {
        options.max_output_bytes.filter(|value| *value > 0)
    }
}

/// 一次编码的分发：本产物线的编码器（inproc 提前返回，其余按格式/类型路由）。
///
/// 双线骨架沿用 engine 的既有约定：inproc 模式提前 return，cli 模式走下面的默认实现
/// （见 AGENTS.md 强制规则 1-3）。`allow` 挂在 fn 上而不是语句上，cfg 消除 return 时
/// 属性不会跟着消失。
#[allow(unreachable_code)]
async fn encode_source(
    file: &Path,
    options: &CompressOptions,
    rule: OutputRule,
    source_type: &str,
) -> EngineResult {
    // App Store 产物线：路由到进程内实现（尺寸上限已经在 prepare_working_file 里兑现，
    // 传进来的就是那个已经处理好的工作文件）。
    #[cfg(feature = "inproc-backends")]
    return engine_inproc::compress_image(file, options, rule, source_type).await;

    if options.output_format != "original" {
        return route_to_format(file, &options.output_format, options, rule).await;
    }
    match source_type {
        "png" => compress_png(file, options, rule).await,
        "jpg" => compress_jpg(file, options, rule).await,
        "gif" => compress_gif(file, options, rule).await,
        "webp" => compress_to_webp(file, options, rule).await,
        "heic" => compress_heic(file, options, rule).await,
        _ => {
            // 读源文件失败必须说"读不到"，不能把它伪装成"不支持这个格式"（§58 §59）。
            let original = match read_source(file, source_type) {
                Ok(bytes) => bytes,
                Err(failure) => return failure,
            };
            rejected(
                original,
                source_type,
                "none",
                ReasonCode::UnsupportedFormat,
                format!("这个格式暂不支持原地压缩：{source_type}"),
            )
        }
    }
}

/// 显式目标格式的路由（原先写在 `compress_to_format` 里的那个 match）。
async fn route_to_format(
    file: &Path,
    target: &str,
    options: &CompressOptions,
    rule: OutputRule,
) -> EngineResult {
    match target {
        "webp" => compress_to_webp(file, options, rule).await,
        "avif" => compress_to_avif(file, options, rule).await,
        "jxl" => compress_to_jxl(file, options, rule).await,
        "jpg" | "jpeg" => compress_jpg(file, options, rule).await,
        "png" => compress_png(file, options, rule).await,
        "heic" | "heif" => compress_heic(file, options, rule).await,
        "gif" => compress_gif(file, options, rule).await,
        _ => {
            let original = match read_source(file, target) {
                Ok(bytes) => bytes,
                Err(failure) => return failure,
            };
            rejected(
                original,
                target,
                "none",
                ReasonCode::UnsupportedFormat,
                format!("不支持的目标格式：{target}"),
            )
        }
    }
}

/// 单文件的一次完整压缩：尺寸上限 → 编码器 → 用**原图**复核结果。
///
/// `source_type` 是**用户原图**的逻辑格式。工作文件为了缩放可能被写成 PNG，
/// 但最终输出必须仍按原格式路由 —— 否则 WebP 会被悄悄变成 PNG（§21）。
async fn compress_core(
    file: &Path,
    options: &CompressOptions,
    rule: OutputRule,
    source_type: &str,
) -> EngineResult {
    let working = prepare_working_file(file, options);
    let src = working.path.clone();
    let result = encode_source(&src, options, rule, source_type).await;
    settle_working_file(file, options, working, result)
}

pub async fn compress_image(file: &Path, options: &CompressOptions) -> EngineResult {
    compress_image_with_cancel(file, options, || false).await
}

/// 压缩（§18.1）：只有结果有收益才提交。`cancel` 在**每一次编码启动之前**问一次，
/// 语义与批次闸门完全一致；绝不中途 kill 正在跑的编码。
pub async fn compress_image_with_cancel(
    file: &Path,
    options: &CompressOptions,
    cancelled: impl Fn() -> bool + Send + Sync,
) -> EngineResult {
    let source_type = detect_image_type(file);
    compress_entry(file, options, OutputRule::BenefitOnly, &source_type, &cancelled).await
}

/// 压缩到指定格式（§18.1）：仍然只有变小才提交。
pub async fn compress_to_format_with_cancel(
    file: &Path,
    target: &str,
    options: &CompressOptions,
    cancelled: impl Fn() -> bool + Send + Sync,
) -> EngineResult {
    let source_type = detect_image_type(file);
    let mut opts = options.clone();
    opts.output_format = target.into();
    compress_entry(file, &opts, OutputRule::BenefitOnly, &source_type, &cancelled).await
}

/// 显式格式转换（§18.2）：用户选的是容器，不是"必须更小"。
/// 只要编码和最终格式验证通过就提交，即使产物比原文件大。
pub async fn convert_to_format_with_cancel(
    file: &Path,
    target: &str,
    options: &CompressOptions,
    cancelled: impl Fn() -> bool + Send + Sync,
) -> EngineResult {
    let source_type = detect_image_type(file);
    let mut opts = options.clone();
    opts.output_format = target.into();
    compress_entry(file, &opts, OutputRule::Convert, &source_type, &cancelled).await
}

async fn compress_entry(
    file: &Path,
    options: &CompressOptions,
    rule: OutputRule,
    source_type: &str,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> EngineResult {
    // 系统模式先于预算：ImageIO 没有质量档可搜，这条事实由 system_mode_result 说明。
    if options.processing_mode == "system" {
        return system_mode_result(file, options);
    }
    match budget_request(options) {
        Some(budget) => solve_to_budget(file, options, budget, rule, source_type, cancelled).await,
        None => compress_core(file, options, rule, source_type).await,
    }
}

/// Smart mode: try the natural compressor; if smart, also try alternatives and pick best.
///
/// 指定体积优先于候选搜索（上限承诺比"多试一个格式"更重要，且它自带 6 次上限，
/// 不会退化成 候选 × 阶梯 的组合爆炸），尺寸上限同样要生效。
pub async fn compress_smart(file: &Path, options: &CompressOptions) -> EngineResult {
    compress_smart_with_cancel(file, options, || false).await
}

pub async fn compress_smart_with_cancel(
    file: &Path,
    options: &CompressOptions,
    cancelled: impl Fn() -> bool + Send + Sync,
) -> EngineResult {
    let source_type = detect_image_type(file);
    if options.processing_mode == "system" {
        return system_mode_result(file, options);
    }
    if let Some(budget) = budget_request(options) {
        return solve_to_budget(
            file, options, budget, OutputRule::BenefitOnly, &source_type, &cancelled,
        )
        .await;
    }

    let working = prepare_working_file(file, options);
    let src = working.path.clone();
    let result = smart_candidates(&src, options, &source_type).await;
    settle_working_file(file, options, working, result)
}

/// smart 的候选搜索本体；Direct 与 App Store 各走自己的编码器，候选集与挑选规则一致。
///
/// §61：`KeptOriginal` 不是候选 —— 它的字节是原图，拿它参与"挑最小"只会让
/// 一张没压小的图冒充赢家。候选集为空时回到普通压缩路径，由它给出真实结论。
#[allow(unreachable_code)]
async fn smart_candidates(
    file: &Path,
    options: &CompressOptions,
    source_type: &str,
) -> EngineResult {
    #[cfg(feature = "inproc-backends")]
    return engine_inproc::compress_smart(file, options, OutputRule::BenefitOnly, source_type).await;

    let quality = if options.quality > 0 { options.quality } else { analyze_quality(file) };

    let mut opts = options.clone();
    opts.quality = quality;

    // 显式目标格式不是一个"候选"：只许产那个格式，不许拿另一种更小的格式顶替。
    if opts.output_format != "original" {
        return route_to_format(file, &opts.output_format, &opts, OutputRule::BenefitOnly).await;
    }

    let mut candidates: Vec<EngineResult> = Vec::new();
    let mut push = |result: EngineResult| {
        if result.encode == EncodeVerdict::Candidate && !result.compressed.is_empty() {
            candidates.push(result);
        }
    };

    match source_type {
        "png" => {
            push(compress_png(file, &opts, OutputRule::BenefitOnly).await);
            // PNG → WebP 是智能模式的既定候选（见 frontend/intents.js 的意图说明）。
            push(compress_to_webp(file, &opts, OutputRule::BenefitOnly).await);
        }
        "jpg" => push(compress_jpg(file, &opts, OutputRule::BenefitOnly).await),
        "gif" => push(compress_gif(file, &opts, OutputRule::BenefitOnly).await),
        "webp" => push(compress_to_webp(file, &opts, OutputRule::BenefitOnly).await),
        "heic" => push(compress_heic(file, &opts, OutputRule::BenefitOnly).await),
        _ => {}
    }
    drop(push);

    if let Some(best) = candidates.into_iter().min_by_key(|result| result.compressed.len()) {
        return best;
    }

    // 没有候选：走普通压缩，让"没收益 / 失败 / 不支持"由同一个地方给出。
    encode_source(file, &opts, OutputRule::BenefitOnly, source_type).await
}

#[cfg(test)]
mod cpu_budget_tests {
    use super::*;

    /// 某个工具真正拿到的 CPU 相关命令行参数。
    fn flags_of(tool: &str) -> Vec<String> {
        cpu_flags(&tool_name(Path::new(tool)), per_task_threads())
    }

    fn env_value(key: &str) -> Option<String> {
        cpu_env()
            .into_iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn avifenc_gets_an_explicit_job_count_instead_of_the_default() {
        // avifenc 的 --jobs 在不同发行版里默认值不一致（有版本是 all），必须显式给。
        assert_eq!(flags_of("avifenc"), vec!["--jobs".to_string(), "1".to_string()]);
    }

    #[test]
    fn oxipng_is_pinned_to_one_worker() {
        assert_eq!(flags_of("oxipng"), vec!["--threads".to_string(), "1".to_string()]);
    }

    #[test]
    fn cwebp_does_not_opt_into_multithreading_under_one_permit() {
        // -mt 只能开关、不能指定线程数；单 worker 预算下传它就是超发。
        assert!(flags_of("cwebp").is_empty());
        assert_eq!(cpu_flags("cwebp", 4), vec!["-mt".to_string()]);
    }

    #[test]
    fn thread_env_is_set_for_every_cli_tool() {
        assert_eq!(env_value("RAYON_NUM_THREADS").as_deref(), Some("1"));
        assert_eq!(env_value("OMP_NUM_THREADS").as_deref(), Some("1"));
    }

    #[test]
    fn exe_extension_does_not_hide_the_tool_name() {
        assert_eq!(tool_name(Path::new("/opt/homebrew/bin/avifenc.exe")), "avifenc");
        assert_eq!(flags_of("avifenc.exe"), flags_of("avifenc"));
    }

    #[test]
    fn cjxl_has_its_default_all_cores_behaviour_pinned_down() {
        // cjxl 的 --num_threads 默认 0 = 按硬件线程数全开，是唯一"没传参就等于超发"的工具。
        assert_eq!(flags_of("cjxl"), vec!["--num_threads=1".to_string()]);
        assert_eq!(
            cpu_flags("cjxl", 4),
            vec!["--num_threads=4".to_string()]
        );
    }

    #[test]
    fn single_threaded_tools_get_no_extra_flags() {
        for tool in ["pngquant", "gifsicle", "cjpeg"] {
            assert!(flags_of(tool).is_empty(), "{tool} 不该拿到额外的 CPU 参数");
        }
    }
}

/// `maxDimensionPx` / `maxOutputBytes` 与结果元数据的行为契约测试。
///
/// 这些用例跑在 `compress_image*` 公共入口上，所以三条命令（默认 CLI 线、
/// `--features inproc-backends`、`--no-default-features --features inproc-backends`）
/// 会把**同一段断言**分别压在 Direct 与 App Store 两条产物线的编码器上。
#[cfg(test)]
mod intent_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 确定性伪随机噪声：同 seed 永远同一张图，测试结果才可复现。
    /// 用噪声是因为"平滑图"会被任何编码器压到几百字节，预算类用例就失去意义了。
    fn noise_rgb(w: u32, h: u32, seed: u32) -> image::RgbImage {
        let mut state = seed | 1;
        image::RgbImage::from_fn(w, h, |_x, _y| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let v = (state >> 16) as u8;
            image::Rgb([v, v.rotate_left(3), v.rotate_right(2)])
        })
    }

    /// 左侧 1/4 全透明的确定性 RGBA 图：alpha 类用例的判据必须真的用到 alpha。
    fn alpha_canvas(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            let v = ((x * 7 + y * 13) % 251) as u8;
            if x < w / 4 {
                image::Rgba([v, v, v, 0])
            } else {
                image::Rgba([v, v.rotate_left(2), v.rotate_right(3), 255])
            }
        })
    }

    fn write_jpeg(dir: &Path, name: &str, img: &image::RgbImage, quality: u8) -> PathBuf {
        use image::ImageEncoder;
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
            .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgb8)
            .unwrap();
        let path = dir.join(name);
        fs::write(&path, buf).unwrap();
        path
    }

    fn write_png(dir: &Path, name: &str, img: &image::RgbaImage) -> PathBuf {
        let path = dir.join(name);
        image::DynamicImage::ImageRgba8(img.clone()).save(&path).unwrap();
        path
    }

    fn dims_of_bytes(bytes: &[u8]) -> (u32, u32) {
        image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap()
    }

    fn warn_text(result: &EngineResult) -> String {
        result.warnings.join("\n")
    }

    fn warn_of(working: &WorkingFile) -> String {
        working.warnings.join("\n")
    }

    // ─── 纯计算：上限与阶梯 ─────────────────────────────────────

    #[test]
    fn the_cap_only_downscales_and_never_upscales() {
        assert_eq!(capped_dims(3000, 2000, 1000), Some((1000, 667)));
        assert_eq!(capped_dims(640, 480, 320), Some((320, 240)), "短边按等比例走");
        assert_eq!(capped_dims(3000, 2000, 3000), None, "正好等于上限不该白白重编码");
        assert_eq!(capped_dims(2000, 3000, 1000), Some((667, 1000)), "竖图看最长边");
        assert_eq!(capped_dims(3000, 2000, 8000), None, "比原图宽松的上限是放大请求，明确拒绝");
        assert_eq!(capped_dims(3000, 2000, 0), None, "0 = 不加约束");
        assert_eq!(capped_dims(4000, 10, 1000), Some((1000, 3)), "极扁图取整后仍至少 1px");
    }

    #[test]
    fn the_ladder_is_six_rungs_from_gentle_to_aggressive() {
        assert_eq!(
            budget_ladder(None),
            vec![
                (None, 80),
                (None, 65),
                (None, 50),
                (Some(2560), 65),
                (Some(1920), 60),
                (Some(1280), 55),
            ]
        );
        assert_eq!(budget_ladder(None).len(), BUDGET_ATTEMPT_LIMIT, "承诺过的是最多 6 次编码");
        // 用户自己的上限压在每一档之上：搜索永远不能把图放大到用户要求之上。
        // §60：2560 档被压成与 65 档相同的 (1500,65)，只编码一次，所以只剩 5 档。
        assert_eq!(
            budget_ladder(Some(1500)),
            vec![
                (Some(1500), 80),
                (Some(1500), 65),
                (Some(1500), 50),
                (Some(1500), 60),
                (Some(1280), 55),
            ]
        );
        // 递进关系是"先只动质量、再动像素"：前三档保原尺寸、质量一档比一档低；
        // 后三档开始缩边，最长边一档比一档短（缩掉的空间再把质量换回来一点，
        // 所以 q50 → q65 不是回退，而是同一档代价里更省字节的那一档）。
        let rungs = budget_ladder(None);
        for pair in rungs[..3].windows(2) {
            assert!(pair[0].0.is_none() && pair[1].0.is_none(), "前三档不许动像素");
            assert!(pair[1].1 < pair[0].1, "同一尺寸下质量必须一档比一档低");
        }
        for pair in rungs[3..].windows(2) {
            assert!(pair[1].0 < pair[0].0, "最长边必须一档比一档短");
        }
        assert!(rungs[3].0.is_some(), "第四档起才允许动像素");
    }

    // ─── 工作文件（尺寸上限的兑现点） ───────────────────────────

    #[test]
    fn a_capped_png_working_file_keeps_its_alpha_channel() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_png(dir.path(), "source.png", &alpha_canvas(1200, 900));
        let before = fs::read(&file).unwrap();

        let mut opts = CompressOptions::default();
        opts.max_dimension_px = Some(300);
        let working = prepare_working_file(&file, &opts);

        assert!(working.resized);
        assert_eq!(image::image_dimensions(&working.path).unwrap(), (300, 225));
        assert!(
            working.warnings.iter().any(|w| w.contains("尺寸上限 300px 生效") && w.contains("只缩不放大")),
            "{:?}",
            working.warnings
        );
        let scaled = image::open(&working.path).unwrap();
        assert_eq!(scaled.color(), image::ColorType::Rgba8, "PNG 工作文件必须保 alpha");
        assert!(
            scaled.to_rgba8().pixels().any(|p| p.0[3] == 0),
            "透明像素在缩放过程中被吃掉了"
        );
        assert_eq!(fs::read(&file).unwrap(), before, "原图必须一个字节都不动");
    }

    #[test]
    fn alpha_flattened_for_jpeg_output_is_warned_not_silently_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_png(dir.path(), "source.png", &alpha_canvas(1200, 900));

        let mut opts = CompressOptions::default();
        opts.max_dimension_px = Some(600);
        opts.output_format = "jpg".into();
        let working = prepare_working_file(&file, &opts);

        assert!(working.resized);
        assert!(
            working.warnings.iter().any(|w| w.contains("透明通道已按白底合成后丢弃")),
            "JPEG 没有 alpha，丢弃必须说出去：{:?}",
            working.warnings
        );
        assert_eq!(image::open(&working.path).unwrap().color(), image::ColorType::Rgb8);
        // 不透明的 PNG 走 JPEG 工作文件时不该唠叨这条告警
        let opaque = write_png(dir.path(), "opaque.png", &{
            let mut img = alpha_canvas(1200, 900);
            for p in img.pixels_mut() {
                p.0[3] = 255;
            }
            img
        });
        opts.output_format = "jpeg".into();
        let quiet = prepare_working_file(&opaque, &opts);
        assert!(quiet.warnings.iter().all(|w| !w.contains("透明通道")), "{:?}", quiet.warnings);
    }

    #[test]
    fn the_cap_refuses_to_silently_eat_gif_frames() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("anim.gif");
        image::DynamicImage::ImageRgba8(alpha_canvas(800, 600)).save(&file).unwrap();
        assert_eq!(detect_image_type(&file), "gif");

        let mut opts = CompressOptions::default();
        opts.max_dimension_px = Some(200);
        let working = prepare_working_file(&file, &opts);

        assert!(!working.resized, "动图降采样等于丢帧，必须原样交给下游");
        assert_eq!(working.path, file);
        assert!(
            warn_of(&working).contains("尺寸上限暂不作用于") && warn_of(&working).contains("帧"),
            "丢帧风险必须先说清楚：{:?}",
            working.warnings
        );
    }

    #[test]
    fn animated_sources_are_named_before_a_single_frame_encoder_sees_them() {
        // 最小 APNG 头：acTL 帧数 2，且出现在 IDAT 之前。
        let mut apng = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        apng.extend_from_slice(b"\x00\x00\x00\x08acTL\x00\x00\x00\x02");
        apng.extend_from_slice(b"\x00\x00\x00\x00IDAT");
        assert_eq!(animated_source(&apng, "png"), Some("APNG 动画"));

        // 最小动画 WebP 头：VP8X 的 ANIM 标志位。
        let mut webp = b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00".to_vec();
        webp.resize(21, 0);
        webp[20] = 0x02;
        assert_eq!(animated_source(&webp, "webp"), Some("动画 WebP"));

        // 拒绝原话必须点名来源格式、目标格式和「原文件未改动」。
        let rejection = animation_rejection(&webp, "JPEG").expect("动画 WebP 必须被拦下");
        assert!(rejection.contains("动画 WebP"), "{rejection}");
        assert!(rejection.contains("JPEG"), "{rejection}");
        assert!(rejection.contains("原文件未改动"), "{rejection}");
    }

    #[test]
    fn a_static_gif_is_not_mistaken_for_an_animation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("still.gif");
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([10, 20, 30, 255]),
        ))
        .save(&file)
        .unwrap();
        let bytes = fs::read(&file).unwrap();
        assert_eq!(detect_image_format(&bytes), "gif");
        assert_eq!(animated_source(&bytes, "gif"), None, "单帧 GIF 不该被当成动图");
    }

    #[test]
    fn an_unreadable_source_degrades_to_the_original_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("broken.jpg");
        fs::write(&file, b"\xff\xd8\xff\xe0 not really a jpeg").unwrap();

        let mut opts = CompressOptions::default();
        opts.max_dimension_px = Some(200);
        let working = prepare_working_file(&file, &opts);

        assert!(!working.resized);
        assert_eq!(working.path, file);
        assert!(warn_of(&working).contains("尺寸上限未能应用"), "{:?}", working.warnings);
    }

    // ─── 标签必须与字节一致（落盘按 out_type 换扩展名） ──────────

    /// 交回的字节就是源文件时，`out_type` 必须是源格式。
    /// 沿用目标格式的标签会让 commands.rs 把 PNG 字节存成 `.avif` —— 文件打不开，
    /// 历史里的备份也救不回那个假扩展名。
    #[test]
    fn kept_original_labels_the_bytes_with_the_format_they_actually_are() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_png(dir.path(), "photo.png", &alpha_canvas(64, 64));
        let original = fs::read(&file).unwrap();

        let result = kept_original(original.clone(), "avifenc", original.len() as u64, u64::MAX);

        assert_eq!(result.out_type, "png", "没压小回来时不许带着目标格式的标签");
        assert_eq!(result.compressed, original, "交回的必须是源文件本身");
        assert!(result.success, "没有优化空间不是失败");
    }

    /// 尺寸上限那一支的回落同理：中间文件是我缩放过的，交回用户的却是原图，
    /// 标签必须跟着交回的字节走，而不是跟着上游那次没算数的编码走。
    #[test]
    fn a_settled_run_that_keeps_the_original_reports_the_source_format() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_png(dir.path(), "photo.png", &alpha_canvas(1200, 900));
        let original = fs::read(&file).unwrap();

        let mut options = CompressOptions::default();
        options.max_dimension_px = Some(200);
        options.output_format = "avif".into();
        let working = WorkingFile {
            path: dir.path().join("resized.png"),
            _dir: None,
            resized: true,
            warnings: Vec::new(),
        };
        // 上游交回的是原图字节，标签却是目标格式。判据现在由 `encode` 说，
        // 不再靠"error 有没有值"猜 —— 一句提示和一次失败在旧判据下长得一模一样。
        let encoded = EngineResult {
            success: true,
            compressed: original.clone(),
            out_type: "avif".into(),
            algorithm: "avifenc".into(),
            error: Some("编码未产出有效结果".into()),
            encode: EncodeVerdict::KeptOriginal,
            reason: ReasonCode::NoSizeReduction,
            ..Default::default()
        };

        let settled = settle_working_file(&file, &options, working, encoded);

        assert_eq!(settled.out_type, "png", "保留的是原图，标签就得是原图的格式");
        assert_eq!(settled.compressed, original);
        assert!(
            settled.warnings.iter().any(|w| w.contains("已保留原图")),
            "{:?}",
            settled.warnings
        );
    }

    // ─── 端到端：两条产物线共用同一份 prep/settle ───────────────

    #[tokio::test]
    async fn the_dimension_cap_is_applied_before_the_encoder_runs() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "big.jpg", &noise_rgb(3000, 2000, 11), 95);
        let before = fs::read(&file).unwrap();
        assert!(before.len() > 200_000, "夹具太小，断言会失去意义");

        let mut opts = CompressOptions::default();
        opts.max_dimension_px = Some(1000);
        opts.preserve_metadata = false;
        let result = compress_image(&file, &opts).await;

        assert!(result.success, "{:?}", result.error);
        assert_eq!(dims_of_bytes(&result.compressed), (1000, 667), "最长边要正好落在请求值");
        assert!(result.warnings.iter().any(|w| w.contains("尺寸上限 1000px 生效")));
        assert_eq!(fs::read(&file).unwrap(), before, "原图必须一个字节都不动");
    }

    #[tokio::test]
    async fn a_cap_looser_than_the_source_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "small.jpg", &noise_rgb(1200, 800, 13), 92);

        let mut opts = CompressOptions::default();
        opts.max_dimension_px = Some(8000); // 放大请求
        opts.preserve_metadata = false;
        let result = compress_image(&file, &opts).await;

        assert!(result.success, "{:?}", result.error);
        assert_eq!(dims_of_bytes(&result.compressed), (1200, 800), "只缩不放大");
        assert!(result.warnings.is_empty(), "没生效的上限不该被说成生效了：{:?}", result.warnings);
    }

    #[tokio::test]
    async fn a_capped_png_still_has_alpha_at_the_end_of_the_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_png(dir.path(), "alpha.png", &alpha_canvas(1200, 900));

        let mut opts = CompressOptions::default();
        opts.max_dimension_px = Some(300);
        opts.preserve_metadata = false;
        let result = compress_image(&file, &opts).await;

        assert!(result.success, "{:?}", result.error);
        let (w, h) = dims_of_bytes(&result.compressed);
        assert_eq!((w, h), (300, 225));
        let out = image::ImageReader::new(std::io::Cursor::new(&result.compressed))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert!(
            out.to_rgba8().pixels().any(|p| p.0[3] < 255),
            "alpha 一路走到输出就没了，等于把透明图变成白底图"
        );
    }

    #[tokio::test]
    async fn reencoding_pixels_says_where_exif_went() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "photo.jpg", &noise_rgb(800, 600, 31), 92);

        // preserve_metadata 默认 true：重编码丢了元数据就必须说（规则 11）。
        let opts = CompressOptions::default();
        let result = compress_image(&file, &opts).await;
        assert!(result.success, "{:?}", result.error);
        assert!(warn_text(&result).contains("EXIF/GPS"), "{:?}", result.warnings);

        // 用户本来就不要元数据时不该唠叨同一条告警。
        let mut quiet = CompressOptions::default();
        quiet.preserve_metadata = false;
        let result = compress_image(&file, &quiet).await;
        assert!(result.warnings.iter().all(|w| !w.contains("EXIF/GPS")), "{:?}", result.warnings);
    }

    // ─── 指定体积求解器 ─────────────────────────────────────────

    #[tokio::test]
    async fn a_budget_the_source_already_meets_is_reported_met_without_reencoding() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_png(dir.path(), "tiny.png", &alpha_canvas(48, 48));
        let original = fs::read(&file).unwrap();
        let budget = original.len() as u64 + 1024;

        let mut opts = CompressOptions::default();
        opts.max_output_bytes = Some(budget);
        let result = compress_image(&file, &opts).await;

        assert_eq!(result.target_bytes, Some(budget));
        assert_eq!(result.target_met, Some(true), "原图已在预算内就是达成");
        assert_eq!(result.compressed, original, "已达标时不许白白重编码一次");
        assert!(warn_text(&result).contains("已在目标") && warn_text(&result).contains("未做无谓的重编码"));
    }

    #[tokio::test]
    async fn an_unreachable_budget_returns_the_smallest_candidate_and_admits_it_missed() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "big.jpg", &noise_rgb(2400, 1600, 5), 95);
        let original = fs::read(&file).unwrap();
        let budget = 4_000u64; // 噪声图 6 档都到不了

        let mut opts = CompressOptions::default();
        opts.max_output_bytes = Some(budget);
        opts.preserve_metadata = false;
        let result = compress_image(&file, &opts).await;

        assert_eq!(result.target_bytes, Some(budget));
        assert_eq!(result.target_met, Some(false), "没打到预算绝不能报成达成");
        let achieved = result.compressed.len() as u64;
        assert!(achieved < original.len() as u64, "规则 7：交出的必须真的比原图小");
        let text = warn_text(&result);
        assert!(text.contains("未达到"), "{text}");
        assert!(
            text.contains(&format_bytes(budget)) && text.contains(&format_bytes(achieved)),
            "告警里要同时写出实际达到的体积和用户的预算，前端才敢说真话：{text}"
        );
        let (w, h) = dims_of_bytes(&result.compressed);
        assert!(w.max(h) <= 1920, "阶梯后段带了尺寸上限，输出却还是全尺寸：{w}×{h}");
    }

    #[tokio::test]
    async fn a_reachable_budget_is_reported_as_met() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "big.jpg", &noise_rgb(2400, 1600, 3), 95);

        // 自己标定预算：先按阶梯第 3 档（q50、不动尺寸）编一次，拿它的实际体积当目标。
        // 这样"可达"在当前机器、当前产物线上都是确定的，断言不会因编码器差异变脆。
        let mut calib = CompressOptions::default();
        calib.quality = 50;
        calib.preserve_metadata = false;
        let probe = compress_image(&file, &calib).await;
        assert!(probe.success, "{:?}", probe.error);
        let budget = probe.compressed.len() as u64;

        let mut opts = CompressOptions::default();
        opts.max_output_bytes = Some(budget);
        opts.preserve_metadata = false;
        let result = compress_image(&file, &opts).await;

        assert_eq!(result.target_met, Some(true), "{:?}", result.warnings);
        assert_eq!(result.target_bytes, Some(budget));
        assert!((result.compressed.len() as u64) <= budget);
        assert!(
            result.warnings.iter().all(|w| !w.contains("未达到")),
            "命中预算却说了没命中：{:?}",
            result.warnings
        );
    }

    #[tokio::test]
    async fn a_stopped_scheduler_cancels_instead_of_faking_a_result() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "big.jpg", &noise_rgb(2400, 1600, 9), 95);

        let mut opts = CompressOptions::default();
        opts.max_output_bytes = Some(4_000);
        opts.preserve_metadata = false;

        // 判据一上来就是"已取消"：一次编码都不许启动，也不许把原图当成搜索结果交回去。
        let result = compress_image_with_cancel(&file, &opts, || true).await;
        assert_eq!(result.encode, EncodeVerdict::Cancelled);
        assert_eq!(result.reason, ReasonCode::Cancelled);
        assert!(result.compressed.is_empty(), "取消不是产出一个文件");
        assert_eq!(result.target_met, None, "搜索没跑完，不能替用户断言达成与否");

        // 第一档允许跑完、第二档不许启动：证明判据落在两次编码之间（不中途 kill 编码器）。
        let calls = AtomicUsize::new(0);
        let probe = || calls.fetch_add(1, Ordering::SeqCst) >= 1;
        let result = compress_image_with_cancel(&file, &opts, probe).await;
        assert_eq!(result.encode, EncodeVerdict::Cancelled);
        assert!(result.compressed.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "第二档查询后就必须停");
    }

    /// macOS：系统转换真的会跑，所以必须证明"没跑体积搜索"这件事**不许**冒充成达成目标。
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn system_mode_does_not_pretend_to_have_run_a_budget_search() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "photo.jpg", &noise_rgb(900, 600, 21), 92);

        let mut opts = CompressOptions::default();
        opts.processing_mode = "system".into();
        opts.max_output_bytes = Some(10); // ImageIO 这条线没有质量档可搜
        let result = compress_image(&file, &opts).await;

        assert_eq!(result.target_bytes, Some(10));
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.target_met, Some(false), "没跑搜索就不能报达成");
        assert!(warn_text(&result).contains("系统转换不做指定体积搜索"), "{:?}", result.warnings);
    }

    /// 非 macOS：`convert_with_system` 只有"拒绝"这一条诚实的回答，绝不许它把失败
    /// 说成成功。断言的是这次系统模式**如实失败**：判据是 UnsupportedFormat + Rejected
    /// （不是 Failed/Unknown 那种含糊），请求的体积目标原样回显但绝不报达成，
    /// 文案里必须看得见"系统转换仅支持 macOS"。
    ///
    /// 注意文案的位置：`rejected()` 把话写进 `error`，而 `system_mode_result` 只在
    /// `success` 为真时才追加告警 —— 所以这条线上 `warnings` 是空的，必须读 `error`。
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn system_mode_reports_unsupported_off_macos() {
        let dir = tempfile::tempdir().unwrap();
        let file = write_jpeg(dir.path(), "photo.jpg", &noise_rgb(900, 600, 21), 92);

        let mut opts = CompressOptions::default();
        opts.processing_mode = "system".into();
        opts.max_output_bytes = Some(10); // 这条路径连搜索都没有，谈不上命中
        let result = compress_image(&file, &opts).await;

        assert!(!result.success, "非 macOS 的系统转换必须如实失败，不能报成功");
        assert_eq!(result.reason, ReasonCode::UnsupportedFormat);
        assert_eq!(result.encode, EncodeVerdict::Rejected);
        // 交回的是原图字节：这说明失败判据来自"格式不支持"，而不是源文件没读到
        // （读失败是 encode=Failed + SourceReadFailed/NotFound，见 read_source）。
        assert!(!result.compressed.is_empty(), "拒绝不是产出一个空文件");
        assert_eq!(result.target_bytes, Some(10), "被拒绝也要把用户要的目标说清楚");
        assert_ne!(result.target_met, Some(true), "什么都没编码，绝不能报达成");
        assert_eq!(result.target_met, Some(false));
        let message = result.error.unwrap_or_default();
        assert!(
            message.contains("系统转换仅支持 macOS"),
            "error={message:?}, warnings={:?}",
            result.warnings
        );
    }

    // ─── 线上字段契约 ───────────────────────────────────────────

    #[test]
    fn old_payloads_still_deserialize_after_the_fake_controls_were_deleted() {
        let legacy = r#"{
            "processingMode":"advanced","systemImageSize":"actual","preserveMetadata":true,
            "quality":80,"outputFormat":"original","outputMode":"suffix",
            "outputSuffix":"_compressed","outputDir":null,"sourceRoots":[],
            "smartMode":true,"backend":"mozjpeg","effort":7,"convertToWebp":true,"lossless":false,
            "maxDimensionPx":1280,"maxOutputBytes":524288
        }"#;
        let opts: CompressOptions = serde_json::from_str(legacy).unwrap();
        assert_eq!(opts.quality, 80);
        // 被删掉的五个假字段：带着它们也必须能读，且不参与任何引擎决策。
        assert_eq!(opts.max_dimension_px, Some(1280), "camelCase 线上名必须是 maxDimensionPx");
        assert_eq!(opts.max_output_bytes, Some(524288), "camelCase 线上名必须是 maxOutputBytes");

        let empty: CompressOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.max_dimension_px, None, "缺省 = 不加尺寸约束");
        assert_eq!(empty.max_output_bytes, None, "缺省 = 不跑体积搜索");
        assert_eq!(empty.quality, 75);
    }

    #[test]
    fn new_result_fields_are_camel_case_and_optional() {
        let mut result = CompressResult::default();
        result.source_width = Some(3000);
        result.source_height = Some(2000);
        result.output_width = Some(1000);
        result.output_height = Some(667);
        result.elapsed_ms = Some(123);
        result.target_bytes = Some(524288);
        result.target_met = Some(false);
        result.warnings = Some(vec!["尺寸上限 1000px 生效".into()]);
        let keys: Vec<String> = serde_json::to_value(&result)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for key in [
            "sourceWidth",
            "sourceHeight",
            "outputWidth",
            "outputHeight",
            "elapsedMs",
            "targetBytes",
            "targetMet",
            "warnings",
        ] {
            assert!(keys.iter().any(|k| k == key), "前端要读的键 {key} 不存在：{keys:?}");
        }
        assert!(!keys.iter().any(|k| k.starts_with("max")), "结果里不该回流请求字段：{keys:?}");
        // §139 第四阶段：旧 success 旗标已删除，不再序列化到前端。
        assert!(!keys.iter().any(|k| k == "success"), "success 旗标已删除：{keys:?}");

        // 已经落盘的老历史没有这些键，必须仍能反序列化（规则 10）。
        // 老载荷里若还带着 success，它只是被忽略的未知字段 —— 没有 outcome 就没有产物。
        let legacy = r#"{"success":true,"file":"/tmp/a.jpg","originalSize":10,"compressedSize":8,
            "savings":20.0,"originalSizeFormatted":"10.0B","compressedSizeFormatted":"8.0B",
            "type":"jpg","algorithm":"mozjpeg","error":null,"outputPath":"/tmp/a_c.jpg",
            "backupPath":null,"outputMode":null}"#;
        let parsed: CompressResult = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.out_type, "jpg");
        assert_eq!(parsed.source_width, None);
        assert_eq!(parsed.elapsed_ms, None);
        assert_eq!(parsed.target_bytes, None, "没跑过搜索的历史不能凭空有个 true");
        assert_eq!(parsed.target_met, None);
        assert_eq!(parsed.warnings, None);
        assert!(!parsed.has_product(), "没有结论的旧载荷不许拿 outputPath 猜产物");
    }
}
