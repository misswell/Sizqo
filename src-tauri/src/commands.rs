// Sizqo - Tauri command handlers (replaces Electron IPC)

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

use crate::app_settings::{effective_cpu_limit, AppSettings, SettingsStore};
use crate::engine::{self, CompressOptions, CompressResult, EngineResult};
use crate::history::{HistoryEntry, HistoryStore, RestoreError};
use crate::output_transaction::{
    self, OutputNameAllocator, OutputNaming, ReplaceTransaction, StagedOutput, TransactionMode,
    TransactionStore,
};
use crate::result_model::{self, EncodeVerdict, Evaluation, ReasonCode, ResultOutcome};
use crate::sandbox_access::FileAccess;
use crate::source_identity::{SourceCheck, SourceIdentity};
use crate::system_info::{CpuInfo, CpuStatus};

/// Shared app state.
pub struct AppState {
    pub cancel_queue: Mutex<HashSet<String>>,
    /// 主窗口打开对比时暂存的待渲染载荷，compare 窗口加载完成后取走。
    pub pending_compare: Mutex<Option<serde_json::Value>>,
    pub compression: std::sync::Arc<CompressionScheduler>,
    pub history_store: std::sync::Arc<HistoryStore>,
    /// replace 覆盖的事务日志：崩溃后据此把原图恢复回来。
    pub transactions: std::sync::Arc<TransactionStore>,
    pub settings_store: std::sync::Arc<SettingsStore>,
    /// 跨启动的文件访问授权：Direct 版直通，沙盒版用 security-scoped bookmark。
    pub access: std::sync::Arc<dyn FileAccess>,
    /// 启动时检测一次的本机 CPU 能力：设置页展示 + 上限天花板。
    pub cpu_info: CpuInfo,
    /// 本次运行的起点：「不保留」档靠它区分"这次造的记录"和"上次崩溃留下的记录"。
    pub run_started_at: i64,
    /// 上次运行是否走完了退出清理。为假时说明磁盘上那批备份是崩溃现场留下的、
    /// 可能是原图唯一的副本，本次退出得再留它一次。
    pub previous_run_ended_cleanly: bool,
}

/// 批次阶段。这是前端渲染按钮与状态文案的**唯一**依据（经 `compression-state-change`
/// 事件下发），前端不再自己从"批次还没结束"猜当前是什么状态。
///
/// - `idle`：没有批次在跑。
/// - `running`：批次在跑，闸门开着。
/// - `paused`：闸门对"还没开始"的文件关着，已经在跑的继续跑完。
/// - `stopping`：用户点了停止。闸门对"还没开始"的文件**永久**关闭（暂停中也能停），
///   已经在跑的跑完，批次收尾后回到 `idle`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CompressionState {
    Idle,
    Running,
    Paused,
    Stopping,
}

/// 压缩调度器：暂停 / 停止与 CPU 使用上限共用一个闸门。
///
/// 三者语义相同 —— 只决定"要不要再启动新任务"，绝不干预已经在跑的子进程或线程：
/// 暂停或停止时正在压的那张继续跑完；上限从 8 调到 2 时已有的 8 个也允许跑完，
/// 只是不再启动新的。反过来 2 → 8 立刻唤醒等待者。
///
/// 用 Notify + 轮询超时兜底：notify_waiters 与 notified.await 之间存在丢唤醒的
/// 窗口，超时让最坏情况只是延迟几百毫秒，而不是永久卡住。
/// 压缩会话与维护操作（恢复/清理）共用同一个原子生命周期状态仲裁：
/// 0=空闲 1=批次 2=维护。"检查时空闲、拿到手前批次开跑"的窗口从结构上不存在，
/// 这是恢复与压缩之间真正的互斥（§2.7），不再是一次状态检查加祈祷。
pub struct CompressionScheduler {
    lifecycle: AtomicUsize,
    paused: AtomicBool,
    /// 批次级的「停止」。与文件级的 `cancel_queue` 是两件事：停止不需要逐条记账，
    /// 闸门本身对后来者永久关闭。
    stopping: AtomicBool,
    max_parallelism: AtomicUsize,
    /// §63 并发账和内存账必须在同一把锁里结：拆成两个原子量分别 CAS，
    /// 两张 700 MB 的图会同时看见"还有余量"然后一起挤进 2 GB 预算。
    usage: Mutex<GateUsage>,
    memory_budget: AtomicUsize,
    notify: tokio::sync::Notify,
    /// 仅测试：在 usage 锁**内**统计 permit 发放次数。锁外计数会跟丢
    /// "发放发生在 pause 之前、记账发生在 pause 之后"的时序，锁内计数才能
    /// 精确断言"pause() 返回后不再有新发放"。
    #[cfg(test)]
    test_grants: AtomicUsize,
}

#[derive(Default)]
struct GateUsage {
    active: usize,
    /// 已发放 permit 的估算解码内存之和（字节）。
    memory: usize,
    /// 一张超大图独占期间，小图也不许再启动 —— 否则"单任务"只是句空话。
    exclusive: bool,
}

/// 一份 CPU 并行预算。Drop 即归还，因此提前 return、panic 都不会把闸门卡死。
pub struct ParallelPermit {
    scheduler: std::sync::Arc<CompressionScheduler>,
    /// 这张图记在内存账上的字节数；0 表示调用方没提供估算（测试 / 非图片任务）。
    memory: usize,
    exclusive: bool,
}

/// 等闸门等的三种结局。**必须分开**，不能再像老实现那样用 `Option` 一个 `None`
/// 同时表示"这个文件被用户取消"和"整批被停止"：
///
/// - `Cancelled`：用户明确把**这个文件**移出队列（点 ×、清空队列）。它真的不该再被压。
/// - `Stopped`：用户停的是**这一轮**。这个文件仍然该压，只是本轮没轮到它 ——
///   队列里保持 `pending`，下一次「继续压缩」自然带上。
///
/// 两者混在一起，就是"停止后 70 张图全部变成已跳过、再也没法继续"的根源。
pub enum AcquireOutcome {
    Acquired(ParallelPermit),
    Cancelled,
    Stopped,
}

/// 上限的最小值 —— 0 会让一个任务都启动不了。
pub const MIN_PARALLELISM: usize = 1;

impl CompressionScheduler {
    pub fn new(max_parallelism: usize) -> Self {
        Self::with_memory_budget(max_parallelism, crate::image_memory::budget_bytes())
    }

    /// 指定解码预算的构造器。生产用物理内存推导；测试需要可重复的账本，
    /// 不能因为跑测试的机器内存大小不同而改变并发行为。
    pub fn with_memory_budget(max_parallelism: usize, memory_budget: usize) -> Self {
        Self {
            lifecycle: AtomicUsize::new(LIFECYCLE_IDLE),
            paused: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            max_parallelism: AtomicUsize::new(max_parallelism.max(MIN_PARALLELISM)),
            usage: Mutex::new(GateUsage::default()),
            memory_budget: AtomicUsize::new(memory_budget.max(1)),
            notify: tokio::sync::Notify::new(),
            #[cfg(test)]
            test_grants: AtomicUsize::new(0),
        }
    }

    pub fn pause(&self) {
        // 暂停的生效与 permit 的发放共用同一把 usage 锁：try_acquire 在锁内读
        // paused，所以 pause() 一返回，就不再有任何"拿着没暂停的旧判断"出场的新
        // permit —— 以前这读写在锁外，窗口里抢到的 permit 会让用户看着「已暂停」
        // 还冒出新任务。
        let _usage = self.usage.lock().unwrap();
        self.paused.store(true, Ordering::Release);
    }

