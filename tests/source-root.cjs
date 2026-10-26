// 来源根（§39 §40）：队列项创建那一刻就记住 sourceRoot / relativePath，
// 之后一轮请求只带这轮文件自己的根，不再拿"历次导入攒下来的清单"重新猜。
//
// 报上来的毛病是「同一批文件，换个轮次重试，输出目录里的层级就变了」：
// 后端每批都在 `inputPaths` 里重新挑最深根，前端又随时往里追加 —— 两边一起漂。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { source, slice } = require('./app-slices.cjs');

const context = vm.createContext({
  console, Map, Set, Object, Number, String, Array,
  window: { crypto: { randomUUID: (() => { let n = 0; return () => 'task-' + (++n); })() } },
  files: [],
  queueItems: new Map(),
});
// 切片自带 createMediaTaskId / normalizedMediaKind / mergeQueueMedia 三个真实函数。
vm.runInContext(slice('function createMediaTaskId(', 'function handleFilePaths('), context);
vm.runInContext(slice('function sourceRootsForPaths(', 'function getCurrentCompressionConfig('), context);

// ── ① 导入即固定：目录导入保住层级，单文件导入落在父目录 ─────────────────
const added = context.mergeQueueMedia([
  { path: '/photos/2026/a.jpg', mediaKind: 'image', size: 100, sourceRoot: '/photos', relativePath: '2026/a.jpg' },
  { path: '/photos/2026/b.png', mediaKind: 'image', size: 200, sourceRoot: '/photos', relativePath: '2026/b.png' },
  { path: '/loose/c.mp4', mediaKind: 'video', size: 300, sourceRoot: '/loose', relativePath: 'c.mp4' },
]);
assert.equal(added.length, 3);
assert.deepEqual(
  ['sourceRoot', 'relativePath'].map(key => context.queueItems.get('/photos/2026/a.jpg')[key]),
  ['/photos', '2026/a.jpg'],
  '队列项必须原样记住导入时定下的来源根与相对路径'
);

// ── ② 一轮请求只带这一轮自己的根 ─────────────────────────────────────────
assert.deepEqual(
  Array.from(context.sourceRootsForPaths(['/photos/2026/a.jpg', '/photos/2026/b.png'])),
  ['/photos'],
  '同一根下的多个文件只发一次根'
);
assert.deepEqual(
  Array.from(context.sourceRootsForPaths(['/photos/2026/a.jpg', '/loose/c.mp4'])),
  ['/photos', '/loose'],
  '混合来源的轮次按文件自己的根发送'
);

// ── ③ 后来又导入别的目录，先来的队列项一个字都不改 ──────────────────────
context.mergeQueueMedia([
  { path: '/elsewhere/d.jpg', mediaKind: 'image', size: 400, sourceRoot: '/elsewhere', relativePath: 'd.jpg' },
]);
assert.deepEqual(
  ['sourceRoot', 'relativePath'].map(key => context.queueItems.get('/photos/2026/a.jpg')[key]),
  ['/photos', '2026/a.jpg'],
  '新导入不许改写已有队列项的来源根'
);
assert.deepEqual(
  Array.from(context.sourceRootsForPaths(['/photos/2026/a.jpg'])),
  ['/photos'],
  '重试 / 续跑只发自己那一项固定的根，不受后来导入影响'
);

// ── ④ §64 队列项只存 path / metadata / state，不长期背图片载荷 ─────────
const imageItem = context.queueItems.get('/photos/2026/a.jpg');
for (const banned of ['bytes', 'bitmap', 'pixels', 'candidate', 'dataUrl', 'base64']) {
  assert.equal(
    Object.prototype.hasOwnProperty.call(imageItem, banned), false,
    `队列项里出现了 ${banned}：图片字节/解码位图/候选只能在处理这一项时短暂存在`
  );
}

// ── ④ 前端不许再用攒出来的清单当根 ──────────────────────────────────────
assert.ok(!source.includes('sourceRoots: inputPaths.slice()'),
  'sourceRoots 不许再读取历次导入攒下来的 inputPaths');
assert.match(source, /sourceRoots: sourceRootsForPaths\(paths\)/,
  '图片请求的来源根必须来自这一轮文件自己的队列项');
assert.match(source, /sourceRoots: sourceRootsForPaths\(batchPaths\)/,
  '混合 / 视频请求的来源根必须来自这一轮的批次文件');

// ── ⑤ 后端与前端字段同名，且两条线共用同一份根解析 ──────────────────────
const root = path.join(__dirname, '..');
const commands = fs.readFileSync(path.join(root, 'src-tauri', 'src', 'commands.rs'), 'utf8');
const videoCommands = fs.readFileSync(path.join(root, 'src-tauri', 'src', 'video_commands.rs'), 'utf8');
const videoTxn = fs.readFileSync(path.join(root, 'src-tauri', 'src', 'video', 'transaction.rs'), 'utf8');
assert.match(videoCommands, /source_root:String,relative_path:String/,
  '导入结果必须带上 sourceRoot / relativePath（camelCase 序列化后前端才读得到）');
assert.match(commands, /crate::source_root::relative_to_roots/,
  '图片线的目录输出必须走共享的来源根解析');
assert.match(videoTxn, /crate::source_root::relative_to_roots/,
  '视频线的目录输出必须走共享的来源根解析');
assert.ok(!commands.includes('fallback_root'),
  '图片线不许再按"这一批只有一个目录 / 文件父目录"补一个来源根');

console.log('PASS: 来源根在入队时固定，图片 / 视频两线共用一份解析，重试与续跑不改层级');
