// 历史页：密集列表、状态文案、只发 historyId 的恢复调用，以及冲突后的强制重试。
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const {
  slice, installStateMachine, installQueueCore, installQueueRowPainter, installMediaWorkspaceStubs,
  installPathUtils, installDetailHelpers,
} = require('./app-slices.cjs');

function makeEl(tag) {
  let html = '';
  const node = {
    tagName: tag, children: [], style: {}, dataset: {}, attrs: {},
    className: '', title: '', hidden: false, handlers: {},
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
    addEventListener(type, fn) { node.handlers[type] = fn; },
    setAttribute(key, value) { node.attrs[key] = value; },
    getAttribute(key) { return node.attrs[key]; },
    querySelector(selector) {
      node._parts = node._parts || {};
      if (!node._parts[selector]) node._parts[selector] = makeEl('span');
      return node._parts[selector];
    },
  };
  Object.defineProperty(node, 'innerHTML', {
    get: () => html,
    set(value) { html = value; if (value === '') node.children.length = 0; },
  });
  Object.defineProperty(node, 'textContent', {
    get: () => node._text || '',
    set(value) { node._text = String(value); node.children.length = 0; },
  });
  return node;
}

const ids = {
  mainView: makeEl('div'), historyView: makeEl('div'), settingsView: makeEl('div'),
  historyViewBtn: makeEl('button'), settingsViewBtn: makeEl('button'),
  historyList: makeEl('div'), historyEmpty: makeEl('div'), historyMeta: makeEl('span'),
  historyClearBtn: makeEl('button'), historyDetailBody: makeEl('div'),
  compressBtnText: makeEl('span'), pauseCompressBtn: makeEl('button'), pauseBtnText: makeEl('span'),
  queueSummary: makeEl('span'), restoreAllBtn: makeEl('button'), retentionDays: makeEl('select'),
  retentionCopy: makeEl('span'),
};
const invoked = [];
let historyReply = [];
let retentionReply = 0;   // 后端默认档位：不保留
let restoreReply = { success: true, conflict: false, filePath: '/Pictures/a.png', historyIds: ['1'] };
let restoreAllReply = { success: true, restored: 0, failed: 0, message: '', restoredFiles: [] };
let confirmAnswer = true;
const toasts = [];

const context = vm.createContext({
  console, Set, Map, Promise, JSON, Math, Date, Number, String, Array, Object, isNaN,
  files: ['/Pictures/a.png', '/Pictures/b.png'], fileRows: {},
  queueItems: new Map(['/Pictures/a.png', '/Pictures/b.png'].map(path => [path, {
    path, state: 'done', result: null, originalSize: 2048576,
  }])),
  compressBtnText: makeEl('span'), startCompressBtn: makeEl('button'),
  compressBtnFill: makeEl('div'),
  // 两条选中线：本轮队列一项、历史一项。真实代码里它们是 app.js 的全局变量，
  // 切片只切到用它们的那几行，所以初值由测试给。
  selectedQueueFile: null, selectedHistoryKey: null,
  confirm: () => confirmAnswer,
  document: {
    getElementById: id => ids[id] || null,
    createElement: makeEl,
    querySelector: () => null,
    addEventListener() {},
  },
  iconMarkup: name => '<svg>' + name + '</svg>',
  showToast: message => toasts.push(message),
  copyTextToClipboard: () => true,
  // updateQueueSummary / renderStartButton / applyQueueView 都是真实实现（队列那一段），
  // 只把 CPU 摘要补成空串 —— 这个测试不关心它。
  cpuLimitText: () => '',
  emitCompareResultsChanged() {}, showResults() {},
  renderRestoredActions() {},
  applyQueueView() {}, updateBulkActionButtons() {}, setPauseButtonVisible() {},
  renderPauseControls() {}, initUpdatePanel() {},
  // 详情列的预览格会真的去解码原图，这个测试只关心格子上写了什么字。
  loadOriginalImage() {},
  loadCpuSetting: async () => {},
  invoke: async (command, args) => {
    invoked.push([command, args]);
    if (command === 'list_history') return historyReply;
    if (command === 'get_app_settings') return { originalRetentionDays: retentionReply };
    if (command === 'set_original_retention_days') {
      retentionReply = args.days;
      return { originalRetentionDays: args.days };
    }
    if (command === 'restore_all') return restoreAllReply;
    if (command === 'restore_history_entry' || command === 'restore_original') {
      // 后端只在没被 force 时报冲突，否则就是死循环了。
      return restoreReply.conflict && !args.force
        ? restoreReply
        : { success: true, conflict: false, filePath: '/Pictures/a.png',
            historyIds: ['e1'], outputMode: 'replace' };
    }
    return null;
  },
});
installMediaWorkspaceStubs(context);
installStateMachine(context);
installQueueCore(context);
installQueueRowPainter(context);
installPathUtils(context);
installDetailHelpers(context);
vm.runInContext(slice('function imageTypeLabel(', '/// 来源行只写'), context);
vm.runInContext(slice('function formatBytes(', '// ─── 页面导航'), context);
vm.runInContext(slice('var VIEWS = ', '// ─── 压缩状态机'), context);
vm.runInContext(slice('var RESTORE_CONFLICT_TEXT', 'async function exportAll('), context);
vm.runInContext(slice('function renderQueueResultActions(', 'function copyCompressLog('), context);
vm.runInContext(slice('var historyEntries = [];', '// ─── 设置页'), context);
vm.runInContext(
  slice('// ─── 设置页：原图备份保留时间', '// ─── 设置页：CPU 使用上限'), context);

