// 一轮压完之后，界面必须给出回到"把文件拖进来"那一页的真出口。
//
// 现象：压缩完成后主按钮写着「压缩完成」，按下去什么也不发生（pending 归零后
// startCompression 直接 return）；唯一看得见的出口挂着垃圾桶图标和「清空全部」
// 的确认框，读起来像要删掉刚压好的东西 —— 于是用户找不到回去的路。
// 三条不变量：
//   ① 拖放区只在清单为空时出现：非空时它是队列前面一块永远走不掉的画框；
//   ② 这一轮收口时（total>0 且没有等待中的文件、会话也不在跑）给出「回到导入」，
//      它只收走清单，不碰磁盘，所以不需要确认框；
//   ③ 没有下一步可按时，主按钮就是实况牌，必须按不动，不能装成还能点。
const assert = require('node:assert/strict');
const vm = require('node:vm');
const { source, slice, installQueueCore, installMediaWorkspaceStubs } = require('./app-slices.cjs');

const html = require('node:fs').readFileSync('frontend/index.html', 'utf8');

// ── ① 界面上：图标与文案不再把"移出清单"说成删除 ──────────────────────
const runBar = html.slice(html.indexOf('class="run-bar"'), html.indexOf('</footer>'));
assert.ok(runBar.includes('id="nextRoundBtn"'), '运行条上要有回到导入的出口');
assert.match(runBar, /id="nextRoundBtn"[^>]*hidden/, '#nextRoundBtn 起手必须收起：空队列时它没有意义');
const clearBtn = runBar.slice(runBar.indexOf('id="clearQueueBtn"'), runBar.indexOf('</button>', runBar.indexOf('id="clearQueueBtn"')));
assert.ok(clearBtn.length > 0, '#clearQueueBtn 还在运行条上');
assert.ok(!clearBtn.includes('icon-trash'), '移出清单不该挂垃圾桶：它一个字节都不删');
assert.ok(clearBtn.includes('icon-minus'), '移出清单用「移出」这个图标，和行上那条一致');
assert.ok(!runBar.includes('清除全部'), '「清除全部」这个名字会把"移出清单"读成"删除文件"');

