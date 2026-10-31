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
assert.ok(Object.keys(context.fileRows).length < TOTAL, '不预建一万行 DOM');

console.log('PASS: 10000-item queue virtualizes to a bounded window with dirty-sort semantics');
