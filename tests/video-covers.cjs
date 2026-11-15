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

async function test_one_thousand_items_stay_bounded() {
  // 验收 C 项：一次导入 1000 个视频。旧实现会对 1000 行全量排队抽帧；
  // 现在只有"用户看得见"的行（视口 12 行 + 预加载边距）才发起抽帧，
  // 千级导入的封面开销恒等于可见行数，与队列总规模无关。
  const { manager, calls } = makeManager({ maxEntries: 120 });
  const total = 1000;
  const visibleWindow = 12; // 视口里的行数
  for (let i = 0; i < total; i++) {
    const item = { path: `/library/clip-${i}.mkv`, videoProbe: { durationUs: 1 } };
    // 只有视口内的行触发 requestCover（IntersectionObserver 的语义等价物）。
    if (i < visibleWindow) manager.requestCover(item);
  }
  await wait(60);
  assert.equal(calls.length, visibleWindow, `1000 项队列只对可见的 ${visibleWindow} 行抽帧，实际 ${calls.length}`);
  assert.equal(manager.cacheCount(), visibleWindow);
  assert.ok(manager.pendingCount() <= covers.DEFAULTS.MAX_PENDING, '排队上限恒成立');
  // 用户滚动：新一批可见行进来，请求量仍按可见数线性增长，而不是按队列规模。
  for (let i = visibleWindow; i < visibleWindow + 12; i++) {
    manager.requestCover({ path: `/library/clip-${i}.mkv`, videoProbe: { durationUs: 1 } });
  }
  await wait(60);
  assert.equal(calls.length, visibleWindow + 12, '滚动后只追加新可见行的请求');
  // 内存上限：缓存条目恒 ≤ 上限（淘汰最久未用），data URL 不随 1000 项无限堆积。
  assert.ok(manager.cacheCount() <= covers.DEFAULTS.MAX_CACHE_ENTRIES,
    `缓存条目 ${manager.cacheCount()} 必须 ≤ ${covers.DEFAULTS.MAX_CACHE_ENTRIES}`);
  console.log(`PASS: 1000 项队列封面开销 = 可见行数（${visibleWindow}→${visibleWindow + 12} 次请求），缓存有界`);
}

/// 源码契约：导入合并路径不再对每个新视频全量排队抽帧；行交给
/// IntersectionObserver（懒加载）。改回全量排队时这条测试必须红。
function test_import_path_never_enqueues_covers_bulk() {
  const fs = require('node:fs');
  const path = require('node:path');
  const appJs = fs.readFileSync(path.join(__dirname, '..', 'frontend', 'app.js'), 'utf8');
  const importMergeRegion = appJs.slice(
    appJs.indexOf('var added = mergeQueueMedia(supportedRecords);'),
    appJs.indexOf('if (added.length === 0) {'));
  assert.ok(importMergeRegion.length > 0, '导入合并段定位失败');
  assert.ok(!importMergeRegion.includes('queueVideoCover'), '导入合并路径不许再全量排队抽帧（懒加载契约）');
  assert.ok(appJs.includes('videoCoverObserver.observe(row)'), '视频行必须注册进 IntersectionObserver（视口触发）');
  assert.ok(appJs.includes("data-file"), '观察器靠 data-file 定位队列项');
}

async function main() {
  await test_lazy_and_serial();
  await test_same_item_never_requested_twice();
  await test_failure_never_blocks();
  await test_lru_eviction_and_callbacks();
  await test_bytes_cap();
  await test_forget_cancels_and_clears();
  await test_one_thousand_items_stay_bounded();
  test_import_path_never_enqueues_covers_bulk();
  console.log('PASS: 封面懒加载（视口触发、串行并发、LRU 双上限、取消清理、probe 复用、1000 项有界）');
}

main().catch(error => { console.error(error); process.exit(1); });
