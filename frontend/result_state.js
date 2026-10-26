// ─── 结果契约的唯一解释器（重构方案 §3 §8 §67 §68 §69）─────────────────
//
// 后端把一轮的最终结论写成 `outcome` + `reasonCode`（见 src-tauri/src/result_model.rs）。
// 前端只在这一个文件里读它一次：队列行、详情列、行内按钮、批量工具条、日志、
// 独立对比窗口用的都是这里的判据。
//
// 为什么单独成一个文件，而不是写在 app.js 里：判据必须只有一个住处，而对比窗口
// 是另一个页面、另一份脚本。放在 app.js 里，那个页面就只能自己再猜一遍 ——
// 于是同一项可以一边显示「已完成」、一边显示「立即压缩」。
//
// 这个文件里没有 DOM、没有队列、没有 invoke：它只把一条结果读成事实。

/// §70 措辞出口：优先用调用方所在页面的 localizeUiText（测试会给桩），
/// 其次直接走词典，最后原样给出 —— 判据本身绝不依赖翻译是否成功。
function contractText(value) {
  if (typeof localizeUiText === 'function') return localizeUiText(value);
  if (typeof window !== 'undefined' && window.SizqoI18n) return window.SizqoI18n.translateText(value);
  return value;
}

/// 后端原话只走整句词典：子串替换会把一句人话切成半中半英，比留原文更难读。
function contractBackendText(value) {
  if (typeof localizeBackendText === 'function') return localizeBackendText(value);
  if (typeof window !== 'undefined' && window.SizqoI18n) return window.SizqoI18n.translateMessage(value);
  return value;
}

var OUTCOME_COMMITTED = 'committed';
var OUTCOME_SKIPPED_NO_BENEFIT = 'skipped_no_benefit';
var OUTCOME_SKIPPED_ALREADY = 'skipped_already_under_target';
var OUTCOME_TARGET_UNMET = 'target_unmet';
var OUTCOME_CANCELLED = 'cancelled';
var OUTCOME_FAILED = 'failed';

/// 把一条后端结果读成前端要用的三个事实。任何别的文件都不许自己解析 result。
/// §139 第四阶段：`success` 已删除，也不再按 outputPath 猜"有没有产物"——
/// 没有结论就是没有结论（未产出、Failed、日志写 unknown），绝不当成完成。
function mediaResultFacts(result) {
  if (!result) return null;
  var outcome = typeof result.outcome === 'string' ? result.outcome : null;
  return {
    outcome: outcome,
    reason: typeof result.reasonCode === 'string' ? result.reasonCode : null,
    hasProduct: outcome === OUTCOME_COMMITTED || outcome === OUTCOME_TARGET_UNMET,
    // §52 §71 提交过不等于此刻还在：用户在访达里删掉产物后，前端下一次操作
    // 会把这个事实标上来。它是**磁盘的事实**，不改写结论，只收走指向那个文件的按钮。
    outputMissing: result.outputMissing === true,
  };
}

/// §6.1 这一项在磁盘上到底有没有产物。编码器压出有效字节不等于落了盘：
/// 后端判定"不比原图小"时一个文件都没写；覆盖事务回滚时也会把 outputPath
/// 清成 null。所以"有没有产物"只由结论（outcome）回答。
function resultHasProduct(result) {
  return !!(mediaResultFacts(result) || {}).hasProduct;
}

/// §87 节省率只按字节算，允许负数：变小写 -57.8%，变大写 +20.0% 体积增加。
/// 绝不 clamp 成 0 —— 压完更大也是真实结果，界面不能假装它没发生。
function savingsPercentText(result) {
  if (!result) return '';
  var savings = Number(result.savings);
  if (!Number.isFinite(savings)) {
    var original = Number(result.originalSize != null ? result.originalSize : result.originalBytes);
    var output = Number(result.compressedSize != null ? result.compressedSize : result.outputBytes);
    if (!(original > 0) || !Number.isFinite(output)) return '';
    savings = (original - output) / original * 100;
  }
  var magnitude = Math.abs(savings).toFixed(1) + '%';
  if (savings > 0) return '-' + magnitude;
  if (savings < 0) return '+' + magnitude + ' ' + contractText('体积增加');
  return magnitude;
}

