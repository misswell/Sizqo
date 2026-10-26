const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const { installQueueCore } = require('./app-slices.cjs');
const source = fs.readFileSync('frontend/app.js', 'utf8');
const files = ['/image10.png', '/image2.png', '/image1.png', '/pending.png'];
const states = ['done', 'failed', 'done', 'pending'];
// 排序键全部从 queueItems 读，所以这里造的就是队列项本身（不再是 DOM class）。
const queueItems = new Map(files.map((file, i) => [file, {
  path: file, state: states[i],
  result: [
    {file: files[0], originalSize: 100, compressedSize: 90, savings: 10, outputPath: '/image10_compressed.png', outcome: 'committed'},
    {file: files[1], originalSize: 2000, outcome: 'failed', reasonCode: 'source_read_failed'},
    // 后端判定"没有值得提交的收益"（§6.2）：引擎压出了字节，但一个文件都没落盘。
    // 这一行在"压缩后大小/压缩率"上没有读数，不许凭那串从没写出去的数字排到最前面。
    {file: files[2], originalSize: 300, compressedSize: 30, savings: 90, outputPath: null,
      outcome: 'skipped_no_benefit', reasonCode: 'output_larger_than_source'},
    null,
  ][i],
  originalSize: [100, 2000, 300, undefined][i],
}]));
const rows = Object.fromEntries(files.map(file => [file, {
  file, originalSize: 0,
  classList: { contains: () => false },
}]));
const list = { children: Object.values(rows), insertBefore(row, before) {
  this.children = this.children.filter(item => item !== row);
  const index = before ? this.children.indexOf(before) : this.children.length;
  this.children.splice(index, 0, row);
}};
const controls = {
  fileQueueList: list, queueFailedOnly: {checked:false}, queueSortKey: {value:'import'},
  queueSortDirection: {setAttribute() {}}, queueFilterEmpty: {},
};
const context = vm.createContext({
  files, fileRows: rows, queueItems, queueSortDescending: false,
  basename: file => file.split('/').pop(),
  iconMarkup: name => '<svg>' + name + '</svg>',
  document: {getElementById: id => controls[id]},
});
// applyQueueView 自己只读 queueItems（排序键 + 只看失败筛子），DOM class 已经不参与判断。
installQueueCore(context);
vm.runInContext(source.slice(source.indexOf('function toggleQueueSortDirection('), source.indexOf('async function renderFileQueue(')), context);
const order = () => list.children.filter(row => !row.hidden).map(row => row.file);
const sort = (key, descending = false) => {
  controls.queueSortKey.value = key;
  context.queueSortDescending = descending;
  context.applyQueueView();
};
sort('name'); assert.deepEqual(order(), [files[2],files[1],files[0],files[3]]);
sort('name',true); assert.deepEqual(order(), [files[3],files[0],files[1],files[2]]);
sort('original'); assert.deepEqual(order(), [files[0],files[2],files[1],files[3]]);
sort('original',true); assert.deepEqual(order(), [files[1],files[2],files[0],files[3]]);
// files[2] 那个 90% 从没落盘，所以它不占首位；没读数的一律沉到最后（仍按导入次序）。
sort('compressed'); assert.deepEqual(order(), [files[0], files[1], files[2], files[3]]);
sort('ratio',true); assert.deepEqual(order(), [files[0], files[1], files[2], files[3]]);
sort('status'); assert.deepEqual(order(), [files[1],files[3],files[0],files[2]]);
controls.queueFailedOnly.checked = true;
context.applyQueueView(); assert.deepEqual(order(), [files[1]]);
assert.equal(controls.queueFilterEmpty.hidden, true);
states[1] = 'running'; context.queueItems.get(files[1]).state = 'running';
context.applyQueueView();
assert.deepEqual(order(), []); assert.equal(controls.queueFilterEmpty.hidden, false);
states[3] = 'failed'; context.queueItems.get(files[3]).state = 'failed';
context.applyQueueView(); assert.deepEqual(order(), [files[3]]);
controls.queueFailedOnly.checked = false;
sort('import'); assert.deepEqual(order(), files);
assert.deepEqual(files, ['/image10.png','/image2.png','/image1.png','/pending.png']);
console.log('PASS: all sort fields, numeric filenames, missing values, live failure filter, queue isolation');
