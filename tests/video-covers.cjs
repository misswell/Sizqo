// 视频封面管理（PERF-6.2）：懒加载触发、有界并发、LRU 回收、取消清理。
//
// 这组测试钉住的是资源契约，不是画质：
//   ① 只有点过名的行才发起抽帧 —— 千级导入不排队一千个请求；
//   ② 并发恒为 1（不与压缩抢 CPU），排队有上限；
//   ③ 缓存双上限（条目数 / 总字节），淘汰最久未用，淘汰时通知宿主清引用；
//   ④ 队列项被移走时 forget() 取消排队请求并清缓存；
//   ⑤ 导入时缓存的 videoProbe 必须随请求带给后端 —— 不再为每张封面
//      重复 ffprobe。
const assert = require('node:assert/strict');
const covers = require('../frontend/video-covers.js');

function makeManager(overrides) {
  const calls = [];
  const options = Object.assign({
    invoke: async (command, args) => {
      calls.push([command, args]);
      return 'data:image/jpeg;base64,COVER-' + args.path;
    },
    stillQueued: () => true,
    onPaint: () => {},
    onEvict: () => {},
  }, overrides);
  return { manager: covers.create(options), calls, options };
}

const wait = (ms) => new Promise(resolve => setTimeout(resolve, ms));

async function test_lazy_and_serial() {
  const { manager, calls } = makeManager();
  for (let i = 0; i < 5; i++) manager.requestCover({ path: `/v/${i}.mp4`, videoProbe: { durationUs: 1 } });
  // 并发必须恒为 1：同一时刻只有一个在飞。
  await wait(30);
  assert.equal(manager.cacheCount(), 5, '五张封面都应入库');
  assert.equal(calls.length, 5);
  for (const args of calls) {
    assert.equal(args[0], 'preview_video_frame');
    assert.ok(args[1].probe, '导入时缓存的 probe 必须随请求带上（后端不再重复 ffprobe）');
  }
  // 未点名的行不产生请求：requestCover 之外的路径没有任何 invoke。
  const fresh = makeManager();
  fresh.manager.getCached('/never-requested.mp4');
  assert.equal(fresh.calls.length, 0, '没有请求就没有 IPC');
}

async function test_same_item_never_requested_twice() {
  const { manager, calls } = makeManager();
  const item = { path: '/v/a.mp4', videoProbe: {} };
  manager.requestCover(item);
  manager.requestCover(item);
  manager.requestCover(item);
  await wait(20);
  assert.equal(calls.length, 1, '同一行只抽一次帧');
  assert.equal(manager.cacheCount(), 1);
}

async function test_failure_never_blocks() {
  let badFailures = 1; // /v/bad.mp4 第一次失败，之后成功。
  const { manager } = makeManager({
    invoke: async (command, args) => {
      if (args.path === '/v/bad.mp4' && badFailures > 0) { badFailures--; throw new Error('ffmpeg busy'); }
      return 'data:image/jpeg;base64,OK';
    },
  });
  manager.requestCover({ path: '/v/bad.mp4', videoProbe: {} });
  manager.requestCover({ path: '/v/good.mp4', videoProbe: {} });
  await wait(30);
  assert.equal(manager.cacheCount(), 1, '失败的请求不阻塞队列');
  assert.equal(manager.getCached('/v/good.mp4'), 'data:image/jpeg;base64,OK');
  // 失败的行允许再次触发。
  manager.requestCover({ path: '/v/bad.mp4', videoProbe: {} });
  await wait(20);
  assert.equal(manager.getCached('/v/bad.mp4'), 'data:image/jpeg;base64,OK');
}

async function test_lru_eviction_and_callbacks() {
  const evicted = [];
  const { manager } = makeManager({
    maxEntries: 3,
    onEvict: path => evicted.push(path),
  });
  for (let i = 0; i < 5; i++) {
    manager.requestCover({ path: `/v/${i}.mp4`, videoProbe: {} });
    await wait(5);
  }
  assert.equal(manager.cacheCount(), 3, '缓存条目数有上限');
  assert.equal(evicted.length, 2, '淘汰要通知宿主清行上的引用');
  assert.deepEqual(evicted, ['/v/0.mp4', '/v/1.mp4'], '淘汰最久未用的');
  assert.equal(manager.getCached('/v/0.mp4'), undefined);
  // 触碰会刷新 LRU 顺序：命中 /v/2 之后再满员，被淘汰的是 3 而不是 2。
  manager.getCached('/v/2.mp4');
  manager.requestCover({ path: '/v/5.mp4', videoProbe: {} });
  await wait(15);
  assert.equal(manager.cacheCount(), 3);
  assert.equal(manager.getCached('/v/2.mp4'), 'data:image/jpeg;base64,COVER-/v/2.mp4', '命中过的条目活得更久');
  assert.equal(manager.getCached('/v/3.mp4'), undefined);
}

async function test_bytes_cap() {
  const { manager } = makeManager({ maxBytes: 100 });
  manager.requestCover({ path: '/v/big1.mp4', videoProbe: {} });
  manager.requestCover({ path: '/v/big2.mp4', videoProbe: {} });
  await wait(20);
  assert.ok(manager.cacheBytes() <= 100, `总字节上限必须生效（实际 ${manager.cacheBytes()}）`);
}

async function test_forget_cancels_and_clears() {
  const { manager, calls } = makeManager();
  manager.requestCover({ path: '/v/x.mp4', videoProbe: {} });
  manager.requestCover({ path: '/v/y.mp4', videoProbe: {} });
  // /v/y 还在排队：forget 必须取消它。
  manager.forget('/v/y.mp4');
  await wait(30);
  assert.equal(manager.getCached('/v/y.mp4'), undefined, '被移走的队列项不保留封面');
  assert.ok(calls.every(call => call[1].path !== '/v/y.mp4'), '被移走的队列项不再发起抽帧');
  // 已缓存的也会被清掉。
  manager.forget('/v/x.mp4');
  assert.equal(manager.getCached('/v/x.mp4'), undefined);
  manager.clear();
  assert.equal(manager.cacheCount(), 0);
  assert.equal(manager.pendingCount(), 0);
}

async function main() {
  await test_lazy_and_serial();
  await test_same_item_never_requested_twice();
  await test_failure_never_blocks();
  await test_lru_eviction_and_callbacks();
  await test_bytes_cap();
  await test_forget_cancels_and_clears();
  console.log('PASS: 封面懒加载（视口触发、串行并发、LRU 双上限、取消清理、probe 复用）');
}

main().catch(error => { console.error(error); process.exit(1); });