/// §8 §68 一个结果允许哪些操作，只在这里回答一次。
/// 没有产物 = 每一个指向那个文件的按钮都不许出现，而不是"点了再报错"。
function resultCapabilities(result) {
  var facts = mediaResultFacts(result);
  if (!facts || !facts.hasProduct) {
    // 跳过 / 取消 / 失败三者的差别只在"怎么说"，不在"能给什么按钮"。
    return { save: false, compare: false, restore: false, finder: false, retry: true, log: true };
  }
  // §52 产物已经不在磁盘上：另存 / 对比 / 访达指向的都是一个不存在的文件，
  // 只保留「恢复原图」（备份还在的话）和日志。这不是失败，重试也不是它该给的下一步。
  if (facts.outputMissing) {
    return { save: false, compare: false, restore: true, finder: false, retry: false, log: true };
  }
  // §70 「未达目标」既有产物、也还要再压一次：它的下一步正是换个目标再来。
  return {
    save: true, compare: true, restore: true, finder: true, log: true,
    retry: facts.outcome === OUTCOME_TARGET_UNMET,
  };
}

/// §70 结论的用户措辞。措辞是前端的事实：后端那句原话只进悬停和日志，
/// 不再当状态用（以前「原图已是最优，无需替换」既是原因又是状态，换句话就换个显示）。
function resultStatusText(result) {
  var facts = mediaResultFacts(result);
  if (!facts) return '';
  // §52 §71 提交是真的，产物没了也是真的。状态说的是**此刻**：它不再是一个可用的结果。
  if (facts.outputMissing) return contractText('压缩结果已不存在');
  if (facts.outcome === OUTCOME_TARGET_UNMET) return contractText('已完成 · 未达到目标');
  if (facts.outcome === OUTCOME_CANCELLED) return contractText('已取消');
  if (facts.outcome === OUTCOME_FAILED) return contractText('处理失败');
  if (facts.hasProduct) return contractText('已完成');
  if (facts.outcome === OUTCOME_SKIPPED_ALREADY) return contractText('已跳过');
  if (facts.outcome === OUTCOME_SKIPPED_NO_BENEFIT) return contractText('已跳过');
  return contractText('未产出文件');
}

/// §70 结论下面那一行"为什么"。前端只按 reasonCode 选句子，不解析后端文案。
function resultReasonText(result) {
  var facts = mediaResultFacts(result);
  if (!facts) return '';
  if (facts.outputMissing) return contractText('压缩结果已不存在');
  var reason = facts.reason;
  if (reason === 'already_under_target') return contractText('原文件已经小于目标体积');
  if (reason === 'no_size_reduction') return contractText('压缩后没有变小');
  if (reason === 'output_larger_than_source') return contractText('压缩后文件更大');
  if (reason === 'target_unmet') return contractText('未达到目标体积');
  if (reason === 'cancelled') return contractText('这一项被取消，没有开始处理');
  if (reason === 'output_write_failed') return contractText('压缩结果写入失败');
  if (reason === 'history_write_failed') return contractText('历史记录保存失败，已自动恢复原图');
  if (reason === 'unsupported_format') return contractText('这个格式暂不支持');
  if (reason === 'invalid_input') return contractText('输入无效');
  if (reason === 'decode_failed') return contractText('无法解码这个文件');
  if (reason === 'encode_failed') return contractText('编码失败');
  if (reason === 'source_read_failed') return contractText('无法读取源文件');
  if (reason === 'source_not_found') return contractText('源文件不存在');
  if (reason === 'permission_denied') return contractText('没有权限访问这个文件');
  if (reason === 'source_changed') return contractText('源文件在处理期间被改动，已保留你的版本');
  if (reason === 'backup_failed') return contractText('无法保存原图备份');
  if (reason === 'transaction_failed') return contractText('无法登记这次覆盖');
  if (reason === 'rollback_failed') return contractText('自动恢复原图也没成功，备份仍在');
  if (reason === 'output_collision') return contractText('目标位置已有同名文件');
  if (reason === 'output_validation_failed') return contractText('输出校验未通过');
  if (reason === 'output_missing') return contractText('压缩结果已不存在');
  if (reason === 'stopped') return contractText('已停止，未开始');
  if (reason === 'unknown') return contractText('原因未上报');
  // 后端还没迁移的结果：没有判据可说，就不编一句原因。
  return '';
}