const flush = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
const entry = extra => Object.assign({
  id: 'e1', createdAt: Date.now(), expiresAt: 0,
  sourcePath: '/Pictures/a.png', outputPath: '/Pictures/a.png', fileName: 'a.png',
  outputMode: 'replace', originalSize: 2048576, compressedSize: 204800, savings: 90,
  outType: 'webp', algorithm: 'webp-mozquant', backupPath: '/app/backup/a.png',
  status: 'compressed', restoredAt: null, outputModifiedAt: 0,
  sourceExists: true, backupExists: true, outputExists: true,
}, extra);
const text = node => {
  const own = node.textContent || '';
  const kids = node.children.length ? node.children.map(text).join(' ') : '';
  // querySelector 在桩里是按需造一个空壳子节点（innerHTML 不真解析），
  // 真实代码往这些壳子上写字段，断言也要把它们算进这一行的文本。
  const parts = node._parts ? Object.keys(node._parts).map(key => text(node._parts[key])).join(' ') : '';
  return [own, kids, parts].filter(Boolean).join(' ');
};
const last = name => [...invoked].reverse().find(call => call[0] === name);
/// 一行的按钮身份：dataset.historyAction 就是它要做的事。
/// 按 .media-actions 这个格子找，不按第几个子节点数 —— 布局改了断言不该跟着抖。
const actionsOf = row => row.querySelector('.media-actions').children
  .map(btn => btn.dataset.historyAction);

