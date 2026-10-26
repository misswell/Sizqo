// 前端是单文件脚本，测试靠"按标记切一段真实代码 + 给上下文补桩"来跑。
// 状态机（compressionState / setCompressionState / 暂停·继续·停止按钮）被四个测试共用，
// 切片标记只写在这里一处 —— 免得某个测试的范围悄悄漂掉，跑的已经不是真实代码。
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '..', 'frontend', 'app.js'), 'utf8');

function slice(from, to) {
  const start = source.indexOf(from);
  const end = source.indexOf(to);
  if (start < 0 || end < 0 || end <= start) {
    throw new Error(`切片标记找不到（${from} → ${to}）`);
  }
  return source.slice(start, end);
}

function installTranslationFallback(context) {
  if (typeof context.localizeUiText !== 'function') context.localizeUiText = value => value;
  if (typeof context.localizeBackendText !== 'function') context.localizeBackendText = value => value;
}

/// 「队列状态（唯一真相）」整段：queueItems + 派生进度 + 队列摘要。
/// 任何跟"这个文件还需不需要处理"有关的判断都必须从这里读，所以测试也只切这里。
function queueCore() {
  return slice('// ─── 队列状态（唯一真相，DOM 只能照着它画）', '// ─── 队列渲染（排序 / 视图 / 每一行的画法）');
}

/// 「把队列项画到那一行上」整段：paintQueueRow + 错误图标。
/// 方向只有一个：queueItems → DOM；这一段是唯一允许写行内状态的地方。
function queueRowPainter() {
  return slice('/// 把一个队列项此刻的状态', 'function createQueueRow(');
}

/// 「结果契约的唯一解释器」现在是一份独立文件（frontend/result_state.js）：
/// 主窗口和独立对比窗口共用同一份判据，测试装的也必须是那个文件本身 ——
/// 在 app.js 里再抄一份判据来测，测的就不是线上跑的那一份了。
/// 它之上还压着队列自己的两个事实（已恢复 / 已移除 的不算产物、产物清单），一起装。
function productRule() {
  const contract = fs.readFileSync(path.join(__dirname, '..', 'frontend', 'result_state.js'), 'utf8');
  return contract + '\n' + slice('/// 一项现在到底有没有产物', 'function updateBulkActionButtons(');
}

/// §44 会话闸门：队列摘要 / 历史行 / 所有恢复入口共用的唯一判据。
/// 它定义在压缩状态机段，但队列核心里的 updateBulkActionButtons 也要读，
/// 所以单独切一份装上——测的仍是线上那份代码，不在测试里重写。
function restoreGate() {
  return slice('/// §44 压缩会话与恢复是两条生命周期', '/// 后端阶段变化的确认');
}

/// 「执行会话（一轮）」整段：executionSession + 会话身份判定。
function sessionModel() {
  return slice('// ─── 执行会话（一轮）', '// ─── 压缩主流程');
}

/// 「压缩状态机 … 暂停 / 继续 / 停止」整段：状态变量 + 写入口 + 三个按钮的动作。
function stateMachine() {
  return slice('// ─── 压缩状态机', '// ─── 历史记录页');
}

function installQueueCore(context) {
  installTranslationFallback(context);
  // 摘要 / 批量按钮要按 §44 的会话闸门决定「恢复全部」是否可点：
  // 闸门本体属于状态机，队列核心单独使用时也要装同一份真代码。
  vm.runInContext(restoreGate(), context);
  // 队列视图按「压缩后大小」排序时读的是产物判据（没落盘的不许冒充读数），
  // 所以装队列核心必须连契约一起装 —— 否则跑的不是真判据。
  vm.runInContext(productRule(), context);
  vm.runInContext(queueCore(), context);
}

function installQueueRowPainter(context) {
  installTranslationFallback(context);
  vm.runInContext(productRule(), context);
  vm.runInContext(queueRowPainter(), context);
}

/// 把状态机装进测试上下文。必须在其他切片之前调用：它声明
/// compressionState / isCompressing / compressionPaused 三个变量（初值 idle）。
function installStateMachine(context) {
  installTranslationFallback(context);
  vm.runInContext(stateMachine(), context);
}

/// 把执行会话装进上下文。必须在状态机之后（beginExecutionSession 读 compressionState）。
function installSessionModel(context) {
  installTranslationFallback(context);
  vm.runInContext(sessionModel(), context);
}

/// 路径小工具（basename / extname / dirname / imageFileSrc）。
/// 行的"名字 + 所在目录"和详情列的格式标签都从这几个函数读，测试装真的那份。
function installPathUtils(context) {
  vm.runInContext(slice('function basename(p) {', 'function setImageSource('), context);
}

/// 详情列的那几个格子（section / text / specs / callout / actions / preview）。
/// 本轮和历史的详情列共用它们：断言详情列写了什么，就是断言这里。
function installDetailHelpers(context) {
  installTranslationFallback(context);
  vm.runInContext(slice('function detailSection(', 'function renderDetailPane('), context);
}

