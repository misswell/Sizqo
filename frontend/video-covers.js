// 视频封面管理（PERF-6.2）：视口懒加载 + 有界并发 + LRU 内存上限。
//
// 旧实现有两个必须修掉的问题：
//   ① 导入完成就对**每个**新视频排队抽帧 —— 一次导入 1000 个视频，即使
//      串行排队，后面的任务也要等几十分钟才轮到，而且用户永远只看得见
//      视口里的十几行；
//   ② 封面 data URL 挂在队列项上终身持有 —— 1000 张 base64 缩略图就是
//      上百 MB 常驻内存。
// 现在的契约：
//   - 只有「用户看得见的行」才发起抽帧（IntersectionObserver 触发 requestCover）；
//   - 并发恒为 1（不与压缩任务抢 CPU/IO），排队上限 MAX_PENDING，满了丢最旧的
//      （滚出视口的旧请求没有价值）；
//   - 封面缓存在本模块内做 LRU：条目数与总字节数双上限，淘汰最久未用的；
//      淘汰时通过 onEvict 通知宿主清掉行上的引用；
//   - 每次抽帧把导入时缓存的 videoProbe 一并交给后端 —— preview_video_frame
//      有 probe 就不再重复 ffprobe；
//   - 队列清空（resetQueue）时 clear()：取消排队请求、清空缓存。
// 纯逻辑模块：不碰 DOM（observer 由宿主接线），node 测试可以直接驱动。
(function(global) {
  'use strict';

  var MAX_CACHE_ENTRIES = 120;
  var MAX_CACHE_BYTES = 32 * 1024 * 1024; // base64 字符串按 UTF-16 长度近似计
  var MAX_PENDING = 64;

  function byteLengthOf(dataUrl) {
    return typeof dataUrl === 'string' ? dataUrl.length : 0;
  }

  function createCovers(options) {
    options = options || {};
    var invokeFn = options.invoke || function() {
      return Promise.reject(new Error('video covers: no invoke bridge configured'));
    };
    var stillQueued = options.stillQueued || function() { return true; };
    var onPaint = options.onPaint || function() {};
    var onEvict = options.onEvict || function() {};
    var maxEntries = options.maxEntries || MAX_CACHE_ENTRIES;
    var maxBytes = options.maxBytes || MAX_CACHE_BYTES;
    var maxPending = options.maxPending || MAX_PENDING;

    // LRU：Map 的插入序就是访问序 —— 命中时删除重插，淘汰时取首个 key。
    var cache = new Map(); // path -> dataUrl
    var cacheBytes = 0;
    var pending = []; // [{path, videoProbe}] 排队中（FIFO，有界）
    var inflight = null; // path
    var requested = new Set(); // 已请求过（含排队/在飞/完成）——同一行不重复排队
    var busy = false;

    function evictIfNeeded() {
      while (cache.size > maxEntries || (cacheBytes > maxBytes && cache.size > 0)) {
        var oldest = cache.keys().next().value;
        if (oldest === undefined) break;
        var bytes = byteLengthOf(cache.get(oldest));
        cache.delete(oldest);
        cacheBytes -= bytes;
        requested.delete(oldest);
        onEvict(oldest);
      }
    }

    function store(path, dataUrl) {
      if (typeof dataUrl !== 'string' || dataUrl.indexOf('data:image/') !== 0) return;
      if (cache.has(path)) {
        cacheBytes -= byteLengthOf(cache.get(path));
        cache.delete(path);
      }
      cache.set(path, dataUrl);
      cacheBytes += byteLengthOf(dataUrl);
      requested.delete(path); // 完成：不再视作"排队中"
      evictIfNeeded();
      onPaint(path);
    }

    function dropPending(path) {
      for (var i = 0; i < pending.length; i++) {
        if (pending[i].path === path) {
          pending.splice(i, 1);
          requested.delete(path);
          return true;
        }
      }
      return false;
    }

    async function drain() {
      if (busy) return;
      busy = true;
      try {
        while (pending.length) {
          var job = pending.shift();
          if (!job || !stillQueued(job.path) || cache.has(job.path)) continue;
          inflight = job.path;
          try {
            var frame = await invokeFn('preview_video_frame', {
              path: job.path,
              timeUs: 0,
              // 导入时缓存的 probe：后端不再为每张封面重复 ffprobe。
              probe: job.videoProbe || null,
            });
            var dataUrl = typeof frame === 'string' ? frame : frame && (frame.dataUrl || frame.frame);
            if (stillQueued(job.path)) store(job.path, dataUrl);
          } catch (_) {
            // 封面失败永远不阻塞导入/选择/压缩；允许以后再次触发。
            requested.delete(job.path);
          } finally {
            inflight = null;
          }
        }
      } finally {
        busy = false;
      }
    }

    return {
      /// 用户看得见了（IntersectionObserver / 详情选中）才调用。
      requestCover: function(item) {
        if (!item || !item.path) return;
        if (cache.has(item.path) || requested.has(item.path)) return;
        if (!stillQueued(item.path)) return;
        requested.add(item.path);
        pending.push({ path: item.path, videoProbe: item.videoProbe || null });
        if (pending.length > maxPending) {
          // 满了丢最旧的：滚出视口的请求没有价值，重进视口会再次触发。
          var dropped = pending.shift();
          requested.delete(dropped.path);
        }
        drain();
      },
      getCached: function(path) {
        if (!cache.has(path)) return undefined;
        // 触碰：重插以刷新 LRU 顺序。
        var dataUrl = cache.get(path);
        cache.delete(path);
        cache.set(path, dataUrl);
        return dataUrl;
      },
      forget: function(path) {
        var removed = dropPending(path);
        if (cache.has(path)) {
          cacheBytes -= byteLengthOf(cache.get(path));
          cache.delete(path);
          removed = true;
        }
        requested.delete(path);
        return removed;
      },
      clear: function() {
        pending.length = 0;
        requested.clear();
        cacheBytes = 0;
        cache.clear();
      },
      pendingCount: function() { return pending.length; },
      cacheCount: function() { return cache.size; },
      cacheBytes: function() { return cacheBytes; },
      inflightPath: function() { return inflight; },
      isBusy: function() { return busy; },
      // __test 钩子：让测试在 await 之间插桩。
      __drain: drain,
    };
  }

  var api = { create: createCovers, DEFAULTS: { MAX_CACHE_ENTRIES: MAX_CACHE_ENTRIES, MAX_CACHE_BYTES: MAX_CACHE_BYTES, MAX_PENDING: MAX_PENDING } };
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  global.SizqoVideoCovers = api;
})(typeof window !== 'undefined' ? window : globalThis);