// ── ③ 落点不靠拖放区露面 ─────────────────────────────────────────────
// 拖放事件属于整个 webview：画框收起后文件照样进得来，所以高亮必须画在
// 清单框上，否则就成了「能拖但看不见地方」。
const css = require('node:fs').readFileSync('frontend/style.css', 'utf8');
assert.match(css, /body\.dropping[^{]*\.list-scroll\s*\{/,
  '拖放区收起后，拖入高亮要画在清单框上');
const drag = source.slice(source.indexOf('async function setupDragDrop('), source.indexOf('setupDragDrop();'));
assert.ok(drag.includes("document.body.classList.add('dropping')"), '拖进来时要在界面上留痕迹');
assert.doesNotMatch(drag, /dropzone\.addEventListener/,
  'HTML5 兜底不能只绑拖放区：导入后它收起，落点会跟着一起消失');

// ── ② 行为：跑真实的 updateQueueSummary / renderStartButton / startNextRound ──
function el() {
  return {
    hidden: false, disabled: false, textContent: '', innerHTML: '', className: '', title: '',
    style: {}, children: [], classList: { toggle() {}, add() {}, remove() {} },
  };
}
const elements = {};
const byId = id => elements[id] || (elements[id] = el());
const calls = [];
const paths = ['/a.png', '/b.png', '/c.png'];
const context = vm.createContext({
  console, Set, Map, Promise, JSON, Number, String, Array, Object,
  files: [],
  inputPaths: [], fileRows: {},
  queueItems: new Map(),
  queueRevision: 0, pendingAutoCompress: false, selectedQueueFile: null, runIntentSnapshot: null,
  isCompressing: false, compressionState: 'idle',
  COMPRESSION_IDLE: 'idle', COMPRESSION_RUNNING: 'running',
  COMPRESSION_PAUSED: 'paused', COMPRESSION_STOPPING: 'stopping',
  executionSession: null, confirm: () => true, cpuStatus: null, mediaFilter: 'all',
  document: { getElementById: byId, querySelectorAll: () => [] },
  window: { SizqoI18n: { translateText: value => value } },
  applyQueueView() {}, renderRunBar() {}, iconMarkup: name => '<svg></svg>',
  progressButtonMarkup: () => '', processingActionText: stage => stage,
  localizeUiText: value => value, cpuLimitText: () => '',
  emitCompareResultsChanged() {}, showView(name) { calls.push(['showView', name]); },
  showToast(message) { calls.push(['toast', message]); },
  invoke: async (command, args) => { calls.push([command, args]); return null; },
});
installMediaWorkspaceStubs(context);
installQueueCore(context);
vm.runInContext(slice('function updateQueueSummary(', 'function toggleQueueSortDirection('), context);
vm.runInContext(slice('function renderStartButton(', 'function setPauseButtonVisible('), context);
vm.runInContext(slice('function clearAllFiles(', '// ─── Compression'), context);

const queue = states => {
  context.files = paths.slice();
  context.queueItems = new Map(paths.map((p, i) => [p, {
    path: p, mediaKind: 'image', state: states[i], result: null, originalSize: 10,
  }]));
  context.updateQueueSummary();
};

// 空清单：拖放区就是那一页的主体，回到导入没有意义。
context.updateQueueSummary();
assert.equal(byId('dropzone').hidden, false, '清单为空时拖放区必须露着');
assert.equal(byId('nextRoundBtn').hidden, true, '什么都没压过时不该有「回到导入」');
assert.equal(byId('startCompressBtn').disabled, false);

// 有活儿在等：清单把拖放区让位，主按钮可按，回到导入还不该出现。
queue(['pending', 'pending', 'pending']);
assert.equal(byId('dropzone').hidden, true, '清单非空时拖放区不许继续压在队列前面');
assert.equal(byId('nextRoundBtn').hidden, true, '还有等待中的文件时按「回到导入」会把没干的活儿收走');
assert.equal(byId('startCompressBtn').disabled, false, '有待处理文件时主按钮必须真的能按');

// 压完：拖放区仍然收着（清单还在），但界面必须给出出口，主按钮不能再装成能按。
queue(['done', 'done', 'done']);
assert.equal(byId('dropzone').hidden, true);
assert.equal(byId('nextRoundBtn').hidden, false, '这一轮收口后必须看得见回到导入的出口');
assert.equal(byId('startCompressBtn').disabled, true, '「压缩完成」是实况，不是一个按下去没反应的按钮');

// 停止后还剩没跑的活儿：出口收回，主按钮回到「继续压缩」。
queue(['done', 'pending', 'pending']);
assert.equal(byId('nextRoundBtn').hidden, true);
assert.equal(byId('startCompressBtn').disabled, false);

// 会话还在跑时不给出口（按下去会连未开始的一起收走）。
queue(['done', 'done', 'done']);
context.isCompressing = true;
context.updateQueueSummary();
assert.equal(byId('nextRoundBtn').hidden, true, '压缩中不许出现回到导入');
context.isCompressing = false;

// 按下去：清单收走、拖放区回来、切回工作台，且不发任何删文件的请求。
context.startNextRound();
assert.equal(context.files.length, 0);
assert.equal(context.queueItems.size, 0, '真实代码里 files 与 queueItems 必须一起清空，否则拖放区按 files 判据就露不出来');
assert.equal(byId('dropzone').hidden, false, '回到导入 = 拖放区重新出现');
assert.equal(context.queueRevision, 1, '旧会话的事件要靠 revision 挡掉，收走清单必须推进它');
assert.deepEqual(calls.filter(c => c[0] === 'showView'), [['showView', 'main']]);
assert.ok(calls.some(c => c[0] === 'toast' && c[1].includes('历史')), '收走清单后要说清结果去哪了');
assert.equal(calls.filter(c => c[0] === 'invoke').length, 0, '回到导入不碰磁盘，一个请求都不许发');

// 「移出清单」仍然只发一次整批取消，且文案不再吓人。
context.files = paths.slice();
context.queueItems = new Map(paths.map(p => [p, { path: p, state: 'pending', result: null, originalSize: 10 }]));
context.isCompressing = true;
context.executionSession = { targetPaths: paths.slice() };
const asked = [];
context.confirm = message => { asked.push(message); return true; };
context.clearAllFiles();
assert.ok(asked[0].includes('移出清单'), `确认框要说清只是移出清单：${asked[0]}`);
assert.ok(!asked[0].includes('清空全部'), '不再用"清空全部"这种读起来像删文件的说法');

console.log('PASS: 拖放区随清单显隐、收口一轮给出回到导入的出口、完成态主按钮不再装成能按');
