// 「引擎压成功了」和「磁盘上有这个文件」是两件事，界面上不许把它们混成一件事。
//
// 现象（用户截图）：一张图压完，行上写「-0.0% · mozjpeg」和 2.1MB，运行条写
// 「压缩完成 · 1 / 1」「省 0B · 0.0% · 1 个产物」；按下行上那个垃圾桶，换来一句
// 「找不到这条历史记录」。后端过去在这里既报 success=true 又什么都不写，
// 前端只能从 success / outputPath / error 文案里猜 —— 三个地方猜出三种说法。
//
// 现在这些猜测全部收敛到一个解释器：结论（outcome）+ 原因码（reasonCode），
// 它就住在 frontend/result_state.js —— 主窗口和独立对比窗口读的是同一份。
// 这份测试跑的是那几个真实函数，断言的是**六种结论各自的呈现**。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {
  source, slice,
  installMediaWorkspaceStubs, installQueueCore, installQueueRowPainter,
  installStateMachine, installActionText, installRunBar, installPathUtils,
} = require('./app-slices.cjs');

const strip = js => js.split('\n').filter(line => !/^\s*(?:\/\/|\/\*|\*)/.test(line)).join('\n');

// ── §140 明令禁止的那几种"补丁式"写法，一条都不许回来 ──────────────────
const code = strip(source);
for (const forbidden of [
  /outputPath\s*={2,3}\s*null/,          // 用"没路径"当跳过的判据
  /\.error\s*(?:\.includes|indexOf)\(/,   // 嗅后端文案当状态
  /targetMet\s*&&|if\s*\(\s*result\.success\s*&&\s*!queueProducts/,
]) {
  assert.ok(!forbidden.test(code), `§140 禁止的写法又出现了：${forbidden}`);
}

// ── §68 两个窗口共用一份判据：对比窗口不许在自己页面里再猜一次 ─────────
// 独立对比窗口是一个单独的 HTML 页面、单独一份 JS，它拿的是同一批 result 对象。
// 以前它按 `r && r.success` 挑"可对比的结果"，于是"没落盘的成功"被送进对比视图，
// 右边那张图指向一个磁盘上不存在的文件。这里钉两件事：脚本必须装上，判据必须问契约。
const frontend = path.join(__dirname, '..', 'frontend');
for (const page of ['index.html', 'compare.html']) {
  const markup = fs.readFileSync(path.join(frontend, page), 'utf8');
  assert.ok(markup.includes('<script src="result_state.js"></script>'), `${page} 没装上结果契约，页面各判一次`);
}
const compareCode = strip(fs.readFileSync(path.join(frontend, 'compare_window.js'), 'utf8'));
assert.match(compareCode, /filter\(resultHasProduct\)/, '对比窗口挑可对比结果时只问契约');
assert.doesNotMatch(compareCode.replace(/outcome\.success/g, ''), /\.success\b/,
  '对比窗口里唯一允许读 success 的地方，是恢复命令自己的结论（outcome.success）');

// §139 第四阶段：CompressResult.success 已删除，契约里不许再出现旧旗标或它的兜底。
const contractCode = strip(fs.readFileSync(path.join(frontend, 'result_state.js'), 'utf8'));
assert.doesNotMatch(contractCode, /\bresult\.success\b|legacyHasProduct/,
  '结果契约里还留着 success 的旧读法');

// ── 判据本身：五种结论的呈现全在同一张表里，跑真实代码 ─────────────────
function el() {
  const node = {
    hidden: false, disabled: false, className: '', title: '', style: {}, children: [], attrs: {},
    classList: {
      set: new Set(),
      add(...names) { names.forEach(name => node.classList.set.add(name)); },
      remove(...names) { names.forEach(name => node.classList.set.delete(name)); },
      toggle(name, force) {
        const on = force === undefined ? !node.classList.set.has(name) : !!force;
        on ? node.classList.set.add(name) : node.classList.set.delete(name);
      },
      contains: name => node.classList.set.has(name),
    },
    appendChild(child) { node.children.push(child); return child; },
    addEventListener() {},
    setAttribute(key, value) { node.attrs[key] = value; },
    querySelector(selector) {
      node.parts = node.parts || {};
      if (!node.parts[selector]) node.parts[selector] = el();
      return node.parts[selector];
    },
  };
  Object.defineProperty(node, 'innerHTML', {
    get: () => node._html || '',
    set(value) { node._html = value; if (value === '') node.children.length = 0; },
  });
  Object.defineProperty(node, 'textContent', {
    get: () => node._text || '',
    set(value) { node._text = String(value); node.children.length = 0; },
  });
  return node;
}
const ids = {};
const byId = id => ids[id] || (ids[id] = el());
const PHANTOM = '/m/1tA7yJTT3Ts.jpg';
const RESTORED = '/m/录制于 2026-10-16.mp4';
const REAL = '/m/ok.jpg';
const context = vm.createContext({
  console, Set, Map, Promise, JSON, Math, Date, Number, String, Array, Object, isNaN,
  files: [PHANTOM, RESTORED],
  inputPaths: [PHANTOM, RESTORED], fileRows: {}, queueRevision: 0,
  queueItems: new Map(),
  isCompressing: false, compressionState: 'idle', executionSession: null,
  selectedQueueFile: null, runIntentSnapshot: null, pendingAutoCompress: false,
  cpuStatus: null, mediaFilter: 'all', confirm: () => true,
  document: {
    getElementById: byId,
    createElement: () => el(),
    querySelector: () => null,
    querySelectorAll: () => [],
  },
  window: { SizqoI18n: { translateText: value => value }, crypto: { randomUUID: () => 't' } },
  iconMarkup: name => `<svg>${name}</svg>`,
  progressButtonMarkup: () => '',
  localizeUiText: value => value,
  localizeBackendText: value => value,
  cpuLimitText: () => '',
  showToast() {}, showView() {}, emitCompareResultsChanged() {},
  applyQueueView() {}, renderFileQueue() {}, showErrorDetail() {},
  invoke: async () => null,
});
installMediaWorkspaceStubs(context);
installStateMachine(context);
installQueueCore(context);
installPathUtils(context);
installQueueRowPainter(context);
installRunBar(context);
installActionText(context);
vm.runInContext(slice('function renderStartButton(', 'function setPauseButtonVisible('), context);
vm.runInContext(slice('function renderQueueResultActions(', 'function copyCompressLog('), context);
const read = expression => vm.runInContext(expression, context);

/// 后端在"没有值得提交的产物"那一支真正交回的载荷：结论说清楚了 ——
/// 盘上什么都没发生，所以没有 outputPath，也没有旧 success 旗标。
const skippedResult = {
  file: PHANTOM, originalSize: 2202009, compressedSize: 2205000, savings: 0,
  type: 'jpg', algorithm: 'mozjpeg',
  error: '压缩后 2.1MB > 原始 2.1MB，原图已是最优压缩',
  outputPath: null, backupPath: null, outputMode: null,
  outcome: 'skipped_no_benefit', reasonCode: 'output_larger_than_source',
};
const committedResult = {
  file: REAL, originalSize: 2048000, compressedSize: 1024000, savings: 50,
  type: 'jpg', algorithm: 'mozjpeg', error: null,
  outputPath: '/m/ok_compressed.jpg', outputMode: 'suffix',
  outcome: 'committed', reasonCode: null,
};
const settledVideo = {
  file: RESTORED, success: true, originalBytes: 5000000, outputBytes: 3000000, savings: 40,
  outputPath: '/m/录制于 2026-10-16_compressed.mp4', outputMode: 'suffix', historyId: 'h1',
  outcome: 'committed', reasonCode: null,
};
function seed(items) {
  context.files = items.map(([path]) => path);
  context.queueItems = new Map(items.map(([path, mediaKind, state, result]) => [path, {
    path, mediaKind, state, result, originalSize: result ? (result.originalSize || result.originalBytes) : 10,
    id: path, taskId: path, videoProbe: null, coverUrl: null, videoProgress: null,
  }]));
  context.fileRows = {};
  items.forEach(([path]) => { context.fileRows[path] = el(); });
  context.updateQueueSummary();
}
const rowOf = path => context.fileRows[path];
const actionTitles = path => rowOf(path).querySelector('.media-actions').children.map(node => node.title);
const PRODUCT_BUTTONS = ['另存为', '对比查看', '删除这次压缩结果', '恢复原图', '在访达中显示'];

// ── ① 五种结论各自的呈现，一张表读到底 ────────────────────────────────
const CASES = [
  { outcome: 'committed', reasonCode: null, status: '已完成', state: 'done', hasProduct: true, retry: false },
  { outcome: 'target_unmet', reasonCode: 'target_unmet', status: '已完成 · 未达到目标', state: 'done', hasProduct: true, retry: true },
  { outcome: 'skipped_no_benefit', reasonCode: 'output_larger_than_source', status: '已跳过', state: 'done', hasProduct: false, retry: true },
  { outcome: 'skipped_already_under_target', reasonCode: 'already_under_target', status: '已跳过', state: 'done', hasProduct: false, retry: true },
  { outcome: 'cancelled', reasonCode: 'cancelled', status: '已取消', state: 'done', hasProduct: false, retry: true },
  { outcome: 'failed', reasonCode: 'source_read_failed', status: '处理失败', state: 'failed', hasProduct: false, retry: true },
];
for (const item of CASES) {
  const result = { file: '/m/x.jpg', outcome: item.outcome, reasonCode: item.reasonCode, outputPath: item.hasProduct ? '/m/x_c.jpg' : null };
  assert.equal(read(`resultStatusText(${JSON.stringify(result)})`), item.status, `${item.outcome} 的状态措辞`);
  assert.equal(read(`taskStateFor(${JSON.stringify(result)})`), item.state, `${item.outcome} 的队列生命周期`);
  assert.equal(read(`resultHasProduct(${JSON.stringify(result)})`), item.hasProduct, `${item.outcome} 有没有产物`);
  const caps = read(`JSON.stringify(resultCapabilities(${JSON.stringify(result)}))`);
  const parsed = JSON.parse(caps);
  assert.equal(parsed.retry, item.retry, `${item.outcome} 的重试按钮`);
  for (const button of ['save', 'compare', 'restore', 'finder']) {
    assert.equal(parsed[button], item.hasProduct, `${item.outcome} 的 ${button} 按钮只跟着产物走`);
  }
  assert.equal(parsed.log, true, `${item.outcome} 永远可以复制日志`);
}
// §141 不变量：有产物 ⇔ outcome 是 committed / target_unmet。
for (const outcome of ['committed', 'target_unmet', 'skipped_no_benefit', 'skipped_already_under_target', 'cancelled', 'failed']) {
  const withPath = read(`resultHasProduct({file:'/a', outcome:${JSON.stringify(outcome)}, outputPath:'/a_c.jpg'})`);
  const withoutPath = read(`resultHasProduct({file:'/a', outcome:${JSON.stringify(outcome)}, outputPath:null})`);
  assert.equal(withPath, withoutPath, `${outcome} 的判据不许被 outputPath 左右（§6.1）`);
}

// ── ② 那一行：没落盘就不许报产物的大小和降幅，也不许给出指向产物的按钮 ──
seed([[PHANTOM, 'image', 'done', skippedResult], [RESTORED, 'video', 'restored', settledVideo]]);
context.paintQueueRow(PHANTOM);
assert.equal(rowOf(PHANTOM).querySelector('.media-meta').textContent, '已跳过');
assert.match(rowOf(PHANTOM).querySelector('.media-meta').title, /压缩后文件更大/,
  '悬停先说按 reasonCode 选的那句');
assert.equal(rowOf(PHANTOM).querySelector('.media-size').textContent, '2.1MB',
  '行上的大小必须是原图，不是那串从没写出去的数字');
assert.equal(rowOf(PHANTOM).querySelector('.media-size').title, '没有产物 · 原图未改动');
assert.match(rowOf(PHANTOM).querySelector('.media-state-icon').className, /warning/,
  '三角是"没产出"，不是绿色对勾，也不是红色失败');
const skippedTitles = actionTitles(PHANTOM);
assert.ok(skippedTitles.includes('重试'), `没产出文件至少还能重试：${skippedTitles}`);
for (const forbidden of PRODUCT_BUTTONS) {
  assert.ok(!skippedTitles.includes(forbidden), `${forbidden} 指的是磁盘上不存在的文件，不该出现在这一行上`);
}

// ── ③ 取消不是失败：行上给减号、说「已取消」，但状态仍然是 Done ────────
const cancelledResult = Object.assign({}, skippedResult, {
  outcome: 'cancelled', reasonCode: 'cancelled', error: null,
});
seed([[PHANTOM, 'image', 'done', cancelledResult]]);
context.paintQueueRow(PHANTOM);
assert.equal(rowOf(PHANTOM).querySelector('.media-meta').textContent, '已取消');
assert.equal(rowOf(PHANTOM).querySelector('.media-state-icon').className, 'media-state-icon',
  '取消的行不该挂红色的失败底色');
assert.doesNotMatch(actionTitles(PHANTOM).join('|'), /删除这次压缩结果/);

// ── ④ 未达目标：产物是真的，按钮是真的，"再来一次"也是真的 ────────────
const targetUnmetResult = {
  file: REAL, originalSize: 3145728, compressedSize: 3145728 * 0.6, savings: 40,
  type: 'jpg', algorithm: 'mozjpeg', outputPath: '/m/ok_c.jpg', outputMode: 'suffix',
  outcome: 'target_unmet', reasonCode: 'target_unmet', targetBytes: 1048576, targetMet: false,
};
seed([[REAL, 'image', 'done', targetUnmetResult]]);
context.paintQueueRow(REAL);
assert.match(rowOf(REAL).querySelector('.media-meta').textContent, /未达到|未达/, '结论行要说未达标');
const unmetTitles = actionTitles(REAL);
assert.ok(unmetTitles.includes('重试') && unmetTitles.includes('删除这次压缩结果'),
  `未达目标既有产物也能重来：${unmetTitles}`);

// ── ⑤ 账：从没落盘的、以及已经撤销掉的产物，都不许记 ──────────────────
seed([[PHANTOM, 'image', 'done', skippedResult], [RESTORED, 'video', 'restored', settledVideo]]);
assert.equal(byId('runGain').textContent, '', '「1 个产物」在这种队列上是一句假话');
assert.equal(byId('restoreAllBtn').hidden, true, '没有产物就不该给"恢复全部原图"');
assert.equal(byId('exportAllBtn').hidden, true, '没有产物就不该给"全部导出"');

// ── ⑥ 数字要和看得见的行对得上：已恢复的那一行不能从总数里凭空消失 ────
assert.equal(byId('queueSummary').textContent, '1 / 1 已处理 · 已恢复 1');
assert.equal(context.getQueueProgress().restored, 1);

// ── ⑦ 阶段文案与主按钮：这一轮完成的只是"试过" ────────────────────────
assert.equal(byId('runPhase').textContent, '未产出文件 · 1 / 1');
assert.equal(byId('startCompressBtn').disabled, true);
assert.match(byId('compressBtnText').innerHTML, /未产出文件/);
assert.doesNotMatch(byId('compressBtnText').innerHTML, /压缩完成/,
  '一个产物都没有，按钮上不许挂"压缩完成"');

// ── ⑧ 同一套判据在"真有产物"时必须照常放行 —— 否则就是把它改成了永远说没有
seed([[PHANTOM, 'image', 'done', skippedResult], [REAL, 'image', 'done', committedResult]]);
context.paintQueueRow(REAL);
assert.match(actionTitles(REAL).join('|'), /删除这次压缩结果/, '真落盘的产物照样能删');
assert.equal(byId('runPhase').textContent, '压缩完成 · 2 / 2');
assert.match(byId('runGain').textContent, /1 个产物/, '账上只数那一个真在盘上的产物');
assert.doesNotMatch(byId('runGain').textContent, /2 个产物/);
assert.match(byId('runGain').textContent, /省 /);
assert.match(byId('compressBtnText').innerHTML, /压缩完成/);

// ── ⑨ 混合队列：视频产物 + 无产物图片，「N 个产物」只数真在盘上的 ──────
seed([[PHANTOM, 'image', 'done', skippedResult],
  [RESTORED, 'video', 'done', Object.assign({}, settledVideo, { outputPath: '/m/v_c.mp4' })]]);
assert.match(byId('runGain').textContent, /1 个产物/,
  `用户截图里那句「2 个产物」就是这么来的：${byId('runGain').textContent}`);

// ── ⑩ §139 第四阶段：success 已删除，没有结论的载荷不再被猜成完成 ──────
// 即使旧载荷还带着 success=true 与 outputPath，前端也只看 outcome；
// 没有 outcome 就没有产物、没有完成，日志照实写 unknown。
const legacy = { file: '/m/old.jpg', success: true, originalSize: 10, compressedSize: 5, outputPath: '/m/old_c.jpg' };
assert.equal(read(`resultHasProduct(${JSON.stringify(legacy)})`), false,
  '没有结论就没有产物，旧旗标和路径都不许替它作证');
assert.equal(read(`taskStateFor(${JSON.stringify(legacy)})`), 'failed',
  '没有结论不许读成 Done');
assert.equal(read(`resultStatusText(${JSON.stringify(legacy)})`), '未产出文件',
  '没有结论的行说「未产出文件」，不说「已完成」');
assert.equal(read(`resultLogStatus(${JSON.stringify(legacy)}).status`), 'unknown',
  '日志写契约原词 unknown，不反推 success');

// ── ⑪ §52 提交过、但产物已经被删掉：结论不变，指向那个文件的按钮全部收走 ──
// 这是**磁盘的事实**，不是新的结果：hasProduct 仍是 true（那次提交真的发生过），
// 只是此刻没有一个可用的文件。前端把它标在 result.outputMissing 上。
const missingOutput = Object.assign({}, committedResult, { outputMissing: true });
assert.equal(read(`resultStatusText(${JSON.stringify(missingOutput)})`), '压缩结果已不存在');
assert.equal(read(`resultReasonText(${JSON.stringify(missingOutput)})`), '压缩结果已不存在');
assert.equal(read(`resultHasProduct(${JSON.stringify(missingOutput)})`), true,
  '产物曾经提交过是历史事实，不许因为文件被删就改写成"从未产出"');
const missingCaps = JSON.parse(read(`JSON.stringify(resultCapabilities(${JSON.stringify(missingOutput)}))`));
assert.deepEqual(
  [missingCaps.save, missingCaps.compare, missingCaps.finder, missingCaps.retry],
  [false, false, false, false],
  '另存 / 对比 / 访达 / 重试 都指向一个不存在的文件');
assert.equal(missingCaps.restore, true, '备份还在时仍然能恢复原图');
assert.equal(missingCaps.log, true, '日志永远可以复制');
assert.equal(read(`rowStateVisual({state:'done'}, ${JSON.stringify(missingOutput)}).tone`), 'warning',
  '缺产物的行是三角，不是绿色对勾');
seed([[REAL, 'image', 'done', missingOutput]]);
context.paintQueueRow(REAL);
assert.equal(rowOf(REAL).querySelector('.media-meta').textContent, '压缩结果已不存在');
assert.equal(rowOf(REAL).querySelector('.media-size').textContent, '2.0MB',
  '行上只报还在盘上的原图大小，不报那个已经不在的产物大小');
const missingTitles = actionTitles(REAL);
for (const gone of ['另存为', '对比查看', '在访达中显示', '重试']) {
  assert.ok(!missingTitles.includes(gone), `${gone} 指向不存在的文件，不该出现：${missingTitles}`);
}
assert.ok(missingTitles.includes('复制日志'), `日志还在：${missingTitles}`);

console.log('PASS: 结论（outcome + reasonCode）是行、账、按钮、阶段文案的唯一出处 —— 六种结论各有真实呈现');