    /// 解除暂停并唤醒等待中的 worker。**只有用户点「继续」和批次起止才许调用**。
    pub fn resume(&self) {
        self.paused.store(false, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// 停止整批：闸门对"还没开始"的文件永久关闭，已经在跑的跑完各自收尾。
    ///
    /// 与"取消某个文件"是两件事：`cancel_file` 要逐条记账（`cancel_queue`），
    /// 停止只是把闸门焊死 —— 后来者不需要谁替它记账，自己就会在
    /// `acquire` 里看见 `stopping` 并拿到 `AcquireOutcome::Stopped`。
    ///
    /// 绝不 kill 正在跑的进程：那会留下写了一半的临时文件、悬空的覆盖事务和
    /// 对不上账的历史，正是「宁可慢一点也不能弄丢原图」要避免的。
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    /// 只唤醒等待者，不改动暂停态。
    ///
    /// 与 `resume()` 的区别必须分清：取消一个文件需要正在等的 worker 醒来看到这个取消，
    /// 但**绝不**需要顺便把整批解除暂停 —— 老实现调的是 `resume()`，于是"暂停中移除一张图"
    /// 会让其余所有等待中的文件偷偷继续压缩，而 UI 上仍写着「暂停中…」。
    pub fn wake_waiters(&self) {
        self.notify.notify_waiters();
    }

    /// 是否有批次在跑。清空历史这类破坏性操作的硬保护看这个，不看前端状态。
    /// 用户点了停止但批次还在收尾时它仍然为真 —— 备份还在被认领，历史就不许被清。
    pub fn is_batch_active(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) == LIFECYCLE_BATCH
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// 运行中改上限：不回收已在跑的 permit，只影响之后的启动。
    ///
    /// 与 pause() 同理：上限的写入和"按上限放行"共用 usage 锁，调小的瞬间不会有
    /// 等待者拿着旧上限挤进临界区，active 瞬时超过新上限的窗口不存在了。
    pub fn set_max_parallelism(&self, limit: usize) {
        let _usage = self.usage.lock().unwrap();
        self.max_parallelism
            .store(limit.max(MIN_PARALLELISM), Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn max_parallelism(&self) -> usize {
        self.max_parallelism.load(Ordering::Acquire).max(MIN_PARALLELISM)
    }

    /// 当前正在占用的 CPU 并行份数。UI 展示的是"上限/核数"而不是这个瞬时值，
    /// 所以只有调度器测试会读它 —— 事件里也不带它：快照在下一次事件之前不会更新，
    /// 显示出去就是一句很快就会变假的话。
    #[cfg(test)]
    pub fn active(&self) -> usize {
        self.usage.lock().unwrap().active
    }

    /// 当前这一轮的解码预算（字节）。§63：并发数不再是唯一的闸门。
    #[cfg(test)]
    pub fn memory_budget(&self) -> usize {
        self.memory_budget.load(Ordering::Acquire).max(1)
    }

    #[cfg(test)]
    pub fn memory_in_use(&self) -> usize {
        self.usage.lock().unwrap().memory
    }

    /// 占住这一轮 compression session —— §37：**全场只有这一个入口**，
    /// 图片、视频、混合三条命令都从这里拿，拿不到就拒绝第二次开跑。
    ///
    /// 成功的会话一定从"未暂停、未停止"开始，不继承上一轮遗留的位。
    /// 失败时**一个原子位都不改**：抢会话的那个入口没有权利把正在跑的会话暂停，
    /// 更没有权利替它解除暂停。
    pub fn try_begin_batch(&self) -> bool {
        if self.lifecycle.compare_exchange(LIFECYCLE_IDLE, LIFECYCLE_BATCH, Ordering::AcqRel, Ordering::Acquire).is_err() { return false; }
        self.paused.store(false, Ordering::Release);
        self.stopping.store(false, Ordering::Release);
        true
    }

    /// 维护租约：恢复原图 / 清理历史在动手**之前**必须持有。与会话启动在同一个
    /// 原子状态上仲裁，恢复与压缩不可能各拿一半检查再各走各的。
    pub fn begin_maintenance(self: &std::sync::Arc<Self>) -> Option<MaintenanceLease> {
        self.lifecycle
            .compare_exchange(LIFECYCLE_IDLE, LIFECYCLE_MAINTENANCE, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(MaintenanceLease { scheduler: self.clone() })
    }

    pub fn end_batch(&self) {
        self.paused.store(false, Ordering::Release);
        self.stopping.store(false, Ordering::Release);
        // 只有真正持有批次的收尾才许清状态：维护租约在场时这里不可能被调用
        //（会话根本开不起来），防御性地 CAS 而不是无条件 store。
        let _ = self.lifecycle.compare_exchange(LIFECYCLE_BATCH, LIFECYCLE_IDLE, Ordering::AcqRel, Ordering::Acquire);
        *self.usage.lock().unwrap() = GateUsage::default();
        self.notify.notify_waiters();
    }

    /// idle / running / paused / stopping —— 前端按钮与 summary 的唯一真相来源。
    pub fn state(&self) -> CompressionState {
        if self.lifecycle.load(Ordering::Acquire) != LIFECYCLE_BATCH {
            return CompressionState::Idle;
        }
        // 停止盖过暂停：暂停中点停止，状态就该是"正在停止"，而不是一直显示暂停。
        if self.is_stopping() {
            return CompressionState::Stopping;
        }
        if self.is_paused() {
            CompressionState::Paused
        } else {
            CompressionState::Running
        }
    }

    /// 拿一份 CPU 预算：暂停中或名额已满就等，拿到时闸门是开着的。
    ///
    /// 老实现是"等信号量 → 再查一次暂停"两步，因为信号量不知道暂停。
    /// 现在 paused 的判断、账本的记账同在一把 usage 锁里完成，拿到 permit 的那一刻
    /// 必然既没暂停也没超载；acquire 拿到后还会复核一次暂停。
    /// 测试专用：先拿一份预算，不关心演出条件（闸门一定是开的）。
    #[cfg(test)]
    pub async fn acquire_permit(self: &std::sync::Arc<Self>) -> ParallelPermit {
        // 这里传一个永远为假的取消判据：调用方不关心取消，只是要闸门开。
        match self.acquire(|| false).await {
            AcquireOutcome::Acquired(permit) => permit,
            // 测试里的调用点都保证闸门开着；真出现说明用例自己写错了。
            AcquireOutcome::Cancelled | AcquireOutcome::Stopped => {
                unreachable!("这个调用永远不会返回 Cancelled / Stopped")
            }
        }
    }

    /// 拿一份 CPU 预算；等待期间 `cancelled()` 变真、或批次被「停止」，就退出等待。
    ///
    /// 判定顺序是刻意的，**不能改**：取消 → 停止 → 暂停/名额。
    /// - 取消排最前：`paused == true` 且这个文件已被取消时，worker 能马上退出，
    ///   而暂停状态原样保留给其余还在排队的文件。
    /// - 停止排在暂停之前：暂停中按下停止，等待者必须能看见"整批已停"并退出，
    ///   否则它们会一直堵在关着的闸门上，直到用户点「继续」——那正是「停止」的反面。
    ///
    /// 三种结局必须分开回给调用方（`AcquireOutcome`）：取消是"这个文件不再参与队列"，
    /// 停止是"这一轮没轮到它，队列里它还是 pending"，两者的收尾动作完全相反。
    pub async fn acquire<F>(self: &std::sync::Arc<Self>, cancelled: F) -> AcquireOutcome
    where
        F: FnMut() -> bool,
    {
        self.acquire_with_memory(0, cancelled).await
    }

    /// §63 带内存估算的闸门：CPU 名额与解码预算**同时**满足才放行。
    ///
    /// 判定顺序仍是取消 → 停止 → 暂停/名额/预算：内存只是多了一道"还不能开工"
    /// 的理由，绝不改变取消与停止的语义，也不打断已经在跑的任务。
    pub async fn acquire_with_memory<F>(
        self: &std::sync::Arc<Self>,
        memory: usize,
        mut cancelled: F,
    ) -> AcquireOutcome
    where
        F: FnMut() -> bool,
    {
        loop {
            if cancelled() {
                return AcquireOutcome::Cancelled;
            }
            if self.is_stopping() {
                return AcquireOutcome::Stopped;
            }
            if let Some(permit) = self.try_acquire_with_memory(memory) {
                // 拿到的这一刻才被取消 / 才点了停止 / 才按了暂停：前两者还回预算收场，
                // 暂停则是还回预算继续排队等 resume —— 暂停不是停止，不能当成退出。
                if cancelled() {
                    drop(permit);
                    return AcquireOutcome::Cancelled;
                }
                if self.is_stopping() {
                    drop(permit);
                    return AcquireOutcome::Stopped;
                }
                if self.is_paused() {
                    drop(permit);
                    continue;
                }
                return AcquireOutcome::Acquired(permit);
            }
            let notified = self.notify.notified();
            let _ = tokio::time::timeout(Duration::from_millis(250), notified).await;
        }
    }

    pub fn try_acquire(self: &std::sync::Arc<Self>) -> Option<ParallelPermit> {
        self.try_acquire_with_memory(0)
    }

    /// 三档调度由账本自然得出，不需要另一份"小图/大图"分类表：
    /// - 小图：几 MB，CPU 名额先用完，和以前完全一样；
    /// - 大图：几张就吃满预算，并发被预算压低；
    /// - 超大图：一张就超过半份预算，独占运行，别的任务一个都不放。
    ///
    /// 比总预算还大的图在空场时仍然放行 —— 拒绝它就等于这张图永远压不了；
    /// 它进来后 `fits` 永远为假，后来者自然全被挡在外面。
    pub fn try_acquire_with_memory(self: &std::sync::Arc<Self>, memory: usize) -> Option<ParallelPermit> {
        let mut usage = self.usage.lock().unwrap();
        // paused 必须在这把锁里读：pause() 拿同一把锁置位，两边不可能交错，
        // "检查时没暂停、放行时已暂停"的窗口从结构上不存在。
        if self.is_paused() {
            return None;
        }
        if usage.exclusive {
            return None;
        }
        if usage.active >= self.max_parallelism() {
            return None;
        }
        let budget = self.memory_budget.load(Ordering::Acquire).max(1);
        let exclusive = memory > budget / 2;
        let fits = usage.memory.saturating_add(memory) <= budget;
        if usage.active > 0 && (exclusive || !fits) {
            return None;
        }
        usage.active += 1;
        usage.memory = usage.memory.saturating_add(memory);
        usage.exclusive = exclusive;
        #[cfg(test)]
        self.test_grants.fetch_add(1, Ordering::AcqRel);
        Some(ParallelPermit {
            scheduler: self.clone(),
            memory,
            exclusive,
        })
    }
}

impl Drop for ParallelPermit {
    fn drop(&mut self) {
        // saturating：end_batch 会把账本归零，迟到的 Drop 不能减成天文数字。
        {
            let mut usage = self.scheduler.usage.lock().unwrap();
            usage.active = usage.active.saturating_sub(1);
            usage.memory = usage.memory.saturating_sub(self.memory);
            if self.exclusive {
                usage.exclusive = false;
            }
        }
        self.scheduler.notify.notify_waiters();
    }
}

/// 一轮 compression session 的把持凭证（§37 §38）。
///
/// 图片、视频、混合三条入口都只能从这里开跑：拿到凭证 = 全场只有我在跑，
/// 拿不到就直接拒绝，**绝不并跑**。这不是洁癖 —— 两轮会话共用同一个闸门时，
/// 先结束那一轮的 `end_batch` 会把后一轮的暂停、停止和取消账本一起清零，
/// 于是"点了暂停却还在压""停止后进度凭空跳一截"全都回来了。
///
/// 它同时是一张收据：`compress_batch_with_session` 要求调用方出示它，
/// 没有凭证就连编译都进不了批处理核心，"忘记结束会话"也因此不可能发生 ——
/// Drop 是唯一的收口。
pub(crate) struct SessionGuard {
    app: AppHandle,
    scheduler: std::sync::Arc<CompressionScheduler>,
}

/// 抢不到会话时回的那句原话。前端按它整句判断"顶回来的是并发会话，不是压坏了"
/// （frontend/app.js 的 SESSION_BUSY_MESSAGE），改一个字都得两边一起改。
pub const SESSION_BUSY_MESSAGE: &str = "Another media session is already running";

impl SessionGuard {
    pub(crate) fn begin(app: &AppHandle, state: &AppState) -> Result<Self, String> {
        if !state.compression.try_begin_batch() {
            return Err(SESSION_BUSY_MESSAGE.into());
        }
        // 每轮开始按当前设置重算并发度：换过机器 / 手改过 settings.json 之后，
        // 上限也来得及跟上，而不是沿用启动时那个数。
        let settings = state.settings_store.load();
        state.compression.set_max_parallelism(effective_cpu_limit(
            settings.cpu_thread_limit,
            state.cpu_info.budget_ceiling(),
        ));
        // 会话真的开跑了才播报 running：前端在拿到这条事件之前不许显示「压缩中…」。
        emit_compression_state(app, &state.compression);
        Ok(Self { app: app.clone(), scheduler: state.compression.clone() })
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        // 先清本轮的取消账本、再交还批次凭证：end_batch 会立刻放行下一轮会话，
        // 若顺序颠倒，下一轮刚登记的用户取消会被这里顺手清空（取消丢失，
        // 该文件仍被压缩）。清完账本再 end_batch，新会话登记的取消不会被误删。
        self.app.state::<AppState>().cancel_queue.lock().unwrap().clear();
        self.app
            .state::<crate::video_commands::VideoRuntime>()
            .cancellations
            .lock()
            .unwrap()
            .clear();
        self.scheduler.end_batch();
        emit_compression_state(&self.app, &self.scheduler);
    }
}

/// 独立对比窗口的固定 label（capabilities/default.json 按 label 授权）。
pub const COMPARE_WINDOW_LABEL: &str = "compare";

// ─── Progress event payload ─────────────────────────────────────
/// 单个文件的进度状态。**用枚举而不是字符串**：老实现里"完成了但失败"和"被跳过"
/// 都靠一个空字符串 + 有没有 result 去猜，前端只能反过来推。
///
/// - `Queued`：这一轮开始了，它排在队列里。
/// - `Starting`：真的开工了（已经拿到闸门名额）。
/// - `Completed` / `Failed`：跑完了，`result` 里带着明细。
/// - `Cancelled`：用户明确把这个文件移出了队列 —— 它不再参与本轮，也不该再回队列。
/// - `Deferred`：整批被「停止」，这一轮没轮到它 —— **队列里它仍然是 pending**，
///   下一次「继续压缩」会带上它。它没有 `result`，因为它压根没执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgressStatus {
    Queued,
    Starting,
    Completed,
    Failed,
    Cancelled,
    Deferred,
}

/// 一条进度事件。`session_id` 是这一轮执行会话的标识：**逐条事件都带着它**，
/// 前端据此彻底阻断跨会话污染（上一轮迟到的 deferred 事件不许影响下一轮）。
///
/// `session_total` / `session_processed` 只描述**这一轮**（诊断用）：
/// 界面上的总进度由前端的队列状态派生（见 frontend/app.js 的 getQueueProgress），
/// 不是这个计数 —— 停止后重新开始一轮时，sessionTotal 会变小，而队列进度不该变小。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPayload {
    session_id: String,
    /// Frontend queue generation; an event from before Clear All must be ignored.
    queue_revision: u64,
    file: String,
    status: ProgressStatus,
    /// Unix milliseconds, for diagnostics and consistent event envelopes.
    timestamp: u64,
    /// 本轮的目标文件数（停止后重新开始的那一轮只数 pending）。
    session_total: usize,
    /// 本轮真正处理完的数量 + 明确取消的数量；`Deferred` **不增加**它。
    session_processed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<CompressResult>,
}

fn event_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 一轮执行的收尾报告。只用于诊断：界面总进度看队列，不看这里。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CompressionSessionResult {
    session_id: String,
    /// 真正跑过的文件数（= results.len()）。
    attempted: usize,
    completed: usize,
    failed: usize,
    /// 因为「停止」这一轮没轮到的文件数 —— 它们在队列里仍然是 pending。
    deferred: usize,
    /// 被用户明确取消（移出队列）的文件数。
    cancelled: usize,
    pub results: Vec<CompressResult>,
}

/// 批次阶段变化的载荷。
///
/// 只有状态，**不含"还有几个在跑"这类计数**：事件只在阶段真的变化时发一次，快照里的
/// 数字在下一个事件之前不会再更新，放进 UI 就是一句很快就变假的话。谁还在跑，队列里
/// 那几个仍在转圈的行就是活的答案。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CompressionStatePayload {
    state: CompressionState,
}

/// 阶段变化的唯一播报口：每次状态真的变了都走这里，前端不再猜。
pub(crate) fn emit_compression_state(app: &AppHandle, scheduler: &CompressionScheduler) {
    let _ = app.emit(
        "compression-state-change",
        CompressionStatePayload {
            state: scheduler.state(),
        },
    );
}

// ─── File collection ────────────────────────────────────────────
fn collect_image_files(file_paths: &[String]) -> Vec<String> {
    let exts = [
        "png", "jpg", "jpeg", "gif", "webp", "bmp", "avif", "jxl", "heic", "heif", "tif",
        "tiff",
    ];
    let mut all = Vec::new();
    let mut seen_files = HashSet::new();
    let mut visited_dirs = HashSet::new();
    for fp in file_paths {
        let path = PathBuf::from(fp);
        if path.is_dir() {
            walk_dir(&path, &exts, &mut all, &mut seen_files, &mut visited_dirs);
        } else if path.is_file() {
            add_image_file(path, &exts, &mut all, &mut seen_files);
        }
    }
    all
}

fn path_identity(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn add_image_file(
    path: PathBuf,
    exts: &[&str],
    out: &mut Vec<String>,
    seen_files: &mut HashSet<String>,
) {
    // 导入按**内容**识别（probe_media 用的是同一份 image_header 魔数嗅探），
    // 会话侧只按扩展名过滤会把 .jfif / 无扩展名的图片静默丢出本轮 ——
    // 前端队列里它永远停在 pending，再无事件可收。扩展名不认识时退回内容嗅探，
    // 两侧判据归一。
    let is_image = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| exts.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
        || crate::video_commands::image_header(&path);
    if is_image {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen_files.insert(canonical.to_string_lossy().into_owned()) {
            // Return the canonical path so separate imports using relative,
            // symlinked, or `..` aliases also deduplicate in the frontend.
            out.push(canonical.to_string_lossy().into_owned());
        }
    }
}

fn walk_dir(
    dir: &Path,
    exts: &[&str],
    out: &mut Vec<String>,
    seen_files: &mut HashSet<String>,
    visited_dirs: &mut HashSet<String>,
) {
    if !visited_dirs.insert(path_identity(dir)) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    for path in paths {
        if path.is_dir() {
            walk_dir(&path, exts, out, seen_files, visited_dirs);
        } else if path.is_file() {
            add_image_file(path, exts, out, seen_files);
        }
    }
}

// ─── Result building ────────────────────────────────────────────
fn build_result(file_path: &str, engine_result: &EngineResult) -> CompressResult {
    let path = PathBuf::from(file_path);
    let original_size = engine::get_file_size(&path);
    let compressed_size = engine_result.compressed.len() as u64;
    let savings = if original_size > 0 {
        ((original_size - compressed_size.min(original_size)) as f64 / original_size as f64) * 100.0
    } else {
        0.0
    };
    let (source_width, source_height) = source_dimensions(&path);
    let (output_width, output_height) = output_dimensions(&engine_result.compressed);
    CompressResult {
        file: file_path.into(),
        original_size,
        compressed_size,
        savings: (savings * 10.0).round() / 10.0,
        original_size_formatted: engine::format_bytes(original_size),
        compressed_size_formatted: engine::format_bytes(compressed_size),
        out_type: engine_result.out_type.clone(),
        algorithm: engine_result.algorithm.clone(),
        error: engine_result.error.clone(),
        output_path: None,
        backup_path: None,
        output_mode: None,
        source_width,
        source_height,
        output_width,
        output_height,
        // 墙钟耗时由调用方测量：引擎只对字节负责，不知道自己被调度器排到了什么时候。
        elapsed_ms: None,
        target_bytes: engine_result.target_bytes,
        // 结论只来自引擎：没跑过搜索就是 None，不能在这里替它编一个 true。
        target_met: engine_result.target_met,
        warnings: if engine_result.warnings.is_empty() {
            None
        } else {
            Some(engine_result.warnings.clone())
        },
        // 结论由 Publish 那一步写（`write_output_file`）：这里只知道引擎交了什么字节，
        // 还不知道磁盘上发生了什么。此时给结论就是"编码成功 = 压缩成功"那个老错。
        outcome: None,
        reason_code: None,
        history_id: None,
    }
}

/// 用户原图的像素尺寸：只读文件头，不解码整幅像素。
/// 认不出的容器（HEIC/AVIF/JXL，image crate 当前没开对应解码器）返回 None ——
/// 详情页据此显示"未知"而不是编一个 0×0。
fn source_dimensions(path: &Path) -> (Option<u32>, Option<u32>) {
    match image::image_dimensions(path) {
        Ok((width, height)) => (Some(width), Some(height)),
        Err(_) => (None, None),
    }
}

/// 输出字节流的像素尺寸（同样只读头部）。
fn output_dimensions(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
    else {
        return (None, None);
    };
    match reader.into_dimensions() {
        Ok((width, height)) => (Some(width), Some(height)),
        Err(_) => (None, None),
    }
}

fn normalized_output_suffix(value: Option<&str>) -> String {
    let mut suffix = value
        .unwrap_or("_compressed")
        .trim()
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' || ch == '\0' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();

    if suffix.is_empty() {
        suffix = "_compressed".into();
    }
    suffix.chars().take(64).collect()
}

/// §28 输出目录：产物永远落在源文件旁边（跨挂载点的 rename 不是原子操作）。
/// 相对路径的 `parent()` 会是 Some("")，那种目录名不能拿去开文件 —— 按当前目录算。
fn output_directory(file_path: &Path) -> &Path {
    file_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn file_stem_of(file_path: &Path) -> String {
    file_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("file")
        .to_string()
}

/// 引擎交回的字节属于哪种格式，文件就得是什么扩展名。
///
/// jpeg/jpg、heif/heic 是同一种格式的不同拼法：把它们判成"转换"会让每个 `.jpeg`
/// 都在原图旁边长出一个 `.jpg`。
fn same_output_format(file_path: &Path, out_format: &str) -> bool {
    let current_ext = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();
    current_ext == out_format
        || (current_ext == "jpeg" && out_format == "jpg")
        || (current_ext == "jpg" && out_format == "jpeg")
        || (current_ext == "heif" && out_format == "heic")
}

/// 输出目录模式下产物在输出目录里的相对位置。
///
/// 只认导入时冻结的来源根（§39 / §40）：根对不上就没有层级可用，产物直接落在
/// 输出目录顶层。这里不再按"这一批只有一个目录""就是文件的父目录"去补一个根 ——
/// 那等于每跑一批重新猜一次，重试、续跑、导出各自猜出不同的答案。
fn relative_output_path(file_path: &Path, options: &CompressOptions) -> PathBuf {
    crate::source_root::relative_to_roots(file_path, &options.source_roots).unwrap_or_else(|| {
        file_path
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("image"))
    })
}

/// 落盘没成，收回这次占住的输出名。
///
/// 同格式覆盖的目标**就是用户原图**，那种情况下"收回占位"等于删原图 —— 所以界线就是
/// `out_path == file_path` 这一条：只有我们新建出来的空占位才归自己删。
/// 收回前必须核对占位凭证 `(dev,ino)`：外部程序即使删掉占位又放进自己的文件，
/// 这里的清理也不会碰它（§28 审查意见 A）。
fn release_output_placeholder(
    out_path: &Path,
    file_path: &Path,
    placeholder: Option<(u64, u64)>,
) {
    if out_path == file_path {
        return;
    }
    let still_ours = match (placeholder, crate::output_transaction::file_identity(out_path)) {
        (Some(expected), Some(actual)) => expected == actual,
        // 没有凭证可比（非 unix / 文件已消失）：文件已消失就无需清理；
        // 非 unix 退化为旧行为。
        (None, None) => !out_path.exists(),
        _ => false,
    };
    if still_ours {
        let _ = fs::remove_file(out_path);
    }
}

/// 落盘（Commit）阶段的失败分类。
///
/// 谁把这些失败写进结果字段不由这里决定 —— §13 规定 Publish 是唯一能给最终事实的地方。
/// `write_output_file` 拿到 Err 之后调 `publish_failure`，调用方只负责记日志。
#[derive(Debug)]
pub enum OutputWriteError {
    BackupFailed(String),
    TransactionFailed(String),
    OutputWriteFailed(String),
    HistoryWriteFailed,
    RollbackFailed(String),
    /// 评估说「该落盘」，输出方式却给不出目标位置（folder 没选目录 / 未知的模式）。
    /// 这不是跳过，是配置没兑现。
    InvalidDestination(String),
    /// §28 输出目录里排到第 10 000 个名字都还撞车：一个字节都不许写，让用户自己清。
    /// 走到这里说明目录里全是 `foo_compressed.jpg`、`foo_compressed_1.jpg`…，
    /// 继续猜名字只会猜出一个谁都对不上号的产物。
    Collision(String),
    /// §25 §26 覆盖前复核发现源文件已经不是这次压缩所依据的那份字节。
    /// 处置只有「拒绝覆盖」一种：候选丢掉，用户当前文件原样留着 ——
    /// 拿改动后的文件当"原图"备份，恢复出来的东西谁都认不出。
    SourceChanged(String),
    /// §24 连"这份文件是什么"都读不出来（stat 失败 / 内容指纹读不动）。
    /// 编码前就走到这里的话，原图一个字节都没被碰过。
    SourceUnreadable(String),
}

impl OutputWriteError {
    /// §4 §59：失败原因是枚举值，不是文案。前端拿它选措辞、选操作，
    /// 永远不需要 `error.includes("larger")` 这种一换文案就失效的判据。
    pub fn reason_code(&self) -> ReasonCode {
        match self {
            OutputWriteError::BackupFailed(_) => ReasonCode::BackupFailed,
            OutputWriteError::TransactionFailed(_) => ReasonCode::TransactionFailed,
            OutputWriteError::OutputWriteFailed(_) => ReasonCode::OutputWriteFailed,
            OutputWriteError::HistoryWriteFailed => ReasonCode::HistoryWriteFailed,
            OutputWriteError::RollbackFailed(_) => ReasonCode::RollbackFailed,
            OutputWriteError::InvalidDestination(_) => ReasonCode::InvalidInput,
            OutputWriteError::Collision(_) => ReasonCode::OutputCollision,
            OutputWriteError::SourceChanged(_) => ReasonCode::SourceChanged,
            OutputWriteError::SourceUnreadable(_) => ReasonCode::SourceReadFailed,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            OutputWriteError::BackupFailed(detail) => {
                format!("无法保存原图备份，已跳过覆盖（{detail}）")
            }
            OutputWriteError::TransactionFailed(detail) => {
                format!("无法登记这次覆盖，已跳过覆盖（{detail}）")
            }
            OutputWriteError::OutputWriteFailed(detail) => {
                format!("压缩结果写入失败，原图未被覆盖（{detail}）")
            }
            OutputWriteError::HistoryWriteFailed => {
                "压缩结果已生成，但历史记录保存失败，已自动恢复原图".into()
            }
            OutputWriteError::RollbackFailed(detail) => format!(
                "历史记录保存失败，且自动恢复原图也没成功：原图还在那份备份里，请不要清理备份，重试恢复即可（{detail}）"
            ),
            OutputWriteError::InvalidDestination(detail) => detail.clone(),
            OutputWriteError::Collision(detail) => {
                format!("目标位置已有同名文件，且已经多到无法再起一个名字（{detail}）")
            }
            OutputWriteError::SourceChanged(detail) => format!(
                "这个文件在压缩期间被改动过，已放弃覆盖，你现在的版本保持原样（{detail}）"
            ),
            OutputWriteError::SourceUnreadable(detail) => {
                format!("读不出这个文件的当前状态，已跳过覆盖（{detail}）")
            }
        }
    }
}

/// §10 Encode 阶段交回的东西：候选字节 + 引擎自己知道的那点事实。
///
/// Encode **不能决定最终任务是否成功**。`engine_success` 被带到这里的唯一目的，是让
/// Evaluate 能认出「判据与旗标打架」（生产者忘了标 verdict）这种 bug 并判成失败 ——
/// 而不是让它替结论说话（§140）。
struct EncodedArtifact<'a> {
    bytes: &'a [u8],
    format: &'a str,
    verdict: EncodeVerdict,
    reason: ReasonCode,
    engine_success: bool,
    target_met: Option<bool>,
}

impl<'a> EncodedArtifact<'a> {
    fn from_engine(engine: &'a EngineResult) -> Self {
        EncodedArtifact {
            bytes: &engine.compressed,
            format: &engine.out_type,
            verdict: engine.encode,
            reason: engine.reason,
            engine_success: engine.success,
            target_met: engine.target_met,
        }
    }
}

/// §19 §20 最终格式验证：扩展名和 `out_type` 都是我们自己写的字符串，
/// 只有字节头才是格式自己说的话。任何一个不一致都不许落盘。
///
/// - 显式请求了容器：字节必须就是那个容器（HEIF 请求不许拿 JPEG 冒充）；
/// - 未指定格式：字节必须与引擎标签一致（标签决定落盘扩展名）。
fn artifact_format_error(
    artifact: &EncodedArtifact,
    file_path: &Path,
    options: &CompressOptions,
) -> Option<String> {
    // 只有**候选**才需要这一步：拒绝/失败带回来的字节是原图，标签也是源格式，
    // 拿去跟目标格式比只会把一条真实的拒绝原因盖成"格式不符"。空候选归 Evaluate。
    if artifact.verdict != EncodeVerdict::Candidate || artifact.bytes.is_empty() {
        return None;
    }
    let actual = engine::detect_image_format(artifact.bytes);
    if actual == "unknown" {
        return Some(format!(
            "输出字节不是可识别的图片格式（引擎标签 {}），已拒绝落盘",
            artifact.format
        ));
    }
    let expected = if options.output_format != "original" && !options.output_format.is_empty() {
        engine::requested_format(&engine::detect_image_type(file_path), options).to_string()
    } else {
        artifact.format.to_string()
    };
    if !engine::same_kind(actual, &expected) {
        return Some(format!(
            "请求的输出格式是 {expected}，实际产出的是 {actual}；已拒绝落盘"
        ));
    }
    if !engine::same_kind(actual, artifact.format) {
        return Some(format!(
            "引擎标签写的是 {}，实际字节是 {actual}；已拒绝按错误扩展名落盘",
            artifact.format
        ));
    }
    None
}

/// §11 Evaluate：全程序只有这一处回答"这些字节值不值得落盘"。
fn evaluate_output(
    result: &CompressResult,
    artifact: &EncodedArtifact,
    file_path: &Path,
    options: &CompressOptions,
) -> Evaluation {
    Evaluation::evaluate(
        artifact.verdict,
        artifact.reason,
        artifact.engine_success,
        artifact.bytes.len(),
        result.original_size,
        engine::is_format_conversion(file_path, options),
        artifact.target_met,
    )
}

/// §13 Publish：整条链的事实到此才变成用户看到的那份结果。
///
/// 「结论 + 体积账」必须一次写完，别处再补一句就成了第二个真相来源。
/// §139 第四阶段：`success` 旗标已删除，结论只由 `outcome` 说 —— 引擎成功却没落盘
/// 的那一支，历史上正是靠那个镜像冒充过「压缩完成」。
fn publish_conclusion(
    result: &mut CompressResult,
    outcome: ResultOutcome,
    reason_code: Option<ReasonCode>,
    output_path: Option<PathBuf>,
) {
    result.outcome = Some(outcome);
    result.reason_code = reason_code;
    result.output_path = output_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    if output_path.is_none() {
        // 没有产物，盘上躺着的就还是原图：体积账必须按这个事实结，
        // 否则一次「压完更大所以跳过」会被统计成省了负数（§2.1）。
        result.compressed_size = result.original_size;
        result.savings = 0.0;
        result.compressed_size_formatted = engine::format_bytes(result.original_size);
    }
}

/// 落盘之前就能定案的那三支（没收益 / 原图已达标 / 连候选都没有）：
/// 没有产物，就没有 outputPath、没有历史、没有"对比/恢复/另存"。
fn publish_precommit(result: &mut CompressResult, evaluation: &Evaluation) {
    let (outcome, reason_code) = evaluation.outcome();
    // evaluate 只在 Commit 时返回 None，走到这里必然已经有结论。
    let outcome = outcome.unwrap_or(ResultOutcome::Failed);
    publish_conclusion(result, outcome, reason_code, None);
}

/// Commit 失败同样是一个结论，而且必须带上"没产物"的账（§6.3）。
fn publish_failure(result: &mut CompressResult, error: &OutputWriteError) {
    publish_conclusion(result, ResultOutcome::Failed, Some(error.reason_code()), None);
    result.error = Some(error.user_message());
}

/// §33 取消发生在 Commit 之前：候选可以生成，但一个字节都不许落盘。
/// 它是一次 Publish，因为结论只允许在 §13 那一处写；reason 分「用户取消这一个」
/// 和「整批被停止」两种说法（§35），两者都没有产物、没有历史。
fn publish_cancelled(result: &mut CompressResult, reason_code: ReasonCode) {
    publish_conclusion(result, ResultOutcome::Cancelled, Some(reason_code), None);
}

/// §141 违反不变式是后端的 bug，只能降级成失败把问题说出来。它同样是一次 Publish，
/// 所以结论字段只能在这儿写 —— 至于原图到底动没动，我们并不知道，绝不许替它保证。
fn publish_violation(result: &mut CompressResult, violation: &str) {
    publish_conclusion(result, ResultOutcome::Failed, Some(ReasonCode::Unknown), None);
    result.error = Some(format!("{violation}；这次结果已按失败处理"));
}

/// §12 Commit 的产出：文件真的在磁盘上的哪个位置。
///
/// 它不含结论 —— `outcome` 只允许由 §13 Publish 写。
struct CommitFacts {
    out_path: PathBuf,
}

/// §33 Commit 这一步只有两种下场：真的落盘了，或者在碰到磁盘之前被取消挡住。
/// 被挡住时磁盘一个字节都没动，所以调用方连"清理候选"都不用做。
enum CommitResult {
    Committed(CommitFacts),
    Cancelled,
}

/// §29 落盘只有两种模式：**放回原文件的位置**，或者**只新建一个文件**。
///
/// 这是全程序唯一一处回答"这次要不要备份、事务算哪一类、目标名字凭什么可用"的地方。
/// 以前这里躺着 `is_replace` 和 `cross_format` 两个布尔，各自只说自己那一半：后缀模式
/// 既不进事务也没有独占名字，于是 `photo_compressed.jpg` 存在时第二次压缩直接把它盖掉，
/// 而用户以为"各留一份"。模式一旦定下来，三件事必须由它派生，不许再分别判。
enum OutputPlacement {
    /// 压缩结果落在原文件位置。同格式时目标就是原路径；跨格式时原图会被删掉、
    /// 产物是分配出来的新名字 —— 两种都动过原图，所以都靠备份恢复。
    ReplaceExisting { cross_format: bool },
    /// 原图一个字节都不动（后缀 / 输出目录）：名字必须独占申请，恢复只许删自己生成的文件。
    CreateNew,
}

impl OutputPlacement {
    fn transaction_mode(&self) -> TransactionMode {
        match self {
            OutputPlacement::ReplaceExisting { .. } => TransactionMode::ReplaceExisting,
            OutputPlacement::CreateNew => TransactionMode::CreateNew,
        }
    }

    fn cross_format(&self) -> bool {
        matches!(self, OutputPlacement::ReplaceExisting { cross_format: true })
    }

    /// 覆盖模式的记账凭证必须落到目录里（AGENTS 规则 8：durable precommit journal）；
    /// 新建模式丢了日志最多下次启动多留一个空占位，不值得每个文件多一次目录 fsync。
    fn journal(&self, transactions: &TransactionStore, txn: &ReplaceTransaction) -> Result<(), String> {
        match self {
            OutputPlacement::ReplaceExisting { .. } => transactions.prepare_durable(txn),
            OutputPlacement::CreateNew => transactions.prepare(txn),
        }
    }
}

/// §24 §25 编码**之前**就要备齐的覆盖依据：这次压缩基于哪一份字节，以及那份字节的快照。
///
/// 为什么必须早于编码：覆盖模式的备份以前写在 Commit 里，也就是"压完了才留原图"。
/// 用户在编码期间改了这张图时，Commit 备份到的是**改动后**的内容 —— 历史从此声称它能
/// 恢复原图，恢复出来的却是一张谁都没见过的图。快照提前到编码前，复核才有了对象。
#[derive(Debug)]
pub enum ReplaceBaseline {
    /// 覆盖模式：编码前读到的指纹 + 编码前写好的原图快照。
    Ready {
        identity: SourceIdentity,
        backup: std::path::PathBuf,
    },
    /// 后缀 / 输出目录：原图从不动，没有要复核的对象，也不需要快照。
    NotReplace,
    /// 覆盖依据没能备齐。结论照旧由 Publish 写，所以这里只带错误、不提前 emit。
    Unavailable(OutputWriteError),
}

/// 跑 §25 流程的前三步（读指纹 → 立刻留快照 → 才许编码）。
///
/// 非覆盖模式一次磁盘写都不做：那些模式的原图从未被盖过，快照与复核都没有对象。
fn capture_replace_baseline(
    path: &Path,
    options: &CompressOptions,
    history: &HistoryStore,
) -> ReplaceBaseline {
    if options.output_mode != "replace" {
        return ReplaceBaseline::NotReplace;
    }
    let identity = match SourceIdentity::read_with_hash(path) {
        Ok(identity) => identity,
        Err(error) => return ReplaceBaseline::Unavailable(OutputWriteError::SourceUnreadable(error.to_string())),
    };
    // 复用旧备份前先对当前字节指纹：两轮之间用户改过原图时，旧备份已经不代表
    // 「这一次被盖掉的原图」，恢复它会静默回滚用户的编辑。
    match history.ensure_backup_verified(path, identity.sha256.as_deref()) {
        Ok(backup) => ReplaceBaseline::Ready { identity, backup },
        Err(detail) => ReplaceBaseline::Unavailable(OutputWriteError::BackupFailed(detail)),
    }
}

/// §9 核心流程的四阶段总入口。Encode 已经在引擎里跑完（`engine_result` 就是它的产物），
/// 这里依次 Evaluate → Commit → Publish：
///
/// 1. **Evaluate** 只有 `evaluate_output` 一处回答"这些字节值不值得落盘"；
/// 2. **Commit** 值得才开事务、备份、写临时文件、fsync、rename、记历史；
/// 3. **Publish** 只有真的落盘并记了历史，才允许写 `outcome = Committed/TargetUnmet`。
///
/// 返回的 `Err` 只为调用方记日志和分流程保留；结果字段一律由 Publish 写，
/// 调用方不许再替这次任务下结论。
///
/// `cancel` 是 §34 的 Commit 前检查点：取消可以发生在编码期间，候选照样能生成，
/// 但它绝不能等到磁盘上多出个文件之后才算数。
///
/// `baseline` 是 §24 §25 在编码**之前**备好的覆盖依据。它只在 Commit 这一步才开口说话：
/// 评估决定不落盘时（没收益 / 已达标 / 被取消），磁盘什么都没动，快照缺失也不重要，
/// 结论仍由 Publish 那一处写。
#[allow(clippy::too_many_arguments)]
fn write_output_file(
    result: &mut CompressResult,
    engine_result: &EngineResult,
    file_path: &Path,
    options: &CompressOptions,
    history: &HistoryStore,
    transactions: &TransactionStore,
    retention_days: u32,
    cancel: &(dyn Fn() -> bool + Sync),
    baseline: ReplaceBaseline,
) -> Result<(), OutputWriteError> {
    let artifact = EncodedArtifact::from_engine(engine_result);
    // §19 §20：字节、引擎标签、请求格式三者不一致时，这张图**没有**可提交的产物。
    // 它比 Evaluate 更早 —— 评估问的是"值不值得落盘"，这里问的是"这是不是一个
    // 我们敢按那个扩展名写下去的文件"。
    if let Some(detail) = artifact_format_error(&artifact, file_path, options) {
        publish_conclusion(
            result,
            ResultOutcome::Failed,
            Some(ReasonCode::OutputValidationFailed),
            None,
        );
        result.error = Some(detail);
        return Ok(());
    }
    let evaluation = evaluate_output(result, &artifact, file_path, options);
    if !matches!(evaluation, Evaluation::Commit { .. }) {
        publish_precommit(result, &evaluation);
        return Ok(());
    }
    let facts = match commit_output(
        result, &artifact, file_path, options, history, transactions, retention_days,
        cancel, baseline,
    ) {
        Ok(CommitResult::Committed(facts)) => facts,
        // §33 取消打在落盘之前：磁盘一个字节都没动，所以这条发布的是 Cancelled，
        // 不是失败 —— 系统没坏，是用户不要了（§6.4）。
        Ok(CommitResult::Cancelled) => {
            publish_cancelled(result, ReasonCode::Cancelled);
            return Ok(());
        }
        Err(error) => {
            publish_failure(result, &error);
            return Err(error);
        }
    };
    let (outcome, reason_code) = evaluation.committed();
    publish_conclusion(result, outcome, reason_code, Some(facts.out_path));
    Ok(())
}

/// §12 Commit：事务、临时文件、fsync、改名、历史、回滚。
///
/// 原图快照**不在这里做**（§24）：它已经在编码前由 `capture_replace_baseline` 备好，
/// 这里只负责复核它。replace 模式必须走完整的顺序，任何一步失败都不得留下
/// "覆盖了但没人知道"的状态：
///
/// ```text
/// ① 完整复核源文件（stat + 内容哈希）  ② 独占目标名 + 独占临时名  ③ 写 transaction
/// ④ 写满候选 → flush + fsync  ⑤ 改名前再核一次指纹 + 确认备份字节对得上
/// ⑥ rename 覆盖目标  ⑦ history.add  ⑧ 删 transaction
/// ```
///
/// ⑦ 失败才回滚：用备份把真正原图写回去、删掉这次生成的文件，并把错误报给用户。
/// ①②③④⑤⑥ 失败**不回滚** —— 那些时刻用户的文件还完好在原地（或改名没成功），
/// 撤回的只能是我们自己的临时文件和占位名（§34「不能做假取消」同理：
/// 没动过的东西不许假装恢复）。
///
/// 这里唯一写回 `result` 的是历史条目要用的**输入事实**（备份路径、输出方式）；
/// 结论与产物路径归 Publish。
///
/// `cancel` 是 §34 的第三次检查：编码期间用户按了「取消这一个」，候选已经在内存里，
/// 但事务、临时文件、目标文件都不许出现。检查放在最前面 —— 一旦进入下面的
/// 原子顺序就不再打断（§34「不能做假取消」：喊了停却照样覆盖，比不停更糟）。
#[allow(clippy::too_many_arguments)]
fn commit_output(
    result: &mut CompressResult,
    artifact: &EncodedArtifact,
    file_path: &Path,
    options: &CompressOptions,
    history: &HistoryStore,
    transactions: &TransactionStore,
    retention_days: u32,
    cancel: &(dyn Fn() -> bool + Sync),
    baseline: ReplaceBaseline,
) -> Result<CommitResult, OutputWriteError> {
    if cancel() {
        return Ok(CommitResult::Cancelled);
    }
    let compressed = artifact.bytes;
    // 目标位置的扩展名以 **Encode 交回的字节格式** 为准（§10）。它就是 result.out_type
    // 的同一个来源，但 Commit 不该隔着结果字段去猜自己要把文件叫什么。
    let out_format = artifact.format.to_string();
    let mut backup_file: Option<PathBuf> = None;
    // 只有覆盖模式才有"要复核的对象"；后缀/目录模式的原图从头到尾没被碰过。
    let mut replace: Option<SourceIdentity> = None;

    // §29 先定落盘模式，再由这一个决定派生出"要不要备份""名字怎么来""事务算哪一类"。
    // 拿到手的目标名已经是独占申请到的（同格式覆盖除外：它的目标本来就是用户那个文件）。
    let (out_path, placeholder, placement) = match options.output_mode.as_str() {
        "replace" => {
            // §25 覆盖的依据必须在场：读不到源文件指纹、或编码前的快照没写成，
            // 就一个字节都不许碰用户文件。原因由 Publish 统一发布成对应结论。
            let (identity, backup) = match baseline {
                ReplaceBaseline::Ready { identity, backup } => (identity, backup),
                ReplaceBaseline::Unavailable(error) => return Err(error),
                ReplaceBaseline::NotReplace => {
                    return Err(OutputWriteError::BackupFailed(
                        "覆盖模式没有编码前留下的原图快照".into(),
                    ))
                }
            };
            // ①复核放在占名之前：判定一旦变成"文件已经不是你压缩的那份了"，
            //   用户目录里连一个多余文件都不该出现过（§25「删除 candidate」）。
            //   这里必须是完整 `verify`：改名前那道快速门看不出"同大小、时间戳没动"
            //   的原地改动，而那种改动恰恰是最需要拦的。
            if let SourceCheck::Changed(detail) = identity.verify(file_path) {
                return Err(OutputWriteError::SourceChanged(detail));
            }
            result.backup_path = Some(backup.to_string_lossy().into_owned());
            backup_file = Some(backup);
            replace = Some(identity);
            // 字节属于哪种格式，文件就得叫什么扩展名：覆盖模式把 WebP 字节塞进
            // photo.png（或把 AVIF 塞进 photo.heic）会得到一个两边都打不开的文件。
            // 引擎在"没压小就保留原图"时交回的是源格式（engine::kept_original），
            // 所以这里判到的格式差异只可能是真转换，不是失败的回落。
            if same_output_format(file_path, &out_format) {
                (file_path.to_path_buf(), None, OutputPlacement::ReplaceExisting { cross_format: false })
            } else {
                // 跨格式转换的产物是新文件，但名字照样要独占申请：photo.webp 可能是
                // 邻居的成品，"转换"把它盖掉就不是转换而是毁掉别人的东西（§28）。
                let claimed = OutputNameAllocator::claim(
                    output_directory(file_path),
                    &OutputNaming::Converted {
                        stem: file_stem_of(file_path),
                        extension: out_format.clone(),
                    },
                )
                .map_err(OutputWriteError::Collision)?;
                let identity = claimed.identity();
                (claimed.into_path(), identity, OutputPlacement::ReplaceExisting { cross_format: true })
            }
        }
        "suffix" => {
            let claimed = OutputNameAllocator::claim(
                output_directory(file_path),
                &OutputNaming::WithSuffix {
                    stem: file_stem_of(file_path),
                    suffix: normalized_output_suffix(Some(&options.output_suffix)),
                    extension: out_format.clone(),
                },
            )
            .map_err(OutputWriteError::Collision)?;
            let identity = claimed.identity();
            (claimed.into_path(), identity, OutputPlacement::CreateNew)
        }
        "folder" => {
            // 评估说"该落盘"，输出方式却给不出目标位置（folder 没选目录）。这不是跳过，
            // 是配置没兑现：以前这里 `return Ok(())` 且 success 保持 true，用户看到
            // 「压缩完成」而磁盘上什么都没有（§71 明令禁止的幽灵输出）。
            let Some(out_dir) = options.output_dir.as_deref() else {
                return Err(OutputWriteError::InvalidDestination(
                    "请先选择输出目录".into(),
                ));
            };
            let rel = relative_output_path(file_path, options);
            let dir = PathBuf::from(out_dir).join(rel.parent().unwrap_or(Path::new("")));
            // 子目录必须先存在：否则独占申请报出来的是"找不到目录"，
            // 和用户能理解的毛病（输出目录还没建）对不上。
            if let Err(detail) = fs::create_dir_all(&dir) {
                return Err(OutputWriteError::OutputWriteFailed(format!(
                    "{}: {detail}",
                    dir.display()
                )));
            }
            let claimed = OutputNameAllocator::claim(
                &dir,
                &OutputNaming::WithSuffix {
                    stem: rel
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or("image")
                        .to_string(),
                    suffix: String::new(),
                    extension: out_format.clone(),
                },
            )
            .map_err(OutputWriteError::Collision)?;
            let identity = claimed.identity();
            (claimed.into_path(), identity, OutputPlacement::CreateNew)
        }
        // 未知的输出方式：同样是配置没兑现，不许冒充"压缩完成"。
        _ => {
            return Err(OutputWriteError::InvalidDestination(
                "输出方式无效".into(),
            ))
        }
    };
    let cross_format = placement.cross_format();

    // 事务 id 必须在覆盖之前定下来：历史落盘成功与否就靠它和 transaction 对账。
    //   两种模式都要记账 —— 新建模式那份日志负责"证明不了提交就删掉自己生成的文件"（§32）。
    let history_id = HistoryEntry::new_id(file_path);

    // ②临时名先独占、再记账：日志必须在一个字节落盘之前就知道它叫什么。反过来做的话，
    //   崩在"写完临时文件"与"改名"之间时，那个 .tmp 是唯一知道自己在场的记录还没写。
    let staged = match StagedOutput::claim(&out_path, ".sizqo-write-") {
        Ok(staged) => staged,
        Err(detail) => {
            release_output_placeholder(&out_path, file_path, placeholder);
            return Err(OutputWriteError::OutputWriteFailed(detail));
        }
    };
    let txn = ReplaceTransaction {
        id: history_id.clone(),
        history_id: history_id.clone(),
        source_path: path_identity(file_path),
        output_path: out_path.to_string_lossy().into_owned(),
        backup_path: backup_file
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        temp_output_path: Some(staged.temp.to_string_lossy().into_owned()),
        original_size: result.original_size,
        expected_output_size: compressed.len() as u64,
        // §2.5：事务必须带着身份证据落账。original 来自编码前钉下的源指纹，
        // output 来自还没落盘的产物字节 —— 启动恢复靠这两个哈希证明归属，
        // 证明不了就不会拿备份去盖一个被外部改过的文件。
        original_sha256: replace.as_ref().and_then(|identity| identity.sha256.clone()),
        output_sha256: Some(crate::source_identity::sha256_bytes(compressed)),
        placeholder_identity: placeholder,
        created_at: crate::history::now_millis(),
        cross_format,
        mode: placement.transaction_mode(),
    };
    // ③记账先于动手：写不进日志就还不许碰用户文件，占好的名字也得收回去。
    if let Err(detail) = placement.journal(transactions, &txn) {
        staged.discard();
        release_output_placeholder(&out_path, file_path, placeholder);
        return Err(OutputWriteError::TransactionFailed(detail));
    }

    // 改名**之前**失败都只做一件事：把这次自己造出来的东西收回去。用户的文件一个字没动，
    // 所以绝不 rollback —— 把快照写回去反而会把用户在此期间的新改动盖掉（§25）。
    let withdraw = |error: OutputWriteError| -> OutputWriteError {
        transactions.finish(&history_id);
        staged.discard();
        release_output_placeholder(&out_path, file_path, placeholder);
        error
    };

    // ④候选写满 → flush + fsync。到这一步结束，目标文件依然没被动过一个字节。
    if let Err(detail) = staged.write_synced(compressed) {
        return Err(withdraw(OutputWriteError::OutputWriteFailed(detail)));
    }

    // ⑤改名前的最后两道门（§26「最终 Commit 前必须确认当前 source == 本次压缩所基于的
    //   source」+ §31 的 backup check）。这里用**完整 verify**：快速门只 stat 不读内容，
    //   "同大小、同修改时间"的原地改动只有内容哈希看得出来 —— 宁可多读一遍源文件，
    //   也不能把用户刚改过的内容盖掉（与视频线同一标准）。备份则按 §27 验字节。
    if let Some(identity) = replace.as_ref() {
        if let SourceCheck::Changed(detail) = identity.verify(file_path) {
            return Err(withdraw(OutputWriteError::SourceChanged(detail)));
        }
        let backup = backup_file
            .as_ref()
            .expect("覆盖模式的快照必定已在 ① 之前备好");
        if let Err(detail) = history.backup_is_faithful(backup) {
            return Err(withdraw(OutputWriteError::BackupFailed(detail)));
        }
    }

    // ⑥原子改名。到这里用户的原图才真的被换掉。提交带占位凭证：目标路径上
    //   必须还是我们认领的那个 inode，外部放进去的内容绝不盖（§28 审查意见 A）。
    if let Err(detail) = staged.publish_claimed(&out_path, placeholder) {
        return Err(withdraw(OutputWriteError::Collision(detail)));
    }
    staged.settle();

    if cross_format {
        // 删不掉（权限/被占用）时不许静默：磁盘上会同时留着原图和新格式产物。
        // 产物已提交、历史已记，不能反过来撤销一次成功的压缩 —— 留警告给用户。
        if let Err(error) = fs::remove_file(file_path) {
            log::warn!("跨格式替换：原文件删除失败（产物已保留）: {error}");
            result
                .warnings
                .get_or_insert_with(Vec::new)
                .push(format!("原格式文件删除失败，已保留: {}", file_path.display()));
        }
    }
    // 输出方式是历史要记的**输入事实**，不是结论；产物路径要等 Publish 才写。
    result.output_mode = Some(options.output_mode.clone());

    // 输出确认落盘之后才记历史：历史里绝不出现没写成的文件。
    let entry = HistoryEntry::record_with_id(
        history_id.clone(),
        file_path,
        result,
        &out_path,
        backup_file.as_deref(),
        retention_days,
    );
    if let Err(error) = history.add(entry.clone()) {
        // ⑦失败 = 这次落盘不能算数。覆盖模式用备份把真正原图写回去；新建模式从没动过
        //    原图，它删掉自己生成的那个文件（§32「磁盘没有 orphan output」）。
        //    走到这里备份刚在 ⑤ 验过字节，所以"恢复得出原图"这句话是站得住的。
        log::warn!("写入压缩历史失败: {error}");
        match output_transaction::rollback(&txn) {
            Ok(()) => {
                transactions.finish(&txn.id);
                result.backup_path = None;
                return Err(OutputWriteError::HistoryWriteFailed);
            }
            // 回滚也没成：事务日志必须留着，下次启动的 recovery 再试一次。
            Err(rollback) => {
                return Err(OutputWriteError::RollbackFailed(format!(
                    "{error}；{rollback}"
                )));
            }
        }
    }
    // ⑧历史已经落盘 = 这次覆盖有据可查，销账。留着的话下次启动会误判成中断事务。
    transactions.finish(&history_id);
    // §77 结果随身带着这条历史：恢复全部按 historyId 对齐，不靠源路径猜。
    result.history_id = Some(history_id);
    Ok(CommitResult::Committed(CommitFacts { out_path }))
}

// ─── Batch compression core ─────────────────────────────────────
fn take_cancelled(cancel_queue: &Mutex<HashSet<String>>, file_path: &str) -> bool {
    cancel_queue.lock().unwrap().remove(file_path)
}

// 启动下一个文件前的闸门就是 `CompressionScheduler::acquire`：暂停与 CPU 上限在同一次
// CAS 里判断，拿到 permit 的那一刻闸门必然是开的。老实现要"等信号量 → 再查一次暂停"，
// 因为信号量不知道暂停这回事。

/// 跑一轮执行会话：只处理传进来的这些文件（前端的「继续压缩」只提交 pending）。
///
/// 一轮的流程（与前端 Session 一一对应）：
/// `SessionGuard::begin` → 逐个文件过闸门 → 拿到名额的开工 → 等所有真正在跑的收尾 →
/// 凭证 Drop（`end_batch` + 清取消账本）→ 回报本轮结果。
/// 被「停止」挡在闸门外的文件发 `deferred` 事件后就地结束，**不进 results**：
/// 它们没执行过，队列里仍然是 pending，下一轮自然带上。
///
/// `_session` 不读也不写，只作证明：出示不了已经占住的会话，就进不了这个批处理核心。
/// 会话的开跑播报、暂停/停止位与取消账本的清理都归凭证的 Drop，这里一处都不必再管。
pub(crate) async fn compress_batch(
    app: &AppHandle,
    state: &AppState,
    _session: &SessionGuard,
    session_id: String,
    queue_revision: u64,
    file_paths: Vec<String>,
    options: CompressOptions,
    use_smart: bool,
) -> CompressionSessionResult {
    // Keep cancellations that arrive between the frontend's start request and
    // this command's first poll. The queue is cleared after all workers exit,
    // so a completed batch cannot leak cancellation state into the next one.
    let all_files = collect_image_files(&file_paths);
    let session_total = all_files.len();

    // 沙盒版只对用户刚选中的路径持有授权：趁现在还能访问，把这批输入存成书签，
    // 否则重启后历史记录指向的位置再也读不到。
    state.access.remember_all(&all_files);
    let settings = state.settings_store.load();
    let retention_days = settings.original_retention_days;
    // 批次真的开跑了才播报 running：前端在拿到这条事件之前不许显示「压缩中…」。
    emit_compression_state(app, &state.compression);

    // Emit "queued" for all files
    for fp in &all_files {
        let _ = app.emit(
            "compress-progress",
            ProgressPayload {
                session_id: session_id.clone(),
                queue_revision,
                file: fp.clone(),
                status: ProgressStatus::Queued,
                timestamp: event_timestamp(),
                session_total,
                session_processed: 0,
                result: None,
            },
        );
    }

    // 并发度由 CPU 上限决定（不再是写死的 3）
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let options = Arc::new(options);
    let app_arc = Arc::new(app.clone());
    let cancel_queue = &state.cancel_queue;
    let control = state.compression.clone();
    let history = state.history_store.clone();
    let transactions = state.transactions.clone();
    let results_arc = Arc::new(Mutex::new(Vec::<CompressResult>::new()));
    // 本轮真正处理完的数量 + 明确取消的数量。`deferred` 绝不加它：
    // 没执行过的文件不算处理过，否则"停止后继续"的进度会凭空多出一截。
    let processed_arc = Arc::new(AtomicUsize::new(0));
    let deferred = Arc::new(AtomicUsize::new(0));
    let cancelled_count = Arc::new(AtomicUsize::new(0));
    let session_id_arc = Arc::new(session_id.clone());

    let mut handles: Vec<(tokio::task::JoinHandle<()>, String)> = Vec::new();
    for file_path in all_files {
        // §63 先量尺寸再排队：只读文件头，不把像素读进内存。
        // 一张 200 MP 的图不该和一张缩略图按同一个并发名额起跑。
        let memory = crate::image_memory::estimate(Path::new(&file_path)).bytes;
        // 闸门：暂停、名额用完或解码预算不够，都在这里等；拿到的瞬间三者都已满足。
        // 等待期间这个文件被取消就直接退出，**绝不因此解除暂停**。
        let outcome = control
            .acquire_with_memory(memory, || take_cancelled(cancel_queue, &file_path))
            .await;
        let permit = match outcome {
            AcquireOutcome::Acquired(permit) => permit,
            // 用户明确把这个文件移出队列：报一条 cancelled，它本轮到此为止。
            AcquireOutcome::Cancelled => {
                let done = processed_arc.fetch_add(1, Ordering::SeqCst) + 1;
                cancelled_count.fetch_add(1, Ordering::SeqCst);
                let _ = app_arc.emit(
                    "compress-progress",
                    ProgressPayload {
                        session_id: session_id_arc.to_string(),
                        queue_revision,
                        file: file_path.clone(),
                        status: ProgressStatus::Cancelled,
                        timestamp: event_timestamp(),
                        session_total,
                        session_processed: done,
                        // §35 §6.4 取消是一个明确的结果，不是一次故障。以前这条事件不带
                        // result，前端只能自己猜"它算什么"，于是取消长得像失败。
                        result: Some(cancelled_result(&file_path)),
                    },
                );
                continue;
            }
            // 整批被停止：这一轮没轮到它。**不生成任何 result、也不计入 session_processed**，
            // 只告诉前端"这一个是 deferred"——它在前端的队列里仍然是 pending。
            AcquireOutcome::Stopped => {
                deferred.fetch_add(1, Ordering::SeqCst);
                let done = processed_arc.load(Ordering::SeqCst);
                let _ = app_arc.emit(
                    "compress-progress",
                    ProgressPayload {
                        session_id: session_id_arc.to_string(),
                        queue_revision,
                        file: file_path.clone(),
                        status: ProgressStatus::Deferred,
                        timestamp: event_timestamp(),
                        session_total,
                        session_processed: done,
                        result: None,
                    },
                );
                continue;
            }
        };

        // Emit "starting" only after the worker is available.
        {
            let pr = processed_arc.load(Ordering::SeqCst);
            let _ = app_arc.emit(
                "compress-progress",
                ProgressPayload {
                    session_id: session_id_arc.to_string(),
                    queue_revision,
                    file: file_path.clone(),
                    status: ProgressStatus::Starting,
                    timestamp: event_timestamp(),
                    session_total,
                    session_processed: pr,
                    result: None,
                },
            );
        }

        let opts = options.clone();
        let app_c = app_arc.clone();
        let history_c = history.clone();
        let transactions_c = transactions.clone();
        let results_c = results_arc.clone();
        let processed_c = processed_arc.clone();
        let cancelled_c = cancelled_count.clone();
        let session_id_c = session_id_arc.clone();
        let fp = file_path.clone();

        let fp_for_track = fp.clone();
        handles.push((tokio::spawn(async move {
            let _permit = permit; // 持有信号量直到压缩完成

            // §34 会改写结论的取消检查点共用这一份判据：用 `take` 而不是 `contains`。
            // 一次取消只允许被认领一次，否则闸门、Encode 前、Commit 前会把同一次取消
            // 各记一遍账，队列里就多出几个"取消了两次"的文件。
            let cancel_requested = {
                let app_for_cancel = app_c.clone();
                let fp_for_cancel = fp.clone();
                move || take_cancelled(&app_for_cancel.state::<AppState>().cancel_queue, &fp_for_cancel)
            };

            // 预算阶梯的取消判据：只认「用户明确取消了这个文件」。整批停止**不在**这里：
            // 停止只拦截还没轮到的文件（闸门返回 Stopped → Deferred，留在队列等下一轮），
            // 已经开跑的这一次编码必须跑完 —— 中途按停止把运行项记成 Cancelled 会让
            // 前端把它从队列里彻底移除，「停止」就变成了「丢弃正在压缩的那个文件」。
            // 用 contains 而不是 take_cancelled：记账「这个文件不再参与本轮」只发生在
            // 会改变结果的地方（闸门、Encode 前、Commit 前）。
            let cancel_probe = {
                let app_for_probe = app_c.clone();
                let fp_for_probe = fp.clone();
                move || {
                    app_for_probe
                        .state::<AppState>()
                        .cancel_queue
                        .lock()
                        .unwrap()
                        .contains(&fp_for_probe)
                }
            };

            // §34 Encode 前：排队期间就被取消的文件，连编码都不该开始。
            // 记账与闸门那一支完全一致（处理数 +1、取消数 +1、发一条 Cancelled），
            // 它不进本轮 results —— 没产出任何事实的任务不该出现在总结里。
            if cancel_requested() {
                let done = processed_c.fetch_add(1, Ordering::SeqCst) + 1;
                cancelled_c.fetch_add(1, Ordering::SeqCst);
                let _ = app_c.emit(
                    "compress-progress",
                    ProgressPayload {
                        session_id: session_id_c.to_string(),
                        queue_revision,
                        file: fp.clone(),
                        status: ProgressStatus::Cancelled,
                        timestamp: event_timestamp(),
                        session_total,
                        session_processed: done,
                        result: Some(cancelled_result(&fp)),
                    },
                );
                return;
            }

            let path = PathBuf::from(&fp);
            // §24 §25 覆盖模式的原图快照必须在**编码之前**落好：编码要跑几十秒，
            // 期间用户完全可能又改了这张图。到 Commit 才备份，备份到的就是改完的那份，
            // 「恢复原图」从此恢复不出用户认得出的东西。非覆盖模式一次磁盘写都不做。
            let baseline = capture_replace_baseline(&path, &opts, &history_c);
            let started = Instant::now();
            // §18.2 显式换格式走转换语义：用户要的是容器，不适用"没变小就跳过"。
            // 压缩（含智能候选）仍然是收益语义；这两条不能靠一个 bool 在引擎里来回猜。
            let engine_result = if engine::is_format_conversion(&path, &opts) {
                engine::convert_to_format_with_cancel(
                    &path,
                    &opts.output_format,
                    &opts,
                    cancel_probe,
                )
                .await
            } else if use_smart {
                engine::compress_smart_with_cancel(&path, &opts, cancel_probe).await
            } else {
                engine::compress_image_with_cancel(&path, &opts, cancel_probe).await
            };
            let elapsed_ms = started.elapsed().as_millis() as u64;

            let mut result = build_result(&fp, &engine_result);
            result.elapsed_ms = Some(elapsed_ms);
            if let Err(error) = write_output_file(
                &mut result,
                &engine_result,
                &path,
                &opts,
                &history_c,
                &transactions_c,
                retention_days,
                &cancel_requested,
                baseline,
            ) {
                // 结论已经由 Publish 写进 result（失败、无产物、体积账按原图结），
                // 这里只留调用方该做的事：把这条失败记进日志。
                eprintln!("[ERROR] {fp} 落盘失败: {error:?}");
            }
            // §33 编码跑完、Commit 前被拦下的那一支：候选已经丢在内存里，磁盘没动过。
            // 它和闸门那一样都要计入取消，否则总结说"一个都没取消"而队列里全是「已取消」。
            if result.outcome == Some(ResultOutcome::Cancelled) {
                cancelled_c.fetch_add(1, Ordering::SeqCst);
            }
            // §141 发布前最后一道核对：结论和字段不自洽是后端的 bug，不是用户的显示问题。
            // 宁可把它降级成 Failed，也绝不让"UI 显示完成、磁盘上什么都没有"上线。
            if let Some(violation) =
                result_model::invariant_violation("image", &fp, result.outcome, result.output_path.as_deref())
            {
                eprintln!("[ERROR] {violation}");
                publish_violation(&mut result, &violation);
            }

            {
                let done = processed_c.fetch_add(1, Ordering::SeqCst) + 1;
                // 进度事件里的 status 只能说"这一个轮到结尾了、成还是败"，
                // 具体是完成/跳过/取消由 result.outcome 说 —— 前端不再自己猜（§8 §67）。
                let status = match result.outcome {
                    Some(ResultOutcome::Failed) => ProgressStatus::Failed,
                    Some(ResultOutcome::Cancelled) => ProgressStatus::Cancelled,
                    _ => ProgressStatus::Completed,
                };
                let _ = app_c.emit(
                    "compress-progress",
                    ProgressPayload {
                        session_id: session_id_c.to_string(),
                        queue_revision,
                        file: fp.clone(),
                        status,
                        timestamp: event_timestamp(),
                        session_total,
                        session_processed: done,
                        result: Some(result.clone()),
                    },
                );
            }

            let mut res = results_c.lock().await;
            res.push(result);
        }), fp_for_track));
    }

    // 等待所有任务完成
    for (handle, fp) in handles {
        if let Err(e) = handle.await {
            eprintln!("[ERROR] compression task panicked for {}: {:?}", fp, e);
            let done = processed_arc.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = app_arc.emit(
                "compress-progress",
                ProgressPayload {
                    session_id: session_id_arc.to_string(),
                    queue_revision,
                    file: fp.clone(),
                    status: ProgressStatus::Failed,
                    timestamp: event_timestamp(),
                    session_total,
                    session_processed: done,
                    result: Some(failed_result(fp)),
                },
            );
        }
    }

    let final_results = results_arc.lock().await.clone();
    // 本轮的账按 outcome 结，不再按 `success` 结：「压了但没收益」和「没达到目标体积」
    // 都是处理过并给出了结论的任务，而 `success` 把它们和真的产出混在了一起。
    let completed = final_results
        .iter()
        .filter(|r| {
            matches!(
                r.outcome,
                Some(ResultOutcome::Committed)
                    | Some(ResultOutcome::TargetUnmet)
                    | Some(ResultOutcome::SkippedNoBenefit)
                    | Some(ResultOutcome::SkippedAlreadyUnderTarget)
            )
        })
        .count();
    let failed = final_results
        .iter()
        .filter(|r| r.outcome == Some(ResultOutcome::Failed))
        .count();
    CompressionSessionResult {
        session_id,
        attempted: final_results.len(),
        completed,
        failed,
        deferred: deferred.load(Ordering::SeqCst),
        cancelled: cancelled_count.load(Ordering::SeqCst),
        results: final_results,
    }
}

fn failed_result(file: String) -> CompressResult {
    let mut result = empty_result(file);
    publish_conclusion(&mut result, ResultOutcome::Failed, Some(ReasonCode::Unknown), None);
    result.error = Some("压缩过程发生内部错误".into());
    result
}

/// §6.4 取消是一个明确结论：没有产物、没有历史，但可以重试。
/// 它和 `failed_result` 的区别就是「用户不要了」和「系统坏了」的区别。
fn cancelled_result(file: &str) -> CompressResult {
    let mut result = empty_result(file.to_string());
    // 走 Publish 而不是手写各字段：结论写两处就有两种可能不一样。
    // 取消按 §6.4 是 Done + Cancelled（取消不是系统故障），
    // 而它没有产物这一点由 output_path=None 与 has_product() 保证。
    publish_cancelled(&mut result, ReasonCode::Cancelled);
    result
}

/// 一条还没有任何事实的结果：任务在轮到这个文件之前就结束了（取消 / 内部错误）。
fn empty_result(file: String) -> CompressResult {
    CompressResult {
        file,
        original_size: 0,
        compressed_size: 0,
        savings: 0.0,
        original_size_formatted: "0 B".into(),
        compressed_size_formatted: "0 B".into(),
        out_type: "unknown".into(),
        algorithm: "none".into(),
        error: None,
        output_path: None,
        backup_path: None,
        output_mode: None,
        ..Default::default()
    }
}

// ─── Tauri commands ─────────────────────────────────────────────

#[tauri::command]
pub async fn select_files(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter(
            "Images and videos",
            &[
                "png", "jpg", "jpeg", "gif", "webp", "bmp", "avif", "jxl", "heic", "heif",
                "tif", "tiff", "mp4", "mov", "mkv", "webm", "m4v", "avi", "ts", "mts", "m2ts",
            ],
        )
        .pick_files(move |files| {
            let _ = tx.send(files);
        });
    let files = rx.await.map_err(|e| e.to_string())?.unwrap_or_default();
    let paths: Vec<String> = files
        .into_iter()
        .filter_map(|fp| fp.into_path().ok().map(|p| p.to_string_lossy().into_owned()))
        .collect();
    // 刚选中的路径带着系统授权，此时存书签才有效。
    state.access.remember_all(&paths);
    Ok(paths)
}

#[tauri::command]
pub async fn select_folder(app: AppHandle, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.await.map_err(|e| e.to_string())?;
    let paths: Vec<String> = folder
        .into_iter()
        .filter_map(|fp| fp.into_path().ok().map(|p| p.to_string_lossy().into_owned()))
        .collect();
    state.access.remember_all(&paths);
    Ok(paths)
}

#[tauri::command]
pub async fn select_output_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.await.map_err(|e| e.to_string())?;
    let paths: Vec<String> = folder
        .into_iter()
        .filter_map(|fp| fp.into_path().ok().map(|p| p.to_string_lossy().into_owned()))
        .collect();
    state.access.remember_all(&paths);
    Ok(paths)
}

#[tauri::command]
pub fn expand_image_files(file_paths: Vec<String>) -> Vec<String> {
    collect_image_files(&file_paths)
}

/// 一轮执行会话的入口。`session_id` 由前端生成并原样回传，所有进度事件都带着它 ——
/// 前端据此把"上一轮迟到的收尾事件"和"这一轮正在进行的事件"彻底分开。
#[tauri::command]
pub async fn compress_files(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    queue_revision: u64,
    file_paths: Vec<String>,
    options: CompressOptions,
) -> Result<CompressionSessionResult, String> {
    let _access = prepare_compression_access(&app, state.inner(), &file_paths, &options)?;
    let _session = SessionGuard::begin(&app, state.inner())?;
    Ok(compress_batch(&app, state.inner(), &_session, session_id, queue_revision, file_paths, options, false).await)
}

#[tauri::command]
pub async fn compress_smart(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    queue_revision: u64,
    file_paths: Vec<String>,
    options: CompressOptions,
) -> Result<CompressionSessionResult, String> {
    let _access = prepare_compression_access(&app, state.inner(), &file_paths, &options)?;
    let _session = SessionGuard::begin(&app, state.inner())?;
    Ok(compress_batch(&app, state.inner(), &_session, session_id, queue_revision, file_paths, options, true).await)
}

/// Keep directory scopes alive through encoding, staging, history commit and
/// rollback. Single-file grants do not cover atomic sibling writes in a sandbox.
pub(crate) fn prepare_compression_access(
    _app: &AppHandle,
    _state: &AppState,
    _file_paths: &[String],
    _options: &CompressOptions,
) -> Result<Vec<crate::sandbox_access::AccessGuard>, String> {
    #[allow(unused_mut)]
    let mut guards = Vec::new();
    #[cfg(all(target_os = "macos", feature = "inproc-backends"))]
    {
        let files = collect_image_files(_file_paths);
        let mut directories = std::collections::BTreeSet::new();
        if _options.output_mode == "folder" {
            let directory = _options.output_dir.as_ref().ok_or("请先选择输出目录")?;
            directories.insert(PathBuf::from(directory));
        } else {
            for file in &files {
                if let Some(parent) = Path::new(file).parent() {
                    directories.insert(parent.to_path_buf());
                }
            }
        }
        for directory in directories {
            let mut reauth = || request_folder_access(_app, _state, &directory);
            let guard = crate::sandbox_access::acquire_writable_directory(
                _state.access.as_ref(), &directory, &mut reauth,
            ).ok_or_else(|| format!("请授权输出文件夹后重试：{}", directory.display()))?;
            guards.push(guard);
        }
        for file in files {
            let path = Path::new(&file);
            let mut reauth = || request_folder_access(_app, _state, path);
            guards.push(_state.access.acquire(path, &mut reauth)
                .ok_or_else(|| format!("无法访问图片，请重新选择：{file}"))?);
        }
    }
    Ok(guards)
}

#[tauri::command]
pub async fn compress_single(
    file_path: String,
    options: CompressOptions,
) -> Result<CompressResult, String> {
    let path = PathBuf::from(&file_path);
    let started = Instant::now();
    // §10 Encode：引擎只交回候选字节，它不知道自己最后有没有成为产物。
    // §18.2：显式换格式是一条独立语义，不能再借"压缩"的判据。
    let engine_result = if engine::is_format_conversion(&path, &options) {
        engine::convert_to_format_with_cancel(&path, &options.output_format, &options, || false).await
    } else {
        engine::compress_image(&path, &options).await
    };
    let mut result = build_result(&file_path, &engine_result);
    result.elapsed_ms = Some(started.elapsed().as_millis() as u64);

    // §11 Evaluate：重压缩走的是同一处判据。以前这里 `if engine_result.success` 直接
    // 写文件并把 success 原样带出去，于是"压完更大"的一支也带着产物上路。
    let artifact = EncodedArtifact::from_engine(&engine_result);
    // §19 §20：对比窗口写出去的临时文件同样要过字节级格式验证。
    if let Some(detail) = artifact_format_error(&artifact, &path, &options) {
        publish_conclusion(
            &mut result,
            ResultOutcome::Failed,
            Some(ReasonCode::OutputValidationFailed),
            None,
        );
        result.error = Some(detail);
        return Ok(result);
    }
    let evaluation = evaluate_output(&result, &artifact, &path, &options);
    if !matches!(evaluation, Evaluation::Commit { .. }) {
        publish_precommit(&mut result, &evaluation);
        return Ok(result);
    }
    // §12 Commit：这一支只写对比窗口要展示的临时文件，不碰用户目录。
    let out_file = display_output_path(&artifact);
    match fs::write(&out_file, artifact.bytes) {
        Ok(()) => {
            // §13 Publish：文件真的写成了，才第一次允许说有产物。
            let (outcome, reason_code) = evaluation.committed();
            publish_conclusion(&mut result, outcome, reason_code, Some(out_file));
        }
        Err(error) => {
            publish_failure(&mut result, &OutputWriteError::OutputWriteFailed(error.to_string()));
        }
    }
    Ok(result)
}

/// 单图重压缩的展示文件位置。每次一个新名字，绝不覆盖上一次留下的那份。
fn display_output_path(artifact: &EncodedArtifact) -> PathBuf {
    let dir = std::env::temp_dir().join("sizqo-display");
    let _ = fs::create_dir_all(&dir);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.join(format!("recompress-{}.{}", ts, artifact.format))
}

/// 清理临时目录：`sizqo-display`（单图重压缩对比文件）、`sizqo-work`
/// （压缩中间产物）。原图备份在 AppData 下的 history/backups，按保留档位决定去留，
/// 这里绝不能再删；旧版本遗留在 TMP 的备份目录没有任何历史引用它，继续回收。
pub fn cleanup_temp_dirs() {
    for name in ["sizqo-display", "sizqo-work", "sizqo-backups"] {
        let _ = fs::remove_dir_all(std::env::temp_dir().join(name));
    }
}

/// 退出时按当前保留档位清一次历史记录与原图备份 —— 这是保留档位唯一的执行点。
///
/// 只在正常退出跑，启动时不清：崩溃或强杀现场的那份备份可能是唯一还活着的原图，
/// 删掉它就把"覆盖不可逆"变成"原图彻底没了"。中途改档位也在这一刻生效。
pub fn apply_retention_on_exit(app: &AppHandle) {
    let state = app.state::<AppState>();
    // 与 clear_history 同一道判据：批次还在跑或事务未收口时，备份目录可能刚建好
    // 还没写进 history（rename 之后、history.add 之前），sweep 会把它当孤儿删掉，
    // 而它可能正是那份原图唯一的副本。跳过本次清理，下次退出再收账；
    // 不留"已清账"记号，下次运行继续按"上次未清账"对待。
    if state.compression.is_batch_active() {
        log::warn!("退出时批次仍在运行，跳过保留档位清理，原图备份全部保留");
        return;
    }
    if state.transactions.has_pending() {
        log::warn!("退出时仍有文件事务未收口，跳过保留档位清理，原图备份全部保留");
        return;
    }
    let report = state.history_store.apply_retention_on_exit(
        state.settings_store.load().original_retention_days,
        state.run_started_at,
        state.previous_run_ended_cleanly,
    );
    if report.removed_entries > 0 || report.removed_backups > 0 {
        log::info!(
            "退出清理: 历史记录 -{} 条，原图备份 -{} 份，保留 {} 份",
            report.removed_entries,
            report.removed_backups,
            report.kept_backups
        );
    }
    for warning in &report.warnings {
        log::warn!("退出清理未完成: {warning}");
    }
    // 只有账真结了才留记号。历史不可信或写盘失败时跳过清理，那批备份就得继续留着，
    // 于是下一次退出仍然按"上次异常退出"处理，多给它一个会话的机会。
    if report.warnings.is_empty() {
        state.history_store.mark_clean_exit();
    }
}

#[tauri::command]
pub fn cancel_file(file_path: String, state: State<'_, AppState>) -> bool {
    state.cancel_queue.lock().unwrap().insert(file_path);
    // 只把正在等的 worker 叫醒，暂停态原样保留 —— 取消一张图不等于继续整批。
    state.compression.wake_waiters();
    true
}

/// 一次 IPC 取消整批：前端 Clear All 不再循环 N 次 `cancel_file`。
///
/// 逐个 invoke 会让"部分已取消、部分还在排队"的中间态暴露给调度器，
/// 几十个异步 IPC 之间只要有一次暂停/名额变化就会出现竞态。
#[tauri::command]
pub fn cancel_batch(file_paths: Vec<String>, state: State<'_, AppState>) -> usize {
    let unique: HashSet<String> = file_paths.into_iter().collect();
    let queued = unique.len();
    state.cancel_queue.lock().unwrap().extend(unique);
    // 一批取消只唤醒一次：与逐个 invoke 相比，中间态少得多。
    state.compression.wake_waiters();
    queued
}

#[tauri::command]
pub fn clear_cancel_queue(state: State<'_, AppState>) -> bool {
    state.cancel_queue.lock().unwrap().clear();
    state.compression.wake_waiters();
    true
}

#[tauri::command]
pub fn pause_compression(app: AppHandle, state: State<'_, AppState>) -> CompressionState {
    state.compression.pause();
    emit_compression_state(&app, &state.compression);
    state.compression.state()
}

#[tauri::command]
pub fn resume_compression(app: AppHandle, state: State<'_, AppState>) -> CompressionState {
    state.compression.resume();
    emit_compression_state(&app, &state.compression);
    state.compression.state()
}

/// 停止**这一轮**：还没轮到的文件不再启动，正在压的几个跑完，本轮收尾后回到 idle。
///
/// 停止**不是取消**：没轮到的文件各自收到一条 `deferred` 事件，队伍里仍然是 pending，
/// 用户点「继续压缩」时新一轮会把它们带上（见 `AcquireOutcome`）。
///
/// 不需要前端把路径传进来 —— 闸门自己认得 `stopping`，排在后面的文件
/// 一个个在 `acquire` 里自行退出。这也是"停止"比"逐个取消"更可靠的地方：
/// 不存在"部分已停、部分还在排队"的中间态。
#[tauri::command]
pub fn stop_compression(app: AppHandle, state: State<'_, AppState>) -> CompressionState {
    state.compression.stop();
    emit_compression_state(&app, &state.compression);
    state.compression.state()
}

#[tauri::command]
pub fn get_compression_state(state: State<'_, AppState>) -> CompressionState {
    state.compression.state()
}

#[tauri::command]
pub fn list_history(state: State<'_, AppState>) -> Vec<HistoryEntry> {
    state.history_store.list()
}

/// 清空历史 = 连原图备份一起删。这是全 App 里破坏性最大的操作，
/// 所以判据必须在**后端**，不能指望前端把按钮禁用住。
#[tauri::command]
pub fn clear_history(state: State<'_, AppState>) -> Result<usize, String> {
    // §2.7 维护租约：与会话启动在同一原子状态上仲裁，清历史的检查与执行之间
    // 不再留给会话一个开跑的窗口。
    let _lease = match state.compression.begin_maintenance() {
        Some(lease) => lease,
        None => return Err("压缩进行中，无法清空历史记录".into()),
    };
    // 批次可能刚好结束、事务还挂着（比如 worker 被取消后迟到的写入）：
    // 这时清历史会连正在跑那批的备份一起删掉，比 batch active 更严格的判据。
    if state.transactions.has_pending() {
        return Err("仍有文件事务正在处理，暂时无法清空历史记录".into());
    }
    let report = state.history_store.clear()?;
    for warning in report.warnings {
        log::warn!("清理历史失败: {warning}");
    }
    Ok(report.removed_entries)
}

#[tauri::command]
pub fn get_app_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings_store.load()
}

