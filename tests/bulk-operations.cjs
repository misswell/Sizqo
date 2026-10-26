// §49 §52 §77 §78 §79：批量恢复 / 导出 / 另存 / 对比的输入、输出与磁盘复核。
//
// 这份测试跑的是 frontend/app.js 的真实切片：队列的产物判据来自 result_state.js，
// 批量参数来自 queueProducts()，摘要来自 bulkSummaryText()，缺产物标记来自
// markQueueOutputMissing()。前端不许在这里另写一套"成功/失败"的读法。
const assert = require('node:assert/strict');
const vm = require('node:vm');
const {
  slice, installStateMachine, installQueueCore, installQueueRowPainter,
  installMediaWorkspaceStubs, installPathUtils,
} = require('./app-slices.cjs');

function makeEl() {
  const node = {
    children: [], style: {}, dataset: {}, attrs: {}, title: '', hidden: false, className: '',
    classList: {
      _set: new Set(),
      add(...names) { names.forEach(name => node.classList._set.add(name)); },
      remove(...names) { names.forEach(name => node.classList._set.delete(name)); },
      toggle(name, force) {
        const on = force === undefined ? !node.classList._set.has(name) : !!force;
        on ? node.classList._set.add(name) : node.classList._set.delete(name);
      },
      contains: name => node.classList._set.has(name),
    },
    appendChild(child) { node.children.push(child); return child; },
    addEventListener() {},
    setAttribute(key, value) { node.attrs[key] = value; },
    querySelector(selector) {
      node._parts = node._parts || {};
      if (!node._parts[selector]) node._parts[selector] = makeEl();
      return node._parts[selector];
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

const IMAGE_A = '/Pictures/a.jpg';
const IMAGE_B = '/Pictures/b.jpg';
const VIDEO_C = '/Videos/c.mp4';
const IMAGE_D = '/Pictures/d.jpg';

const toasts = [];
const invoked = [];
const exists = new Map();
let exportReply = null;
let restoreAllReply = null;
let saveReply = '/Users/me/Desktop/a.jpg';

const context = vm.createContext({
  console, Set, Map, Promise, JSON, Math, Date, Number, String, Array, Object, isNaN,
  files: [], fileRows: {}, queueItems: new Map(), queueRevision: 0,
  selectedQueueFile: null, isCompressing: false, compressionState: 'idle', executionSession: null,
  cpuStatus: null, mediaFilter: 'all', historyFilter: 'all', confirm: () => true,
  document: {
    getElementById: () => null,
    createElement: () => makeEl(),
    querySelector: () => null,
    querySelectorAll: () => [],
  },
  window: {},
  iconMarkup: name => `<svg>${name}</svg>`,
  progressSpinnerMarkup: () => '<i></i>',
  localizeUiText: value => value,
  localizeBackendText: value => value,
  showToast: message => toasts.push(message),
  cue: () => '',
  emitCompareResultsChanged() {},
  renderDetailPane() {},
  renderRestoredActions() {},
  refreshHistoryIfOpen() {},
  getResultOutputSuffix: () => '_compressed',
  restoreBlockedBySession: () => false,
  restoreBlockedText: () => '',
  copyCompressLog() {},
  invoke: async (command, args) => {
    invoked.push([command, args]);
    if (command === 'path_exists') {
      return exists.has(args.filePath) ? exists.get(args.filePath) : true;
    }
    if (command === 'export_all') return exportReply;
    if (command === 'restore_all') return restoreAllReply;
    if (command === 'save_file') return saveReply;
    if (command === 'open_compare_window') return null;
    if (command === 'open_in_finder' || command === 'open_media_file') return null;
    return null;
  },
});
installMediaWorkspaceStubs(context);
installStateMachine(context);
installQueueCore(context);
installQueueRowPainter(context);
installPathUtils(context);
vm.runInContext(slice('async function pathExists(', '// ─── 页面导航'), context);
vm.runInContext(slice('async function compressOneFile(', 'async function pathExists('), context);
vm.runInContext(slice('function renderQueueResultActions(', 'function copyCompressLog('), context);
vm.runInContext(slice('function comparableResults(', '// 结果集变化（恢复/清空/压缩完成等）'), context);

const flush = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
const last = name => [...invoked].reverse().find(call => call[0] === name);
const calls = name => invoked.filter(call => call[0] === name);
const actionTitles = path => context.fileRows[path].querySelector('.media-actions').children
  .map(node => node.title);

function imageResult(file, outputPath, extra) {
  return Object.assign({
    file, originalSize: 2048, compressedSize: 1024, savings: 50, type: 'jpg',
    algorithm: 'mozjpeg', outputMode: 'suffix', outputPath, outcome: 'committed',
  }, extra || {});
}
function videoResult(file, outputPath, extra) {
  return Object.assign({
    mediaKind: 'video', file, sourcePath: file, outputPath, outputMode: 'suffix',
    outcome: 'committed', reasonCode: null, historyId: 'h-c',
    originalBytes: 5000, outputBytes: 2500, originalSize: 5000, compressedSize: 2500,
    savings: 50, encoderActual: 'libx264',
  }, extra || {});
}
function seed(items) {
  context.files = items.map(item => item.path);
  context.queueItems = new Map(items.map(item => [item.path, {
    path: item.path, id: item.path, taskId: item.path, state: item.state,
    mediaKind: item.result && item.result.mediaKind, result: item.result,
    originalSize: item.result ? (item.result.originalSize || item.result.originalBytes) : 10,
    videoProbe: null, coverUrl: null, videoProgress: null,
  }]));
  context.fileRows = {};
  items.forEach(item => { context.fileRows[item.path] = makeEl(); });
  context.updateQueueSummary();
}

(async () => {
  // ── §77 批量恢复只收「有产物」的项，失败 / 跳过 / 已恢复一个都不进 ─────────
  seed([
    { path: IMAGE_A, state: 'done', result: imageResult(IMAGE_A, '/Pictures/a_c.jpg') },
    { path: IMAGE_B, state: 'done', result: imageResult(IMAGE_B, '/Pictures/b_c.jpg', { outcome: 'target_unmet' }) },
    { path: IMAGE_D, state: 'failed', result: imageResult(IMAGE_D, null, { outcome: 'failed', outputPath: null }) },
    { path: '/Pictures/e.jpg', state: 'restored', result: imageResult('/Pictures/e.jpg', '/Pictures/e_c.jpg') },
  ]);
  invoked.length = 0;
  toasts.length = 0;
  restoreAllReply = {
    operation: 'restore', success: true, successCount: 2, failedCount: 0,
    missingCount: 0, skippedCount: 0, warningCount: 0, items: [],
    restoredFiles: [IMAGE_A, IMAGE_B], message: '（后端中文摘要不参与措辞）',
  };
  await context.restoreAllOriginals();
  const restoreCall = last('restore_all');
  assert.deepEqual(Array.from(restoreCall[1].results, result => result.file), [IMAGE_A, IMAGE_B],
    '失败 / 已恢复的项连后端都不该进，批量语义不靠后端再过滤一次');
  assert.equal(toasts[toasts.length - 1], '恢复完成 · 成功 2',
    '摘要按真实计数在前端措辞，不念后端那句中文');
  assert.equal(context.queueItems.get(IMAGE_A).state, 'restored');
  assert.equal(context.queueItems.get(IMAGE_B).state, 'restored');
  assert.equal(context.queueItems.get(IMAGE_D).state, 'failed', '失败项不许被"恢复全部"顺手改掉');
  assert.equal(calls('restore_original').length, 0, '恢复全部不走单文件命令');

  // ── §49 §52 §78 导出混合媒体：视频也进批量，缺产物的项单独计数并标回行 ────
  seed([
    { path: IMAGE_A, state: 'done', result: imageResult(IMAGE_A, '/Pictures/a_c.jpg') },
    { path: IMAGE_B, state: 'done', result: imageResult(IMAGE_B, '/Pictures/b_c.jpg') },
    { path: VIDEO_C, state: 'done', result: videoResult(VIDEO_C, '/Videos/c_c.mp4') },
    { path: IMAGE_D, state: 'failed', result: imageResult(IMAGE_D, null, { outcome: 'failed', outputPath: null }) },
  ]);
  invoked.length = 0;
  toasts.length = 0;
  exists.set('/Pictures/a_c.jpg', false);
  exportReply = {
    operation: 'export', success: false, successCount: 2, failedCount: 0,
    missingCount: 1, skippedCount: 0, warningCount: 0,
    items: [
      { file: IMAGE_A, success: false, reasonCode: 'output_missing', outputPath: null, alreadyRestored: false },
      { file: IMAGE_B, success: true, reasonCode: null, outputPath: '/Pictures/b_compressed.jpg', alreadyRestored: false },
      { file: VIDEO_C, success: true, reasonCode: null, outputPath: '/Videos/c_compressed.mp4', alreadyRestored: false },
    ],
    restoredFiles: [], message: '（后端中文摘要不参与措辞）',
  };
  await context.exportAll();
  const exportCall = last('export_all');
  assert.deepEqual(Array.from(exportCall[1].results, result => result.file),
    [IMAGE_A, IMAGE_B, VIDEO_C],
    '图片和视频共用一条批量契约；失败项不进清单');
  assert.equal(toasts[toasts.length - 1], '导出完成 · 成功 2 · 文件不存在 1',
    '成功与"文件不存在"必须分开报');
  assert.equal(context.queueItems.get(IMAGE_A).result.outputMissing, true,
    '导出时复核出的缺失要标回结果，下一次点击不再撞同一个空文件');
  assert.equal(context.queueItems.get(IMAGE_B).result.outputMissing, undefined);
  const afterMissing = actionTitles(IMAGE_A);
  assert.ok(!afterMissing.includes('另存为') && !afterMissing.includes('对比查看')
    && !afterMissing.includes('在访达中显示'), `缺产物的行不许再给产物按钮：${afterMissing}`);

  // ── §52 §53 另存为 / 访达 / 对比在动手前复核磁盘，不拿旧路径撞错误 ───────
  seed([
    { path: IMAGE_A, state: 'done', result: imageResult(IMAGE_A, '/Pictures/a_c.jpg') },
    { path: IMAGE_B, state: 'done', result: imageResult(IMAGE_B, '/Pictures/b_c.jpg') },
    { path: VIDEO_C, state: 'done', result: videoResult(VIDEO_C, '/Videos/c_c.mp4') },
  ]);
  invoked.length = 0;
  toasts.length = 0;
  exists.clear();

  await context.saveResult(IMAGE_A);
  assert.equal(last('save_file')[1].sourcePath, '/Pictures/a_c.jpg');
  assert.equal(toasts[toasts.length - 1], '已保存到: a.jpg');

  // 保存时文件刚好被删：后端回 output_missing，前端把结果标成缺产物，而不是报"保存失败"。
  exists.set('/Pictures/a_c.jpg', false);
  invoked.length = 0;
  toasts.length = 0;
  await context.saveResult(IMAGE_A);
  assert.equal(context.queueItems.get(IMAGE_A).result.outputMissing, true);
  assert.equal(toasts[toasts.length - 1], '压缩结果已不存在');
  assert.equal(calls('save_file').length, 0, '已知源不在就不必再弹一次另存对话框');

  // 对比只接受真实存在的图片产物：视频不进这个窗口，缺产物的图片也不进。
  exists.clear();
  context.queueItems.get(IMAGE_A).result.outputMissing = true;
  assert.deepEqual(Array.from(context.comparableResults(), result => result.file), [IMAGE_B],
    '同批对比里不许混入视频或已缺失的产物');
  invoked.length = 0;
  context.openCompareByFile(IMAGE_B);
  await flush();
  const compareCall = last('open_compare_window');
  assert.deepEqual(Array.from(compareCall[1].payload.results, result => result.file), [IMAGE_B]);
  assert.equal(compareCall[1].payload.index, 0, '下标按点中的文件重新定位');

  // 点中的那一项产物已被删除：不开窗，直接把结果标成缺失。
  context.queueItems.get(IMAGE_B).result.outputMissing = false;
  exists.set('/Pictures/b_c.jpg', false);
  invoked.length = 0;
  toasts.length = 0;
  await context.openCompare(context.queueItems.get(IMAGE_B).result);
  await flush();
  assert.equal(calls('open_compare_window').length, 0, '产物不存在就不打开一个注定空白的窗口');
  assert.equal(context.queueItems.get(IMAGE_B).result.outputMissing, true);
  assert.equal(toasts[toasts.length - 1], '压缩结果已不存在');

  // 访达同理：目标不在就不调用后端，且把缺失标回该行。
  seed([{ path: IMAGE_B, state: 'done', result: imageResult(IMAGE_B, '/Pictures/b_c.jpg') }]);
  exists.set('/Pictures/b_c.jpg', false);
  invoked.length = 0;
  toasts.length = 0;
  await context.openInFinder(IMAGE_B);
  assert.equal(calls('open_in_finder').length, 0, '不存在的位置不该交给访达');
  assert.equal(context.queueItems.get(IMAGE_B).result.outputMissing, true);
  assert.equal(toasts[toasts.length - 1], '压缩结果已不存在');

  // ── §79 批量恢复的逐项告警也进摘要，不把"恢复成功 + 产物没删掉"报成纯成功 ──
  seed([
    { path: IMAGE_A, state: 'done', result: imageResult(IMAGE_A, '/Pictures/a_c.jpg') },
    { path: IMAGE_B, state: 'done', result: imageResult(IMAGE_B, '/Pictures/b_c.jpg') },
  ]);
  invoked.length = 0;
  toasts.length = 0;
  restoreAllReply = {
    operation: 'restore', success: false, successCount: 1, failedCount: 1,
    missingCount: 0, skippedCount: 0, warningCount: 1,
    items: [], restoredFiles: [IMAGE_A], message: '（后端中文摘要不参与措辞）',
  };
  await context.restoreAllOriginals();
  assert.equal(toasts[toasts.length - 1], '恢复完成 · 成功 1 · 失败 1 · 已恢复但压缩产物未能删除 1',
    '成功、失败、告警三个数都必须出现');
  assert.equal(context.queueItems.get(IMAGE_A).state, 'restored');
  assert.equal(context.queueItems.get(IMAGE_B).state, 'done', '没被后端确认恢复的行不许提前标记');

  // ── §75 恢复后重新压缩 = 新的一次尝试：旧产物路径 / 旧 historyId 不许被带过去 ──
  seed([{ path: IMAGE_A, state: 'done', result: imageResult(IMAGE_A, '/Pictures/a_c.jpg', { historyId: 'h-old' }) }]);
  context.queueItems.get(IMAGE_A).attemptCount = 1;
  let atStart = null;
  context.startCompressionForPaths = async (single, paths) => {
    atStart = {
      single, paths: Array.from(paths), state: context.queueItems.get(IMAGE_A).state,
      result: context.queueItems.get(IMAGE_A).result,
    };
    context.queueItems.get(IMAGE_A).result = imageResult(IMAGE_A, '/Pictures/a_new.jpg', { historyId: 'h-new' });
    context.queueItems.get(IMAGE_A).state = 'done';
    context.queueItems.get(IMAGE_A).attemptCount = 2;
    // 真实实现会返回「这一轮是否真的开跑」；桩模拟开跑成功。
    return true;
  };
  await context.compressOneFile(IMAGE_A);
  assert.deepEqual(atStart.paths, [IMAGE_A], '重压只这一项，不重跑整轮');
  assert.equal(atStart.state, 'pending', '重压前必须回到 pending');
  assert.equal(atStart.result, null, '旧 outputPath / historyId 不许带进新一次尝试');
  assert.equal(context.queueItems.get(IMAGE_A).result.historyId, 'h-new');
  assert.notEqual(context.queueItems.get(IMAGE_A).result.historyId, 'h-old');
  assert.equal(context.queueItems.get(IMAGE_A).attemptCount, 2, '新尝试是一次新的计数，不是旧结果的续用');

  console.log('PASS: 批量恢复/导出只收有产物的项；缺产物标记、磁盘复核与逐项计数在真实代码上成立');
})();