/// §2 §67 队列生命周期由**结论**派生，不再由 `success` 派生：
/// Done 不等于"有产物"（跳过、取消都算处理完了这一轮），只有真的坏了才是 Failed。
function taskStateFor(result) {
  var facts = mediaResultFacts(result);
  var outcome = facts && facts.outcome;
  if (!outcome) return 'failed';
  return outcome === OUTCOME_FAILED ? 'failed' : 'done';
}

/// 进度事件里的 status 只是"这一个轮到结尾了"，成/败同样由结论说 ——
/// 会话收尾补偿路径必须和实时事件用同一个派生，否则刷新一次就换个说法。
function taskEventStatusFor(result) {
  return taskStateFor(result) === 'failed' ? 'failed' : 'completed';
}

/// 悬停里说的"为什么"：前端按 reasonCode 选的那句打头，后端原话跟在后面备查。
/// 后端原话只出现在这里和日志里 —— 它不再是任何一处状态或按钮的判据。
function resultDetailTitle(result) {
  var parts = [];
  var reason = resultReasonText(result);
  if (reason) parts.push(reason);
  if (result && result.error) parts.push(contractBackendText(result.error));
  return parts.join(' · ');
}

/// §69 行上的图标由同一份结论派生：图片行与视频行不再各判一次。
/// 三角只说"这一项没有产物"或"产物没达标"；红色只留给真的坏了；
/// 减号留给"这一项到此为止"（被移除 / 被取消）。
function rowStateVisual(item, result) {
  var state = item && item.state;
  if (state === 'running') return { icon: 'spinner', tone: 'running' };
  if (state === 'failed') return { icon: 'error', tone: 'failed' };
  if (state === 'restored') return { icon: 'restore', tone: 'restored' };
  if (state === 'removed') return { icon: 'minus', tone: 'removed' };
  if (state !== 'done') return { icon: 'queue', tone: state || '' };
  var facts = mediaResultFacts(result);
  if (!facts) return { icon: 'check', tone: 'done' };
  if (facts.outputMissing) return { icon: 'warning', tone: 'warning' };
  if (facts.outcome === OUTCOME_CANCELLED) return { icon: 'minus', tone: '' };
  if (!facts.hasProduct || facts.outcome === OUTCOME_TARGET_UNMET) return { icon: 'warning', tone: 'warning' };
  return { icon: 'check', tone: 'done' };
}

/// 日志里那两行结论写的是**契约原词**（outcome / reasonCode），不是中文句子：
/// 日志要能被机器比对，也要在截图里能反查到后端到底判了什么。
/// 没有结论就照实写 unknown，不再从旧旗标反推一个 success。
function resultLogStatus(result) {
  var facts = mediaResultFacts(result);
  if (facts && facts.outcome) return { status: facts.outcome, reason: facts.reason || '' };
  return { status: 'unknown', reason: '' };
}

/// 这一项现在到底能不能算作"压出了一个文件"由 hasProduct 回答；
/// "队列还要不要把它算进账里"（已恢复 / 已移除）是队列的事实，留在 app.js 里判。

(function exportResultContract(scope) {
  if (!scope) return;
  var api = {
    OUTCOME_COMMITTED: OUTCOME_COMMITTED,
    OUTCOME_SKIPPED_NO_BENEFIT: OUTCOME_SKIPPED_NO_BENEFIT,
    OUTCOME_SKIPPED_ALREADY: OUTCOME_SKIPPED_ALREADY,
    OUTCOME_TARGET_UNMET: OUTCOME_TARGET_UNMET,
    OUTCOME_CANCELLED: OUTCOME_CANCELLED,
    OUTCOME_FAILED: OUTCOME_FAILED,
    facts: mediaResultFacts,
    hasProduct: resultHasProduct,
    capabilities: resultCapabilities,
    statusText: resultStatusText,
    reasonText: resultReasonText,
    savingsPercentText: savingsPercentText,
    taskState: taskStateFor,
    taskEventStatus: taskEventStatusFor,
    detailTitle: resultDetailTitle,
    logStatus: resultLogStatus,
    rowStateVisual: rowStateVisual,
  };
  scope.SizqoResultContract = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window
  : (typeof globalThis !== 'undefined' ? globalThis : null));