/// 只改持久值：清理只在正常退出时跑，所以这里绝不顺手删东西，生效时点就是下一次关闭应用。
#[tauri::command]
pub fn set_original_retention_days(
    state: State<'_, AppState>,
    days: u32,
) -> Result<AppSettings, String> {
    state.settings_store.set_retention_days(days)
}

/// 本机 CPU 能力 + 用户配置 + 当前真正生效值。
#[tauri::command]
pub fn get_cpu_info(state: State<'_, AppState>) -> CpuStatus {
    cpu_status(state.inner())
}

#[tauri::command]
pub fn get_cpu_resource_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings_store.load()
}

/// 运行中改上限：已在跑的任务继续跑完，之后的新任务按新值启动 —— 与暂停同一套语义。
#[tauri::command]
pub fn set_cpu_thread_limit(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<CpuStatus, String> {
    let settings = state.settings_store.set_cpu_thread_limit(limit)?;
    let effective =
        effective_cpu_limit(settings.cpu_thread_limit, state.cpu_info.budget_ceiling());
    state.compression.set_max_parallelism(effective);
    Ok(cpu_status(state.inner()))
}

fn cpu_status(state: &AppState) -> CpuStatus {
    let settings = state.settings_store.load();
    CpuStatus {
        info: state.cpu_info.clone(),
        configured_limit: settings.cpu_thread_limit,
        effective_limit: effective_cpu_limit(
            settings.cpu_thread_limit,
            state.cpu_info.budget_ceiling(),
        ),
    }
}

/// §52 保存前再次确认源文件真的还在：UI 手里的 Result 是过去的事实。
fn output_missing_error(path: &str) -> String {
    format!(
        "{}: 压缩文件不存在或已被移动：{path}",
        ReasonCode::OutputMissing.as_str()
    )
}

#[tauri::command]
pub async fn save_file(source_path: String, app: AppHandle) -> Result<Option<String>, String> {
    let p = PathBuf::from(&source_path);
    // §52 §53 对话框都不该为一个已经不存在的文件弹出来。
    if !p.is_file() {
        return Err(output_missing_error(&source_path));
    }
    let file_name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("compressed")
        .to_string();
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_string();

    let save_path = app
        .dialog()
        .file()
        .set_file_name(&file_name)
        .add_filter("Image", &[&ext])
        .blocking_save_file();

    let Some(dest) = save_path.and_then(|fp| fp.into_path().ok()) else {
        // 用户取消不是错误：没有保存这件事，前端也不该显示"已保存"。
        return Ok(None);
    };
    // §53 复制失败必须原样返回。以前 `let _ = fs::copy(...)` 之后无条件回路径，
    // 磁盘写满或目标只读时，前端照样弹「已保存到…」。
    // 另存为同样走暂存：先在同目录写满临时文件并 fsync，再原子改名覆盖用户选的位置；
    // 复制中途失败不会留下半个文件，也不会先把用户原有的目标文件删掉。
    StagedOutput::copy_from(Path::new(&source_path), &dest)
        .map(StagedOutput::settle)
        .map_err(|error| format!("保存失败: {error}"))?;
    Ok(Some(dest.to_string_lossy().into_owned()))
}

/// §52 前端在 Save / Open / Compare / Restore 之前用它复核磁盘事实。
#[tauri::command]
pub fn path_exists(file_path: String) -> bool {
    Path::new(&file_path).is_file()
}

#[tauri::command]
pub fn open_in_finder(file_path: String) -> bool {
    // §52 目标已经不在时就别假装打开了；前端会据 false 刷新状态。
    if !Path::new(&file_path).exists() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &file_path])
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let path = PathBuf::from(&file_path);
        let canonical = path.canonicalize().unwrap_or(path);
        if canonical.is_file() {
            let _ = std::process::Command::new("explorer.exe")
                .arg("/select,")
                .arg(&canonical)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        } else {
            let dir = if canonical.is_dir() {
                canonical
            } else {
                canonical
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from(&file_path))
            };
            let _ = std::process::Command::new("explorer.exe")
                .arg(dir)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(file_path)
            .spawn();
    }
    true
}

