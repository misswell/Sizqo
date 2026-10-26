const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const { installQueueCore } = require('./app-slices.cjs');
const source = fs.readFileSync('frontend/app.js', 'utf8');
const slice = source.slice(
  source.indexOf('function toggleQueueSortDirection('),
  source.indexOf('async function renderFileQueue('));

// §6.1 回归绊线：行循环里的成员判断必须走一次构建的 Set，
// 不许回退到对每行做一次 `files.includes(file)` 的 O(N²)。
assert.ok(slice.includes('new Set(files)'), 'applyQueueView 必须构建一次 Set(files)');
assert.ok(!slice.includes('files.includes('), 'applyQueueView 里不许再出现 O(N²) 的 files.includes');

// 2000 项队列：排序与筛选语义在大队列下仍然正确。
const total = 2000;
const files = Array.from({ length: total }, (_, i) => '/f' + String(i).padStart(5, '0') + '.png');
const rows = Object.fromEntries(files.map(file => [file, { file, hidden: false }]));
const list = { children: files.map(file => rows[file]), insertBefore(row, before) {
  this.children = this.children.filter(item => item !== row);
  const index = before ? this.children.indexOf(before) : this.children.length;
  this.children.splice(index, 0, row);
}};
const controls = {
  fileQueueList: list, queueFailedOnly: { checked: false }, queueSortKey: { value: 'import' },
  queueSortDirection: { setAttribute() {} }, queueFilterEmpty: {},
};
const queueItems = new Map(files.map(file => [file, { path: file, state: 'pending' }]));
const context = vm.createContext({
  files, fileRows: rows, queueItems, queueSortDescending: false,
  basename: file => file.split('/').pop(),
  iconMarkup: name => '<svg>' + name + '</svg>',
  document: { getElementById: id => controls[id] },
});
installQueueCore(context);
vm.runInContext(slice, context);

context.applyQueueView();
const visible = list.children.filter(row => !row.hidden);
assert.equal(visible.length, total, '全部行可见');
assert.equal(visible[0].file, files[0]);
assert.equal(visible[total - 1].file, files[total - 1]);

// 名字降序：编号大的排前面；隐藏一批后再恢复，成员判断依然正确。
controls.queueSortKey.value = 'name';
context.queueSortDescending = true;
context.applyQueueView();
assert.equal(list.children[0].file, files[total - 1]);
assert.equal(list.children[total - 1].file, files[0]);

// 从成员集合里移除一批文件后，对应行必须被隐藏，剩余顺序保持。
// 先把排序键复位回导入序：上一段把它留在了名字降序。
controls.queueSortKey.value = 'import';
context.queueSortDescending = false;
files.splice(0, 1500);
context.applyQueueView();
const remaining = list.children.filter(row => !row.hidden);
assert.equal(remaining.length, total - 1500, '从成员集合里移除的行必须被隐藏');
assert.equal(remaining[0].file, files[0]);
console.log('PASS: 2000-item queue keeps order semantics with Set-based membership');