(async () => {
  historyReply = [entry({}), entry({ id: 'e2', fileName: 'b.png', sourcePath: '/Pictures/b.png' })];
  await context.refreshHistory();
  // 历史行与本轮队列行共用同一套格子：同一 kinds 的事实放同一列。
  assert.deepEqual(ids.historyList.children.map(row => row.className),
    ['media-item done', 'media-item done']);
  assert.equal(ids.historyList.children.length, 2, 'renders newest first, as stored');
  assert.equal(ids.historyMeta.textContent, '2 条');
  assert.equal(ids.historyEmpty.hidden, true);
  const row = ids.historyList.children[0];
  assert.match(text(row), /a\.png/);
  assert.match(text(row), /\/Pictures/);
  assert.match(text(row), /webp-mozquant/);
  assert.match(text(row), /已压缩/);
  // 密集列表里一行只占一列宽度：这一列报**产物多大**，原始→产物的那一对是它的出处，
  // 挂在 title 上；省下多少进详情列的大数字，不挤在行里。
  assert.equal(row.querySelector('.media-size').textContent, '200.0KB');
  assert.equal(row.querySelector('.media-size').title, '2.0MB → 200.0KB');

  // 选中第一项 = 详情列不是空的：这条记录此刻在磁盘上还剩什么，全在那里。
  assert.equal(context.selectedHistoryKey, 'e1', '刷新历史必须自动选中第一条');
  const detail = text(ids.historyDetailBody);
  assert.match(detail, /-90\.0%/, '节省率是详情列的大数字');
  assert.match(detail, /2\.0MB → 200\.0KB/);
  assert.match(detail, /a\.png/);
  assert.match(detail, /原图备份/);

  // 一行的按钮 = 这条记录此刻真能做到的事；只有覆盖过且备份、原位置都在才谈得上恢复。
  assert.deepEqual(actionsOf(row), ['save', 'compare', 'restore', 'finder', 'log'],
    '覆盖模式：与压缩完成那一行同一套动作');

  const suffix = entry({ id: 'e3', outputMode: 'suffix', status: 'compressed',
    outputPath: '/Pictures/a_compressed.png' });
  ids.historyList.children = [];
  context.historyEntries = [suffix];
  context.renderHistory();
  const suffixRow = ids.historyList.children[0];
  assert.ok(text(suffixRow).includes('原图未覆盖'));
  // 后缀模式的原图从没被盖过：给它「恢复原图」是假的，对等的反悔是删掉这次产物。
  assert.deepEqual(actionsOf(suffixRow), ['save', 'compare', 'deleteOutput', 'finder', 'log']);
  assert.ok(!actionsOf(suffixRow).includes('restore'), '后缀模式绝不许出现恢复原图');

  // 删除这次压缩结果 = 同一个后端服务、同样只交 historyId，前端不碰文件路径。
  invoked.length = 0;
  toasts.length = 0;
  suffixRow.querySelector('.media-actions').children[2].handlers.click();
  await flush();
  // 跨 vm 上下文的对象比不了引用，逐字段来。
  assert.equal(last('restore_history_entry')[1].historyId, 'e3');
  assert.equal(last('restore_history_entry')[1].force, false,
    '非覆盖模式没有"原图被改过"这回事，不该偷偷 force 什么');
  assert.match(toasts[toasts.length - 1], /已删除这次压缩结果/, '不许把删产物报成恢复原图');
  assert.doesNotMatch(toasts[toasts.length - 1], /已恢复原图/);

  // 用户自己把压缩产物删了：指向它的那几个按钮一起收起。
  assert.deepEqual(
    actionsOf(context.historyRow(entry({ id: 'e4', outputMode: 'suffix',
      outputPath: '/Pictures/a_compressed.png', outputExists: false }))),
    ['finder', 'log'],
    '后缀模式下产物没了，这一行就不配再有反悔按钮');
  // 但覆盖模式不一样：产物被删了，原图备份还在 → 恢复必须照给，那才是这条记录的意义。
  assert.ok(actionsOf(context.historyRow(entry({ outputExists: false }))).includes('restore'),
    '压缩结果被删不影响"换回原图"');
  // 备份被清掉：恢复不许继续挂在页面上骗人，也不能退化成删源文件。
  assert.deepEqual(
    actionsOf(context.historyRow(entry({ backupExists: false }))),
    ['save', 'finder', 'log'],
    '原图备份已清理 → 恢复和对比一起收起');
  // 已恢复的记录：反悔已经用掉了，产物这层身份也一起没了 ——
  // 覆盖模式下 outputPath 指向的已经是写回后的原图，绝不能再拿它另存为/对比。
  const restoredRow = context.historyRow(entry({ status: 'restored', restoredAt: entry().createdAt }));
  assert.deepEqual(actionsOf(restoredRow), ['finder', 'log'],
    '已恢复的行只剩定位与日志：另存为 / 对比 / 播放输出都把原图当成了产物');
  assert.ok(text(restoredRow).includes('已恢复 · 今天'));
  // §48 恢复成功但产物没删掉：状态行必须同时说清这两件事，且同样不再有产物按钮。
  const warnedRow = context.historyRow(entry({ status: 'restoredWithWarning', restoredAt: entry().createdAt }));
  assert.ok(text(warnedRow).includes('已恢复'));
  assert.ok(text(warnedRow).includes('压缩结果未能删除'));
  assert.deepEqual(actionsOf(warnedRow), ['finder', 'log']);
  assert.ok(text(context.historyRow(entry({ sourceExists: false }))).includes('原文件位置不存在'));
  assert.ok(text(context.historyRow(entry({ backupExists: false }))).includes('原图备份已清理'));
  // §76 产物被外部删除：状态必须直接说这件事；备份还在就仍可恢复，备份也没了就只剩日志。
  const missingOutput = context.historyRow(entry({ outputExists: false, sourceExists: false }));
  assert.ok(text(missingOutput).includes('输出文件不存在'));
  assert.deepEqual(actionsOf(missingOutput), ['restore', 'log'],
    '产物没了但备份还在：恢复仍然是这条记录唯一有意义的动作');
  const incomplete = context.historyRow(entry({ outputExists: false, sourceExists: false, backupExists: false }));
  assert.ok(text(incomplete).includes('记录不完整 · 原图备份已清理'));
  assert.deepEqual(actionsOf(incomplete), ['log'],
    '产物、原图、备份都不在：不许留下任何指向不存在文件的按钮');

  // 主队列那一行的反悔按钮同样跟着输出方式走文案：后缀模式删的是产物，不是"恢复原图"。
  vm.runInContext(slice('function renderQueueResultActions(', 'function copyCompressLog('), context);
  const queueTitles = mode => {
    const queueRow = makeEl('div');
    context.renderQueueResultActions(queueRow, {
      file: '/Pictures/a.png', outcome: 'committed', outputMode: mode,
      outputPath: '/Pictures/a.png',
    });
    return queueRow.querySelector('.media-actions').children.map(b => b.title);
  };
  assert.ok(queueTitles('replace').includes('恢复原图'), '覆盖模式仍然叫恢复原图');
  const suffixTitles = queueTitles('suffix');
  assert.ok(!suffixTitles.includes('恢复原图'), '后缀模式的队列行不许写恢复原图');
  assert.ok(suffixTitles.includes('删除这次压缩结果'));

  // ── history.json 损坏后按备份重建出来的条目：明细丢了，但原图还能一键恢复 ──
  const recovery = context.historyRow(
    entry({ id: 'recovery-k1', status: 'recoveryAvailable', savings: 0, algorithm: 'recovery' }));
  const recoveryText = text(recovery);
  assert.ok(recoveryText.includes('明细已丢失（按原图备份重建）'), '重建条目在行上就写清明细没了');
  assert.doesNotMatch(recoveryText, /节省/, '没有真实明细就不许报一个算出来的 0.0%');
  // 重建条目连压缩明细都没有，产物按钮（另存为/对比）一律不许出现，只留恢复。
  assert.deepEqual(actionsOf(recovery), ['restore', 'finder', 'log'],
    '重建条目只能恢复原图，不许拿源文件冒充产物');
  // 详情列把"为什么没有明细"讲完整，而不是只给一个状态词。
  context.historyEntries = [entry({ id: 'recovery-k1', status: 'recoveryAvailable', savings: 0, algorithm: 'recovery' })];
  context.selectedHistoryKey = 'recovery-k1';
  ids.historyDetailBody.children.length = 0;
  ids.historyDetailBody._parts = {};
  context.renderHistoryDetail();
  assert.ok(text(ids.historyDetailBody).includes('这条记录是从备份目录重建的'));
  assert.ok(!text(ids.historyDetailBody).includes('0.0%'), '重建条目不许编一个节省率');
  assert.deepEqual(
    actionsOf(context.historyRow(
      entry({ id: 'r2', status: 'recoveryAvailable', backupExists: false }))),
    ['finder', 'log'],
    '备份已被清掉的重建条目不该再挂恢复按钮，也不许冒出产物按钮');

  // 冲突：先确认，再带 force 重来；取消则一发都不发。
  restoreReply = { success: false, conflict: true, filePath: '/Pictures/a.png', error: '这个文件在压缩后又被修改过', historyIds: [] };
  confirmAnswer = false;
  await context.restoreFromHistory(entry());
  assert.equal(last('restore_history_entry')[1].force, false, 'first attempt must not force');
  assert.ok(![...invoked].some(call => call[1] && call[1].force === true), '取消后不得强制覆盖');
  confirmAnswer = true;
  await context.restoreFromHistory(entry());
  const forced = last('restore_history_entry');
  assert.equal(forced[1].force, true, 'confirming retries with force');
  assert.deepEqual(
    Object.keys(forced[1]).sort(), ['acceptLegacy', 'force', 'historyId'],
    '前端不拼路径，只交 historyId（acceptLegacy 是 §2.3 旧备份确认位，false 也是显式交）');

  // 主队列的「恢复原图」必须走同一个后端服务：前端只交 filePath，路径由后端从历史里取。
  restoreReply = { success: true, conflict: false, filePath: '/Pictures/a.png', historyIds: ['e1'] };
  const rowA = makeEl('div');
  context.fileRows['/Pictures/a.png'] = rowA;
  context.queueItems.set('/Pictures/a.png', {
    path: '/Pictures/a.png', state: 'done', originalSize: 2048576,
    result: { file: '/Pictures/a.png', outcome: 'committed', backupPath: '/app/backup/a.png', outputPath: '/Pictures/a.png' },
  });
  await context.restoreOriginal('/Pictures/a.png');
  assert.deepEqual(
    Object.keys(last('restore_original')[1]).sort(), ['acceptLegacy', 'filePath', 'force'],
    '主队列只交 filePath（acceptLegacy 是 §2.3 旧备份确认位）');
  assert.equal(context.queueItems.get('/Pictures/a.png').state, 'restored', '恢复过的项离开结果集');
  assert.equal(context.queueItems.get('/Pictures/a.png').result, null, '结果一并清掉');
  assert.equal(rowA.querySelector('.media-meta').textContent, '已恢复');

  // 恢复一条旧历史时，同路径新导入的待压缩项仍是新任务，不能跟着变 restored。
  context.queueItems.set('/Pictures/a.png', {
    path: '/Pictures/a.png', state: 'pending', result: null, originalSize: 2048576,
  });
  context.markQueueRowRestored('/Pictures/a.png');
  assert.equal(context.queueItems.get('/Pictures/a.png').state, 'pending',
    '历史恢复只允许 done → restored，不吞掉同路径新任务');

  // 恢复全部：一次后端调用，按后端回报的文件逐行标记。
  const rowB = makeEl('div');
  context.fileRows['/Pictures/b.png'] = rowB;
  // 队列里三条：两条成功可恢复、一条失败（失败的那条不该被带走）。
  context.queueItems = new Map([
    ['/Pictures/a.png', { path: '/Pictures/a.png', state: 'done', originalSize: 1,
      result: { file: '/Pictures/a.png', outcome: 'committed' } }],
    ['/Pictures/b.png', { path: '/Pictures/b.png', state: 'done', originalSize: 1,
      result: { file: '/Pictures/b.png', outcome: 'committed' } }],
    ['/Pictures/c.png', { path: '/Pictures/c.png', state: 'failed', originalSize: 1,
      result: { file: '/Pictures/c.png', outcome: 'failed' } }],
  ]);
  context.files = ['/Pictures/a.png', '/Pictures/b.png', '/Pictures/c.png'];
  invoked.length = 0;
  restoreAllReply = { success: true, successCount: 2, failedCount: 0, missingCount: 0,
    skippedCount: 0, warningCount: 0, items: [], message: '恢复完成：成功 2 个',
    restoredFiles: ['/Pictures/a.png', '/Pictures/b.png'] };
  await context.restoreAllOriginals();
  const all = last('restore_all');
  // §77 批量恢复的输入是「有产物」的项：失败项连循环都不该进。
  assert.equal(all[1].results.length, 2, '只把有产物的项交给后端，不在后端伪造一遍过滤');
  assert.deepEqual(Array.from(all[1].results, result => result.file), ['/Pictures/a.png', '/Pictures/b.png']);
  assert.ok(!invoked.some(call => call[0] === 'restore_original'), '恢复全部不再走单文件命令');
  assert.equal(context.queueItems.get('/Pictures/b.png').state, 'restored');
  assert.match(toasts[toasts.length - 1], /恢复完成 · 成功 2/, '批量摘要按真实计数说，不念后端那句中文');
  // Array.from：queueResults() 的数组造在 vm 那个 realm 里，跨 realm 比不了原型。
  assert.deepEqual(Array.from(context.queueResults(), item => item.file),
    ['/Pictures/c.png'], '失败行留在队列里');

  // 页面切换只改 pane 的 hidden：队列数据与正在跑的任务一概不动。
  context.showView('history');
  await flush();
  assert.equal(context.currentView, 'history');
  assert.deepEqual(context.files, ['/Pictures/a.png', '/Pictures/b.png', '/Pictures/c.png']);
  assert.equal(ids.mainView.hidden, true);
  assert.equal(ids.historyView.hidden, false);
  context.showView('settings');
  assert.equal(ids.settingsView.hidden, false);
  assert.equal(ids.historyView.hidden, true, '离开历史页要把它收起来');
  context.showView('nonsense');
  assert.equal(ids.mainView.hidden, false, 'unknown view falls back to main');

  // ── 原图备份保留时间：「不保留」= 0，是一个真实档位而不是"没设置" ──
  // 清理只有一个时机：关闭应用。两条文案都不许再说"下次启动"。
  retentionReply = 7;
  await context.loadRetentionSetting();
  assert.equal(ids.retentionDays.value, '7');
  assert.match(ids.retentionCopy.textContent, /过期的历史记录和原图备份将在关闭应用时自动清理/);
  assert.doesNotMatch(ids.retentionCopy.textContent, /下次启动/, '按天保留不该承诺下次启动才生效');
  assert.doesNotMatch(ids.retentionCopy.textContent, /本次运行/, '按天档该说过期窗口，不是本次会话');
  retentionReply = 0;
  await context.loadRetentionSetting();
  assert.equal(ids.retentionDays.value, '0', '0 不能被真值判断吞掉，停在 7 天');
  assert.match(ids.retentionCopy.textContent, /本次运行的压缩记录和原图备份都会在关闭应用时清理/);
  assert.match(ids.retentionCopy.textContent, /恢复原图/, '要说清楚这期间仍可恢复');

  // 选「不保留」必须原样传 0 —— `parseInt(...) || 3` 会把它偷偷变成 3 天。
  invoked.length = 0;
  toasts.length = 0;
  ids.retentionDays.value = '0';
  ids.retentionDays.handlers.change();
  await flush();
  assert.equal(last('set_original_retention_days')[1].days, 0);
  assert.match(toasts[toasts.length - 1], /不保留/);
  assert.match(toasts[toasts.length - 1], /关闭应用时清理记录和备份/, '要说清记录也一起走');
  assert.doesNotMatch(toasts[toasts.length - 1], /保留 0 天/, '不许说"保留 0 天"这种半截话');

  ids.retentionDays.value = '3';
  ids.retentionDays.handlers.change();
  await flush();
  assert.equal(last('set_original_retention_days')[1].days, 3);
  assert.match(toasts[toasts.length - 1], /关闭应用时清理过期项/);
  assert.match(ids.retentionCopy.textContent, /关闭应用时自动清理/);

  const settingsHtml = fs.readFileSync('frontend/index.html', 'utf8');
  assert.match(settingsHtml, /<option value="0">不保留<\/option>/);
  assert.ok(
    settingsHtml.indexOf('<option value="0">') < settingsHtml.indexOf('<option value="1">'),
    '「不保留」是默认档，排第一');
  // HTML 里的静态文案 = 默认档那一句：设置读回来之前那一瞬也不许先承诺"下次启动"。
  assert.ok(
    settingsHtml.includes('本次运行的压缩记录和原图备份都会在关闭应用时清理'),
    '默认文案要与 retentionDays === 0 的分支逐字一致');
  assert.doesNotMatch(settingsHtml, /下次启动应用时自动清理/);

  // ── 压缩进行中不许清空历史：后端会拒绝，前端先收成不可点，别让人撞报错 ──
  context.historyEntries = [entry({})];
  context.setCompressionState('running', true);
  context.renderHistory();
  assert.equal(ids.historyClearBtn.disabled, true, '批次跑着的时候清空按钮必须不可点');
  assert.match(ids.historyClearBtn.title, /压缩进行中/);
  invoked.length = 0;
  toasts.length = 0;
  await context.clearHistory();
  assert.ok(!invoked.some(call => call[0] === 'clear_history'), '前端就不该发这次清空');
  assert.match(toasts[toasts.length - 1], /压缩进行中/);

  // ── §44 压缩进行中不许恢复：会话与历史管理是两条生命周期 ──────────────
  // 恢复会改写当前队列（done → restored），进度就从 5/10 跳成 5/9。
  context.setCompressionState('running', true);
  context.historyEntries = [entry({})];
  context.selectedHistoryKey = 'e1';
  context.renderHistory();
  const blockedActions = [...ids.historyList.children[0].querySelector('.media-actions').children];
  const restoreButton = blockedActions.find(btn => btn.dataset.historyAction === 'restore');
  assert.ok(restoreButton, '历史行本来有恢复按钮，禁用不是把它藏起来');
  assert.equal(restoreButton.disabled, true, '§44 会话期间历史行恢复必须禁用');
  assert.match(restoreButton.title, /压缩进行中/);

  // 队列批量按钮同样禁用：队列里有一条可恢复的产物。
  context.queueItems = new Map([['/Pictures/a.png', {
    path: '/Pictures/a.png', state: 'done', originalSize: 1,
    result: { file: '/Pictures/a.png', outcome: 'committed', outputPath: '/Pictures/a.png' },
  }]]);
  context.files = ['/Pictures/a.png'];
  context.updateBulkActionButtons();
  assert.equal(ids.restoreAllBtn.disabled, true, '§44 会话期间「恢复全部原图」必须禁用');
  assert.match(ids.restoreAllBtn.title, /压缩进行中/);

  // 三个入口就算绕过按钮也一个都不许发后端调用。
  invoked.length = 0;
  toasts.length = 0;
  await context.restoreOriginal('/Pictures/a.png');
  await context.restoreAllOriginals();
  await context.restoreFromHistory(entry());
  assert.ok(!invoked.some(call => ['restore_original', 'restore_all', 'restore_history_entry'].includes(call[0])),
    '§44 会话期间恢复 / 恢复全部 / 历史恢复都不许落到后端');
  assert.equal(toasts.length, 3, '三次尝试各自说明为什么被拒');
  toasts.forEach(message => assert.match(message, /压缩进行中/));

  // 后端是最后一道闸：对比窗口等入口绕过按钮，也必须在统一服务里被拒。
  const rustHistory = fs.readFileSync(
    require('node:path').join(__dirname, '..', 'src-tauri', 'src', 'history.rs'), 'utf8');
  const rustBlocked = (rustHistory.match(/pub const RESTORE_BLOCKED_MESSAGE: &str = "([^"]+)"/) || [])[1];
  assert.equal(rustBlocked, '压缩进行中，暂不能恢复；请先停止或等这一轮结束',
    '§44 Rust 的拒绝原话必须与前端一致');
  const rustCommands = fs.readFileSync(
    require('node:path').join(__dirname, '..', 'src-tauri', 'src', 'commands.rs'), 'utf8');
  const restoreService = (rustCommands.match(/fn restore_entry[\s\S]*?let mut reauth/) || [''])[0];
  // §2.7：闸门已升级为维护租约 —— 与会话启动在同一原子状态上仲裁，
  // "检查时空闲、动手前批次开跑"的窗口从结构上不存在。
  assert.match(restoreService, /state\.compression\.begin_maintenance\(\)/,
    '§44 统一恢复服务必须持有维护租约，才能与压缩会话真正互斥');
  assert.match(restoreService, /RestoreError::SessionBusy/,
    '§44 租约拿不到时仍然返回 SessionBusy');

  context.setCompressionState('idle', true);
  context.renderHistory();
  const idleRestore = [...ids.historyList.children[0].querySelector('.media-actions').children]
    .find(btn => btn.dataset.historyAction === 'restore');
  assert.equal(idleRestore.disabled, false, '§44 会话结束后恢复按钮必须放出来');
  invoked.length = 0;
  await context.restoreFromHistory(entry());
  assert.ok(invoked.some(call => call[0] === 'restore_history_entry'), '空闲时恢复照常走后端');

  context.setCompressionState('idle', true);
  context.renderHistory();
  assert.equal(ids.historyClearBtn.disabled, false, '批次结束后必须恢复可用');
  invoked.length = 0;
  // 清空确认框要数得清这一次带走几份原图备份 —— 「手动清理，不用等过期」是这一键的全部含义。
  let question = '';
  context.confirm = message => { question = message; return true; };
  context.historyEntries = [entry({}), entry({ id: 'e9', backupExists: false })];
  await context.clearHistory();
  context.confirm = () => confirmAnswer;
  assert.match(question, /立即删除 Sizqo 保存的 1 份原图备份/);
  assert.match(question, /不会删除你的任何图片文件/);
  assert.ok(invoked.some(call => call[0] === 'clear_history'), '空闲时清空照常走后端');

  console.log('PASS: dense history rows, 每行按钮=这条记录真能做的事, 重建条目可恢复, restore without paths, conflict force retry, queue restore shares the service, 不保留档位, 压缩中拒绝清空');
})().catch(error => { console.error(error); process.exitCode = 1; });
