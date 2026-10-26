// 意图编译器：把「要多小、给谁用」翻成引擎真正读得懂的参数。
// 纯函数、不碰 DOM —— tests/intent-model.cjs 直接在 node 里断言这张表。
// 规范见 core/intent-model.md：任何一个意图都必须对应引擎已实现的行为，
// 做不到的请求不许出现在这里（AGENTS.md：不许有假控件、不许静默丢轨道）。
(function (global) {
  'use strict';

  var IMAGE_INTENTS = [
    {
      id: 'losslessFirst',
      label: '肉眼无差优先',
      note: '质量 90，不改变像素尺寸；候选搜索挑最小，PNG 可能被换成更小的 WebP。',
      // 系统转换之外的「保留元数据」只有 ImageIO 路径做得到；高级路径会重编码像素，
      // 这一点必须由 note 说清楚，不能让用户以为 EXIF 还在。
      warning: '高级路径重编码像素，EXIF 与 GPS 不会带进输出；要保留元数据请改用系统转换。',
      request: { quality: 90, outputFormat: 'original', maxDimensionPx: null },
      useSmart: true,
    },
    {
      id: 'balanced',
      label: '均衡',
      note: '质量 75，不改变像素尺寸；候选搜索挑最小，PNG 可能被换成更小的 WebP。默认。',
      warning: '高级路径重编码像素，EXIF 与 GPS 不会带进输出；要保留元数据请改用系统转换。',
      request: { quality: 75, outputFormat: 'original', maxDimensionPx: null },
      useSmart: true,
      isDefault: true,
    },
    {
      id: 'asSmallAsPossible',
      label: '尽量变小',
      note: '质量 60，候选搜索取最小；PNG 会同时试 WebP 并按更小者输出，JPG 只压 JPG。',
      warning: '质量档偏低，细节损失看得出来；高级路径重编码像素，EXIF 与 GPS 不会带进输出。',
      request: { quality: 60, outputFormat: 'original', maxDimensionPx: null },
      useSmart: true,
    },
    {
      id: 'webSize',
      label: '网页尺寸',
      note: '最长边压到 2560 px 以内（只缩不放），质量 72，格式保持不变。',
      warning: '会改变像素尺寸；高级路径重编码像素，EXIF 与 GPS 不会带进输出。',
      request: { quality: 72, outputFormat: 'original', maxDimensionPx: 2560 },
      // 尺寸意图不掺候选搜索：PNG 不会被换成 WebP，用户要的只是"缩到网页尺寸"。
      useSmart: false,
      editableTarget: 'maxDimensionPx',
    },
    {
      id: 'targetSize',
      label: '指定体积',
      note: '在体积预算内自动组合尺寸与质量，取达标的最小结果；达不到就报最接近的结果，不谎报成功。',
      warning: '达标与否逐文件报告；预算远低于素材下限时只能给出最接近的结果。高级路径重编码像素，EXIF 与 GPS 不会带进输出。',
      request: { quality: 0, outputFormat: 'original', maxDimensionPx: null, maxOutputBytes: 512 * 1024 },
      // 预算模式由求解器负责，候选搜索在此毫无意义（两轨的 compress 都会先落到求解器）。
      useSmart: false,
      editableTarget: 'maxOutputBytes',
      presets: [
        { bytes: 200 * 1024, label: '200 KB' },
        { bytes: 512 * 1024, label: '500 KB' },
        { bytes: 1024 * 1024, label: '1 MB' },
        { bytes: 5 * 1024 * 1024, label: '5 MB' },
      ],
    },
    {
      id: 'systemConvert',
      label: '系统转换',
      note: '用 macOS 自带的图像转换，行为对齐 Finder 的「快速操作 → 转换图像」，可保留元数据属性。',
      request: { processingMode: 'system' },
      useSmart: false,
      macOnly: true,
    },
    {
      id: 'custom',
      label: '自定义',
      note: '直接用下面这些参数，意图不再介入：怎么写就怎么发，PNG 不会被换成更小的 WebP。改动任一参数即切到这里。',
      warning: '高级路径重编码像素，EXIF 与 GPS 不会带进输出。',
      // 自定义不发常量（编译结果完全来自编辑值），也不掺候选搜索。
      useSmart: false,
    },
  ];

  var VIDEO_INTENTS = [
    {
      id: 'videoSmall',
      label: '通用更小',
      note: 'CRF 28 · MP4，兼容音轨优先复制。',
      request: { mode: 'crf', crf: 28, container: 'mp4', audioPolicy: 'copy', resolution: 0 },
      isDefault: true,
    },
    {
      id: 'videoHq',
      label: '高画质 · 保留轨道',
      note: 'CRF 20，音轨与字幕原样复制，元数据保留。体积减少有限。',
      request: { mode: 'crf', crf: 20, container: 'mp4', audioPolicy: 'copy', subtitlePolicy: 'copy', metadataPolicy: 'preserve', resolution: 0 },
    },
    {
      id: 'videoTarget',
      label: '指定体积',
      note: '按目标体积反推码率并走两遍编码。结果是估算，不是保证。',
      warning: '目标体积是估算：编码内容决定最终大小，实际会有百分之几的偏差。',
      request: { mode: 'targetSize', targetMiB: 100, container: 'mp4', audioPolicy: 'copy' },
      editableTarget: 'targetMiB',
    },
    {
      id: 'videoWeb',
      label: '1080p 网页',
      note: '最长边不超过 1080 · CRF 26 · MP4 · AAC 160 kbps。',
      request: { mode: 'crf', crf: 26, container: 'mp4', audioPolicy: 'aac', resolution: 1080 },
    },
    {
      id: 'videoCustom',
      label: '自定义',
      note: '直接用高级选项里的参数，意图不再介入。',
      request: {},
    },
  ];

  function find(list, id) {
    for (var i = 0; i < list.length; i++) {
      if (list[i].id === id) return list[i];
    }
    return null;
  }

  // §22：输出格式的权威是后端下发的 ImageCapabilityMatrix。这里只缓存；
  // 后端还没答复时用一份保守的回落集合，绝不把「本机写不出来」的格式发出去。
  var imageCapabilities = null;

  function setImageCapabilities(matrix) {
    imageCapabilities = matrix && Array.isArray(matrix.formats) ? matrix : null;
  }

  function imageOutputTargets(system) {
    var targets = imageCapabilities
      ? (system ? imageCapabilities.systemOutputTargets : imageCapabilities.outputTargets)
      : null;
    if (!Array.isArray(targets)) {
      targets = system ? ['jpg', 'png', 'heif'] : ['jpg', 'png', 'webp', 'avif'];
    }
    return targets.slice();
  }

  function imageIntentList(isMac) {
    return IMAGE_INTENTS.filter(function (intent) {
      return !intent.macOnly || isMac;
    });
  }

  function defaultImageIntentId() {
    var d = IMAGE_INTENTS.filter(function (i) { return i.isDefault; })[0];
    return d ? d.id : 'balanced';
  }

  function defaultVideoIntentId() {
    var d = VIDEO_INTENTS.filter(function (i) { return i.isDefault; })[0];
    return d ? d.id : 'videoSmall';
  }

  function clampBytes(value) {
    var n = Number(value);
    if (!Number.isFinite(n) || n <= 0) return null;
    return Math.min(Math.max(Math.round(n), 10 * 1024), 2 * 1024 * 1024 * 1024);
  }

  function clampDimension(value) {
    var n = Number(value);
    if (!Number.isFinite(n) || n <= 0) return null;
    return Math.min(Math.max(Math.round(n), 144), 16384);
  }

  /// 意图 + 当前编辑值 → 引擎请求。只有 custom 才把用户的原始字段当权威；
  /// 其余意图的 quality/outputFormat 来自这张表，界面上改一下就落到 custom。
  function compileImage(state, isMac) {
    var intents = imageIntentList(isMac);
    var intent = find(intents, state && state.intent) || find(intents, defaultImageIntentId());
    var request = { processingMode: 'advanced', outputFormat: 'original', maxDimensionPx: null, maxOutputBytes: null };
    var spec = intent.request || {};

    if (spec.processingMode === 'system') {
      request.processingMode = 'system';
      request.outputFormat = normalizeFormat(state.systemOutputFormat, 'original', true);
      request.systemImageSize = normalizeSystemSize(state.systemImageSize);
      request.preserveMetadata = state.preserveMetadata !== false;
      return {
        intent: intent.id,
        options: request,
        useSmart: false,
        warning: '',
        summary: ['系统转换', formatLabel(request.outputFormat), systemSizeLabel(request.systemImageSize)].filter(Boolean).join(' · '),
      };
    }

    if (intent.id === 'custom') {
      request.quality = clampQuality(state.quality, 75);
      request.outputFormat = normalizeFormat(state.outputFormat, 'original');
      request.maxDimensionPx = clampDimension(state.maxDimensionPx);
      request.maxOutputBytes = clampBytes(state.maxOutputBytes);
    } else {
      request.quality = clampQuality(spec.quality, 75);
      request.outputFormat = normalizeFormat(spec.outputFormat, 'original');
      if (intent.editableTarget === 'maxDimensionPx') {
        request.maxDimensionPx = clampDimension(state.maxDimensionPx) || spec.maxDimensionPx;
        request.quality = clampQuality(spec.quality, 72);
      } else if (intent.editableTarget === 'maxOutputBytes') {
        request.maxOutputBytes = clampBytes(state.maxOutputBytes) || spec.maxOutputBytes;
        // 预算模式由引擎的尺寸×质量阶梯决定质量，前端不预设单一质量值。
        request.quality = 0;
      } else {
        request.maxDimensionPx = clampDimension(spec.maxDimensionPx);
      }
    }

    return {
      intent: intent.id,
      options: request,
      useSmart: intent.useSmart !== false,
      warning: intent.warning || '',
      summary: imageSummary(intent, request),
    };
  }

  function clampQuality(value, fallback) {
    var n = Number(value);
    if (!Number.isFinite(n) || n <= 0) return fallback;
    return Math.min(Math.max(Math.round(n), 10), 100);
  }

  function normalizeFormat(value, fallback, system) {
    if (value === 'original') return 'original';
    return imageOutputTargets(system).indexOf(value) >= 0 ? value : fallback;
  }

  function normalizeSystemSize(value) {
    return ['actual', 'large', 'medium', 'small'].indexOf(value) >= 0 ? value : 'actual';
  }

  function formatLabel(format) {
    if (!format || format === 'original') return '原格式';
    if (imageCapabilities) {
      for (var i = 0; i < imageCapabilities.formats.length; i++) {
        var capability = imageCapabilities.formats[i];
        if (capability && capability.id === format && capability.label) return capability.label;
      }
    }
    return String(format).toUpperCase();
  }

  function systemSizeLabel(size) {
    return { actual: '实际大小', large: '大 1280px', medium: '中 640px', small: '小 320px' }[size] || '实际大小';
  }

  function imageSummary(intent, request) {
    if (intent.request && intent.request.processingMode === 'system') {
      return ['系统转换', formatLabel(request.outputFormat)].join(' · ');
    }
    var parts = [intent.label];
    if (request.maxOutputBytes) {
      parts.push('预算 ' + Math.round(request.maxOutputBytes / 1024) + ' KB');
    } else {
      parts.push('Q' + request.quality);
    }
    parts.push(formatLabel(request.outputFormat));
    if (request.maxDimensionPx) parts.push('最长边 ' + request.maxDimensionPx + ' px');
    return parts.filter(Boolean).join(' · ');
  }

  /// 视频意图 → SizqoVideo 的选项形状；调用方再走 validateOptions 做真实校验。
  function compileVideo(state, baseOptions, SizqoVideo) {
    var intent = find(VIDEO_INTENTS, state && state.intent) || find(VIDEO_INTENTS, defaultVideoIntentId());
    var merged = {};
    var base = baseOptions || (SizqoVideo ? SizqoVideo.DEFAULT_OPTIONS : {});
    Object.keys(base).forEach(function (key) { merged[key] = base[key]; });
    if (intent.id !== 'videoCustom') {
      Object.keys(intent.request || {}).forEach(function (key) { merged[key] = intent.request[key]; });
      if (intent.editableTarget === 'targetMiB') {
        var miB = Number(state.targetMiB);
        merged.targetMiB = Number.isFinite(miB) && miB > 0 ? miB : (intent.request.targetMiB || 100);
      }
    } else {
      Object.keys(state.options || {}).forEach(function (key) { merged[key] = state.options[key]; });
    }
    return {
      intent: intent.id,
      options: merged,
      warning: intent.warning || '',
      summary: videoSummary(intent, merged, SizqoVideo),
    };
  }

  function videoSummary(intent, options, SizqoVideo) {
    var parts = [intent.label];
    if (SizqoVideo && typeof SizqoVideo.controlSummary === 'function' && options) {
      // 复用视频侧既有的参数口径，避免两处文案漂移。SizqoVideo.videoSummary 描述的是
      // 源文件，不是请求，这里不能用它。
      parts.push(SizqoVideo.controlSummary(options));
      return parts.filter(Boolean).join(' · ');
    }
    if (options.mode === 'targetSize') parts.push('目标 ' + options.targetMiB + ' MiB（估算）');
    else if (options.mode === 'crf') parts.push('CRF ' + options.crf);
    else if (options.bitrateKbps) parts.push(options.bitrateKbps + ' kbps');
    if (options.resolution) parts.push('最长边 ' + options.resolution);
    return parts.filter(Boolean).join(' · ');
  }

  /// 从已存的原始参数反推意图：只有完全等于某个意图的编译结果时才回到那个意图，
  /// 否则一律 custom ——「看起来像」不算。
  function classifyImage(state, isMac) {
    var intents = imageIntentList(isMac);
    for (var i = 0; i < intents.length; i++) {
      var intent = intents[i];
      if (intent.id === 'custom') continue;
      var compiled = compileImage(assign({}, state, { intent: intent.id }), isMac);
      if (intent.editableTarget) continue;
      if (sameRequest(compiled.options, state.raw)) return intent.id;
    }
    return 'custom';
  }

  function assign(target) {
    for (var i = 1; i < arguments.length; i++) {
      var src = arguments[i] || {};
      Object.keys(src).forEach(function (key) { target[key] = src[key]; });
    }
    return target;
  }

  function sameRequest(a, b) {
    if (!b) return false;
    var keys = ['processingMode', 'quality', 'outputFormat', 'maxDimensionPx', 'maxOutputBytes', 'systemImageSize', 'preserveMetadata'];
    return keys.every(function (key) {
      if (a[key] === undefined && b[key] === undefined) return true;
      return (a[key] === null || a[key] === undefined ? null : a[key])
        === (b[key] === null || b[key] === undefined ? null : b[key]);
    });
  }

  /// 意图条上的「切换到自定义」：把当前意图编译出来的参数原样塞进 raw，
  /// 用户接手的是此刻真实生效的值，不是一个空白默认表单。
  function seedCustom(compiled) {
    var options = compiled.options || {};
    return {
      quality: options.quality || 75,
      outputFormat: options.outputFormat || 'original',
      maxDimensionPx: options.maxDimensionPx || null,
      maxOutputBytes: options.maxOutputBytes || null,
      systemOutputFormat: options.outputFormat || 'original',
      systemImageSize: options.systemImageSize || 'actual',
      preserveMetadata: options.preserveMetadata !== false,
    };
  }

  var api = {
    IMAGE_INTENTS: IMAGE_INTENTS,
    VIDEO_INTENTS: VIDEO_INTENTS,
    imageIntentList: imageIntentList,
    defaultImageIntentId: defaultImageIntentId,
    defaultVideoIntentId: defaultVideoIntentId,
    compileImage: compileImage,
    compileVideo: compileVideo,
    classifyImage: classifyImage,
    seedCustom: seedCustom,
    clampBytes: clampBytes,
    clampDimension: clampDimension,
    imageSummary: imageSummary,
    formatLabel: formatLabel,
    setImageCapabilities: setImageCapabilities,
    imageOutputTargets: imageOutputTargets,
    normalizeFormat: normalizeFormat,
  };

  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  global.SizqoIntents = api;
})(typeof window !== 'undefined' ? window : globalThis);
