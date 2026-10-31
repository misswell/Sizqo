const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const { installQueueCore } = require('./app-slices.cjs');
const source = fs.readFileSync('frontend/app.js', 'utf8');
// 装订 applyQueueView 全段（含 attachQueueView / 虚拟窗口 / 脏标记助手）。
const begin = source.indexOf('function toggleQueueSortDirection(');
const end = source.indexOf('async function renderFileQueue(');
assert.ok(begin >= 0 && end > begin, 'applyQueueView 切片标记必须存在');
const slice = source.slice(begin, end);

/// §8.3 虚拟列表：>200 项 + 真实滚动容器 = 只附着一屏 ± 20 行；
/// 行 DOM 与业务状态分离，滚走的结果行照样持有数据。
const TOTAL = 10000;
const files = Array.from({ length: TOTAL }, (_, i) => '/v' + String(i).padStart(6, '0') + '.mp4');

function fakeRow(file) {
  const row = {
    file,
    hidden: false,
    parentNode: null,
    offsetHeight: 60,
    classList: { toggle() {} },
    // 与真实 DOM 同语义：remove() 必须把自己从父节点的 children 里摘掉。
    remove() {
      if (this.parentNode) {
        const siblings = this.parentNode.children;
        const index = siblings.indexOf(row);
        if (index >= 0) siblings.splice(index, 1);
      }
      this.parentNode = null;
    },
  };
  return row;
}

const scroller = {
  scrollTop: 0,
  clientHeight: 600,
  __sizqoScrollHooked: false,
  listeners: {},
  addEventListener(name, fn) { this.listeners[name] = fn; },
  closest(selector) { return selector === '.list-scroll' ? scroller : null; },
};
const list = {
  children: [],
  closest(selector) { return selector === '.list-scroll' ? scroller : null; },
  appendChild(row) { row.parentNode = this; this.children.push(row); },
  insertBefore(row, before) {
    row.parentNode = this;
    this.children = this.children.filter(item => item !== row);
    const index = before ? this.children.indexOf(before) : -1;
    if (index >= 0) this.children.splice(index, 0, row);
    else this.children.push(row);
  },
};

const queueItems = new Map(files.map(file => [file, { path: file, state: 'pending' }]));
const context = vm.createContext({
  files,
  fileRows: {},
  queueItems,
  queueSortDescending: false,
  mediaFilter: 'all',
  basename: file => file.split('/').pop(),
  iconMarkup: name => '<svg>' + name + '</svg>',
  resultHasProduct: () => false,
  document: {
    getElementById: id => (id === 'fileQueueList' ? list : null),
    createElement: tag => ({ id: '', style: {}, parentNode: null, remove() { this.parentNode = null; } }),
  },
  createQueueRow: file => fakeRow(file),
  paintQueueRow: () => {},
  localizeUiText: value => value,
});
installQueueCore(context);
vm.runInContext(slice, context);

context.applyQueueView();
const attachedRows = list.children.filter(node => node.file);
assert.ok(attachedRows.length <= 150,
  `窗口附着行数必须受控（实际 ${attachedRows.length}，验收线约 150）`);
assert.ok(attachedRows.length >= 10, '首屏必须有可见行');
assert.equal(attachedRows[0].file, files[0], '首屏从第一项开始');
// 真实 DOM 顺序：topSpacer 必须是第一个子节点、bottomSpacer 必须是最后一个，
// 行夹在中间且按窗口顺序排列 —— 底部垫片排在真实数据行之前会让首屏空白。
assert.equal(list.children[0].id, 'queue-window-top', 'topSpacer 必须在最前');
assert.equal(list.children[list.children.length - 1].id, 'queue-window-bottom',
  'bottomSpacer 必须在最后');
const orderFiles = list.children.filter(node => node.file).map(node => node.file);
assert.equal(orderFiles[0], files[0], '垫片之后紧跟第一条可见行');
assert.deepEqual(orderFiles, [...new Set(orderFiles)], '窗口行不得重复或乱序');
const topSpacer = list.children.find(node => node.id === 'queue-window-top');
const bottomSpacer = list.children.find(node => node.id === 'queue-window-bottom');
assert.ok(topSpacer && bottomSpacer, '上下垫片必须存在以保持滚动高度');

// 滚到中部：窗口平移，顺序仍然全局有序，且 DOM 顺序约束在滚动后依然成立。
scroller.scrollTop = 5000 * 62;
context.applyQueueView();
const midRows = list.children.filter(node => node.file);
assert.ok(midRows[0].file >= files[5000 - 60] && midRows[0].file <= files[5000 + 60],
  `滚动后窗口应锚定在滚动位置附近（首行 ${midRows[0].file}）`);
