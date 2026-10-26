// §67–§70 §87 §88：界面只读结论，能力表、状态视觉与百分比都在契约文件里回答一次。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const frontend = path.join(__dirname, '..', 'frontend');
const source = fs.readFileSync(path.join(frontend, 'result_state.js'), 'utf8');
const context = vm.createContext({
  console, Number, Math,
  localizeUiText: value => value,
  window: { SizqoI18n: { translateText: value => value, translateMessage: value => value } },
});
vm.runInContext(source, context);
const contract = context.window.SizqoResultContract;

const result = extra => Object.assign({
  file: '/Pictures/a.jpg', outcome: 'committed', reasonCode: null,
  originalSize: 1000, compressedSize: 500, savings: 50, outputPath: '/Pictures/a_c.jpg',
}, extra || {});

const plain = value => JSON.parse(JSON.stringify(value));

// ── §67 §68 能力只由 outcome 回答：success 不再是任何按钮的依据 ─────────────
assert.deepEqual(Object.keys(contract.capabilities(result())).sort(),
  ['compare', 'finder', 'log', 'restore', 'retry', 'save']);
assert.deepEqual(plain(contract.capabilities(result())),
  { save: true, compare: true, restore: true, finder: true, log: true, retry: false });
assert.equal(contract.capabilities(result({ outcome: 'target_unmet', reasonCode: 'target_unmet' })).retry, true,
  '未达目标仍然有产物，同时该给"再压一次"');
for (const outcome of ['skipped_no_benefit', 'skipped_already_under_target', 'cancelled', 'failed']) {
  const caps = contract.capabilities(result({ outcome, reasonCode: null, outputPath: null }));
  assert.deepEqual(plain(caps),
    { save: false, compare: false, restore: false, finder: false, retry: true, log: true },
    `${outcome} 不许出现任何指向产物的按钮`);
}
assert.deepEqual(plain(contract.capabilities(result({ outputMissing: true }))),
  { save: false, compare: false, restore: true, finder: false, retry: false, log: true },
  '产物被外部删掉：只留恢复与日志');

const pageCode = ['app.js', 'compare_window.js']
  .map(name => fs.readFileSync(path.join(frontend, name), 'utf8'))
  .join('\n');
assert.doesNotMatch(pageCode, /function resultCapabilities\(/, '能力表不许在页面文件里长出第二份');
// 页面里允许读的是恢复命令自己的 success；压缩结果的能力判断一律不许碰 success。
const successReads = pageCode.split('\n')
  .map((line, index) => ['line ' + (index + 1), line.trim()])
  .filter(([, line]) => /(?:result|r)\.success\b/.test(line));
assert.deepEqual(successReads, [], `页面又用 result.success 判压缩结果：${successReads.map(([n, l]) => `${n}: ${l}`).join(' | ')}`);

// ── §69 行与详情共用同一套状态视觉：同一份事实不会一边完成一边等待 ─────────
assert.deepEqual(plain(contract.rowStateVisual({ state: 'done' }, result())), { icon: 'check', tone: 'done' });
assert.deepEqual(plain(contract.rowStateVisual({ state: 'done' }, result({ outcome: 'skipped_no_benefit' }))),
  { icon: 'warning', tone: 'warning' });
assert.deepEqual(plain(contract.rowStateVisual({ state: 'done' }, result({ outcome: 'cancelled' }))),
  { icon: 'minus', tone: '' });
assert.deepEqual(plain(contract.rowStateVisual({ state: 'done' }, result({ outputMissing: true }))),
  { icon: 'warning', tone: 'warning' });
assert.deepEqual(plain(contract.rowStateVisual({ state: 'failed' }, result({ outcome: 'failed' }))),
  { icon: 'error', tone: 'failed' });
assert.deepEqual(plain(contract.rowStateVisual({ state: 'restored' }, null)), { icon: 'restore', tone: 'restored' });

assert.equal(contract.statusText(result()), '已完成');
assert.equal(contract.statusText(result({ outcome: 'target_unmet' })), '已完成 · 未达到目标');
assert.equal(contract.statusText(result({ outcome: 'skipped_no_benefit' })), '已跳过');
assert.equal(contract.statusText(result({ outcome: 'cancelled' })), '已取消');
assert.equal(contract.statusText(result({ outcome: 'failed' })), '处理失败');
assert.equal(contract.statusText(result({ outputMissing: true })), '压缩结果已不存在');
assert.equal(contract.reasonText(result({ outcome: 'failed', reasonCode: 'source_read_failed' })), '无法读取源文件');

// ── §87 节省率按字节算、允许负数、绝不 clamp ────────────────────────────────
assert.equal(contract.savingsPercentText({ savings: 57.8 }), '-57.8%');
assert.equal(contract.savingsPercentText({ savings: -20 }), '+20.0% 体积增加',
  '变大也要如实说，而不是 clamp 成 0 或只留一个加号');
assert.equal(contract.savingsPercentText({ savings: 0 }), '0.0%');
assert.equal(contract.savingsPercentText({ originalSize: 1000, compressedSize: 1200 }), '+20.0% 体积增加',
  '后端没给 savings 时按同一公式从字节补，不许把变大藏起来');
assert.equal(contract.savingsPercentText({ originalSize: 1000, compressedSize: 400 }), '-60.0%');
assert.equal(contract.savingsPercentText({ originalSize: 0, compressedSize: 400 }), '');
assert.equal(contract.savingsPercentText(null), '');

// ── §88 后端存字节，前端最后才格式化：result_state 里不许出现 MB/GB 常量 ──
assert.doesNotMatch(source, /1048576|1024 \* 1024/, '结果契约不负责单位换算，只保留字节与百分比');

console.log('PASS: 能力表 / 状态视觉 / 节省率都由结果契约一处回答，变大不再被 clamp');