#[tauri::command]
pub fn read_image_dataurl(file_path: String, preview: Option<bool>) -> Option<String> {
    let path = PathBuf::from(&file_path);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();
    let preview_data = if preview.unwrap_or(false) {
        preview_image_bytes(&path, &ext)
    } else {
        None
    };
    let has_preview = preview_data.is_some();
    let data = preview_data.or_else(|| fs::read(&path).ok())?;
    let mime = if has_preview {
        if matches!(ext.as_str(), "jpg" | "jpeg") {
            "image/jpeg"
        } else {
            "image/png"
        }
    } else {
        match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "avif" => "image/avif",
            "jxl" => "image/jxl",
            _ => "image/png",
        }
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    Some(format!("data:{};base64,{}", mime, b64))
}

fn preview_image_bytes(path: &Path, ext: &str) -> Option<Vec<u8>> {
    if matches!(ext, "gif" | "avif" | "jxl") {
        return None;
    }

    const MAX_PREVIEW_DIMENSION: u32 = 2048;
    const MAX_RAW_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;

    let original_size = fs::metadata(path).ok()?.len();
    let img = image::open(path).ok()?;
    let needs_preview = original_size > MAX_RAW_PREVIEW_BYTES
        || img.width() > MAX_PREVIEW_DIMENSION
        || img.height() > MAX_PREVIEW_DIMENSION;

    if !needs_preview {
        return None;
    }

    let preview = img.thumbnail(MAX_PREVIEW_DIMENSION, MAX_PREVIEW_DIMENSION);
    let mut buf = Vec::new();

    if matches!(ext, "jpg" | "jpeg") {
        let rgb = preview.to_rgb8();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 84);
        use image::ImageEncoder;
        encoder
            .write_image(
                &rgb,
                preview.width(),
                preview.height(),
                image::ExtendedColorType::Rgb8,
            )
            .ok()?;
    } else {
        let rgba = preview.to_rgba8();
        let encoder = image::codecs::png::PngEncoder::new_with_quality(
            &mut buf,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        );
        use image::ImageEncoder;
        encoder
            .write_image(
                &rgba,
                preview.width(),
                preview.height(),
                image::ExtendedColorType::Rgba8,
            )
            .ok()?;
    }

    Some(buf)
}