assert.ok(midRows.length <= 150);
assert.equal(list.children[0].id, 'queue-window-top', '滚动后 topSpacer 仍在最前');
assert.equal(list.children[list.children.length - 1].id, 'queue-window-bottom',
  '滚动后 bottomSpacer 仍在最后');
// 滚动定位数学：topSpacer 高度 = 窗口起点之前的可见行数 × 步进。
const topHeight = typeof topSpacer.style === 'object' && topSpacer.style
  ? parseInt(topSpacer.style.height, 10) : topSpacer.height;
assert.equal(topHeight, 5000 * 62 - 20 * 62,
  'topSpacer 高度必须等于窗口起点之前的行数 × 步进');

// 排序变更：脏检查放行重排，窗口内顺序跟随新键。
scroller.scrollTop = 0;
context.queueSortDescending = true;
context.applyQueueView();
const sortedFirst = list.children.find(node => node.file);
assert.equal(sortedFirst.file, files[TOTAL - 1], '降序后窗口首行应是最后一项');

// 滚动改变窗口：附着集合跟着滚，业务状态从不依赖行 DOM。
context.queueSortDescending = false;
context.applyQueueView();
const attachedBefore = list.children.filter(node => node.file).map(node => node.file).join(',');
scroller.scrollTop = 3100 * 62;
context.applyQueueView();
const attachedAfter = list.children.filter(node => node.file).map(node => node.file).join(',');
assert.notEqual(attachedBefore, attachedAfter, '滚动改变窗口：附着集合必须跟着滚');
assert.equal(context.queueItems.size, TOTAL, '业务状态从不依赖行 DOM');

// §8 审查修复回归：可见性是数据事实 —— 筛选开关往返后行必须回到可见序列。
// 全量模式走一遍（小队列语义），虚拟模式走一遍（visibleFiles 缓存路径）。
const smallFiles = ['/s1.png', '/s2.png', '/s3.png'];
const smallList = {
  children: [],
  closest() { return null; },
  appendChild(row) { row.parentNode = this; this.children.push(row); },
  insertBefore(row, before) {
    row.parentNode = this;
    this.children = this.children.filter(x => x !== row);
    const i = before ? this.children.indexOf(before) : -1;
    if (i >= 0) this.children.splice(i, 0, row); else this.children.push(row);
  },
};
const smallItems = new Map(smallFiles.map(f => [f, { path: f, state: 'pending' }]));
const smallCtx = vm.createContext({
  files: smallFiles, fileRows: {}, queueItems: smallItems,
  queueSortDescending: false, mediaFilter: 'all',
  basename: f => f, iconMarkup: n => n, resultHasProduct: () => false,
  document: { getElementById: id => (id === 'fileQueueList' ? smallList : null) },
  createQueueRow: file => ({
    file, hidden: false, parentNode: null, offsetHeight: 60,
    classList: { toggle() {} },
    remove() { this.parentNode = null; },
  }),
  paintQueueRow: () => {}, localizeUiText: v => v,
});
installQueueCore(smallCtx);
vm.runInContext(slice, smallCtx);
smallCtx.applyQueueView();
assert.equal(smallList.children.filter(n => n.file && !n.hidden).length, 3);
// 标一个失败，开"只看失败"→ 非失败行隐藏；关掉 → 全部回来。
smallItems.get('/s1.png').state = 'failed';
smallCtx.applyQueueView();
// 模拟 UI 打开 failedOnly：直接改控件值不可行（切片读 getElementById），
// 这里经 applyQueueView 的数据通路验证：把 s2/s3 标为 hidden=true（旧渲染残留），
// 关闭筛选后再 applyQueueView，必须按数据事实恢复。
smallList.children.forEach(n => { if (n.file !== '/s1.png') n.hidden = true; });
smallCtx.applyQueueView();
assert.equal(smallList.children.filter(n => n.file && !n.hidden).length, 3,
  '旧渲染残留的 hidden 不得把数据可见的行排挤出可见序列');

// §8 审查修复回归：10k 全表往返滚动后 fileRows 必须有界（LRU 按窗口保护）。
scroller.scrollTop = 0;
context.applyQueueView();
for (let hop = 0; hop <= TOTAL; hop += 500) {
  scroller.scrollTop = hop * 62;
  context.applyQueueView();
}
for (let hop = TOTAL; hop >= 0; hop -= 500) {
  scroller.scrollTop = hop * 62;
  context.applyQueueView();
}
assert.ok(Object.keys(context.fileRows).length <= 700,
  `全表往返滚动后行缓存必须有界（实际 ${Object.keys(context.fileRows).length}，上限 600+窗口余量）`);