/// 媒体工作台（图片 / 视频过滤、媒体任务导入）在切片外还有几个全局依赖：
/// mediaFilter / renderStartButton / renderDetailPane / window.crypto /
/// document.querySelectorAll。队列与会话测试只关心队列行为，这里统一补桩，
/// 谁先显式给过值就不覆盖。
function installMediaWorkspaceStubs(context) {
  if (typeof context.mediaFilter === 'undefined') context.mediaFilter = 'all';
  if (typeof context.historyFilter === 'undefined') context.historyFilter = 'all';
  if (typeof context.renderStartButton !== 'function') context.renderStartButton = () => {};
  if (typeof context.renderDetailPane !== 'function') context.renderDetailPane = () => {};
  if (typeof context.renderRunBar !== 'function') context.renderRunBar = () => {};
  if (typeof context.renderIntentBar !== 'function') context.renderIntentBar = () => {};
  if (typeof context.window === 'undefined') context.window = { SizqoI18n: { translateText: value => value } };
  if (typeof context.window.crypto === 'undefined') {
    context.window.crypto = { randomUUID: () => `task-${Math.random().toString(36).slice(2)}` };
  }
  if (context.document && typeof context.document.querySelectorAll !== 'function') {
    context.document.querySelectorAll = () => [];
  }
}

/// 意图编译器是真的（frontend/intents.js）：切片外的措辞函数要读它。
/// 测试给的 imageIntent / videoIntent 只是"用户此刻的选择"，请求每轮现编译 ——
/// 和界面一样，中间不留一份游离的参数。
function installIntentModel(context) {
  context.window = context.window || {};
  context.window.SizqoIntents = require('../frontend/intents.js');
  if (typeof context.isMacOS !== 'function') context.isMacOS = () => false;
  if (!context.imageIntent) {
    context.imageIntent = {
      intent: 'balanced', quality: 75, outputFormat: 'original',
      systemOutputFormat: 'original', systemImageSize: 'actual', preserveMetadata: true,
      maxDimensionPx: null, maxOutputBytes: null,
    };
  }
  if (!context.videoIntent) context.videoIntent = { intent: 'videoSmall', targetMiB: 100 };
  if (typeof context.imageRequestFromIntent !== 'function') {
    context.imageRequestFromIntent = () => context.window.SizqoIntents.compileImage(context.imageIntent, context.isMacOS());
  }
  if (typeof context.videoRequestFromIntent !== 'function') {
    context.videoRequestFromIntent = () => context.window.SizqoIntents.compileVideo(
      context.videoIntent, {}, { normalizeOptions: options => Object.assign({}, options) });
  }
  if (typeof context.runIntentSnapshot === 'undefined') context.runIntentSnapshot = null;
}

/// 「系统模式判据」整段：systemModeActive / requestForRun / requestForNext /
/// systemModeForRun / intentLabelForRun。它们只读意图编译结果和开跑快照，
/// 但主流程、状态机、按钮文案都要调 —— 给一个统一安装入口，
/// 免得各测试各自补一个假判据，跑的已经不是那一句真实的判断。
function installSystemMode(context) {
  installTranslationFallback(context);
  installIntentModel(context);
  vm.runInContext(slice('/// 当前意图是不是系统转换', 'function clearAllFiles('), context);
}

/// 选中一项。真实代码只有一处（paintQueueRow 重画旧行 + 新行），测试装它本身，
/// 不装一个假桩 —— 导入后"详情列不许是空的"正是靠这段真实行为。
/// selectedQueueFile 是 app.js 的全局变量，这里给它的初值。
function installSelection(context) {
  installTranslationFallback(context);
  if (typeof context.selectedQueueFile === 'undefined') context.selectedQueueFile = null;
  vm.runInContext(slice('/// 选中一项：行上只有一条选中线', 'function detailSection('), context);
}

/// 「运行条」：阶段文案 + 体积账 + CPU 摘要，全都只从队列快照和状态机派生。
/// 暂停 / 停止必须在这条栏上留下看得见的痕迹，所以阶段测试装的是真实实现，
/// 不是"反正画什么都不看"的假桩。
function installRunBar(context) {
  installTranslationFallback(context);
  vm.runInContext(slice('function formatBytes(', '// ─── 页面导航'), context);
  vm.runInContext(productRule(), context);
  vm.runInContext(slice('/// 队列的体积账', '/// 选中一项：行上只有一条选中线'), context);
}

/// 「processingActionText」：开始 / 继续 / 进度 / 完成的措辞只由意图与阶段决定。
/// 它不属于任何切片，但状态机切片和运行条都调它，所以给它一个安装入口。
function installActionText(context) {
  installSystemMode(context);
  vm.runInContext(slice('function processingActionText(', '// ─── 意图条'), context);
}

module.exports = {
  source, slice,
  installTranslationFallback,
  queueCore, installQueueCore,
  queueRowPainter, installQueueRowPainter,
  productRule,
  sessionModel, installSessionModel,
  stateMachine, installStateMachine,
  installMediaWorkspaceStubs, installIntentModel, installSystemMode, installActionText,
  installSelection, installRunBar, installPathUtils, installDetailHelpers,
};
