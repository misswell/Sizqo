const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const { installQueueCore } = require('./app-slices.cjs');

/// §8.4 性能基准：1,000 / 5,000 / 10,000 文件队列。
/// Node + 假 DOM 的时间是相对参考，不是浏览器实测；它钉住的是复杂度行为：
///   - 全量排序（用户改排序键时的成本）允许 O(N log N)；
///   - 单次进度事件（干净路径）不随 N 显著增长；
///   - 虚拟窗口附着恒定成本、DOM 行数 ≤ ~150；
///   - 100 次连续滚动的重附总成本有界。
const source = fs.readFileSync('frontend/app.js', 'utf8');
const slice = source.slice(
  source.indexOf('function toggleQueueSortDirection('),
  source.indexOf('async function renderFileQueue('));

function bench(total, withScroller) {
  const files = Array.from({ length: total }, (_, i) => '/f' + String(i).padStart(7, '0') + '.png');
  const scroller = {
    scrollTop: 0, clientHeight: 600, __sizqoScrollHooked: false, listeners: {},
    addEventListener(name, fn) { this.listeners[name] = fn; },
    closest(s) { return withScroller && s === '.list-scroll' ? scroller : null; },
  };
  const list = {
    children: [],
    closest(s) { return withScroller && s === '.list-scroll' ? scroller : null; },
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
    files, fileRows: {}, queueItems, queueSortDescending: false, mediaFilter: 'all',
    basename: file => file.split('/').pop(),
    iconMarkup: name => name,
    resultHasProduct: () => false,
    document: {
      getElementById: id => (id === 'fileQueueList' ? list : null),
      createElement: () => ({ id: '', style: {}, parentNode: null, remove() { this.parentNode = null; } }),
    },
    createQueueRow: file => ({
      file, hidden: false, parentNode: null, offsetHeight: 60, classList: { toggle() {} },
      remove() {
        if (this.parentNode) {
          const siblings = this.parentNode.children;
          const index = siblings.indexOf(this);
          if (index >= 0) siblings.splice(index, 1);
        }
        this.parentNode = null;
      },
    }),
    paintQueueRow: () => {},
    localizeUiText: value => value,
  });
  installQueueCore(context);
  vm.runInContext(slice, context);

  // ① 首次全量（排序 + 建行 + 附着）。
  let t0 = process.hrtime.bigint();
  context.applyQueueView();
  const firstPassMs = Number(process.hrtime.bigint() - t0) / 1e6;

  // ② 干净路径（进度事件等效：排序输入没变）。
  t0 = process.hrtime.bigint();
  context.applyQueueView();
  const cleanPathMs = Number(process.hrtime.bigint() - t0) / 1e6;

  // ③ 窗口附着（滚动重附）或全量重排（无窗口时的等效成本）。
  let scrollMsTotal = 0;
  let windowRows = list.children.filter(node => node.file).length;
  const scrolls = 100;
  for (let i = 1; i <= scrolls; i++) {
    scroller.scrollTop = Math.floor((total / scrolls) * i) * 62;
    t0 = process.hrtime.bigint();
    context.applyQueueView();
    scrollMsTotal += Number(process.hrtime.bigint() - t0) / 1e6;
  }
  windowRows = list.children.filter(node => node.file).length;

  // ④ 换排序键（用户操作的全量重排成本）。
  context.queueSortDescending = true;
  t0 = process.hrtime.bigint();
  context.applyQueueView();
  const resortMs = Number(process.hrtime.bigint() - t0) / 1e6;

  return { total, firstPassMs, cleanPathMs, scrollMsTotal, scrollAvgMs: scrollMsTotal / scrolls, resortMs, windowRows, virtualized: withScroller };
}

const results = [1000, 5000, 10000].map(total => bench(total, true));
const fullRender = bench(10000, false);

console.log('queue perf benchmark (Node, fake DOM — 复杂度参考):');
results.forEach(r => {
  console.log(
    `  N=${String(r.total).padStart(6)}  first=${r.firstPassMs.toFixed(1)}ms  ` +
    `clean=${r.cleanPathMs.toFixed(2)}ms  scroll×100=${r.scrollMsTotal.toFixed(1)}ms  ` +
    `resort=${r.resortMs.toFixed(1)}ms  windowRows=${r.windowRows}`);
});
console.log(
  `  N=10000 全量模式(旧语义参考): first=${fullRender.firstPassMs.toFixed(1)}ms  ` +
  `clean=${fullRender.cleanPathMs.toFixed(2)}ms`);

// ── 验收断言（§8.4 表格的可执行版）──────────────────────────────────
for (const r of results) {
  assert.ok(r.windowRows <= 150, `N=${r.total} 可见 DOM 行必须 ≤150（实际 ${r.windowRows}）`);
  assert.ok(r.cleanPathMs < 20, `N=${r.total} 干净路径（进度事件等效）必须 <20ms（实际 ${r.cleanPathMs.toFixed(2)}ms）`);
  assert.ok(r.scrollAvgMs < 25, `N=${r.total} 滚动重附均值必须 <25ms（实际 ${r.scrollAvgMs.toFixed(2)}ms）`);
}
const big = results[2];
assert.ok(big.firstPassMs < 2000, `N=10000 首次建列表必须 <2s（实际 ${big.firstPassMs.toFixed(0)}ms）`);
assert.ok(big.resortMs < 2000, `N=10000 全量重排必须 <2s（实际 ${big.resortMs.toFixed(0)}ms）`);
// 干净路径不随 N 显著增长：10k 与 1k 同量级（3× 以内）。
assert.ok(results[2].cleanPathMs < Math.max(results[0].cleanPathMs * 3, 5),
  `干净路径不得随 N 线性恶化（1k=${results[0].cleanPathMs.toFixed(2)}ms, 10k=${results[2].cleanPathMs.toFixed(2)}ms）`);

console.log('PASS: 1k/5k/10k queue performance benchmarks (bounded window, event-path flat)');