#[tauri::command]
pub fn get_app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// §22：界面上的输出格式选项来自这条产物线的真实能力，不是前端第二份常量表。
#[tauri::command]
pub fn get_image_capabilities() -> crate::image_capabilities::ImageCapabilityMatrix {
    crate::image_capabilities::capabilities()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    success: bool,
    /// true = 压缩之后目标文件又被改过，需要用户确认后用 force = true 再来一次。
    conflict: bool,
    file_path: String,
    error: Option<String>,
    /// 一起被标记为「已恢复」的历史记录：同一张图重压多次会共享那份真正原图。
    history_ids: Vec<String>,
    /// 这条记录当时是哪种输出方式。后缀 / 目录模式下后端做的是"删掉这次的压缩产物"，
    /// 前端要据此措辞 —— 统一服务被三个入口共用，只有后端知道刚刚真正发生了什么。
    output_mode: String,
    /// §46 上次恢复已经把原图写回、只是记录没收敛：这次点按没有再次改动文件。
    already_restored: bool,
    /// §48 原图已恢复，但压缩产物清理失败。恢复成功与告警同时成立，不许只报一半。
    warning: Option<String>,
    /// §4 §79 前端与批量汇总只读这个字段，不再解析错误文案。
    reason_code: Option<ReasonCode>,
}

/// 恢复失败落到契约上的理由。`SessionBusy` 是闸门而不是故障，但它也必须
/// 有名字 —— 前端据此决定是提示稍后再试，还是展示真正的错误。
fn restore_error_code(error: &RestoreError) -> ReasonCode {
    match error {
        RestoreError::NotFound => ReasonCode::SourceNotFound,
        RestoreError::NotRestorable => ReasonCode::Unknown,
        RestoreError::BackupGone => ReasonCode::BackupFailed,
        RestoreError::Conflict => ReasonCode::SourceChanged,
        RestoreError::SessionBusy => ReasonCode::SessionBusy,
        RestoreError::Io(_) => ReasonCode::Unknown,
        RestoreError::AccessDenied => ReasonCode::PermissionDenied,
    }
}

impl RestoreOutcome {
    fn failure(entry: Option<&HistoryEntry>, error: RestoreError) -> Self {
        let detail = match &error {
            RestoreError::Io(inner) => format!("恢复失败: {inner}"),
            other => other.message().to_string(),
        };
        Self {
            success: false,
            conflict: matches!(error, RestoreError::Conflict),
            file_path: entry.map(|e| e.source_path.clone()).unwrap_or_default(),
            error: Some(detail),
            history_ids: Vec::new(),
            output_mode: entry.map(|e| e.output_mode.clone()).unwrap_or_default(),
            already_restored: false,
            warning: None,
            reason_code: Some(restore_error_code(&error)),
        }
    }
}

/// 沙盒下书签失效时，请用户重新授权目标文件夹；Direct 版这条路径不会走到。
///
/// ⚠️ 阻塞式弹窗**绝不能在主线程上调**：它要等主线程 runloop 响应，自己却把
/// 调用线程堵死 —— 主线程上调用 = 弹窗永远弹不出来、整个应用卡死。
/// 所以 `restore_original` / `restore_history_entry` / `restore_all` 三个命令
/// **必须是 async**（async 命令跑在 tokio 工作线程上），别"顺手"改回同步。
fn request_folder_access(app: &AppHandle, state: &AppState, target: &Path) -> bool {
    let Some(picked) = app
        .dialog()
        .file()
        .set_title(format!("请选择文件夹以允许读写：{}", target.display()))
        .set_directory(target.parent().unwrap_or(target))
        .blocking_pick_folder()
        .and_then(|fp| fp.into_path().ok())
    else {
        return false;
    };
    let target = target.canonicalize().unwrap_or_else(|_| target.to_path_buf());
    let picked = picked.canonicalize().unwrap_or(picked);
    let parent_ok = target.starts_with(&picked);
    state.access.remember(&picked);
    parent_ok
}

/// 压缩会话与维护操作（恢复/清理）共用同一个原子生命周期状态仲裁：
/// 0=空闲 1=批次 2=维护。"检查时空闲、拿到手前批次开跑"的窗口从结构上不存在，
/// 这是恢复与压缩之间真正的互斥（§2.7），不再是一次状态检查加祈祷。
const LIFECYCLE_IDLE: usize = 0;
const LIFECYCLE_BATCH: usize = 1;
const LIFECYCLE_MAINTENANCE: usize = 2;

/// 维护租约：持有期间压缩会话无法开始（同一原子状态仲裁），Drop 即释放。
pub struct MaintenanceLease {
    scheduler: std::sync::Arc<CompressionScheduler>,
}
impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        let _ = self.scheduler.lifecycle.compare_exchange(
            LIFECYCLE_MAINTENANCE,
            LIFECYCLE_IDLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// 统一恢复服务：主队列、历史页、恢复全部三个入口都只能走这里。
fn restore_entry(app: &AppHandle, state: &AppState, entry: &HistoryEntry, force: bool) -> RestoreOutcome {
    // §2.7 维护租约放在最前：与会话启动在同一原子状态上仲裁，恢复与压缩
    // 不可能各拿一半检查再各走各的。租约持有到本次恢复结束（RAII）。
    let _lease = match state.compression.begin_maintenance() {
        Some(lease) => lease,
        None => return RestoreOutcome::failure(Some(entry), RestoreError::SessionBusy),
    };
    restore_entry_leased(app, state, entry, force)
}

/// 已持有维护租约的恢复主体。`restore_all` 批量入口整批持一把租约时走这里。
fn restore_entry_leased(app: &AppHandle, state: &AppState, entry: &HistoryEntry, force: bool) -> RestoreOutcome {
    let mut reauth = || {
        request_folder_access(
            app,
            state,
            Path::new(&entry.source_path),
        )
    };
    match state
        .history_store
        .restore(entry, force, state.access.as_ref(), &mut reauth)
    {
        Ok(report) => RestoreOutcome {
            success: true,
            conflict: false,
            file_path: entry.source_path.clone(),
            error: None,
            history_ids: report.history_ids,
            output_mode: entry.output_mode.clone(),
            already_restored: report.already_restored,
            warning: report.warning,
            reason_code: None,
        },
        Err(error) => RestoreOutcome::failure(Some(entry), error),
    }
}

/// 按源路径取最近一条还没恢复的记录（主队列的行只知道自己压的是哪个文件）。
fn entry_for_source(state: &AppState, file_path: &str) -> Option<HistoryEntry> {
    let canonical = PathBuf::from(file_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(file_path));
    state
        .history_store
        .find_latest_for_source(&canonical.to_string_lossy())
}

#[tauri::command]
pub async fn restore_original(
    app: AppHandle,
    state: State<'_, AppState>,
    file_path: String,
    force: Option<bool>,
) -> Result<RestoreOutcome, String> {
    // Result 只是 Tauri 对「async 命令带引用参数」的硬性要求；恢复路径的所有失败
    // 都已经装在 RestoreOutcome.failure 里回给前端了，这里永远 Ok。
    let outcome = match entry_for_source(state.inner(), &file_path) {
        Some(entry) => restore_entry(&app, state.inner(), &entry, force.unwrap_or(false)),
        None => RestoreOutcome::failure(None, RestoreError::NotFound),
    };
    Ok(outcome)
}

#[tauri::command]
pub async fn restore_history_entry(
    app: AppHandle,
    state: State<'_, AppState>,
    history_id: String,
    force: Option<bool>,
) -> Result<RestoreOutcome, String> {
    let outcome = match state.history_store.find(&history_id) {
        Some(entry) => restore_entry(&app, state.inner(), &entry, force.unwrap_or(false)),
        None => RestoreOutcome::failure(None, RestoreError::NotFound),
    };
    Ok(outcome)
}

/// §49 批量操作只接受这一张形状。图片结果、视频结果并排放在一起，
/// **不再**把 `VideoResult` 强转成 `CompressResult` —— 那种转换一遇到必填字段
/// 不同就会整批反序列化失败（视频字段是 `sourcePath`，图片是 `file`）。
///
/// 前端可以把整条 queue result 原样传进来（未知字段被忽略），只有这些字段
/// 参与批量语义。`outcome` 是唯一的产物判据（§6.2 §77）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaOperationItem {
    #[serde(default)]
    pub media_kind: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub output_path: Option<String>,
    #[serde(default)]
    pub history_id: Option<String>,
    #[serde(default)]
    pub output_mode: Option<String>,
    #[serde(default)]
    pub outcome: Option<ResultOutcome>,
}

impl MediaOperationItem {
    fn source(&self) -> Option<&str> {
        self.source_path.as_deref().or(self.file.as_deref())
    }

    /// §77 只有结论允许带产物的项才进入恢复 / 导出；Skipped / Failed / Cancelled 一律不碰。
    fn carries_product(&self) -> bool {
        self.outcome.is_some_and(ResultOutcome::carries_output)
    }

    fn label(&self) -> String {
        self.source()
            .map(str::to_owned)
            .or_else(|| self.output_path.clone())
            .unwrap_or_default()
    }
}

/// §79 逐项结果：UI 能准确说出哪一个失败、为什么，而不是只拿到一个布尔。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BulkOperationItemResult {
    task_id: Option<String>,
    file: String,
    history_id: Option<String>,
    success: bool,
    reason_code: Option<ReasonCode>,
    message: Option<String>,
    output_path: Option<String>,
    already_restored: bool,
}

impl BulkOperationItemResult {
    fn succeeded(item: &MediaOperationItem, output_path: Option<String>) -> Self {
        Self {
            task_id: item.task_id.clone(),
            file: item.label(),
            history_id: item.history_id.clone(),
            success: true,
            reason_code: None,
            message: None,
            output_path,
            already_restored: false,
        }
    }

    fn failed(item: &MediaOperationItem, reason: ReasonCode, message: impl Into<String>) -> Self {
        Self {
            task_id: item.task_id.clone(),
            file: item.label(),
            history_id: item.history_id.clone(),
            success: false,
            reason_code: Some(reason),
            message: Some(message.into()),
            output_path: None,
            already_restored: false,
        }
    }
}

/// §78 §79 批量摘要：成功 / 失败 / 文件不存在分开计数，中间一个坏文件
/// 不会让整批任务失败；每一项都带着自己的结论。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkOperationResult {
    operation: &'static str,
    success: bool,
    success_count: usize,
    failed_count: usize,
    missing_count: usize,
    skipped_count: usize,
    warning_count: usize,
    items: Vec<BulkOperationItemResult>,
    /// 真正被恢复的源路径：前端据此逐行标记「已恢复」，不靠猜。
    restored_files: Vec<String>,
    message: String,
}

impl BulkOperationResult {
    fn new(operation: &'static str) -> Self {
        Self {
            operation,
            success: true,
            success_count: 0,
            failed_count: 0,
            missing_count: 0,
            skipped_count: 0,
            warning_count: 0,
            items: Vec::new(),
            restored_files: Vec::new(),
            message: String::new(),
        }
    }

    fn blocked(operation: &'static str, items: &[MediaOperationItem], message: String) -> Self {
        let mut result = Self::new(operation);
        result.success = false;
        result.failed_count = items.iter().filter(|item| item.carries_product()).count();
        result.skipped_count = items.len() - result.failed_count;
        result.message = message;
        result
    }

    fn finish(mut self) -> Self {
        self.success = self.failed_count == 0 && self.missing_count == 0;
        let missing_label = if self.operation == "export" {
            "文件不存在"
        } else {
            "记录不存在"
        };
        let mut message = format!(
            "{}完成：成功 {} 个",
            if self.operation == "export" { "导出" } else { "恢复" },
            self.success_count
        );
        if self.failed_count > 0 {
            message.push_str(&format!("，失败 {} 个", self.failed_count));
        }
        if self.missing_count > 0 {
            message.push_str(&format!("，{missing_label} {} 个", self.missing_count));
        }
        if self.warning_count > 0 {
            message.push_str(&format!(
                "，{} 个已恢复但压缩产物未能删除",
                self.warning_count
            ));
        }
        if self.skipped_count > 0 {
            message.push_str(&format!("，跳过 {} 个没有产物的项", self.skipped_count));
        }
        self.message = message;
        self
    }
}

/// 一键恢复全部原图。
///
/// §77 恢复的起点是「有产物 + 有 historyId」；只有历史 id 缺失的旧结果才退回
/// 按源路径找最近一条记录。Skipped / Failed / Cancelled 连循环都进不来。
#[tauri::command]
pub async fn restore_all(
    app: AppHandle,
    state: State<'_, AppState>,
    results: Vec<MediaOperationItem>,
) -> Result<BulkOperationResult, String> {
    // restore_entry 也会逐条拦，但批量入口要在第一次落盘前就说清整批为什么没动。
    // 整批持同一把维护租约：批内逐条走 restore_entry_leased，不再各自抢锁。
    let _lease = match state.compression.begin_maintenance() {
        Some(lease) => lease,
        None => {
            return Ok(BulkOperationResult::blocked(
                "restore",
                &results,
                RestoreError::SessionBusy.message().to_string(),
            ))
        }
    };
    let mut summary = BulkOperationResult::new("restore");

    for item in &results {
        if !item.carries_product() {
            summary.skipped_count += 1;
            continue;
        }
        let entry = item
            .history_id
            .as_deref()
            .and_then(|id| state.history_store.find(id))
            .or_else(|| item.source().and_then(|path| entry_for_source(state.inner(), path)));
        let Some(entry) = entry else {
            summary.missing_count += 1;
            summary.items.push(BulkOperationItemResult::failed(
                item,
                ReasonCode::SourceNotFound,
                "找不到对应的历史记录",
            ));
            continue;
        };
        let outcome = restore_entry_leased(&app, state.inner(), &entry, false);
        if outcome.success {
            summary.success_count += 1;
            summary.restored_files.push(outcome.file_path.clone());
            let mut row = BulkOperationItemResult::succeeded(item, Some(outcome.file_path));
            // 以历史里那条记录为准：即便前端传来的 id 缺失/过期，这次恢复也归属于它。
            row.history_id = Some(entry.id.clone());
            row.already_restored = outcome.already_restored;
            if let Some(warning) = outcome.warning {
                summary.warning_count += 1;
                row.message = Some(warning);
            }
            summary.items.push(row);
        } else {
            summary.failed_count += 1;
            let reason = outcome.reason_code.unwrap_or(ReasonCode::Unknown);
            let message = outcome
                .error
                .unwrap_or_else(|| RestoreError::NotFound.message().to_string());
            summary.items.push(BulkOperationItemResult::failed(item, reason, message));
        }
    }
    // Ok 包裹是 Tauri 对「async 命令带引用参数」的硬性要求；逐项失败已经
    // 计在摘要里回给前端了，这里永远 Ok。
    Ok(summary.finish())
}

#[tauri::command]
pub fn get_file_sizes(file_paths: Vec<String>) -> Vec<u64> {
    file_paths
        .iter()
        .map(|fp| engine::get_file_size(&PathBuf::from(fp)))
        .collect()
}