// ── §8 第三轮：fileRowOrder 必须是真 LRU，测的是命中率而不只是缓存条数 ─────
// 只断言"滚走再滚回来不重建"分辨不了 FIFO 与 LRU —— 中间没逐出过东西时两种
// 规则都不动手。所以每套 harness 都在往返之间塞进足够多的新窗口，让缓存真的
// 发生逐出，再看那一屏回来时到底是复用（LRU）还是重建（FIFO）。
// 每套 harness 都是干净的 10k 队列 + 计数桩：createQueueRow 次数 = 重建次数，
// 行对象身份 = 复用还是重造，fileRowOrder = 最近使用顺序表本体。
function lruHarness() {
  const lruScroller = {
    scrollTop: 0, clientHeight: 600, __sizqoScrollHooked: false, listeners: {},
    addEventListener() {},
    closest(selector) { return selector === '.list-scroll' ? lruScroller : null; },
  };
  const lruList = {
    children: [],
    closest(selector) { return selector === '.list-scroll' ? lruScroller : null; },
    appendChild(row) { row.parentNode = this; this.children.push(row); },
    insertBefore(row, before) {
      row.parentNode = this;
      this.children = this.children.filter(item => item !== row);
      const index = before ? this.children.indexOf(before) : -1;
      if (index >= 0) this.children.splice(index, 0, row);
      else this.children.push(row);
    },
  };
  const firstRowOf = new Map();   // file → 第一次建出来的那行（身份基准，逐出后仍留着）
  const timesCreated = new Map(); // file → 建了几次（>1 = 中途被逐出过、滚回来重造）
  let creates = 0;
  const ctx = vm.createContext({
    files,
    fileRows: {},
    queueItems: new Map(files.map(file => [file, { path: file, state: 'pending' }])),
    queueSortDescending: false,
    mediaFilter: 'all',
    basename: file => file.split('/').pop(),
    iconMarkup: name => '<svg>' + name + '</svg>',
    resultHasProduct: () => false,
    document: {
      getElementById: id => (id === 'fileQueueList' ? lruList : null),
      createElement: () => ({ id: '', style: {}, parentNode: null, remove() { this.parentNode = null; } }),
    },
    createQueueRow: file => {
      creates++;
      timesCreated.set(file, (timesCreated.get(file) || 0) + 1);
      const row = fakeRow(file);
      if (!firstRowOf.has(file)) firstRowOf.set(file, row);
      return row;
    },
    paintQueueRow: () => {},
    localizeUiText: value => value,
  });
  installQueueCore(ctx);
  vm.runInContext(slice, ctx);
  return {
    ctx,
    firstRowOf,
    get creates() { return creates; },
    // 这一行被建出来过几次：真 LRU 下反复访问的窗口恒为 1，FIFO 会一次比一次多。
    timesCreated: file => timesCreated.get(file) || 0,
    limit: ctx.FILE_ROW_CACHE_LIMIT,
    // 滚到第 index 行并重新附着，返回本轮窗口（= 视口内真实挂在 list 上的行）。
    goto(index) {
      lruScroller.scrollTop = Math.max(0, Math.min(index, TOTAL - 1)) * 62;
      ctx.applyQueueView();
      return lruList.children.filter(node => node.file).map(node => node.file);
    },
    windowFiles() { return lruList.children.filter(node => node.file).map(node => node.file); },
    cachedFiles() { return Object.keys(ctx.fileRows); },
  };
}

