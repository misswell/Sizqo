const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync('frontend/app.js', 'utf8');
const begin = source.indexOf('async function validateVideoBatch(');
const end = source.indexOf('function renderVideoQueueRows(');
assert.ok(begin >= 0 && end > begin, 'validateVideoBatch 切片标记必须存在');
const slice = source.slice(begin, end);

const files = ['/v-ok.mp4', '/v-bad.mp4', '/v-broken.mp4'];
// v-ok：本地与后端都通过；v-bad：本地通过但后端不兼容；v-broken：本地就不兼容。
const queueItems = new Map(files.map(path => [path, {
  path, mediaKind: 'video', state: 'pending', originalSize: 100,
  videoProbe: { ok: path !== files[2], rejectBackend: path === files[1] },
}]));
const toasts = [];
const context = vm.createContext({
  queueItems,
  videoCapabilities: { available: true },
  localizeUiText: value => value,
  window: {
    SizqoVideo: {
      validateOptions: (options, probe) =>
        ({ valid: !!probe.ok, errors: probe.ok ? [] : ['本地参数不兼容'] }),
    },
  },
  // §7.2 批量接口：一次 invoke 收整批，逐文件独立作答。
  invoke: async (command, { items }) =>
    items.map(entry => entry.probe.rejectBackend
      ? { path: entry.path, valid: false, reason: '与当前设置不兼容', warnings: [] }
      : { path: entry.path, valid: true, reason: null, warnings: ['码率偏低'] }),
  taskStateFor: result => (result && result.outcome === 'failed' ? 'failed' : 'done'),
  showToast: message => toasts.push(message),
});
vm.runInContext(slice, context);

(async () => {
  // §5.3：不再在第一个不兼容处短路 —— 每个不兼容的视频都收进 failures。
  const verdict = await context.validateVideoBatch(files, {});
  assert.equal(verdict.valid, false);
  // vm 切片里产生的数组带着另一个 realm 的原型，先归一到宿主数组再比较。
  // 顺序：本地判据先筛（v-broken），后端批量结果追加在后（v-bad）。
  assert.deepEqual(Array.from(verdict.failures, f => f.path), ['/v-broken.mp4', '/v-bad.mp4']);
  assert.deepEqual(Array.from(verdict.warnings), ['码率偏低'], '有效视频的警告照常收集');

  // 预检失败只标记它自己：其余视频保持 pending，照常进入本轮会话。
  const count = context.markVideoPreflightFailures(verdict.failures);
  assert.equal(count, 2);
  assert.equal(queueItems.get('/v-ok.mp4').state, 'pending', '有效视频不许被连坐');
  assert.equal(queueItems.get('/v-bad.mp4').state, 'failed');
  assert.equal(queueItems.get('/v-bad.mp4').result.reasonCode, 'unsupported_format');
  assert.ok(queueItems.get('/v-bad.mp4').result.reason.length > 0, '失败原因必须带出去');
  assert.equal(queueItems.get('/v-broken.mp4').result.outcome, 'failed');

  // 已经标记过的行不许被二次改写（比如用户又点了一次开始）。
  assert.equal(context.markVideoPreflightFailures(verdict.failures), 0, '只标记 pending 的行');

  // 能力级失效：整批都进不了，全局错误 + 全部失败。
  context.videoCapabilities.available = false;
  context.videoCapabilities.reason = '视频编码当前不可用';
  const blocked = await context.validateVideoBatch(files, {});
  assert.deepEqual(Array.from(blocked.globalErrors), ['视频编码当前不可用']);
  assert.equal(blocked.failures.length, 3);
  assert.equal(blocked.valid, false);

  // 全部有效 → valid，不标记任何行。
  context.videoCapabilities.available = true;
  queueItems.forEach(item => { item.state = 'pending'; item.result = null; });
  const allValid = await context.validateVideoBatch([files[0]], {});
  assert.equal(allValid.valid, true);
  assert.equal(context.markVideoPreflightFailures(allValid.failures), 0);
  console.log('PASS: video batch validation is per-item; preflight failures never sink the batch');
})().catch(error => { console.error(error); process.exit(1); });