/// 打开（或聚焦）独立对比窗口，并把待渲染载荷暂存到 AppState。
///
/// 载荷形如 `{ results: [...成功结果对象...], index: <当前文件下标> }`，由前端组装；
/// compare 窗口页面加载完成后调用 take_compare_window_payload 取走，避免创建竞态。
/// 同步命令默认在主线程执行，可安全创建窗口。
///
/// 两条产物线行为一致，仅前端页面 URL 按 feature 分叉：
/// - Direct（cli-backends）：tauri:// 协议内嵌资源 compare.html
/// - App Store（inproc-backends）：沙盒阻止 tauri://，走本地 HTTP 服务器
///   （http://localhost:<port>/compare.html，端口见 lib.rs 固定端口段）
/// §50 §51 对比窗口只接受**真正存在于磁盘上的图片产物**。
///
/// 前端手里的结果是一段过去的事实：产物可能已经被 Finder 删掉，视频结果也可能
/// 混在同一张列表里。以前这两类都会原样送进 compare_window.js，于是弹出的
/// 要么是空白图，要么是一次注定失败的读取。过滤必须在后端做 —— 它是唯一
/// 能在打开窗口前摸到磁盘的地方。
fn sanitize_compare_payload(mut payload: serde_json::Value) -> Result<serde_json::Value, String> {
    let old_index = payload.get("index").and_then(|value| value.as_u64()).unwrap_or(0) as usize;
    let requested_file = payload
        .get("results")
        .and_then(|value| value.as_array())
        .and_then(|results| results.get(old_index))
        .and_then(|result| result.get("file"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let Some(results) = payload.get_mut("results").and_then(|value| value.as_array_mut()) else {
        return Err("对比载荷缺少结果列表".into());
    };
    results.retain(|result| {
        if result.get("mediaKind").and_then(|value| value.as_str()) == Some("video") {
            return false;
        }
        let outcome = result
            .get("outcome")
            .and_then(|value| serde_json::from_value::<ResultOutcome>(value.clone()).ok());
        if !outcome.is_some_and(ResultOutcome::carries_output) {
            return false;
        }
        let Some(output) = result.get("outputPath").and_then(|value| value.as_str()) else {
            return false;
        };
        if !Path::new(output).is_file() {
            return false;
        }
        // 另一侧也必须在：replace 看备份，后缀 / 目录看原图。否则比的是同一张图。
        result
            .get("backupPath")
            .and_then(|value| value.as_str())
            .or_else(|| result.get("file").and_then(|value| value.as_str()))
            .is_some_and(|original| Path::new(original).is_file())
    });
    if results.is_empty() {
        return Err("没有可对比的图片结果（产物或原图已不存在）".into());
    }
    // 过滤会改变下标：按「用户点中的那个文件」重新定位，而不是沿用旧 index。
    let new_index = requested_file
        .as_deref()
        .and_then(|file| {
            results
                .iter()
                .position(|result| result.get("file").and_then(|value| value.as_str()) == Some(file))
        })
        .unwrap_or(0);
    drop(results);
    payload["index"] = serde_json::json!(new_index);
    Ok(payload)
}

#[tauri::command]
pub fn open_compare_window(
    app: AppHandle,
    state: State<'_, AppState>,
    payload: serde_json::Value,
) -> Result<(), String> {
    let payload = sanitize_compare_payload(payload)?;
    *state.pending_compare.lock().unwrap() = Some(payload.clone());

    if let Some(window) = app.get_webview_window(COMPARE_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        let _ = app.emit_to(COMPARE_WINDOW_LABEL, "compare-open", payload);
        return Ok(());
    }

    let theme = crate::read_startup_theme().unwrap_or("light");
    let background = crate::startup_background(theme);

    #[cfg(feature = "inproc-backends")]
    let url = {
        let port = crate::frontend_http_port()
            .ok_or_else(|| "本地前端服务未启动，无法打开对比窗口".to_string())?;
        let parsed: tauri::Url = format!("http://localhost:{port}/compare.html")
            .parse()
            .map_err(|error| format!("对比窗口地址无效: {error}"))?;
        tauri::WebviewUrl::External(parsed)
    };
    #[cfg(not(feature = "inproc-backends"))]
    let url = tauri::WebviewUrl::App("compare.html".into());

    let builder = tauri::webview::WebviewWindowBuilder::new(&app, COMPARE_WINDOW_LABEL, url)
        .title("原图对比")
        .inner_size(1080.0, 780.0)
        .min_inner_size(560.0, 440.0)
        .resizable(true)
        .background_color(background);
    // 与主窗口同一模式：窗口始终可见，创建即带主题背景色（compare.html 首帧自设
    // 同步主题背景，不会露白）。❌ 不许用 visible(false) + on_page_load 延迟 show：
    // 那里的 webview.show() 经 wry 只作用于 WKWebView 视图，NSWindow 仍隐藏，
    // 沙盒版对比窗口将永远弹不出来（v2.5.44 实测 bug）。
    builder
        .build()
        .map_err(|error| format!("创建对比窗口失败: {error}"))?;
    Ok(())
}

/// compare 窗口页面就绪后取走暂存的对比载荷（取后即清）。
#[tauri::command]
pub fn take_compare_window_payload(state: State<'_, AppState>) -> Option<serde_json::Value> {
    state.pending_compare.lock().unwrap().take()
}

/// §49 §78 导出混合媒体：图片和视频都从 `MediaOperationItem` 读，不再强转。
/// 中间一个产物被删只让那一项记 `missing`，整批继续。
fn export_items(items: &[MediaOperationItem], output_suffix: Option<&str>) -> BulkOperationResult {
    let suffix = normalized_output_suffix(output_suffix);
    let mut summary = BulkOperationResult::new("export");
    for item in items {
        // §68 导出的是产物，不是"引擎自称成功过的那些项"。
        if !item.carries_product() {
            summary.skipped_count += 1;
            continue;
        }
        let Some(out_path) = item.output_path.as_deref().filter(|path| !path.is_empty()) else {
            summary.missing_count += 1;
            summary.items.push(BulkOperationItemResult::failed(
                item,
                ReasonCode::OutputMissing,
                "这条结果没有输出路径",
            ));
            continue;
        };
        // §52 §78 UI 手里的路径是过去的事实：复制之前重新看一次磁盘。
        let src = PathBuf::from(out_path);
        if !src.is_file() {
            summary.missing_count += 1;
            summary.items.push(BulkOperationItemResult::failed(
                item,
                ReasonCode::OutputMissing,
                "输出文件不存在，可能已被移动或删除",
            ));
            continue;
        }
        let Some(source_path) = item.source() else {
            summary.failed_count += 1;
            summary.items.push(BulkOperationItemResult::failed(
                item,
                ReasonCode::SourceNotFound,
                "找不到这条结果的源文件位置",
            ));
            continue;
        };
        let source = Path::new(source_path);
        let dir = output_directory(source);
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png")
            .to_string();
        let naming = OutputNaming::WithSuffix {
            stem: file_stem_of(source),
            suffix: suffix.clone(),
            extension: ext,
        };
        // 产物已经躺在导出的位置上（后缀模式 + 同一个后缀）：那就不必复制第二份。
        if crate::history::same_file(&dir.join(naming.candidate(0)), &src) {
            summary.success_count += 1;
            summary
                .items
                .push(BulkOperationItemResult::succeeded(item, Some(out_path.into())));
            continue;
        }
        // §54 导出同样只走这个分配器：上一次导出的 `foo_compressed.jpg` 已经在目录里，
        // 这次就取 `foo_compressed_1.jpg`，绝不把上一次的成果盖掉。
        let claimed = match OutputNameAllocator::claim(&dir, &naming) {
            Ok(claimed) => claimed,
            Err(error) => {
                summary.failed_count += 1;
                summary.items.push(BulkOperationItemResult::failed(
                    item,
                    ReasonCode::OutputCollision,
                    error,
                ));
                continue;
            }
        };
        let dest = claimed.path().to_path_buf();
        match StagedOutput::copy_from(&src, &dest) {
            Ok(staged) => {
                staged.settle();
                summary.success_count += 1;
                summary.items.push(BulkOperationItemResult::succeeded(
                    item,
                    Some(dest.to_string_lossy().into_owned()),
                ));
            }
            Err(error) => {
                // 复制没成：占住的空名字收回去，别在用户目录里留一个 0 字节的"导出成果"。
                claimed.release();
                summary.failed_count += 1;
                summary.items.push(BulkOperationItemResult::failed(
                    item,
                    ReasonCode::OutputWriteFailed,
                    error,
                ));
            }
        }
    }
    summary.finish()
}

/// Export all compressed results to their original directories with the selected suffix.
#[tauri::command]
pub fn export_all(results: Vec<MediaOperationItem>, output_suffix: Option<String>) -> BulkOperationResult {
    export_items(&results, output_suffix.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        for _ in 0..250 {
            if condition() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        false
    }

    /// `AcquireOutcome` 不是 PartialEq（permit 没法比），断言只看它落在哪一支。
    fn outcome_name(outcome: AcquireOutcome) -> &'static str {
        match outcome {
            AcquireOutcome::Acquired(_) => "acquired",
            AcquireOutcome::Cancelled => "cancelled",
            AcquireOutcome::Stopped => "stopped",
        }
    }

    /// §2.7 恢复走维护租约：会话在跑（运行/暂停/停止）拿不到租约；空闲时拿到，
    /// 且租约在手期间会话也开不起来 —— 两个方向都在同一个原子状态上仲裁。
    #[test]
    fn restore_is_refused_exactly_while_the_compression_session_is_active() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(2));
        let lease = scheduler.begin_maintenance();
        assert!(lease.is_some(), "空闲时必须允许恢复");
        assert!(!scheduler.try_begin_batch(), "维护租约在场，会话必须开不起来");
        drop(lease);
        assert!(scheduler.try_begin_batch(), "租约释放后会话照常开始");
        scheduler.pause();
        assert!(scheduler.begin_maintenance().is_none(), "暂停仍属于会话，恢复继续被拒");
        scheduler.resume();
        scheduler.stop();
        assert!(scheduler.begin_maintenance().is_none(), "停止中仍属于会话，恢复继续被拒");
        scheduler.end_batch();
        assert!(scheduler.begin_maintenance().is_some(), "会话结束后恢复必须放行");
    }

    /// 同一句拒绝原话在 Rust / 前端 / Swift 各有一份实现；任何一份被改写，
    /// 用户都会看到两种解释。这里把 Rust 与前端钉在一起。
    #[test]
    fn the_restore_refusal_uses_the_same_sentence_as_the_ui() {
        assert_eq!(RestoreError::SessionBusy.message(), crate::history::RESTORE_BLOCKED_MESSAGE);
        assert!(include_str!("../../frontend/app.js").contains(crate::history::RESTORE_BLOCKED_MESSAGE));
    }

    /// 上限就是上限：limit=3 时任何时刻最多 3 份在跑；暂停只拦新任务，不 interrupt 已开跑的。
    ///
    /// 用 work_gate（0 号牌的信号量）当"任务什么时候算跑完"的手动开关，避免靠 sleep 猜时序。
    #[tokio::test]
    async fn cpu_limit_caps_running_jobs_and_pause_blocks_only_new_ones() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(3));
        let work_gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let started = std::sync::Arc::new(AtomicUsize::new(0));
        let finished = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        assert!(scheduler.try_begin_batch());

        let driver = {
            let scheduler = scheduler.clone();
            let work_gate = work_gate.clone();
            let started = started.clone();
            let finished = finished.clone();
            let peak = peak.clone();
            tokio::spawn(async move {
                let mut running = Vec::new();
                for _ in 0..10 {
                    // 闸门：暂停或名额满了就等在这里。
                    let permit = scheduler.acquire_permit().await;
                    let running_now = started.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(running_now, Ordering::SeqCst);
                    let work_gate = work_gate.clone();
                    let started = started.clone();
                    let finished = finished.clone();
                    running.push(tokio::spawn(async move {
                        // 模拟已经在跑的子进程：只能等它自己结束，不能被暂停打断。
                        let running_permit = work_gate.acquire_owned().await.unwrap();
                        finished.fetch_add(1, Ordering::SeqCst);
                        started.fetch_sub(1, Ordering::SeqCst);
                        // forget 让一张放行牌只放行一个任务，否则信号量会被反复回收。
                        running_permit.forget();
                        drop(permit);
                    }));
                }
                for task in running {
                    task.await.unwrap();
                }
            })
        };

        // 并发数就是上限：正好 3 个开始，不多不少。
        assert!(wait_until(|| started.load(Ordering::SeqCst) == 3).await);
        scheduler.pause();
        assert!(scheduler.is_paused());
        assert_eq!(scheduler.state(), CompressionState::Paused);

        // 放掉一个在跑的任务，腾出名额 —— 暂停时不能拿它补位。
        work_gate.add_permits(1);
        assert!(wait_until(|| finished.load(Ordering::SeqCst) == 1).await);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(started.load(Ordering::SeqCst), 2, "暂停后仍在启动新任务");

        scheduler.resume();
        assert_eq!(scheduler.state(), CompressionState::Running);
        work_gate.add_permits(20);
        driver.await.unwrap();
        assert_eq!(finished.load(Ordering::SeqCst), 10, "继续后必须把整批跑完");
        assert_eq!(peak.load(Ordering::SeqCst), 3, "任何时刻都不该超过 3 份并行");
        scheduler.end_batch();
        assert_eq!(scheduler.state(), CompressionState::Idle);
        assert_eq!(scheduler.active(), 0);
    }

    /// §37 一轮收尾后，下一轮绝不继承上一轮遗留的暂停 / 停止位。
    #[tokio::test]
    async fn a_new_batch_never_inherits_the_previous_pause() {
        let scheduler = CompressionScheduler::new(3);
        scheduler.pause();
        scheduler.stop();
        assert_eq!(scheduler.state(), CompressionState::Idle, "没有会话占着闸门时就该是 idle");
        assert!(scheduler.try_begin_batch());
        assert_eq!(scheduler.state(), CompressionState::Running, "新一轮带着上一轮的暂停位开跑");
        scheduler.end_batch();
        assert_eq!(scheduler.state(), CompressionState::Idle);
    }

    /// §37 §38 同一时刻只允许一个 compression session。
    ///
    /// 两条都得钉住：① 第二个入口抢不到会话（图片、视频、混合共用同一个闸门）；
    /// ② 抢输的那一方**一个原子位都不许改** —— 否则它会把正在跑那一轮的暂停悄悄
    /// 解除，用户看到的就是"我按了暂停，它怎么又压起来了"。
    #[tokio::test]
    async fn a_second_session_cannot_claim_a_running_one() {
        let scheduler = CompressionScheduler::new(3);
        assert!(scheduler.try_begin_batch());
        assert!(!scheduler.try_begin_batch(), "已经有一轮在跑时，第二个入口必须被拒");
        assert_eq!(scheduler.state(), CompressionState::Running, "被拒的入口不许改动正在跑的会话");

        scheduler.pause();
        assert!(!scheduler.try_begin_batch());
        assert!(scheduler.is_paused(), "被拒的入口把正在跑的会话悄悄解除暂停");
        assert_eq!(scheduler.state(), CompressionState::Paused);

        scheduler.resume();
        scheduler.end_batch();
        assert!(scheduler.try_begin_batch(), "上一轮收尾后，下一轮才准占住会话");
    }

    /// 「停止」= 闸门对还没开始的文件永久关闭，已经在跑的继续跑完（绝不 kill）。
    ///
    /// 逐条钉住四件事：状态是 stopping、等待者立刻退出、已在跑的照常完成、
    /// 停止后即使有人调 `resume()` 也不会把排队的文件放回来。
    #[tokio::test]
    async fn stopping_the_batch_lets_running_jobs_finish_and_closes_the_gate_for_good() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(2));
        assert!(scheduler.try_begin_batch());
        // 先占满两个名额 —— 这两个就是"正在压缩的图片"。
        let first = scheduler.acquire_permit().await;
        let second = scheduler.acquire_permit().await;
        assert_eq!(scheduler.active(), 2);

        let waiter = {
            let scheduler = scheduler.clone();
            tokio::spawn(async move { outcome_name(scheduler.acquire(|| false).await) })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "名额满了，第三个文件就该在闸门上等");

        scheduler.stop();
        assert_eq!(scheduler.state(), CompressionState::Stopping);
        assert_eq!(
            waiter.await.unwrap(),
            "stopped",
            "停止后等待中的文件必须自己退出，不能等到名额空出来再开工"
        );
        assert_eq!(scheduler.active(), 2, "已经在跑的两个不许被抢走名额");
        // 必须报 Stopped 而不是 Cancelled：这个文件该留在队列里等下一轮。
        assert_eq!(
            outcome_name(scheduler.acquire(|| false).await),
            "stopped",
            "停止后闸门对后来者是关的"
        );

        // 关键：停止不是暂停。即使有人误调 resume()，批次的闸门也不该重新打开。
        scheduler.resume();
        assert_eq!(
            outcome_name(scheduler.acquire(|| false).await),
            "stopped",
            "停止不可逆：resume() 不许把停止的批次放回来"
        );
        assert_eq!(scheduler.state(), CompressionState::Stopping);

        // 在跑的两个收尾（permit 归还）之后，批次照样收尾回 idle。
        drop(first);
        drop(second);
        scheduler.end_batch();
        assert_eq!(scheduler.state(), CompressionState::Idle);
    }

    /// 暂停中按停止：等待者必须能看见"整批已停"并退出，而不是继续堵在关着的闸门上。
    #[tokio::test]
    async fn stopping_while_paused_finishes_the_batch_instead_of_waiting_for_resume() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(1));
        assert!(scheduler.try_begin_batch());
        let running = scheduler.acquire_permit().await;
        scheduler.pause();
        assert_eq!(scheduler.state(), CompressionState::Paused);

        let waiter = {
            let scheduler = scheduler.clone();
            tokio::spawn(async move { outcome_name(scheduler.acquire(|| false).await) })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "暂停时等待者必须被拦住");

        scheduler.stop();
        assert_eq!(scheduler.state(), CompressionState::Stopping);
        assert_eq!(
            waiter.await.unwrap(),
            "stopped",
            "暂停中停止：等待者必须退出（判断顺序是 取消 → 停止 → 暂停，不能反过来）"
        );
        assert_eq!(scheduler.active(), 1, "在跑的那个照旧跑完");
        drop(running);
        scheduler.end_batch();
        assert_eq!(scheduler.state(), CompressionState::Idle);
    }

    /// 停止的标志不许泄漏到下一批：批次起止都要把它清干净。
    #[test]
    fn stopping_does_not_leak_into_the_next_batch() {
        let scheduler = CompressionScheduler::new(2);
        assert!(scheduler.try_begin_batch());
        scheduler.stop();
        assert_eq!(scheduler.state(), CompressionState::Stopping);
        scheduler.end_batch();
        assert_eq!(scheduler.state(), CompressionState::Idle);
        assert!(scheduler.try_begin_batch());
        assert_eq!(scheduler.state(), CompressionState::Running);
        assert!(!scheduler.is_stopping(), "新一批必须从「未停止」开始");
    }

    /// 「停止」与「取消」必须给出两种结局：前者是"这一轮没轮到"，后者是"这个文件出局"。
    ///
    /// 混成一个 `None` 时，`compress_batch` 只能统一报 cancelled，于是"停止后 70 张图
    /// 全变成已跳过、再也没法继续"——那是把批次级的动作记成了文件级的终态。
    #[tokio::test]
    async fn stopping_defers_a_waiting_file_while_removing_it_cancels_it() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(1));
        let queue = std::sync::Arc::new(Mutex::new(HashSet::new()));
        assert!(scheduler.try_begin_batch());
        let held = scheduler.acquire_permit().await;

        // ① 停止：等待者拿到的必须是 Stopped（它该留在队列里等下一轮）。
        let deferred = {
            let scheduler = scheduler.clone();
            tokio::spawn(async move { outcome_name(scheduler.acquire(|| false).await) })
        };
        tokio::task::yield_now().await;
        assert!(!deferred.is_finished(), "名额占满时它就该在闸门上等");
        scheduler.stop();
        assert_eq!(
            deferred.await.unwrap(),
            "stopped",
            "停止只结束这一轮，文件不算被取消"
        );

        // ② 取消：新一批里把文件移出队列，拿到的必须是 Cancelled。
        scheduler.end_batch();
        assert!(scheduler.try_begin_batch());
        queue.lock().unwrap().insert("/gone.png".to_string());
        assert_eq!(
            outcome_name(
                scheduler
                    .acquire(|| take_cancelled(&queue, "/gone.png"))
                    .await
            ),
            "cancelled",
            "用户移出队列的文件必须报 Cancelled，而不是 Stopped"
        );

        // ③ 两者同时成立时「取消」优先：用户点过 × 的文件本来就不该再压。
        queue.lock().unwrap().insert("/both.png".to_string());
        scheduler.stop();
        assert_eq!(
            outcome_name(
                scheduler
                    .acquire(|| take_cancelled(&queue, "/both.png"))
                    .await
            ),
            "cancelled",
            "判断顺序是 取消 → 停止"
        );
        drop(held);
        scheduler.end_batch();
    }

    /// 进度状态的字面量就是前端的判据（app.js 的 progressHandler）：改一个字母
    /// 前端就会把「跑完了」当成未知状态，界面停在转圈上。
    #[test]
    fn the_progress_status_wire_format_is_what_the_frontend_matches() {
        for (status, wire) in [
            (ProgressStatus::Queued, "\"queued\""),
            (ProgressStatus::Starting, "\"starting\""),
            (ProgressStatus::Completed, "\"completed\""),
            (ProgressStatus::Failed, "\"failed\""),
            (ProgressStatus::Cancelled, "\"cancelled\""),
            (ProgressStatus::Deferred, "\"deferred\""),
        ] {
            assert_eq!(serde_json::to_string(&status).unwrap(), wire);
        }
    }

    #[test]
    fn progress_events_echo_the_queue_generation() {
        let payload = ProgressPayload {
            session_id: "session-7".into(),
            queue_revision: 42,
            file: "/image.png".into(),
            status: ProgressStatus::Deferred,
            timestamp: event_timestamp(),
            session_total: 1,
            session_processed: 0,
            result: None,
        };
        let wire = serde_json::to_value(payload).unwrap();
        assert_eq!(wire["sessionId"], "session-7");
        assert_eq!(wire["queueRevision"], 42);
        assert_eq!(wire["status"], "deferred");
        assert!(wire["timestamp"].as_u64().unwrap() > 0);
    }

    /// 前端认的是四个字面量（app.js 的 COMPRESSION_STATES 与事件载荷）：序列化结果必须
    /// 逐字一致 —— 改变体名就得同时改前端，否则 UI 会静默不认后端播报的状态。
    #[test]
    fn the_wire_format_is_the_four_state_strings_the_frontend_knows() {
        for (state, wire) in [
            (CompressionState::Idle, "\"idle\""),
            (CompressionState::Running, "\"running\""),
            (CompressionState::Paused, "\"paused\""),
            (CompressionState::Stopping, "\"stopping\""),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), wire);
        }
    }

    /// 暂停中取消一个还没开始的文件：它自己退出等待，但闸门**保持关闭**。
    ///
    /// 老实现在取消路径上调的是 `resume()`，于是"暂停中从队列里移走一张图"
    /// 会让其余排队的文件全部悄悄开跑，而 UI 上仍写着「暂停中…」。
    #[tokio::test]
    async fn cancelling_a_paused_file_leaves_the_rest_still_paused() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(2));
        let queue = std::sync::Arc::new(Mutex::new(HashSet::new()));
        queue.lock().unwrap().insert("/gone.png".to_string());
        assert!(scheduler.try_begin_batch());
        scheduler.pause();

        let leaving = {
            let scheduler = scheduler.clone();
            let queue = queue.clone();
            tokio::spawn(async move {
                outcome_name(
                    scheduler
                        .acquire(|| take_cancelled(&queue, "/gone.png"))
                        .await,
                )
            })
        };
        let staying = {
            let scheduler = scheduler.clone();
            tokio::spawn(async move { scheduler.acquire_permit().await })
        };

        // 必须是 Cancelled 而不是 Stopped：用户移出队列的文件本轮到此为止。
        let exited = matches!(
            tokio::time::timeout(Duration::from_millis(500), leaving).await,
            Ok(Ok(name)) if name == "cancelled"
        );
        assert!(
            exited,
            "被取消的文件必须立刻退出等待，而不是拿着名额去压缩"
        );
        assert!(scheduler.is_paused(), "取消绝不能顺手解除暂停");
        assert_eq!(scheduler.active(), 0, "退出等待必须归还预算");
        assert!(!staying.is_finished(), "还在排队的文件必须继续被闸门拦住");

        // 唤醒只叫醒不开门；真正开门的仍然只有用户点「继续」和批次收尾。
        scheduler.wake_waiters();
        tokio::task::yield_now().await;
        assert!(!staying.is_finished(), "wake_waiters 只叫醒等待者，不放行");
        assert!(scheduler.is_paused());

        scheduler.resume();
        let permit = staying.await.unwrap();
        assert_eq!(scheduler.active(), 1);
        drop(permit);
    }

    /// 暂停中清空队列 / 取消全部 / 退出：批次收尾必须唤醒还堵在闸门上的 worker。
    #[tokio::test]
    async fn ending_a_batch_wakes_workers_still_waiting_on_the_gate() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(1));
        assert!(scheduler.try_begin_batch());
        // 先占满唯一的名额，下一个任务就会堵在闸门上。
        let held = scheduler.acquire_permit().await;
        scheduler.pause();

        let waiter = {
            let scheduler = scheduler.clone();
            tokio::spawn(async move {
                let permit = scheduler.acquire_permit().await;
                drop(permit);
                true
            })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "暂停时闸门必须拦住新任务");

        scheduler.end_batch();
        drop(held);
        assert!(waiter.await.unwrap(), "收尾后闸门必须放行");
    }

    /// 名额空出来了，但闸门是暂停着的：等待者必须继续等，不许顶着「已暂停」开工。
    #[tokio::test]
    async fn a_paused_gate_stays_shut_even_when_capacity_frees_up() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(1));
        assert!(scheduler.try_begin_batch());
        let held = scheduler.acquire_permit().await;
        let waiter = {
            let scheduler = scheduler.clone();
            tokio::spawn(async move {
                let permit = scheduler.acquire_permit().await;
                drop(permit);
                true
            })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "名额被占着，等待者必须等");

        // 名额释放的同一拍按了暂停：空出来的名额也不许放行。
        scheduler.pause();
        drop(held);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "暂停期间就算有空名额也不得开工");

        scheduler.resume();
        assert!(waiter.await.unwrap(), "resume 之后等待者必须被放行");
    }

    /// §3.4 的硬保证：pause() 一返回，就不再有任何新 permit 出场。
    /// 多线程运行时下真的去抢那个旧窗口（锁外读 paused → 锁内放行）；
    /// 判据用 usage 锁内的发放计数，锁外记账会把"pause 前发放、pause 后
    /// 才被 hammer 记到"误判成违规。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn no_new_permit_outlives_a_returned_pause() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::with_memory_budget(
            2,
            usize::MAX / 4,
        ));
        assert!(scheduler.try_begin_batch());
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let mut hammers = Vec::new();
        for _ in 0..4 {
            let scheduler = scheduler.clone();
            let stop = stop.clone();
            hammers.push(tokio::spawn(async move {
                while !stop.load(Ordering::Acquire) {
                    if let Some(permit) = scheduler.try_acquire() {
                        drop(permit);
                    }
                    tokio::task::yield_now().await;
                }
            }));
        }
        for _ in 0..40 {
            scheduler.pause();
            let after = scheduler.test_grants.load(Ordering::Acquire);
            tokio::time::sleep(Duration::from_millis(2)).await;
            assert_eq!(
                scheduler.test_grants.load(Ordering::Acquire),
                after,
                "pause() 返回后不得再放行任何新 permit"
            );
            scheduler.resume();
            tokio::task::yield_now().await;
        }
        stop.store(true, Ordering::Release);
        for hammer in hammers {
            let _ = hammer.await;
        }
        assert_eq!(scheduler.active(), 0, "收尾后账本必须归零");
    }

    /// 8 → 2：已在跑的 8 份必须被放过，只是不再启动新的（绝不"强杀 6 个"）。
    #[tokio::test]
    async fn lowering_the_limit_never_recovers_permits_from_running_jobs() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(8));
        assert!(scheduler.try_begin_batch());
        let mut permits = Vec::new();
        for _ in 0..8 {
            permits.push(scheduler.acquire_permit().await);
        }
        assert_eq!(scheduler.active(), 8);

        scheduler.set_max_parallelism(2);
        assert_eq!(scheduler.max_parallelism(), 2);
        assert_eq!(scheduler.active(), 8, "降上限不能把已在跑的名额抢回来");
        assert!(scheduler.try_acquire().is_none(), "超额期间不能再启动新任务");

        // 交还到只剩 2 份之前，闸门一直是关的；到了 2 份之后仍然不能再多。
        for permit in permits.drain(..6) {
            drop(permit);
            assert!(scheduler.try_acquire().is_none(), "active 仍 >= 新上限时不能放行");
        }
        assert_eq!(scheduler.active(), 2);
        drop(permits.pop().unwrap());
        assert!(scheduler.try_acquire().is_some(), "回落到上限以下后必须能继续开工");
    }

    /// 2 → 6：提高上限要立刻唤醒等待者，而不是等下一次超时轮询。
    #[tokio::test]
    async fn raising_the_limit_wakes_waiting_workers_immediately() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(2));
        assert!(scheduler.try_begin_batch());
        let held = vec![scheduler.acquire_permit().await, scheduler.acquire_permit().await];

        let waiters: Vec<_> = (0..4)
            .map(|_| {
                let scheduler = scheduler.clone();
                tokio::spawn(async move {
                    let permit = scheduler.acquire_permit().await;
                    drop(permit);
                })
            })
            .collect();
        tokio::task::yield_now().await;
        assert_eq!(scheduler.active(), 2, "2 个名额时不该有第 3 个开工");

        scheduler.set_max_parallelism(6);
        for waiter in waiters {
            waiter.await.unwrap();
        }
        assert_eq!(scheduler.active(), 2, "等待者放行后又要归位");
        drop(held);
        assert_eq!(scheduler.active(), 0);
    }

    #[tokio::test]
    async fn one_permit_serialises_the_whole_batch() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::new(1));
        assert!(scheduler.try_begin_batch());
        let held = scheduler.acquire_permit().await;
        assert!(scheduler.try_acquire().is_none(), "limit=1 时任何时刻只能有一个任务");
        drop(held);
        assert!(scheduler.try_acquire().is_some());
    }

    /// §63 预算先于 CPU 上限生效：4 个名额还在，但两张 400 MB 的大图已经吃满 1000 MB。
    #[tokio::test]
    async fn the_memory_budget_tightens_concurrency_before_the_cpu_limit_does() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::with_memory_budget(4, 1000));
        assert_eq!(scheduler.memory_budget(), 1000);
        assert!(scheduler.try_begin_batch());
        let AcquireOutcome::Acquired(first) = scheduler.acquire_with_memory(400, || false).await else {
            panic!("空场时第一张大图必须能开工");
        };
        let second = scheduler
            .try_acquire_with_memory(400)
            .expect("800/1000 仍在预算内，第二张该放行");
        assert!(
            scheduler.try_acquire_with_memory(400).is_none(),
            "第三张会让预算变成 1200/1000，必须等"
        );
        assert_eq!(scheduler.active(), 2, "CPU 还有名额，拦住它的是内存预算");
        assert_eq!(scheduler.memory_in_use(), 800);

        drop(second);
        let third = scheduler
            .try_acquire_with_memory(400)
            .expect("归还一份预算后必须能继续开工");
        drop(third);
        drop(first);
        assert_eq!(scheduler.active(), 0);
        assert_eq!(scheduler.memory_in_use(), 0, "permit 归还时内存账必须一起销掉");
    }

    /// §63 超大图独占：哪怕 CPU 名额空着，也不能在它旁边再开一张。
    #[tokio::test]
    async fn a_huge_image_gets_the_whole_machine_to_itself() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::with_memory_budget(8, 1000));
        assert!(scheduler.try_begin_batch());
        let small = scheduler.try_acquire().unwrap();

        assert!(
            scheduler.try_acquire_with_memory(600).is_none(),
            "别人还在跑时，超大图必须排队而不是硬挤"
        );
        drop(small);

        let huge = scheduler
            .try_acquire_with_memory(600)
            .expect("空场时超大图必须能跑");
        assert!(
            scheduler.try_acquire().is_none(),
            "超大图独占期间，连 0 预算的小任务也不许开工"
        );
        drop(huge);
        assert!(scheduler.try_acquire().is_some(), "超大图结束后闸门要立刻恢复");
    }

    #[test]
    fn an_image_larger_than_the_entire_budget_still_runs_alone() {
        // 拒绝它就等于这张图永远压不了；空场时放行，后来者由预算自然挡住。
        let scheduler = std::sync::Arc::new(CompressionScheduler::with_memory_budget(4, 1000));
        assert!(scheduler.try_begin_batch());
        let huge = scheduler
            .try_acquire_with_memory(5000)
            .expect("比总预算还大的单张图不能被饿死");
        assert!(scheduler.try_acquire_with_memory(0).is_none());
        drop(huge);
        assert!(scheduler.try_acquire_with_memory(0).is_some());
    }

    #[test]
    fn ending_a_batch_clears_the_memory_ledger_too() {
        let scheduler = std::sync::Arc::new(CompressionScheduler::with_memory_budget(4, 1000));
        assert!(scheduler.try_begin_batch());
        let permit = scheduler.try_acquire_with_memory(600).unwrap();
        scheduler.end_batch();
        assert_eq!(scheduler.memory_in_use(), 0);
        // 迟到的 Drop 只能把已经归零的账再减一次 0，不能变成天文数字。
        drop(permit);
        assert_eq!(scheduler.memory_in_use(), 0);
        assert!(scheduler.try_begin_batch());
        assert!(scheduler.try_acquire_with_memory(600).is_some(), "下一轮不继承上一轮的内存账");
    }

    /// §64 队列和结果对象只留路径 / 元数据。候选字节只活在 Encode→Commit 之间，
    /// 一旦落盘（或放弃）就必须释放 —— 不能让 1 万条结果各背一份图片。
    #[test]
    fn queue_and_results_carry_metadata_never_image_payloads() {
        let source = include_str!("commands.rs");
        assert!(
            source.contains("file_paths: Vec<String>"),
            "批处理队列必须只是路径列表"
        );
        let result = serde_json::to_value(CompressResult::default()).unwrap();
        let fields = result.as_object().expect("结果必须序列化成对象");
        for banned in ["compressed", "bytes", "bitmap", "pixels", "candidates", "buffer", "data"] {
            assert!(
                !fields.contains_key(banned),
                "结果对象里出现了原始图片载荷字段 {banned}：候选字节必须在 Commit 后释放"
            );
        }
    }

    #[test]
    fn a_zero_limit_still_runs_one_job() {
        // 上限 0 会让闸门永远关闭，比"慢"更糟，所以夹到 1。
        let scheduler = CompressionScheduler::new(0);
        assert_eq!(scheduler.max_parallelism(), MIN_PARALLELISM);
        scheduler.set_max_parallelism(0);
        assert_eq!(scheduler.max_parallelism(), MIN_PARALLELISM);
    }

    #[test]
    fn collect_image_files_deduplicates_overlapping_inputs_in_stable_order() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        let first = temp.path().join("first.png");
        let second = nested.join("second.JPG");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();

        let files = collect_image_files(&[
            first.to_string_lossy().into_owned(),
            temp.path().to_string_lossy().into_owned(),
            nested.to_string_lossy().into_owned(),
        ]);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0], first.canonicalize().unwrap().to_string_lossy());
        assert_eq!(files[1], second.canonicalize().unwrap().to_string_lossy());
    }

    #[test]
    fn folder_output_keeps_the_selected_root_when_queue_is_expanded() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("source");
        let nested = source_root.join("nested");
        let output_root = temp.path().join("output");
        fs::create_dir_all(&nested).unwrap();
        let input = nested.join("photo.png");
        fs::write(&input, png_fixture(64, 64, 1)).unwrap();

        let compressed = small_png();
        let engine_result = engine_result(&compressed, "png");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "folder".into(),
            output_dir: Some(output_root.to_string_lossy().into_owned()),
            source_roots: vec![source_root.to_string_lossy().into_owned()],
            ..Default::default()
        };

        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history_in(temp.path()),
            &transactions_in(temp.path()),
            3,
        )
        .expect("目录模式的输出必须落进已选根目录");

        assert_eq!(
            result.output_path.as_deref(),
            Some(output_root.join("nested/photo.png").to_string_lossy().as_ref())
        );
    }

    /// 目录/后缀模式只记录历史，不留原图备份；备份只在 replace 模式产生。
    #[test]
    fn only_replace_mode_keeps_an_original_backup() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let compressed = small_png();
        let engine_result = engine_result(&compressed, "png");

        for (mode, suffix) in [("suffix", "_c"), ("folder", "_f")] {
            let input = temp.path().join(format!("{}-in.png", mode));
            fs::write(&input, png_fixture(64, 64, 2)).unwrap();
            let mut result = build_result(&input.to_string_lossy(), &engine_result);
            let options = CompressOptions {
                output_mode: mode.into(),
                output_suffix: suffix.into(),
                output_dir: Some(temp.path().join("out").to_string_lossy().into_owned()),
                ..Default::default()
            };
            write_output(
                &mut result,
                &engine_result,
                &input,
                &options,
                &history,
                &transactions,
                3,
            )
            .expect("输出写入成功必须产生历史");
            assert_eq!(result.backup_path, None);
            let entry = history
                .list()
                .into_iter()
                .next()
                .expect("这次压缩必须留下一条历史");
            assert_eq!(entry.output_mode, mode);
            assert_eq!(entry.backup_path, None);
            // 非覆盖模式不动用户原图，也就不需要事务凭证。
            assert!(!transactions.has_pending());
        }

        let input = temp.path().join("replace-in.png");
        fs::write(&input, png_fixture(64, 64, 3)).unwrap();
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "replace".into(),
            ..Default::default()
        };
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect("覆盖模式必须产出历史");
        let entry = history.list().into_iter().next().unwrap();
        assert!(PathBuf::from(entry.backup_path.unwrap()).exists());
        assert_eq!(fs::read(&input).unwrap(), compressed);
        // 提交完成 = 事务凭证必须收走，否则下次启动会白回滚一次。
        assert!(!transactions.has_pending());
        // Publish：只有真的落盘 + 记了历史的这一条，才允许说"有产物"。
        assert_eq!(result.outcome, Some(ResultOutcome::Committed));
        assert_eq!(result.reason_code, None);
    }

    /// §71 §72 唯一的合法"有产物"结论是 Committed：它指向的文件必须真的在盘上，
    /// 历史里那条记录也得指向同一个文件。反过来，别的结论连历史都不该占。
    #[test]
    fn a_committed_result_and_its_history_point_at_a_file_that_really_exists() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        fs::write(&input, png_fixture(64, 64, 9)).unwrap();
        let compressed = small_png();
        let engine_result = engine_result(&compressed, "png");

        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "suffix".into(),
            output_suffix: "_c".into(),
            ..Default::default()
        };
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect("有产物就必须落盘记账");

        assert_eq!(result.outcome, Some(ResultOutcome::Committed));
        let output = result.output_path.clone().expect("Committed 必须带产物路径");
        assert_eq!(fs::read(&output).unwrap(), compressed, "路径上的字节必须就是这次产物");
        let id = result.history_id.clone().expect("Committed 必须带历史 id");
        let entry = history.find(&id).expect("只有落盘成功的结果才配出现在历史里");
        assert_eq!(entry.output_path.as_deref(), Some(output.as_str()));
        assert!(entry.output_exists);
        assert_eq!(
            result_model::invariant_violation(
                "image",
                &input.to_string_lossy(),
                result.outcome,
                result.output_path.as_deref(),
            ),
            None
        );
    }

    /// §1 §14 「编码成功 ≠ 压缩成功」的机器版判据：什么都没写出来时，
    /// 结果里必须既没有产物、也没有历史，而且结论说的是"跳过"。
    #[test]
    fn a_run_that_writes_nothing_publishes_skipped_instead_of_a_product() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        let original = small_png();
        fs::write(&input, &original).unwrap();
        // 一份真实可识别、但比原图更大的候选：判据必须说"跳过"，而不是格式失败。
        let engine_result = engine_result(&png_fixture(64, 64, 4), "png");

        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "suffix".into(),
            output_suffix: "_c".into(),
            ..Default::default()
        };
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect("跳过不是失败，不该报错");

        assert_eq!(result.outcome, Some(ResultOutcome::SkippedNoBenefit));
        assert_eq!(result.reason_code, Some(ReasonCode::OutputLargerThanSource));
        assert_eq!(result.output_path, None);
        assert_eq!(result.backup_path, None);
        assert!(!temp.path().join("photo_c.png").exists(), "跳过不许留下任何文件");
        assert!(history.list().is_empty(), "没有产物的运行不配占一条历史");
        assert!(!transactions.has_pending());
        // 结论与字段自洽 —— 这条 invariant 就是 UI「不许显示对比/恢复」的依据。
        assert_eq!(
            result_model::invariant_violation(
                "image",
                &input.to_string_lossy(),
                result.outcome,
                result.output_path.as_deref()
            ),
            None
        );
    }

    /// §16 压到了文件却没达目标：有产物，但结论必须说清承诺没兑现。
    #[test]
    fn a_committed_run_that_missed_the_target_publishes_target_unmet_with_its_product() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("big.png");
        fs::write(&input, png_fixture(64, 64, 5)).unwrap();
        let compressed = small_png();
        let engine_result = engine_result(&compressed, "png");
        let engine_result = EngineResult {
            target_bytes: Some(100),
            target_met: Some(false),
            ..engine_result
        };

        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "suffix".into(),
            output_suffix: "_c".into(),
            ..Default::default()
        };
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect("有产物就必须落盘记账");

        assert_eq!(result.outcome, Some(ResultOutcome::TargetUnmet));
        assert_eq!(result.reason_code, Some(ReasonCode::TargetUnmet));
        assert!(result.output_path.is_some());
        assert_eq!(fs::read(temp.path().join("big_c.png")).unwrap(), compressed);
    }

    /// §9 §13 四阶段分家的核心判据：Encode 说「我压完了」不算数。
    ///
    /// 引擎交回一个**更大**的东西时，Evaluate 必须把它拦在 Commit 之前，Publish 则把体积
    /// 账改回盘上真正躺着的那份原图 —— 否则「省了负数」和「2 个产物」都会从这儿长出来。
    #[test]
    fn the_publish_stage_owns_the_conclusion_and_republishes_the_volume_account() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("tight.png");
        let original = small_png();
        fs::write(&input, &original).unwrap();
        // 交回的是一份真实可识别、却比原图更大的 PNG：Encode 的 success 不能阻止 Evaluate 拦下它。
        let engine_result = engine_result(&png_fixture(64, 64, 6), "png");

        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        assert_eq!(result.outcome, None, "Publish 之前谁都不许给结论");
        assert!(!result.has_product(), "Publish 之前谁都不许声称有产物");

        write_output(
            &mut result,
            &engine_result,
            &input,
            &CompressOptions { output_mode: "replace".into(), ..Default::default() },
            &history,
            &transactions,
            3,
        )
        .expect("跳过不是事务错误");

        assert_eq!(result.outcome, Some(ResultOutcome::SkippedNoBenefit));
        assert_eq!(result.reason_code, Some(ReasonCode::OutputLargerThanSource));
        assert_eq!(result.output_path, None);
        assert_eq!(result.backup_path, None, "没进入事务就不该有备份");
        assert_eq!(result.compressed_size, result.original_size, "没有产物时体积账就是那份原图");
        assert_eq!(result.savings, 0.0);
        assert_eq!(result.compressed_size_formatted, result.original_size_formatted);
        assert_eq!(fs::read(&input).unwrap(), original, "原图一个字节都没被动过");
        assert!(history.list().is_empty(), "跳过不进历史");
    }

    /// 「选了输出到目录却没选目录」以前会 return Ok(()) 且 success 保持 true：
    /// 用户看到「压缩完成」，磁盘上什么都没有。这条判据把它钉死。
    #[test]
    fn folder_mode_without_a_directory_fails_instead_of_claiming_completion() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        fs::write(&input, png_fixture(64, 64, 7)).unwrap();
        let compressed = small_png();
        let engine_result = engine_result(&compressed, "png");

        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "folder".into(),
            output_dir: None,
            ..Default::default()
        };
        let error = write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect_err("配置没兑现是一次明确的失败，不是可以忽略的落盘成功");
        assert!(matches!(error, OutputWriteError::InvalidDestination(_)));

        assert_eq!(result.outcome, Some(ResultOutcome::Failed));
        assert_eq!(result.reason_code, Some(ReasonCode::InvalidInput));
        assert!(!result.has_product(), "失败不许带着产物上屏");
        assert_eq!(result.output_path, None);
        // 失败结论由 Publish 一处写：Commit 只负责把这个 Err 交回去。
        assert_eq!(result.error.as_deref(), Some("请先选择输出目录"));
        assert!(history.list().is_empty());
    }

    fn history_in(dir: &Path) -> HistoryStore {
        HistoryStore::new(dir.join("history")).unwrap()
    }

    fn transactions_in(dir: &Path) -> TransactionStore {
        TransactionStore::new(
            dir.join("history")
                .join(output_transaction::TRANSACTIONS_DIR),
        )
    }

    /// §34 的取消分支另有专门测试；这些用例跑的都是「没人取消」那条正常路径。
    fn never_cancel() -> bool {
        false
    }

    /// 演「编码跑完之后用户按了取消」：Commit 前的检查点一按就响。
    fn always_cancel() -> bool {
        true
    }

    /// 测试用的 Commit 入口：把取消检查点固定成"没有取消"，覆盖依据走**同一个**
    /// `capture_replace_baseline`（§90：判据不许在测试里另写一份），其余参数原样交给
    /// `write_output_file`。
    #[allow(clippy::too_many_arguments)]
    fn write_output(
        result: &mut CompressResult,
        engine_result: &EngineResult,
        file_path: &Path,
        options: &CompressOptions,
        history: &HistoryStore,
        transactions: &TransactionStore,
        retention_days: u32,
    ) -> Result<(), OutputWriteError> {
        write_output_file(
            result,
            engine_result,
            file_path,
            options,
            history,
            transactions,
            retention_days,
            &never_cancel,
            capture_replace_baseline(file_path, options, history),
        )
    }

    /// 历史写不成 = 这次覆盖不能算数：原图必须回到用户手上，事务凭证必须收走。
    #[test]
    fn a_history_write_failure_rolls_the_overwrite_back_to_the_original() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        let original = png_fixture(64, 64, 8);
        fs::write(&input, &original).unwrap();

        // 先把内存快照建立起来：否则下面的堵塞点会被"损坏现场隔离"整目录挪走，
        // 测的就不是写入失败而是损坏恢复了（那条另有测试覆盖）。
        assert!(history.list().is_empty());
        // 注入失败：history.json 的位置放一个**非空**目录，rename 必撞 EISDIR
        // （空目录会被 macOS 直接替换掉，所以必须留一个占位子目录）。
        let blocked = temp.path().join("history").join("history.json");
        fs::create_dir_all(blocked.join("stale")).unwrap();

        let compressed = small_png();
        let engine_result = engine_result(&compressed, "png");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "replace".into(),
            ..Default::default()
        };
        let error = write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect_err("历史落盘失败必须报给调用方");
        assert!(matches!(error, OutputWriteError::HistoryWriteFailed));
        // 覆盖撤销：源文件回到真正原图，UI 侧不能再报"输出在 X"。
        assert_eq!(fs::read(&input).unwrap(), original);
        assert_eq!(result.output_path, None);
        assert_eq!(result.backup_path, None);
        // 凭证已销账，下次启动不会再把这次当成中断事务。
        assert!(!transactions.has_pending());
        // 备份**不删**：它成了无人引用的孤儿，留给退出清理，绝不在这条路径上丢掉原图副本。
        let backups = temp.path().join("history").join("backups");
        assert!(
            fs::read_dir(&backups)
                .map(|mut iter| iter.next().is_some())
                .unwrap_or(false),
            "回滚之后原图副本必须还在磁盘上"
        );
    }

    /// 编一份"比原图小"的引擎产物，够测试用。
    fn engine_result(bytes: &[u8], format: &str) -> EngineResult {
        EngineResult {
            success: true,
            compressed: bytes.to_vec(),
            out_type: format.into(),
            algorithm: "test".into(),
            error: None,
            ..Default::default()
        }
    }

    /// §19 §20 之后，落盘前要拿字节头核对格式：`b"small"` 这种占位符已经不再
    /// 代表一份候选。夹具用真实可识别的 PNG/JPEG 字节，尺寸决定它在测试里的角色：
    /// 噪声大图当"原图"，1×1 小图当"压小的候选"。
    fn png_fixture(width: u32, height: u32, salt: u32) -> Vec<u8> {
        let mut state = salt.wrapping_mul(2_654_435_761).wrapping_add(1);
        let img = image::RgbaImage::from_fn(width, height, |_, _| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            image::Rgba([
                (state >> 24) as u8,
                (state >> 16) as u8,
                (state >> 8) as u8,
                255,
            ])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn jpeg_fixture(width: u32, height: u32, salt: u32) -> Vec<u8> {
        let mut state = salt.wrapping_mul(2_246_822_519).wrapping_add(3);
        let img = image::RgbImage::from_fn(width, height, |_, _| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            image::Rgb([(state >> 24) as u8, (state >> 16) as u8, (state >> 8) as u8])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        bytes
    }

    fn small_png() -> Vec<u8> {
        png_fixture(1, 1, 9)
    }

    fn small_jpeg() -> Vec<u8> {
        jpeg_fixture(1, 1, 9)
    }

    fn replace_options() -> CompressOptions {
        CompressOptions {
            output_mode: "replace".into(),
            ..Default::default()
        }
    }

    /// 数一遍用户目录里除了输入文件还剩什么（临时文件、占位名都算多余东西）。
    fn leftovers(temp: &Path, input_name: &str) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(temp)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != input_name && name != "history")
            .collect();
        names.sort();
        names
    }

    /// §25 §26 编码期间被原地改成同大小、时间戳也没动的另一份字节。
    ///
    /// 这是只看 `stat` 的门看不见的那一支，也是"覆盖会毁掉用户新改动"最疼的一支：
    /// 拦不住的话，盖上去的是按旧字节算的候选，备份里存的却是新改动 ——
    /// 两边都不是用户想要的那张图。
    #[test]
    fn an_edit_during_encoding_voids_the_overwrite_even_when_the_stats_look_identical() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        let original = png_fixture(64, 64, 10);
        fs::write(&input, &original).unwrap();
        let options = replace_options();
        // 编码**之前**的依据：指纹 + 原图快照。
        let baseline = capture_replace_baseline(&input, &options, &history);
        assert!(
            matches!(baseline, ReplaceBaseline::Ready { .. }),
            "覆盖模式编码前必须备齐依据"
        );

        // 「编码期间」用户又改了这张图：长度一样，再把 mtime 钉回改动前那一刻。
        let moment = match &baseline {
            ReplaceBaseline::Ready { identity, .. } => {
                identity.modified.expect("刚 stat 过的文件必有 mtime")
            }
            _ => unreachable!("上一条断言已经确认过分支"),
        };
        let mut edited = original;
        edited[20] ^= 0xFF;
        fs::write(&input, &edited).unwrap();
        SourceIdentity::freeze_mtime_to(&input, moment).unwrap();
        let current = SourceIdentity::read(&input).unwrap();
        match &baseline {
            ReplaceBaseline::Ready { identity, .. } => assert!(
                identity.same_fingerprint(&current),
                "测试前提没立住：改动后的 size/mtime/inode 必须和改动前一致，否则这条测的是快速门"
            ),
            _ => unreachable!(),
        }

        let engine_result = engine_result(&small_png(), "png");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let error = write_output_file(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
            &never_cancel,
            baseline,
        )
        .expect_err("源文件已经不是压缩时那份，覆盖必须作废");

        assert!(
            matches!(error, OutputWriteError::SourceChanged(_)),
            "报出来的不是源文件变更：{error:?}"
        );
        assert_eq!(result.outcome, Some(ResultOutcome::Failed));
        assert_eq!(result.reason_code, Some(ReasonCode::SourceChanged));
        assert_eq!(result.output_path, None, "作废的覆盖却交出产物路径");
        assert_eq!(
            fs::read(&input).unwrap(),
            edited,
            "用户当前那份必须原样留着，一个字节都不许动"
        );
        assert!(history.list().is_empty(), "作废的覆盖记进了历史");
        assert!(!transactions.has_pending(), "作废的覆盖留了个没人认领的事务");
        assert_eq!(leftovers(temp.path(), "photo.png"), Vec::<String>::new(), "作废的覆盖在目录里留下了文件");
    }

    /// §24 快照必须真的在编码之前就位 —— 它得能还原"编码前那张图"，
    /// 哪怕之后源文件被删、被改、被换走。这条盯的是备份时机本身，不是复核。
    #[test]
    fn the_original_snapshot_exists_before_the_encoding_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let input = temp.path().join("photo.png");
        fs::write(&input, b"true-original-bytes").unwrap();
        let options = replace_options();
        let ReplaceBaseline::Ready { identity, backup } = capture_replace_baseline(&input, &options, &history)
        else {
            panic!("覆盖模式的依据没备齐");
        };
        // 还没编码、还没 Commit，快照已经在磁盘上且就是那份原图。
        assert_eq!(fs::read(&backup).unwrap(), b"true-original-bytes");
        assert_eq!(identity.size, b"true-original-bytes".len() as u64);
        assert!(identity.sha256.is_some(), "§26 关键 Commit 要用的内容指纹没算");
        // 编码期间用户把源文件整个删掉：依据（快照）不能跟着一起没。
        fs::remove_file(&input).unwrap();
        assert_eq!(fs::read(&backup).unwrap(), b"true-original-bytes", "快照不是编码前那份原图");
    }

    /// §27 备份的来历记录说"字节对不上"时，这次覆盖作废。
    ///
    /// 走到这一步用户的文件还没被碰，所以处置只有"收回去自己造的东西"一种 ——
    /// 不许 rollback，也不许留下一个自称能恢复原图的历史条目。
    #[test]
    fn a_snapshot_whose_bytes_do_not_add_up_blocks_the_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        let original = png_fixture(64, 64, 11);
        fs::write(&input, &original).unwrap();
        let options = replace_options();
        let baseline = capture_replace_baseline(&input, &options, &history);
        let ReplaceBaseline::Ready { backup, .. } = &baseline else {
            panic!("覆盖模式的依据没备齐");
        };
        // 快照被换成了别的内容（写坏了、被人动过），来历记录里的指纹还对不上。
        fs::write(backup, b"someone-elses-bytes").unwrap();

        let engine_result = engine_result(&small_png(), "png");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let error = write_output_file(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
            &never_cancel,
            baseline,
        )
        .expect_err("对不上号的快照不能撑起一次覆盖");

        assert!(matches!(error, OutputWriteError::BackupFailed(_)), "{error:?}");
        assert_eq!(result.reason_code, Some(ReasonCode::BackupFailed));
        assert_eq!(fs::read(&input).unwrap(), original, "被拦下的覆盖动了用户文件");
        assert!(history.list().is_empty());
        assert!(!transactions.has_pending());
        assert_eq!(leftovers(temp.path(), "photo.png"), Vec::<String>::new());
    }

    /// 升级前留下的备份（来历记录里没有指纹）照旧能用：覆盖继续。
    ///
    /// "证明不了"不等于"证明坏了"。把老备份一律判成死刑，用户就再也压不了他压过的图。
    #[test]
    fn a_snapshot_recorded_before_fingerprints_existed_is_still_usable() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        let original = png_fixture(64, 64, 12);
        fs::write(&input, &original).unwrap();
        let options = replace_options();
        let baseline = capture_replace_baseline(&input, &options, &history);
        let ReplaceBaseline::Ready { backup, .. } = &baseline else {
            panic!("覆盖模式的依据没备齐");
        };
        // 把来历记录退回版本 2：没有两份指纹，但备份文件本身是完整的原图。
        let meta_path = backup.parent().unwrap().join("backup-meta.json");
        fs::write(
            &meta_path,
            serde_json::json!({
                "version": 2,
                "sourcePath": input.to_string_lossy(),
                "createdAt": 1,
                "originalSize": original.len(),
                "originalModifiedAt": null,
                "originalExtension": "png"
            })
            .to_string(),
        )
        .unwrap();

        let compressed = small_png();
        let engine_result = engine_result(&compressed, "png");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        write_output_file(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
            &never_cancel,
            baseline,
        )
        .expect("老记录的备份读得出字节，这次覆盖就该照常完成");

        assert_eq!(fs::read(&input).unwrap(), compressed, "没指纹的老备份被误判成坏了");
        assert_eq!(result.outcome, Some(ResultOutcome::Committed));
        assert_eq!(result.reason_code, None);
        assert_eq!(history.list().len(), 1);
        assert!(!transactions.has_pending());
    }

    /// §24 连"这份文件是什么"都读不出来时，覆盖模式一个字节都不许写。
    #[test]
    fn a_source_that_cannot_be_read_leaves_the_replace_before_any_write() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        let options = replace_options();
        // 文件压根不在（排队期间被移走）：读指纹必先失败。
        let baseline = capture_replace_baseline(&input, &options, &history);
        assert!(
            matches!(baseline, ReplaceBaseline::Unavailable(OutputWriteError::SourceUnreadable(_))),
            "读不到的源文件必须报成读不到"
        );
        let recreated = png_fixture(64, 64, 13);
        fs::write(&input, &recreated).unwrap();

        let engine_result = engine_result(&small_png(), "png");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let error = write_output_file(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
            &never_cancel,
            baseline,
        )
        .expect_err("依据没备齐就不许覆盖");

        assert!(matches!(error, OutputWriteError::SourceUnreadable(_)), "{error:?}");
        assert_eq!(result.reason_code, Some(ReasonCode::SourceReadFailed));
        assert_eq!(fs::read(&input).unwrap(), recreated, "被拦下的覆盖动了用户的文件");
        assert!(history.list().is_empty());
        assert!(!transactions.has_pending());
        assert_eq!(leftovers(temp.path(), "photo.png"), Vec::<String>::new());
    }

    /// 覆盖模式下的真格式转换：字节是 JPEG，文件就得叫 `.jpg`。
    /// 把 JPEG 塞进 photo.png 会得到一个扩展名骗人的文件，两边都打不开。
    #[test]
    fn replace_mode_renames_the_file_to_the_format_its_bytes_actually_are() {
        let temp = tempfile::tempdir().unwrap();
        let history = history_in(temp.path());
        let transactions = transactions_in(temp.path());
        let input = temp.path().join("photo.png");
        fs::write(&input, png_fixture(64, 64, 14)).unwrap();

        let converted_bytes = jpeg_fixture(8, 8, 1);
        let engine_result = EngineResult {
            success: true,
            compressed: converted_bytes.clone(),
            out_type: "jpg".into(),
            algorithm: "mozjpeg".into(),
            error: None,
            ..Default::default()
        };
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "replace".into(),
            output_format: "jpg".into(),
            ..Default::default()
        };
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect("格式转换的输出必须落盘");

        let converted = temp.path().join("photo.jpg");
        assert_eq!(
            result.output_path.as_deref(),
            Some(converted.to_string_lossy().as_ref())
        );
        assert_eq!(fs::read(&converted).unwrap(), converted_bytes);
        assert!(!input.exists(), "换了扩展名就不许再留一份假 .png 在原位");
        // 原图不会凭空消失：备份先落盘，历史写成后凭证才销账。
        let entry = history.list().into_iter().next().unwrap();
        assert!(PathBuf::from(entry.backup_path.unwrap()).exists());
        assert!(!transactions.has_pending());
    }

    /// 目标扩展名已经被占用（规则 8）：另起一个不冲突的名字，绝不覆盖邻居。
    #[test]
    fn a_format_conversion_never_clobbers_an_existing_file_with_the_target_name() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("photo.png");
        fs::write(&input, png_fixture(64, 64, 15)).unwrap();
        let neighbour = temp.path().join("photo.jpg");
        fs::write(&neighbour, b"someone-elses-file").unwrap();

        let converted_bytes = jpeg_fixture(8, 8, 2);
        let engine_result = EngineResult {
            success: true,
            compressed: converted_bytes,
            out_type: "jpg".into(),
            algorithm: "mozjpeg".into(),
            error: None,
            ..Default::default()
        };
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "replace".into(),
            output_format: "jpg".into(),
            ..Default::default()
        };
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history_in(temp.path()),
            &transactions_in(temp.path()),
            3,
        )
        .expect("格式转换的输出必须落盘");

        assert_eq!(
            result.output_path.as_deref(),
            Some(
                temp.path()
                    .join("photo_converted.jpg")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(
            fs::read(&neighbour).unwrap(),
            b"someone-elses-file",
            "邻居文件一个字节都不许动"
        );
    }

    /// jpeg/jpg 是同一种格式：拼法不同不算格式转换，
    /// 否则每个 .jpeg 都会在原图旁边长出一个 .jpg。
    #[test]
    fn jpg_bytes_overwrite_a_jpeg_source_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("photo.jpeg");
        fs::write(&input, jpeg_fixture(64, 64, 3)).unwrap();

        let compressed = small_jpeg();
        let engine_result = EngineResult {
            success: true,
            compressed: compressed.clone(),
            out_type: "jpg".into(),
            algorithm: "mozjpeg".into(),
            error: None,
            ..Default::default()
        };
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "replace".into(),
            ..Default::default()
        };
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history_in(temp.path()),
            &transactions_in(temp.path()),
            3,
        )
        .expect("同格式覆盖必须落盘");

        assert_eq!(
            result.output_path.as_deref(),
            Some(input.to_string_lossy().as_ref())
        );
        assert_eq!(fs::read(&input).unwrap(), compressed);
        assert!(
            !temp.path().join("photo.jpg").exists(),
            "同格式不许另起一个扩展名"
        );
    }

    /// §28 §54 转换产物的名字由分配器独占给出，序列与老实现一致，但每一步都是 O_EXCL：
    /// 邻居的文件不但不会被盖掉，连"探到空位再写"之间那段被人插队的窗口都没有。
    #[test]
    fn a_converted_target_never_overwrites_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let naming = || OutputNaming::Converted {
            stem: "photo".into(),
            extension: "jpg".into(),
        };

        let first = OutputNameAllocator::claim(dir, &naming()).unwrap();
        assert_eq!(first.path(), dir.join("photo.jpg"));
        assert_eq!(first.size(), 0, "占位必须是个空文件，不能假装是产物");

        fs::write(dir.join("photo.jpg"), b"somebody-else").unwrap();
        let second = OutputNameAllocator::claim(dir, &naming()).unwrap();
        assert_eq!(second.path(), dir.join("photo_converted.jpg"));
        assert_eq!(
            fs::read(dir.join("photo.jpg")).unwrap(),
            b"somebody-else",
            "分配器把已存在的文件盖掉了"
        );

        fs::write(dir.join("photo_converted.jpg"), b"another").unwrap();
        let third = OutputNameAllocator::claim(dir, &naming()).unwrap();
        assert_eq!(third.path(), dir.join("photo_converted_2.jpg"));
        assert_eq!(fs::read(dir.join("photo_converted.jpg")).unwrap(), b"another");
    }

    /// §28 后缀模式的同名文件：第二次压缩各留一份，谁也不盖谁。
    #[test]
    fn a_suffix_output_never_replaces_the_previous_one() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let input = dir.join("photo.jpg");
        let original = jpeg_fixture(64, 64, 4);
        fs::write(&input, &original).unwrap();
        // 上一次的压缩成果就躺在 photo_compressed.jpg。
        fs::write(dir.join("photo_compressed.jpg"), b"previous-run").unwrap();
        let neighbour = dir.join("photo_compressed_1.jpg");
        fs::write(&neighbour, b"neighbours-photo").unwrap();

        let options = CompressOptions {
            output_mode: "suffix".into(),
            output_suffix: "_compressed".into(),
            ..Default::default()
        };
        let compressed = small_jpeg();
        let engine_result = engine_result(&compressed, "jpg");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history_in(temp.path()),
            &transactions_in(temp.path()),
            3,
        )
        .expect("后缀输出必须落盘");

        assert_eq!(
            result.output_path.as_deref(),
            Some(dir.join("photo_compressed_2.jpg").to_string_lossy().as_ref()),
            "两个已存在的名字都该跳过"
        );
        assert_eq!(fs::read(&input).unwrap(), original, "后缀模式不许动原图");
        assert_eq!(fs::read(dir.join("photo_compressed.jpg")).unwrap(), b"previous-run");
        assert_eq!(fs::read(&neighbour).unwrap(), b"neighbours-photo");
    }

    /// §29 新建模式也进事务：历史没写成时磁盘上不能留下没人知道的产物。
    #[test]
    fn a_new_file_output_leaves_no_orphan_when_history_fails() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let input = dir.join("photo.jpg");
        let original = jpeg_fixture(64, 64, 5);
        fs::write(&input, &original).unwrap();
        let history = history_in(dir);
        let transactions = transactions_in(dir);
        let options = CompressOptions {
            output_mode: "suffix".into(),
            ..Default::default()
        };
        let engine_result = engine_result(&small_jpeg(), "jpg");

        // 注入失败：history.json 的位置放一个**非空**目录，rename 必撞 EISDIR。
        assert!(history.list().is_empty());
        let blocked = dir.join("history").join("history.json");
        fs::create_dir_all(blocked.join("stale")).unwrap();
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let error = write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history,
            &transactions,
            3,
        )
        .expect_err("历史写失败不能算成功");
        assert!(matches!(error, OutputWriteError::HistoryWriteFailed));

        assert_eq!(fs::read(&input).unwrap(), original, "新建模式从没动过原图");
        let leftovers: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|item| item.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("photo_compressed") || name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "磁盘上留下了没人认领的产物：{leftovers:?}");
        assert!(!transactions.has_pending(), "事务日志该跟着这次失败一起销掉");
    }

    /// §28 同一个目标名同时来两个人：只能各拿一个名字，谁也不许盖谁。
    #[test]
    fn two_parallel_commits_to_the_same_desired_name_never_share_a_path() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        let workers: Vec<_> = (0..50)
            .map(|index| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let naming = OutputNaming::WithSuffix {
                        stem: "photo".into(),
                        suffix: "_compressed".into(),
                        extension: "jpg".into(),
                    };
                    let claimed = OutputNameAllocator::claim(&dir, &naming)
                        .unwrap_or_else(|error| panic!("第 {index} 个任务申请不到名字: {error}"));
                    let bytes = format!("payload-{index}").into_bytes();
                    let staged = StagedOutput::claim(claimed.path(), ".sizqo-write-")
                        .expect("临时名必须申请得到");
                    staged.write_synced(&bytes).expect("产物必须落盘");
                    staged.publish(claimed.path()).expect("产物必须原子改名");
                    staged.discard();
                    claimed.into_path()
                })
            })
            .collect();

        let paths: Vec<PathBuf> = workers
            .into_iter()
            .map(|worker| worker.join().expect("并发落盘不该 panic"))
            .collect();
        let unique: std::collections::HashSet<&PathBuf> = paths.iter().collect();
        assert_eq!(unique.len(), paths.len(), "两个任务拿到了同一个输出名");
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(
                fs::read(path).unwrap(),
                format!("payload-{index}").into_bytes(),
                "{} 的内容被后来的任务盖掉了",
                path.display()
            );
        }
    }

    #[test]
    fn custom_suffix_is_used_for_suffix_output() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("photo.png");
        fs::write(&input, png_fixture(64, 64, 16)).unwrap();

        let engine_result = engine_result(&small_png(), "png");
        let mut result = build_result(&input.to_string_lossy(), &engine_result);
        let options = CompressOptions {
            output_mode: "suffix".into(),
            output_suffix: "_small".into(),
            ..Default::default()
        };

        write_output(
            &mut result,
            &engine_result,
            &input,
            &options,
            &history_in(temp.path()),
            &transactions_in(temp.path()),
            3,
        )
        .expect("后缀模式的输出必须落盘");

        assert_eq!(
            result.output_path.as_deref(),
            Some(temp.path().join("photo_small.png").to_string_lossy().as_ref())
        );
    }

    /// §33 §34 取消打在 Commit 之前：候选可以压出来，但磁盘一个字节都不许多。
    ///
    /// 这条判据盯的是原方案点名的最大问题之一：「前端清空 → 后台任务继续 → 后台生成
    /// output」。取消之后既不许有产物，也不许有历史、备份、事务凭证 —— 否则用户明明
    /// 说了不要，磁盘上却多出一个没人认领的文件。
    #[test]
    fn a_cancellation_before_commit_writes_nothing_and_publishes_cancelled() {
        for mode in ["replace", "suffix", "folder"] {
            let temp = tempfile::tempdir().unwrap();
            let history = history_in(temp.path());
            let transactions = transactions_in(temp.path());
            let input = temp.path().join("photo.png");
            let original = png_fixture(64, 64, 17);
            fs::write(&input, &original).unwrap();

            let engine_result = engine_result(&small_png(), "png");
            let mut result = build_result(&input.to_string_lossy(), &engine_result);
            let options = CompressOptions {
                output_mode: mode.into(),
                output_suffix: "_c".into(),
                output_dir: Some(temp.path().join("out").to_string_lossy().into_owned()),
                ..Default::default()
            };

            write_output_file(
                &mut result,
                &engine_result,
                &input,
                &options,
                &history,
                &transactions,
                3,
                &always_cancel,
                // 覆盖依据走同一个 `capture_replace_baseline`：取消打在它之后、Commit 之前，
                // 所以编码前那份快照确实在磁盘上。但它不是产物、不是历史、也不是事务，
                // 下一次退出清理会把它当无人引用收走 —— 用户说的是"这次不要"。
                capture_replace_baseline(&input, &options, &history),
            )
            .expect("取消不是故障，是一次正常的收尾");

            assert_eq!(result.outcome, Some(ResultOutcome::Cancelled), "{mode}");
            assert_eq!(result.reason_code, Some(ReasonCode::Cancelled), "{mode}");
            assert_eq!(result.output_path, None, "{mode} 取消却不许交出产物路径");
            assert_eq!(result.backup_path, None, "{mode} 取消却不该认领一份备份");
            assert!(!result.has_product(), "{mode} 的取消项会被当成有产物");
            // §6.4 取消 = Done + Cancelled，不是系统故障。
            assert_ne!(result.outcome, Some(ResultOutcome::Failed), "{mode} 的取消被读成了故障");
            assert_eq!(result.error, None, "{mode} 的取消不该带一句错误");
            assert_eq!(result.compressed_size, result.original_size);
            assert_eq!(result.savings, 0.0);
            assert_eq!(
                fs::read(&input).unwrap(),
                original,
                "{mode} 模式动了用户文件"
            );
            assert!(history.list().is_empty(), "{mode} 取消却记了历史");
            assert!(!transactions.has_pending(), "{mode} 留了个没人认领的事务");
            let leftovers: Vec<String> = fs::read_dir(temp.path())
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != "photo.png" && name != "history")
                .collect();
            assert!(leftovers.is_empty(), "{mode} 取消了却留下文件: {leftovers:?}");
        }
    }

    // ─── §49 §54 §78 §79 混合媒体批量契约 ─────────────────────────

    fn bulk_item(
        source: &Path,
        output: Option<&Path>,
        outcome: ResultOutcome,
    ) -> MediaOperationItem {
        MediaOperationItem {
            media_kind: None,
            task_id: Some("task-1".into()),
            file: Some(source.to_string_lossy().into_owned()),
            source_path: None,
            output_path: output.map(|path| path.to_string_lossy().into_owned()),
            history_id: Some("history-1".into()),
            output_mode: Some("suffix".into()),
            outcome: Some(outcome),
        }
    }

    /// 图片结果用 `file`、视频结果用 `sourcePath`：同一张批量输入必须都能读懂，
    /// 不许先把 `VideoResult` 强转成图片结果（§49）。
    #[test]
    fn image_and_video_shaped_results_deserialize_into_the_same_bulk_item() {
        let image: MediaOperationItem = serde_json::from_value(serde_json::json!({
            "file": "/tmp/a.png",
            "mediaKind": "image",
            "outputPath": "/tmp/a_compressed.png",
            "outputMode": "suffix",
            "outcome": "committed"
        }))
        .unwrap();
        let video: MediaOperationItem = serde_json::from_value(serde_json::json!({
            "taskId": "video-1",
            "sourcePath": "/tmp/b.mov",
            "mediaKind": "video",
            "outputPath": "/tmp/b_compressed.mp4",
            "historyId": "history-video",
            "outcome": "committed"
        }))
        .unwrap();

        assert_eq!(image.source(), Some("/tmp/a.png"));
        assert_eq!(video.source(), Some("/tmp/b.mov"));
        assert!(image.carries_product() && video.carries_product());
        assert_eq!(video.history_id.as_deref(), Some("history-video"));

        let skipped: MediaOperationItem = serde_json::from_value(serde_json::json!({
            "file": "/tmp/c.png",
            "outcome": "skipped_no_benefit"
        }))
        .unwrap();
        assert!(!skipped.carries_product(), "§77 没有产物的项不许进入批量恢复/导出");
    }

    /// §78 一个产物被外部删掉只记 missing，其余图片/视频照常导出。
    #[test]
    fn exporting_mixed_media_survives_a_missing_output() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        let out_dir = dir.path().join("out");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&out_dir).unwrap();
        let image_source = src_dir.join("photo.png");
        let video_source = src_dir.join("clip.mov");
        fs::write(&image_source, b"original-image").unwrap();
        fs::write(&video_source, b"original-video").unwrap();
        let image_output = out_dir.join("photo.jpg");
        let video_output = out_dir.join("clip.mp4");
        fs::write(&image_output, b"image-product").unwrap();
        fs::write(&video_output, b"video-product").unwrap();
        let gone_output = out_dir.join("gone.jpg");

        let skipped = bulk_item(
            &image_source,
            Some(&image_output),
            ResultOutcome::SkippedNoBenefit,
        );
        let items = vec![
            bulk_item(&image_source, Some(&image_output), ResultOutcome::Committed),
            bulk_item(&video_source, Some(&video_output), ResultOutcome::Committed),
            bulk_item(&image_source, Some(&gone_output), ResultOutcome::Committed),
            skipped,
        ];

        let summary = export_items(&items, Some("_compressed"));
        assert_eq!(summary.success_count, 2);
        assert_eq!(summary.missing_count, 1);
        assert_eq!(summary.skipped_count, 1);
        assert_eq!(summary.failed_count, 0);
        assert!(!summary.success, "有文件不存在时整批不能自称完全成功");
        assert_eq!(
            fs::read(src_dir.join("photo_compressed.jpg")).unwrap(),
            b"image-product"
        );
        assert_eq!(
            fs::read(src_dir.join("clip_compressed.mp4")).unwrap(),
            b"video-product"
        );
        assert_eq!(summary.items.len(), 3, "跳过的项不该伪装成一次导出");
        assert!(summary
            .items
            .iter()
            .any(|item| item.reason_code == Some(ReasonCode::OutputMissing)));

        // 前端只认 camelCase：计数和逐项理由都必须以这套键出门。
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["successCount"], 2);
        assert_eq!(json["missingCount"], 1);
        assert!(json["items"][0]["reasonCode"].is_null());
    }

    /// §50 §51 §52 对比载荷只留下「图片 + 有产物 + 两侧文件都还在」的项。
    #[test]
    fn compare_payload_only_keeps_existing_image_products() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("a.png");
        let output = dir.path().join("a_compressed.png");
        let video = dir.path().join("clip.mov");
        fs::write(&original, b"original").unwrap();
        fs::write(&output, b"product").unwrap();
        fs::write(&video, b"video").unwrap();
        let gone = dir.path().join("gone.png");

        let payload = serde_json::json!({
            "index": 2,
            "results": [
                {
                    "file": video.to_string_lossy(),
                    "mediaKind": "video",
                    "outcome": "committed",
                    "outputPath": video.to_string_lossy()
                },
                {
                    "file": original.to_string_lossy(),
                    "mediaKind": "image",
                    "outcome": "committed",
                    "outputPath": gone.to_string_lossy()
                },
                {
                    "file": original.to_string_lossy(),
                    "mediaKind": "image",
                    "outcome": "committed",
                    "outputPath": output.to_string_lossy()
                }
            ]
        });
        let filtered = sanitize_compare_payload(payload).unwrap();
        let results = filtered["results"].as_array().unwrap();
        assert_eq!(results.len(), 1, "视频与被删产物都不许进对比窗口");
        assert_eq!(results[0]["outputPath"], output.to_string_lossy().as_ref());
        assert_eq!(filtered["index"], 0, "过滤后下标必须重新对准用户点中的那一项");

        let none_left = serde_json::json!({
            "index": 0,
            "results": [{
                "file": original.to_string_lossy(),
                "mediaKind": "image",
                "outcome": "committed",
                "outputPath": gone.to_string_lossy()
            }]
        });
        assert!(sanitize_compare_payload(none_left).is_err());
    }

    /// §54 第二次导出必须取新名字，不能把第一次的成果盖掉。
    #[test]
    fn exporting_twice_never_overwrites_the_previous_export() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        let out_dir = dir.path().join("out");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&out_dir).unwrap();
        let source = src_dir.join("photo.png");
        fs::write(&source, b"original").unwrap();
        let output = out_dir.join("photo.webp");
        fs::write(&output, b"first-product").unwrap();

        let items = vec![bulk_item(&source, Some(&output), ResultOutcome::Committed)];
        let first = export_items(&items, Some("_compressed"));
        assert_eq!(first.success_count, 1);
        assert_eq!(
            fs::read(src_dir.join("photo_compressed.webp")).unwrap(),
            b"first-product"
        );

        fs::write(&output, b"second-product").unwrap();
        let second = export_items(&items, Some("_compressed"));
        assert_eq!(second.success_count, 1, "{}", second.message);
        assert_eq!(
            fs::read(src_dir.join("photo_compressed.webp")).unwrap(),
            b"first-product",
            "第二次导出盖掉了上一次的成果"
        );
        assert_eq!(
            fs::read(src_dir.join("photo_compressed_1.webp")).unwrap(),
            b"second-product"
        );
    }
}