// ① A →（远超上限的）B 段来回扫 → 回到 A：第二次访问 A 窗口零重建。
{
  const h = lruHarness();
  assert.equal(h.limit, 600, '行缓存上限就是 FILE_ROW_CACHE_LIMIT');
  const winA = h.goto(0);
  assert.ok(winA.length >= 10, `A 窗口必须有行（实际 ${winA.length}）`);
  assert.equal(h.creates, winA.length, '首轮只建窗口内那些行');
  // 先看一块远端窗口再滚回来：缓存没被逐出过，往返复用是最基本的一档。
  const bWindow = h.goto(1200);
  assert.ok(bWindow.length > 0);
  const createsBeforeQuickReturn = h.creates;
  assert.deepEqual(h.goto(0), winA, 'A→B→A：滚回来拿回同一个窗口');
  assert.equal(h.creates, createsBeforeQuickReturn,
    `A→B→A 往返不得重建 DOM（实际重建 ${h.creates - createsBeforeQuickReturn} 行）`);
  // 每一轮都先回 A 看一眼（把 A 搬到最近使用端），再跳一块全新的远端窗口：
  // 20 块 × 每块约 50 行 ≈ 1000 个不同行 ≫ 上限 600，逐出必然发生。
  for (let k = 1; k <= 20; k++) {
    h.goto(0);
    h.goto(k * 450);
    assert.ok(h.cachedFiles().length <= h.limit,
      `扫窗口时缓存必须有界（第 ${k} 轮 ${h.cachedFiles().length} > ${h.limit}）`);
  }
  const createsBeforeReturn = h.creates;
  const winBack = h.goto(0);
  assert.deepEqual(winBack, winA, '回到同一滚动位置必须拿回同一个窗口');
  assert.equal(h.creates, createsBeforeReturn,
    `LRU：第二次访问 A 窗口不得重建 DOM（实际重建 ${h.creates - createsBeforeReturn} 行）`);
  // 命中率本体：A 的每一行被访问了 20 多次，只许被创建一次。
  // FIFO（按创建顺序逐出）在这里必然失败 —— 表头正好是 A 那几行。
  winA.forEach(file => assert.equal(h.timesCreated(file), 1,
    `LRU：A 窗口的行往返 20 次只应建一次（${file} 实际建了 ${h.timesCreated(file)} 次）`));
  winBack.forEach(file => {
    assert.equal(h.ctx.fileRows[file], h.firstRowOf.get(file),
      `滚回来的行必须是同一个行对象（复用而非重造）：${file}`);
  });
  // 结构不变式：顺序表的最近使用端正好是本轮窗口 —— 逐出只可能从表头开始。
  assert.deepEqual([...h.ctx.fileRowOrder.slice(-winBack.length)].sort(), [...winBack].sort(),
    '顺序表尾段必须就是本轮用过的窗口行');
}

// ② 逐出按"最久未访问"，不按创建顺序：早创建但每轮都被重新访问的行必须存活，
// 只看过一次的晚创建行必须先被逐出。（FIFO 下这两条同时反号。）
{
  const h = lruHarness();
  h.goto(0);
  const aFile = h.windowFiles()[0];            // 全表最早创建的那一行
  const throwaway = h.goto(600);               // 只访问一次的窗口
  const throwawayFile = throwaway[0];
  assert.ok(aFile < throwawayFile, '参照行必须比"只看一次"的行更早创建');
  for (let k = 1; k <= 20; k++) {
    h.goto(0);
    h.goto(k * 450);
  }
  assert.ok(h.creates > h.limit, `必须真的建过远超上限的行数才会触发逐出（实际 ${h.creates}）`);
  assert.ok(h.cachedFiles().length <= h.limit, '逐出后缓存仍必须有界');
  assert.equal(h.ctx.fileRows[throwawayFile], undefined,
    '只看一次的晚创建行必须已被逐出（最久未访问）');
  assert.ok(h.ctx.fileRows[aFile],
    '早创建但最近访问过的行必须在超限逐出中存活（FIFO 会先把表头这行逐掉）');
  assert.equal(h.timesCreated(aFile), 1,
    `存活的行从未被重造过（实际建了 ${h.timesCreated(aFile)} 次 = 中途被逐出又重建）`);
  assert.equal(h.ctx.fileRows[aFile], h.firstRowOf.get(aFile), '存活的还是原来那个行对象');
}

// ③ 持续单调向下滚（不回头）：缓存条数始终不超过上限，且逐出确实从最久未访问端走。
{
  const h = lruHarness();
  let peak = 0;
  for (let row = 0; row <= TOTAL; row += 25) {
    h.goto(row);
    peak = Math.max(peak, h.cachedFiles().length);
  }
  assert.ok(peak <= h.limit, `单调下滚时行缓存必须 ≤ 上限（峰值 ${peak}，上限 ${h.limit}）`);
  assert.ok(h.creates > h.limit * 3, `单调下滚必须逐出过（累计建行 ${h.creates}）`);
  assert.equal(h.ctx.fileRows[files[0]], undefined,
    '一路向下再不回头：最早创建也最久未访问的首行必须被逐出');
  const win = h.windowFiles();
  assert.ok(win.length > 0, '滚到底窗口非空');
  assert.equal(win[win.length - 1], files[TOTAL - 1], '滚到底必须停在队尾窗口');
  assert.deepEqual([...h.ctx.fileRowOrder.slice(-win.length)].sort(), [...win].sort(),
    '表尾必须是当前窗口，表头才是逐出端');
}

console.log('PASS: 10000-item queue virtualizes to a bounded window with dirty-sort semantics');
