// Sizqo - Tauri frontend
// Uses window.__TAURI__ global API (withGlobalTauri: true)

const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

function localizeUiText(value) {
  if (typeof window === 'undefined' || !window.SizqoI18n) return value;
  return window.SizqoI18n.translateText(value);
}

function iconMarkup(name, small) {
  return '<svg class="symbol-icon' + (small ? ' symbol-icon-small' : '') + '" aria-hidden="true"><use href="#icon-' + name + '"></use></svg>';
}

// ─── Path utilities (replace Node's path module) ────────────────
function basename(p) {
  const parts = String(p).replace(/\\/g, '/').split('/');
  return parts[parts.length - 1] || p;
}
function extname(p) {
  const base = basename(p);
  const idx = base.lastIndexOf('.');
  return idx >= 0 ? base.substring(idx) : '';
}
function dirname(p) {
  const parts = String(p).replace(/\\/g, '/').split('/');
  parts.pop();
  return parts.join('/') || '.';
}

function imageFileSrc(filePath) {
  if (typeof convertFileSrc !== 'function') return null;
  return convertFileSrc(filePath);
}

function setImageSource(img, src, timeoutMs) {
  return new Promise((resolve, reject) => {
    var timer = null;
    var done = function(ok) {
      if (timer) clearTimeout(timer);
      img.onload = null;
      img.onerror = null;
      ok ? resolve(true) : reject(new Error('image load failed'));
    };
    img.onload = () => done(true);
    img.onerror = () => done(false);
    if (timeoutMs) {
      timer = setTimeout(function() { done(false); }, timeoutMs);
    }
    img.src = src;
  });
}

async function loadOriginalImage(img, filePath) {
  img.removeAttribute('src');
  const directSrc = imageFileSrc(filePath);
  if (directSrc) {
    try {
      await setImageSource(img, directSrc, 1500);
      return true;
    } catch (_) {
      img.removeAttribute('src');
    }
  }

  const dataUrl = await invoke('read_image_dataurl', { filePath: filePath, preview: false });
  if (!dataUrl) return false;
  await setImageSource(img, dataUrl, 5000);
  return true;
}

// ─── 主题管理（自动/亮色/暗黑）──────────────────────────────────
// 三种模式：auto（跟随系统）、light、dark，循环切换
const THEMES = ['auto', 'light', 'dark'];
let currentTheme = localStorage.getItem('sizqo-theme') || 'auto';
if (!THEMES.includes(currentTheme)) currentTheme = 'auto';

function getResolvedTheme(theme) {
  if (theme === 'auto') {
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
  return theme;
}

function updateTitlebarIcon(resolvedTheme) {
  const icon = document.querySelector('.titlebar-icon-img');
  if (!icon) return;
  const src = resolvedTheme === 'dark' ? icon.dataset.darkSrc : icon.dataset.lightSrc;
  if (src && icon.getAttribute('src') !== src) icon.setAttribute('src', src);
}

function applyTheme(theme) {
  currentTheme = theme;
  localStorage.setItem('sizqo-theme', theme);

  const resolvedTheme = getResolvedTheme(theme);
  invoke('set_startup_theme', { theme: resolvedTheme }).catch(() => {});
  document.documentElement.setAttribute('data-theme-mode', theme);
  if (resolvedTheme === 'dark') {
    document.documentElement.setAttribute('data-theme', 'dark');
  } else {
    document.documentElement.setAttribute('data-theme', 'light');
  }
  updateTitlebarIcon(resolvedTheme);

  // 设置页「外观」里的三段选择器跟着走（主题入口在设置里，标题栏不再放按钮）
  [['auto', 'Auto'], ['light', 'Light'], ['dark', 'Dark']].forEach(function(pair) {
    var btn = document.getElementById('themeChoice' + pair[1]);
    if (!btn) return;
    var active = pair[0] === theme;
    btn.classList.toggle('active', active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  });
}

/// 主题切换入口：设置页「外观」小节。即时生效并记住；auto 跟随系统。
function setThemePreference(value) {
  var theme = THEMES.includes(value) ? value : 'auto';
  applyTheme(theme);
  var labels = { auto: localizeUiText('自动'), light: localizeUiText('亮色'), dark: localizeUiText('暗黑') };
  showToast(localizeUiText('主题: ') + labels[theme]);
}

// 监听系统主题变化（auto 模式下实时响应）
if (window.matchMedia) {
  const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
  const handler = () => {
    if (currentTheme === 'auto') {
      applyTheme('auto');
    }
  };
  if (mediaQuery.addEventListener) {
    mediaQuery.addEventListener('change', handler);
  } else if (mediaQuery.addListener) {
    mediaQuery.addListener(handler);
  }
}

// 初始化主题
applyTheme(currentTheme);
var BUILD_VARIANT = (window.location.origin.indexOf('http://localhost') === 0) ? 'App Store' : 'Direct';
(function() {
  var tb = document.querySelector('.titlebar-text');
  if (tb) tb.textContent = 'Sizqo (' + BUILD_VARIANT + ')';
  try { document.title = 'Sizqo (' + BUILD_VARIANT + ')'; } catch(e) {}
})();

// ─── 意图状态（界面唯一入口，参数只是它的展开形式）───────────────
// 规范见 core/intent-model.md：用户按「要多小」选择，参数由 SizqoIntents 编译。
// 这里只存"用户此刻的意图 + 被编辑过的目标值"，真正的请求每轮从编译器现取。
var imageIntent = {
  intent: window.SizqoIntents.defaultImageIntentId(),
  quality: 75,
  outputFormat: 'original',
  systemOutputFormat: 'original',
  systemImageSize: 'actual',
  preserveMetadata: true,
  maxDimensionPx: null,
  maxOutputBytes: 512 * 1024,
};
var videoIntent = { intent: window.SizqoIntents.defaultVideoIntentId(), targetMiB: 100 };
// 选中项：清单与详情是同一份状态的两张脸，切换选中只重画详情。
var selectedQueueFile = null;
var selectedHistoryKey = null;
// 本轮开始时冻结的意图快照：中途改意图不会让已完成的行的结论跟着变。
var runIntentSnapshot = null;

// State
//
// 队列 = 唯一真相。`queueItems` 记住每个文件**此刻**处于哪一步，
// `files` 只是它在界面上的顺序（同一批路径，按导入先后排列）。
//
// 老实现里"这个文件还需不需要处理"要靠 DOM class（row.classList.contains('cancelled')）、
// 一个 results[] 数组和一个批次计数器互相推测 —— 停止、继续、重新导入三次之后
// 三份数据就对不上了（这正是"停止后剩下的图再也压不动"的来源）。现在只认 queueItems。
//
// 持久状态只有六种（暂停 / 停止是**会话**的状态，不写进这里）：
//   pending   还需要压缩（首次没开始、暂停中等待、停止后留待下一轮、新导入 —— 都是它）
//   running   真的在压
//   done      压完了（成功）
//   failed    压完了（失败），只有显式「重试」才会回到 pending
//   removed   用户把它移出了队列
//   restored  用户把这次压缩撤销了（原图回来了），要压得重新点
let files = [];
let inputPaths = [];
var queueItems = new Map();
// isCompressing / compressionPaused 都在「压缩状态机」那一节声明：
// 它们是 compressionState 的派生镜像，唯一的写入口是 setCompressionState。
let pendingAutoCompress = false;
let outputDir = null;
let currentCompressOptions = null;
var mediaFilter = 'all';
var historyFilter = 'all';
var mediaTaskSeq = 0;
var videoOutputDir = null;
var videoCapabilities = { backend: 'unavailable', available: false, loading: true, reason: '正在检测视频编码能力…', encoders: [], muxers: [] };
var videoPresets = [];
var selectedVideoPresetId = 'balanced';
var currentVideoOptions = window.SizqoVideo.normalizeOptions();
var videoCompareState = null;
var videoCoverQueue = [];
var videoCoverBusy = false;

// DOM Elements
const dropzone = document.getElementById('dropzone');
const qualitySlider = document.getElementById('qualitySlider');
const qualityValue = document.getElementById('qualityValue');
const outputDirDisplay = document.getElementById('outputDirDisplay');
const outputDirRow = document.getElementById('outputDirRow');
const outputSuffixRow = document.getElementById('outputSuffixRow');
const outputSuffixInput = document.getElementById('outputSuffix');

// Quality slider
qualitySlider.addEventListener('input', () => {
  updateQualitySlider();
});

function updateQualitySlider() {
  if (!qualitySlider) return;
  qualityValue.textContent = qualitySlider.value + '%';
  // 滑轨填充只是读数指示，不做成"看起来像进度条"的装饰。
  qualitySlider.style.background = 'linear-gradient(90deg, var(--accent) ' + qualitySlider.value + '%, var(--slider-track) ' + qualitySlider.value + '%)';
}

function isMacOS() {
  return /Macintosh|Mac OS X/.test(navigator.userAgent);
}

/// 当前图片意图 → 引擎请求。每次要用的时候现编译：界面上不留"半个参数"。
function imageRequestFromIntent() {
  return window.SizqoIntents.compileImage(imageIntent, isMacOS());
}

/// 视频意图 → SizqoVideo 选项。自定义之外，控件只是意图的展开显示，
/// 所以基准一律取默认值再叠意图，避免上一轮残留的参数混进来。
function videoRequestFromIntent() {
  var state = videoIntent.intent === 'videoCustom'
    ? { intent: 'videoCustom', options: currentVideoOptions }
    : { intent: videoIntent.intent, targetMiB: videoIntent.targetMiB };
  return window.SizqoIntents.compileVideo(state, window.SizqoVideo.DEFAULT_OPTIONS, window.SizqoVideo);
}

function processingActionText(stage) {
  // 正在跑 / 刚跑完的这一批按开跑时拍下的意图说话；还没开跑时跟着当前 chips。
  var systemMode = (stage === 'progress' || stage === 'done') ? systemModeForRun() : systemModeActive();
  if (stage === 'progress') return systemMode ? localizeUiText('转换中…') : localizeUiText('压缩中…');
  if (stage === 'done') return systemMode ? localizeUiText('转换完成') : localizeUiText('压缩完成');
  // 「继续」= 队列里还有没处理的（停止之后又重新开始一轮）。
  if (stage === 'continue') return systemMode ? localizeUiText('继续转换') : localizeUiText('继续压缩');
  return systemMode ? localizeUiText('开始转换') : localizeUiText('开始压缩');
}

// ─── 意图条（图片目标 / 视频目标）───────────────────────────────
// 用户回答的是"要多小、给谁用"，参数由 SizqoIntents 编译（core/intent-model.md）。
// 这一节只做三件事：画 chips、把编辑权交回 custom、把当前生效的结论说清楚。
function intentChipIcon(id) {
  if (id === 'targetSize' || id === 'videoTarget') return 'target-size';
  if (id === 'systemConvert') return 'globe';
  if (id === 'webSize') return 'globe';
  return null;
}

function renderIntentChips(containerId, list, activeId, onPick) {
  var box = document.getElementById(containerId);
  if (!box) return;
  box.innerHTML = '';
  list.forEach(function (intent) {
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'chip' + (intent.id === activeId ? ' active' : '');
    btn.dataset.intent = intent.id;
    var icon = intentChipIcon(intent.id);
    btn.innerHTML = (icon ? iconMarkup(icon, true) : '') + '<span>' + localizeUiText(intent.label) + '</span>';
    btn.title = localizeUiText(intent.note || intent.label);
    btn.setAttribute('aria-pressed', intent.id === activeId ? 'true' : 'false');
    btn.addEventListener('click', onPick);
    box.appendChild(btn);
  });
}

function setImageIntent(id) {
  if (imageIntent.intent === id) return;
  var current = imageRequestFromIntent();
  imageIntent.intent = id;
  // custom 接手的是**此刻真实生效**的那套参数：预算模式下质量由阶梯决定、编译值是 0，
  // seedCustom 会把它落回一个能用的数，而不是留一个假刻度在滑块上。
  if (id === 'custom') Object.assign(imageIntent, window.SizqoIntents.seedCustom(current));
  if (id === 'custom' || id === 'systemConvert') paintImageStateControls();
  renderIntentBar();
  saveCompressSettings();
  renderDetailPane();
}

function setVideoIntent(id) {
  if (videoIntent.intent === id) return;
  videoIntent.intent = id;
  // 切意图 = 换一套真实生效的参数：把控件摆成这套参数，别留上一套的假象。
  applyVideoOptions(videoRequestFromIntent().options);
  renderIntentBar();
  saveVideoSettings();
}

/// 把意图编译出来的参数写回原始控件，让「更多参数」显示的就是此刻真正在跑的值。
function paintImageRawControls(compiled) {
  var options = compiled.options || {};
  var setValue = function (id, value) {
    var el = document.getElementById(id);
    if (el && value != null) el.value = String(value);
  };
  if (qualitySlider && options.quality) qualitySlider.value = String(options.quality);
  setValue('outputFormat', options.outputFormat);
  setValue('systemOutputFormat', options.outputFormat);
  setValue('systemImageSize', options.systemImageSize);
  var preserve = document.getElementById('preserveMetadata');
  if (preserve && options.preserveMetadata !== undefined) preserve.checked = options.preserveMetadata !== false;
  var dimCap = document.getElementById('imageDimCap');
  if (dimCap && options.maxDimensionPx) dimCap.value = String(options.maxDimensionPx);
  var targetKib = document.getElementById('imageTargetKib');
  if (targetKib && options.maxOutputBytes) targetKib.value = String(Math.round(options.maxOutputBytes / 1024));
  updateQualitySlider();
}

/// custom / systemConvert 的权威是 imageIntent 里存的这几个字段（不是任何意图的编译结果）：
/// 直接照状态铺，空值就留空 —— 留空是"没有上限"，填一个看起来像默认值的数字是撒谎。
function paintImageStateControls() {
  var setValue = function (id, value) {
    var el = document.getElementById(id);
    if (el) el.value = value === null || value === undefined ? '' : String(value);
  };
  setValue('qualitySlider', imageIntent.quality);
  setValue('outputFormat', imageIntent.outputFormat);
  setValue('systemOutputFormat', imageIntent.systemOutputFormat);
  setValue('systemImageSize', imageIntent.systemImageSize);
  setValue('imageDimCap', imageIntent.maxDimensionPx);
  setValue('imageTargetKib', imageIntent.maxOutputBytes ? Math.round(imageIntent.maxOutputBytes / 1024) : null);
  var preserve = document.getElementById('preserveMetadata');
  if (preserve) preserve.checked = imageIntent.preserveMetadata !== false;
  updateQualitySlider();
}

/// 原始控件反过来更新状态：custom 时控件是权威。
function syncImageRawState() {
  imageIntent.quality = parseInt(qualitySlider.value, 10);
  var fmt = document.getElementById('outputFormat');
  if (fmt) imageIntent.outputFormat = fmt.value;
  var sysFmt = document.getElementById('systemOutputFormat');
  if (sysFmt) imageIntent.systemOutputFormat = sysFmt.value;
  var sysSize = document.getElementById('systemImageSize');
  if (sysSize) imageIntent.systemImageSize = sysSize.value;
  var preserve = document.getElementById('preserveMetadata');
  if (preserve) imageIntent.preserveMetadata = preserve.checked;
  var dimCap = document.getElementById('imageDimCap');
  if (dimCap) imageIntent.maxDimensionPx = window.SizqoIntents.clampDimension(dimCap.value);
  var targetKib = document.getElementById('imageTargetKib');
  if (targetKib) imageIntent.maxOutputBytes = window.SizqoIntents.clampBytes(parseFloat(targetKib.value) * 1024);
}

/// 用户直接改原始参数：意图落到 custom，且**保留他刚改的那一项**。
/// 先按 capture 阶段拦下事件，把当前意图的真实参数铺到控件上，再把这一项放回去 ——
/// 否则接手的是"上一套意图没显示出来的值"，改完跳一大格。
function claimRawEdit(kind, target) {
  var customId = kind === 'video' ? 'videoCustom' : 'custom';
  var state = kind === 'video' ? videoIntent : imageIntent;
  if (state.intent === customId || !target || !target.id) return;
  var editedId = target.id;
  var editedIsCheck = target.type === 'checkbox';
  var editedValue = editedIsCheck ? target.checked : target.value;
  var compiled = kind === 'video' ? videoRequestFromIntent() : imageRequestFromIntent();
  state.intent = customId;
  if (kind === 'video') {
    applyVideoOptions(compiled.options);
  } else {
    Object.assign(imageIntent, window.SizqoIntents.seedCustom(compiled));
    paintImageStateControls();
  }
  var el = document.getElementById(editedId);
  if (el) {
    if (editedIsCheck) el.checked = editedValue;
    else el.value = editedValue;
  }
  if (kind === 'image') syncImageRawState();
  renderIntentBar();
  saveCompressSettings();
}

function renderIntentTargetRow(imageCompiled, videoCompiled) {
  var showBytes = imageIntent.intent === 'targetSize';
  var showDim = imageIntent.intent === 'webSize';
  var showMiB = videoIntent.intent === 'videoTarget';
  var row = document.getElementById('intentTargetRow');
  var bytesField = document.getElementById('targetBytesField');
  var dimField = document.getElementById('targetDimField');
  var miBField = document.getElementById('targetMiBField');
  if (row) row.hidden = !(showBytes || showDim || showMiB);
  if (bytesField) bytesField.hidden = !showBytes;
  if (dimField) dimField.hidden = !showDim;
  if (miBField) miBField.hidden = !showMiB;
  if (showBytes && imageCompiled.options && imageCompiled.options.maxOutputBytes) {
    var kib = document.getElementById('imageTargetKib');
    var want = Math.round(imageCompiled.options.maxOutputBytes / 1024);
    if (kib && kib.value !== String(want)) kib.value = String(want);
  }
  if (showDim && imageCompiled.options && imageCompiled.options.maxDimensionPx) {
    var cap = document.getElementById('imageDimCap');
    if (cap && cap.value !== String(imageCompiled.options.maxDimensionPx)) cap.value = String(imageCompiled.options.maxDimensionPx);
  }
  if (showMiB && videoCompiled.options && videoCompiled.options.targetMiB) {
    var mib = document.getElementById('videoTargetMiBInput');
    if (mib && mib.value !== String(videoCompiled.options.targetMiB)) mib.value = String(videoCompiled.options.targetMiB);
  }
  renderTargetPresetChips(showBytes);
}

/// 预算档位：点一下就是一个真实值，不是滑块上的装饰刻度。
function renderTargetPresetChips(active) {
  var box = document.getElementById('targetPresetChips');
  if (!box) return;
  box.innerHTML = '';
  if (!active) return;
  var intent = window.SizqoIntents.IMAGE_INTENTS.filter(function (i) { return i.id === 'targetSize'; })[0];
  var current = imageRequestFromIntent().options.maxOutputBytes;
  (intent && intent.presets ? intent.presets : []).forEach(function (preset) {
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'chip' + (preset.bytes === current ? ' active' : '');
    btn.textContent = localizeUiText(preset.label);
    btn.addEventListener('click', function () {
      var kib = document.getElementById('imageTargetKib');
      if (kib) kib.value = String(Math.round(preset.bytes / 1024));
      syncImageRawState();
      renderIntentBar();
      saveCompressSettings();
    });
    box.appendChild(btn);
  });
}

/// 意图接管参数时，「更多参数」要说清楚现在谁说了算，并给出接手的入口。
function renderAdvancedLock() {
  var specs = [
    { containerId: 'imageAdvancedRows', kind: 'image', intentId: imageIntent.intent, rawAttr: 'image', systemAttr: 'system' },
    { containerId: 'videoAdvancedRows', kind: 'video', intentId: videoIntent.intent, rawAttr: null, systemAttr: null },
  ];
  specs.forEach(function (spec) {
    var box = document.getElementById(spec.containerId);
    if (!box) return;
    var customId = spec.kind === 'video' ? 'videoCustom' : 'custom';
    var owned = spec.intentId === customId
      || (spec.kind === 'image' && spec.intentId === 'systemConvert');
    var note = box.querySelector('.advanced-lock-note');
    if (owned) {
      if (note) note.remove();
      return;
    }
    var label = intentLabel(spec.kind, spec.intentId);
    if (!note) {
      note = document.createElement('p');
      note.className = 'settings-hint advanced-lock-note';
      box.insertBefore(note, box.firstChild);
    }
    note.textContent = localizeUiText('当前由「' + label + '」接管这些参数；改动任意一项会切到自定义，并保留你要改的那一项。');
  });
  // 意图不相关的原始行不显示：看得见但没人读的开关就是假控件。
  document.querySelectorAll('[data-intent-raw]').forEach(function (row) {
    var attr = row.dataset.intentRaw;
    if (attr === 'system') row.hidden = imageIntent.intent !== 'systemConvert';
    else row.hidden = imageIntent.intent !== 'custom';
  });
}

function intentLabel(kind, id) {
  var list = kind === 'video' ? window.SizqoIntents.VIDEO_INTENTS : window.SizqoIntents.IMAGE_INTENTS;
  var found = list.filter(function (i) { return i.id === id; })[0];
  return found ? found.label : id;
}

function renderIntentBar() {
  var isMac = isMacOS();
  var imageCompiled = imageRequestFromIntent();
  var videoCompiled = videoRequestFromIntent();
  renderIntentChips('imageIntentChips', window.SizqoIntents.imageIntentList(isMac), imageIntent.intent, function (event) {
    setImageIntent(event.currentTarget.dataset.intent);
  });
  renderIntentChips('videoIntentChips', window.SizqoIntents.VIDEO_INTENTS, videoIntent.intent, function (event) {
    setVideoIntent(event.currentTarget.dataset.intent);
  });
  var imageNote = document.getElementById('imageIntentNote');
  if (imageNote) imageNote.textContent = localizeUiText(imageCompiled.warning
    ? imageCompiled.summary + ' · ' + intentNoteFor('image', imageIntent.intent)
    : imageCompiled.summary);
  var videoNote = document.getElementById('videoIntentNote');
  if (videoNote) videoNote.textContent = localizeUiText(videoCompiled.warning
    ? videoCompiled.summary + ' · ' + intentNoteFor('video', videoIntent.intent)
    : videoCompiled.summary);
  var imageSummary = document.getElementById('imageIntentSummary');
  if (imageSummary) imageSummary.textContent = localizeUiText(imageCompiled.summary);
  var videoSummary = document.getElementById('videoIntentSummary');
  if (videoSummary) videoSummary.textContent = localizeUiText(videoCompiled.summary);
  if (videoCompiled.options && videoCompiled.options.targetMiB) videoIntent.targetMiB = videoCompiled.options.targetMiB;

  // 意图不驱动任何控件时不该假装能编辑：非自定义时把意图的值铺上去。
  if (imageIntent.intent !== 'custom' && imageIntent.intent !== 'systemConvert') paintImageRawControls(imageCompiled);
  renderIntentTargetRow(imageCompiled, videoCompiled);
  renderAdvancedLock();

  var advanced = document.getElementById('intentAdvanced');
  if (advanced) {
    var imageGroup = document.getElementById('imageIntentGroup');
    var videoGroup = document.getElementById('videoIntentGroup');
    var imageCol = document.getElementById('imageAdvancedRows');
    var videoCol = document.getElementById('videoAdvancedRows');
    if (imageCol) imageCol.hidden = !(imageGroup && !imageGroup.hidden);
    if (videoCol) videoCol.hidden = !(videoGroup && !videoGroup.hidden);
  }
  var infoBtn = document.getElementById('systemInfoBtn');
  if (infoBtn) infoBtn.hidden = imageCompiled.options.processingMode !== 'system';
  updateAutoCompressLabel();
}

function intentNoteFor(kind, id) {
  var list = kind === 'video' ? window.SizqoIntents.VIDEO_INTENTS : window.SizqoIntents.IMAGE_INTENTS;
  var found = list.filter(function (i) { return i.id === id; })[0];
  return found && found.warning ? found.warning : '';
}

function updateAutoCompressLabel() {
  var autoLabel = document.getElementById('autoCompressLabel');
  if (!autoLabel) return;
  autoLabel.textContent = systemModeActive()
    ? localizeUiText('拖入或选择后自动转换')
    : localizeUiText('拖入或选择后自动压缩');
}

/// 开始按钮的文案只跟着队列与意图走，不读 CSS class —— 实现在「运行条」那一节。

function bindIntentControls() {
  var advanced = document.getElementById('intentAdvanced');
  if (advanced) {
    var claim = function (event) {
      var target = event.target;
      if (!target || !target.closest) return;
      var col = target.closest('.advanced-col');
      if (!col) return;
      claimRawEdit(col.id === 'videoAdvancedRows' ? 'video' : 'image', target);
    };
    advanced.addEventListener('change', claim, true);
    advanced.addEventListener('input', claim, true);
  }
  ['imageTargetKib', 'imageDimCap', 'videoTargetMiBInput'].forEach(function (id) {
    var el = document.getElementById(id);
    if (!el) return;
    var handler = function () {
      if (id === 'videoTargetMiBInput') {
        var n = parseFloat(el.value);
        if (Number.isFinite(n) && n > 0) videoIntent.targetMiB = n;
        // 只有"按体积"这件事归意图管的时候才接手它：自定义之下这格数字是
        // 用户自己填的参数，把它翻成 videoTarget 会连带换掉整套自定义编码参数。
        if (videoIntent.intent !== 'videoCustom' && videoIntent.intent !== 'videoTarget') {
          videoIntent.intent = 'videoTarget';
        }
      } else {
        syncImageRawState();
      }
      renderIntentBar();
      saveCompressSettings();
      saveVideoSettings();
    };
    el.addEventListener('input', handler);
    el.addEventListener('change', handler);
  });
}

// Output mode radio
function currentOutputMode() {
  var selected = document.querySelector('input[name="outputMode"]:checked');
  // 回落值必须与 HTML 里 checked 的那一项一致，也与 Swift 的默认档位一致：
  // 三轨的默认输出方式是同一份契约，不是各页各写一遍。
  return selected ? selected.value : 'replace';
}

function updateOutputModeControls() {
  var mode = currentOutputMode();
  if (outputDirRow) outputDirRow.hidden = mode !== 'folder';
  if (outputSuffixRow) outputSuffixRow.hidden = mode !== 'suffix';
}
document.querySelectorAll('input[name="outputMode"]').forEach(radio => {
  radio.addEventListener('change', () => {
    updateOutputModeControls();
    saveCompressSettings();
    renderDetailPane();
  });
});

function getOutputSuffix() {
  var suffix = outputSuffixInput ? outputSuffixInput.value.trim() : '';
  return suffix || '_compressed';
}

function getResultOutputSuffix(result) {
  return result && result.compressOptions && result.compressOptions.outputSuffix
    ? result.compressOptions.outputSuffix
    : getOutputSuffix();
}

function toggleVideoSettings() {
  var advanced = document.getElementById('intentAdvanced');
  if (advanced) advanced.open = !advanced.open;
}

function videoOptionsFromControls() {
  var resolutionValue = document.getElementById('videoResolution').value;
  return window.SizqoVideo.normalizeOptions({
    encoder: document.getElementById('videoEncoder').value || 'auto',
    mode: document.getElementById('videoMode').value || 'crf',
    crf: parseInt(document.getElementById('videoCrf').value, 10),
    bitrateKbps: parseInt(document.getElementById('videoBitrate').value, 10),
    targetMiB: parseFloat(document.getElementById('videoTargetMiBInput').value),
    preset: document.getElementById('videoSpeed').value || 'medium',
    resolution: resolutionValue === 'custom'
      ? parseInt(document.getElementById('videoResolutionCustom').value, 10)
      : parseInt(resolutionValue, 10),
    frameRate: document.getElementById('videoFrameRate').value
      ? parseInt(document.getElementById('videoFrameRate').value, 10) : null,
    audioPolicy: document.getElementById('videoAudioPolicy').value,
    subtitlePolicy: document.getElementById('videoSubtitlePolicy').value,
    metadataPolicy: document.getElementById('videoMetadataPolicy').value,
    outputMode: document.getElementById('videoOutputMode').value,
    outputDir: document.getElementById('videoOutputMode').value === 'directory' ? videoOutputDir : null,
    outputSuffix: document.getElementById('videoOutputSuffix').value.trim() || '_compressed',
    allowLargerOutput: document.getElementById('videoAllowLarger').checked,
    container: document.getElementById('videoContainer').value,
    threads: parseInt(document.getElementById('videoThreads').value, 10) || 0,
  });
}

function applyVideoOptions(options) {
  currentVideoOptions = window.SizqoVideo.normalizeOptions(options);
  var setValue = function(id, value) {
    var element = document.getElementById(id);
    if (element && value != null) element.value = String(value);
  };
  setValue('videoEncoder', currentVideoOptions.encoder);
  setValue('videoMode', currentVideoOptions.mode);
  setValue('videoSpeed', currentVideoOptions.preset);
  setValue('videoCrf', currentVideoOptions.crf);
  setValue('videoBitrate', currentVideoOptions.bitrateKbps);
  setValue('videoTargetMiBInput', currentVideoOptions.targetMiB);
  setValue('videoResolution', [0, 2160, 1440, 1080, 720].includes(Number(currentVideoOptions.resolution)) ? currentVideoOptions.resolution : 'custom');
  if (![0, 2160, 1440, 1080, 720].includes(Number(currentVideoOptions.resolution))) setValue('videoResolutionCustom', currentVideoOptions.resolution);
  setValue('videoFrameRate', currentVideoOptions.frameRate);
  setValue('videoAudioPolicy', currentVideoOptions.audioPolicy);
  setValue('videoSubtitlePolicy', currentVideoOptions.subtitlePolicy);
  setValue('videoMetadataPolicy', currentVideoOptions.metadataPolicy);
  setValue('videoOutputMode', currentVideoOptions.outputMode);
  setValue('videoOutputSuffix', currentVideoOptions.outputSuffix);
  setValue('videoContainer', currentVideoOptions.container);
  setValue('videoThreads', currentVideoOptions.threads);
  var allowLarger = document.getElementById('videoAllowLarger');
  if (allowLarger) allowLarger.checked = !!currentVideoOptions.allowLargerOutput;
  videoOutputDir = currentVideoOptions.outputDir || videoOutputDir;
  var dirDisplay = document.getElementById('videoOutputDirDisplay');
  if (dirDisplay) { dirDisplay.textContent = videoOutputDir || localizeUiText('未选择'); dirDisplay.title = videoOutputDir || ''; }
  syncVideoSettingsControls();
}

function syncVideoSettingsControls() {
  currentVideoOptions = videoOptionsFromControls();
  var mode = currentVideoOptions.mode;
  document.querySelectorAll('.video-mode-field').forEach(function(row) {
    var modes = (row.dataset.videoModes || '').split(',');
    row.hidden = !modes.includes(mode);
  });
  var customResolution = document.getElementById('videoResolutionCustom');
  if (customResolution) customResolution.hidden = document.getElementById('videoResolution').value !== 'custom';
  var outputMode = currentVideoOptions.outputMode;
  var suffixRow = document.getElementById('videoSuffixRow');
  var directoryRow = document.getElementById('videoDirectoryRow');
  if (suffixRow) suffixRow.hidden = outputMode !== 'suffix';
  if (directoryRow) directoryRow.hidden = outputMode !== 'directory';

  var modes = window.SizqoVideo.encoderModes(videoCapabilities, currentVideoOptions.encoder);
  var modeSelect = document.getElementById('videoMode');
  if (modeSelect) Array.from(modeSelect.options).forEach(function(option) {
    option.disabled = videoCapabilities.available !== true || !modes.includes(option.value);
  });
  updateVideoSettingsSummary();
  renderVideoQueueRows();
}

/// 视频摘要就是"当前意图编译出来的那套参数"，不再另设一个小字区。
/// 这里只改文案、不重建 chips —— 重建会绕回 applyVideoOptions 形成回环。
function updateVideoSettingsSummary() {
  var compiled = videoRequestFromIntent();
  if (compiled.options && compiled.options.targetMiB) videoIntent.targetMiB = compiled.options.targetMiB;
  var summary = document.getElementById('videoIntentSummary');
  if (summary) summary.textContent = localizeUiText(compiled.summary);
  var note = document.getElementById('videoIntentNote');
  if (note) {
    note.textContent = localizeUiText(compiled.warning
      ? compiled.summary + ' · ' + intentNoteFor('video', videoIntent.intent)
      : compiled.summary);
  }
}

function saveVideoSettings() {
  currentVideoOptions = videoOptionsFromControls();
  try { localStorage.setItem('sizqo-video-settings', JSON.stringify({ options: currentVideoOptions, selectedPresetId: selectedVideoPresetId })); } catch (_) {}
  syncVideoSettingsControls();
}

function loadVideoSettings() {
  var saved = null;
  try { saved = JSON.parse(localStorage.getItem('sizqo-video-settings') || 'null'); } catch (_) {}
  if (saved && saved.options) {
    selectedVideoPresetId = saved.selectedPresetId || 'balanced';
    applyVideoOptions(saved.options);
    return true;
  }
  selectedVideoPresetId = 'balanced';
  applyVideoOptions(saved || window.SizqoVideo.DEFAULT_OPTIONS);
  return false;
}

function applyVideoPreset(presetId) {
  var entry = videoPresets.find(function(preset) { return window.SizqoVideo.presetId(preset) === presetId; });
  if (!entry || !entry.options || entry.available === false || entry.directOnly && videoCapabilities.backend !== 'ffmpeg') return false;
  var previous = videoOptionsFromControls();
  selectedVideoPresetId = presetId;
  // Presets configure encoding choices while retaining the user's output and
  // preservation preferences. The advanced controls remain editable after this.
  var options = Object.assign({}, previous, entry.options, {
    outputMode: previous.outputMode,
    outputDir: previous.outputDir,
    outputSuffix: previous.outputSuffix,
    allowLargerOutput: previous.allowLargerOutput,
    metadataPolicy: previous.metadataPolicy,
    subtitlePolicy: previous.subtitlePolicy,
  });
  applyVideoOptions(options);
  saveVideoSettings();
  return true;
}

function renderVideoCapabilities() {
  var note = document.getElementById('videoCapabilityNote');
  var encoderSelect = document.getElementById('videoEncoder');
  var presetSelect = document.getElementById('videoPreset');
  if (note) {
    var message = videoCapabilities.available
      ? (videoCapabilities.reason || (videoCapabilities.backend === 'ffmpeg' ? localizeUiText('Direct 使用独立 FFmpeg 编码能力；编码器按当前运行时检测结果开放。') : localizeUiText('视频编码能力已检测。')))
      : (videoCapabilities.reason || localizeUiText('此版本当前不提供可运行的视频编码器。'));
    note.textContent = message;
    note.classList.toggle('visible', !videoCapabilities.available || !!videoCapabilities.reason);
    note.classList.toggle('warning', !videoCapabilities.available);
  }
  if (encoderSelect) {
    encoderSelect.innerHTML = '';
    if (videoCapabilities.loading) {
      var detecting = document.createElement('option');
      detecting.value = 'auto';
      detecting.textContent = localizeUiText('正在检测视频编码能力…');
      detecting.disabled = true;
      detecting.selected = true;
      encoderSelect.appendChild(detecting);
    } else if (videoCapabilities.available) {
      var autoOption = document.createElement('option');
      autoOption.value = 'auto';
      autoOption.textContent = localizeUiText('自动（按能力选择）');
      encoderSelect.appendChild(autoOption);
      (videoCapabilities.encoders || []).forEach(function(encoder) {
        if (!encoder || !encoder.id) return;
        var option = document.createElement('option');
        option.value = encoder.id;
        option.textContent = encoder.label || encoder.id;
        encoderSelect.appendChild(option);
      });
      if (currentVideoOptions.encoder !== 'auto' && !window.SizqoVideo.findEncoder(videoCapabilities, currentVideoOptions.encoder)) {
        var missing = document.createElement('option');
        missing.value = currentVideoOptions.encoder;
        missing.textContent = currentVideoOptions.encoder + localizeUiText('（当前运行时不可用）');
        missing.disabled = true;
        missing.selected = true;
        encoderSelect.appendChild(missing);
      }
    } else {
      var unavailable = document.createElement('option');
      unavailable.value = 'auto';
      unavailable.textContent = localizeUiText('当前运行时不支持视频编码');
      unavailable.disabled = true;
      encoderSelect.appendChild(unavailable);
    }
  }
  if (presetSelect) {
    presetSelect.innerHTML = '';
    (videoPresets || []).forEach(function(preset) {
      var id = window.SizqoVideo.presetId(preset);
      if (!id) return;
      var option = document.createElement('option');
      option.value = id;
      option.textContent = window.SizqoVideo.presetLabel(preset) + (preset.directOnly ? localizeUiText('（仅 Direct）') : '');
      option.disabled = preset.available === false || (preset.directOnly === true && videoCapabilities.backend !== 'ffmpeg') || videoCapabilities.available !== true;
      presetSelect.appendChild(option);
    });
    if (!videoPresets.length) {
      var unavailablePreset = document.createElement('option');
      unavailablePreset.value = 'balanced';
      unavailablePreset.textContent = videoCapabilities.loading ? localizeUiText('正在检测视频编码能力…') : localizeUiText('视频预设不可用');
      unavailablePreset.disabled = true;
      presetSelect.appendChild(unavailablePreset);
    }
  }
  if (encoderSelect && currentVideoOptions.encoder) encoderSelect.value = currentVideoOptions.encoder;
  if (presetSelect && selectedVideoPresetId) presetSelect.value = selectedVideoPresetId;
  syncVideoSettingsControls();
}

async function loadVideoCapabilities() {
  var results = await Promise.allSettled([invoke('get_video_capabilities'), invoke('get_video_presets')]);
  if (results[0].status === 'fulfilled' && results[0].value) videoCapabilities = results[0].value;
  else videoCapabilities = { backend: 'unavailable', available: false, reason: results[0].reason && (results[0].reason.message || String(results[0].reason)) || localizeUiText('视频能力探测失败'), encoders: [], muxers: [] };
  videoPresets = results[1].status === 'fulfilled' && Array.isArray(results[1].value) ? results[1].value : [];
  var stored = null;
  try { stored = JSON.parse(localStorage.getItem('sizqo-video-settings') || 'null'); } catch (_) {}
  if (!stored && videoCapabilities.available) {
    var balanced = videoPresets.find(function(preset) { return window.SizqoVideo.presetId(preset) === 'balanced' && preset.options; });
    if (balanced) {
      selectedVideoPresetId = 'balanced';
      applyVideoOptions(balanced.options);
    }
  }
  renderVideoCapabilities();
  updateMediaWorkspace();
}

async function selectVideoOutputDir() {
  try {
    var dirs = await invoke('select_output_dir');
    if (!dirs || !dirs.length) return false;
    videoOutputDir = dirs[0];
    var display = document.getElementById('videoOutputDirDisplay');
    if (display) { display.textContent = videoOutputDir; display.title = videoOutputDir; }
    saveVideoSettings();
    return true;
  } catch (error) {
    showToast(localizeUiText('无法选择视频输出目录: ') + (error.message || error));
    return false;
  }
}

function bindVideoSettings() {
  var presetSelect = document.getElementById('videoPreset');
  if (presetSelect) presetSelect.addEventListener('change', function() {
    if (!applyVideoPreset(presetSelect.value)) {
      showToast(localizeUiText('所选视频预设当前不可用'));
      presetSelect.value = selectedVideoPresetId;
    }
  });
  ['videoEncoder', 'videoSpeed', 'videoMode', 'videoCrf', 'videoBitrate', 'videoTargetMiBInput',
    'videoResolution', 'videoResolutionCustom', 'videoFrameRate', 'videoAudioPolicy', 'videoSubtitlePolicy',
    'videoMetadataPolicy', 'videoOutputMode', 'videoOutputSuffix', 'videoContainer', 'videoThreads', 'videoAllowLarger']
    .forEach(function(id) {
      var element = document.getElementById(id);
      if (!element) return;
      element.addEventListener('change', function() { saveVideoSettings(); });
      if (['videoOutputSuffix', 'videoCrf', 'videoBitrate', 'videoTargetMiBInput', 'videoResolutionCustom', 'videoThreads'].includes(id)) {
        element.addEventListener('input', saveVideoSettings);
      }
    });
}

async function validateVideoBatch(paths, options) {
  if (!videoCapabilities.available) return { valid: false, errors: [videoCapabilities.reason || localizeUiText('视频编码当前不可用')] };
  for (var i = 0; i < paths.length; i++) {
    var item = queueItems.get(paths[i]);
    if (!item || item.mediaKind !== 'video') continue;
    var local = window.SizqoVideo.validateOptions(options, item.videoProbe, videoCapabilities);
    if (!local.valid) return local;
    try {
      var verdict = await invoke('validate_video_options', { options: options, probe: item.videoProbe });
      if (verdict === false || (verdict && verdict.valid === false)) {
        return { valid: false, errors: verdict && verdict.errors && verdict.errors.length ? verdict.errors : [localizeUiText('该视频与当前设置不兼容')], warnings: verdict && verdict.warnings || [] };
      }
    } catch (error) {
      return { valid: false, errors: [error.message || localizeUiText('视频参数校验失败')] };
    }
  }
  return { valid: true, errors: [], warnings: [] };
}

function renderVideoQueueRows() {
  files.forEach(function(filePath) {
    var item = queueItems.get(filePath);
    if (item && item.mediaKind === 'video') paintQueueRow(filePath);
  });
}

function openSystemConversionInfo() {
  var backdrop = document.getElementById('systemInfoBackdrop');
  var panel = document.getElementById('systemInfoPanel');
  if (!backdrop || !panel) return;
  backdrop.style.display = 'block';
  panel.style.display = 'block';
  document.body.style.overflow = 'hidden';
}

function closeSystemConversionInfo() {
  var backdrop = document.getElementById('systemInfoBackdrop');
  var panel = document.getElementById('systemInfoPanel');
  if (backdrop) backdrop.style.display = 'none';
  if (panel) panel.style.display = 'none';
  document.body.style.overflow = '';
}

// ─── Drag and drop via Tauri ────────────────────────────────────
async function setupDragDrop() {
  try {
    const { getCurrentWebview } = window.__TAURI__.webview;
    const webview = getCurrentWebview();
    await webview.onDragDropEvent((event) => {
      const payload = event.payload;
      if (payload.type === 'drop') {
        dropzone.classList.remove('dragover');
        if (payload.paths && payload.paths.length > 0) {
          handleFilePaths(payload.paths);
        }
      } else if (payload.type === 'enter' || payload.type === 'over') {
        dropzone.classList.add('dragover');
      } else if (payload.type === 'leave') {
        dropzone.classList.remove('dragover');
      }
    });
  } catch (e) {
    console.error('Tauri drag-drop setup failed, falling back to HTML5:', e);
    // Fallback: HTML5 drag-drop (paths won't be available in Tauri)
    dropzone.addEventListener('dragover', (e) => { e.preventDefault(); dropzone.classList.add('dragover'); });
    dropzone.addEventListener('dragleave', () => { dropzone.classList.remove('dragover'); });
    dropzone.addEventListener('drop', (e) => {
      e.preventDefault();
      dropzone.classList.remove('dragover');
      const droppedFiles = Array.from(e.dataTransfer.files);
      handleFiles(droppedFiles);
    });
  }
}
setupDragDrop();

// File selection
async function selectFiles() {
  try {
    const filePaths = await invoke('select_files');
    if (filePaths && filePaths.length > 0) {
      handleFilePaths(filePaths);
    }
  } catch (error) {
    console.error('File selection failed:', error);
    showToast(localizeUiText('选择图片失败，请重试'));
  }
}

async function selectFolder() {
  try {
    const folderPaths = await invoke('select_folder');
    if (folderPaths && folderPaths.length > 0) {
      handleFilePaths(folderPaths);
    }
  } catch (error) {
    console.error('Folder selection failed:', error);
    showToast(localizeUiText('选择文件夹失败，请重试'));
  }
}

async function selectOutputDir() {
  const dirs = await invoke('select_output_dir');
  if (dirs && dirs.length > 0) {
    outputDir = dirs[0];
    outputDirDisplay.textContent = outputDir;
    outputDirDisplay.title = outputDir;
    return true;
  }
  return false;
}

async function ensureSystemOutputAccess() {
  if (!systemModeActive() || BUILD_VARIANT !== 'App Store') return true;
  if (currentOutputMode() === 'folder' && outputDir) return true;

  showToast(localizeUiText('请选择系统转换文件的输出文件夹'));
  if (!await selectOutputDir()) return false;
  var folderMode = document.querySelector('input[name="outputMode"][value="folder"]');
  if (folderMode) folderMode.checked = true;
  updateOutputModeControls();
  saveCompressSettings();
  renderDetailPane();
  return true;
}

function handleFiles(fileList) {
  const filePaths = [];
  for (const file of fileList) {
    if (file.path) filePaths.push(file.path);
  }
  if (filePaths.length === 0 && fileList.length > 0) {
    showToast(localizeUiText('无法读取拖入文件，请使用“选择文件”'));
    return Promise.resolve([]);
  }
  return handleFilePaths(filePaths);
}

function uniqueFilePaths(paths) {
  var seen = new Set();
  var unique = [];
  (paths || []).forEach(function(path) {
    if (typeof path !== 'string') return;
    var value = path.trim();
    if (!value || seen.has(value)) return;
    seen.add(value);
    unique.push(value);
  });
  return unique;
}

function appendUniquePaths(target, paths) {
  var result = target.slice();
  var seen = new Set(result);
  uniqueFilePaths(paths).forEach(function(path) {
    if (!seen.has(path)) {
      seen.add(path);
      result.push(path);
    }
  });
  return result;
}

function createMediaTaskId() {
  if (typeof mediaTaskSeq === 'number') mediaTaskSeq += 1;
  if (window.crypto && typeof window.crypto.randomUUID === 'function') return window.crypto.randomUUID();
  return 'task-' + Date.now() + '-' + (typeof mediaTaskSeq === 'number' ? mediaTaskSeq : Math.random().toString(36).slice(2));
}

function normalizedMediaKind(value) {
  var kind = String(value || '').toLowerCase();
  if (kind === 'video') return 'video';
  if (kind === 'image') return 'image';
  return null;
}

function mergeQueueMedia(records) {
  var known = new Set(files);
  var added = [];
  var accepted = new Map();
  (records || []).forEach(function(record) {
    if (!record || typeof record !== 'object') return;
    var filePath = typeof record.path === 'string' ? record.path.trim() : '';
    var mediaKind = normalizedMediaKind(record.mediaKind);
    if (!filePath || !mediaKind || accepted.has(filePath)) return;
    accepted.set(filePath, Object.assign({}, record, { path: filePath, mediaKind: mediaKind }));
  });
  accepted.forEach(function(record, filePath) {
    if (known.has(filePath)) return;
    known.add(filePath);
    files.push(filePath);
    var taskId = createMediaTaskId();
    // 重新导入 = 全新的 pending 项：旧的移除记录在这里被覆盖，
    // 不会复活成"已跳过"那种再也压不动的状态。
    queueItems.set(filePath, {
      id: taskId, taskId: taskId, path: filePath,
      mediaKind: record.mediaKind, state: 'pending', result: null,
      originalSize: Number.isFinite(Number(record.size)) ? Number(record.size) : null,
      videoProbe: record.videoProbe || null,
      probeWarning: record.warning || null,
      probeWarnings: Array.isArray(record.warnings) ? record.warnings.slice() : [],
      videoProgress: null, coverUrl: null,
    });
    added.push(filePath);
  });
  return added;
}

function handleFilePaths(filePaths) {
  var incoming = uniqueFilePaths(filePaths);
  if (incoming.length === 0) return Promise.resolve([]);

  // Serialize directory expansion and queue commits. This keeps a slow scan
  // from finishing after a later, faster scan and overwriting the queue.
  pendingImports = pendingImports.then(async function() {
    var probed;
    try {
      // One backend probe expands directories and identifies media from contents.
      // The image-only expansion path must never receive video files.
      probed = await invoke('probe_media', { paths: incoming });
    } catch (e) {
      console.error('Media probe failed:', e);
      showToast(localizeUiText('媒体分析失败，请重试'));
      return [];
    }

    var mediaRecords = Array.isArray(probed) ? probed : [];
    var supportedRecords = mediaRecords.filter(function(record) {
      return record && normalizedMediaKind(record.mediaKind);
    });
    if (supportedRecords.length === 0) {
      var firstWarning = mediaRecords.find(function(record) { return record && record.warning; });
      showToast(firstWarning ? firstWarning.warning : localizeUiText('没有找到可处理的图片或视频'));
      return [];
    }

    var added = mergeQueueMedia(supportedRecords);
    inputPaths = appendUniquePaths(inputPaths, incoming);

    updateQueueSummary();
    renderFileQueue();
    if (typeof updateMediaWorkspace === 'function') updateMediaWorkspace();
    added.forEach(function(filePath) {
      var item = queueItems.get(filePath);
      if (item && item.mediaKind === 'video' && item.videoProbe) queueVideoCover(item);
    });
    // 导入之后详情列不该是空的：默认落到第一个新来的文件，用户一眼能看到"它到底多大"。
    if (added.length > 0) selectQueueFile(added[0]);
    var scroller = document.querySelector('.list-scroll');
    if (scroller) scroller.scrollTop = 0;

    if (added.length === 0) {
      showToast(localizeUiText('所选媒体已在队列中'));
      return added;
    }

    var unsupported = mediaRecords.filter(function(record) { return record && record.warning && !normalizedMediaKind(record.mediaKind); });
    if (unsupported.length > 0) showToast(unsupported[0].warning);

    var ac = document.getElementById('autoCompress');
    if (ac && ac.checked) {
      if (isCompressing) {
        pendingAutoCompress = true;
      } else {
        startCompression(false);
      }
    }
    return added;
  }).catch(function(error) {
    console.error('Queue import failed:', error);
    showToast(localizeUiText('添加媒体失败，请重试'));
    return [];
  });
  return pendingImports;
}

// Global state for compression
var fileRows = {};
var queueSortDescending = false;
var pendingImports = Promise.resolve();
var queueRevision = 0;

function getRunnablePendingPaths(requestedPaths) {
  var requested = getPendingQueuePaths(requestedPaths);
  var filter = typeof mediaFilter === 'string' ? mediaFilter : 'all';
  var canVideo = typeof videoCapabilities === 'undefined' || videoCapabilities.available === true;
  return requested.filter(function(filePath) {
    var item = queueItems.get(filePath);
    if (filter !== 'all' && item && item.mediaKind && item.mediaKind !== filter) return false;
    return !item || item.mediaKind !== 'video' || canVideo;
  });
}

function setMediaFilter(filter) {
  mediaFilter = ['all', 'image', 'video'].includes(filter) ? filter : 'all';
  showView('main');
  // 变量本身不画任何东西：chips 高亮、意图组、队列行过滤都由这次重绘驱动。
  updateMediaWorkspace();
}

function setHistoryFilter(filter) {
  historyFilter = ['all', 'image', 'video'].includes(filter) ? filter : 'all';
  renderHistory();
}

/// chips 的高亮只从 mediaFilter / historyFilter 两个变量派生，绝不去读 CSS class。
function updateFilterChips() {
  document.querySelectorAll('#mediaFilterChips [data-media-filter]').forEach(function(button) {
    var selected = button.dataset.mediaFilter === mediaFilter;
    button.classList.toggle('active', selected);
    button.setAttribute('aria-pressed', selected ? 'true' : 'false');
  });
  document.querySelectorAll('#historyFilterChips [data-history-filter]').forEach(function(button) {
    var selected = button.dataset.historyFilter === historyFilter;
    button.classList.toggle('active', selected);
    button.setAttribute('aria-pressed', selected ? 'true' : 'false');
  });
}

function queueKindCounts() {
  var imageCount = 0;
  var videoCount = 0;
  files.forEach(function(filePath) {
    var item = queueItems.get(filePath);
    if (item && item.mediaKind === 'video') videoCount++;
    else imageCount++;
  });
  return { image: imageCount, video: videoCount };
}

/// 意图条只留这一轮真用得上的那一组：队列里没有视频时，"视频目标"就是装饰；
/// 队列为空时两组都在，因为下一步导入什么还不知道。
function updateIntentGroups() {
  var counts = queueKindCounts();
  var imageGroup = document.getElementById('imageIntentGroup');
  var videoGroup = document.getElementById('videoIntentGroup');
  if (imageGroup) imageGroup.hidden = mediaFilter === 'video' || (mediaFilter === 'all' && counts.video > 0 && counts.image === 0);
  if (videoGroup) videoGroup.hidden = mediaFilter === 'image' || (mediaFilter === 'all' && counts.image > 0 && counts.video === 0);
}

function updateMediaWorkspace() {
  updateFilterChips();
  updateIntentGroups();
  renderIntentBar();
  applyQueueView();
  renderStartButton();
}

// ─── 队列状态（唯一真相，DOM 只能照着它画）───────────────────────
// 这一段是"这个文件还需不需要处理"的**唯一**判据，前端测试直接切这段真实代码来跑。

/// 队列状态的读入口。DOM 只能**照着它**画，不许反过来当状态用。
function queueItem(file) {
  return queueItems.get(file) || null;
}

function queueState(file) {
  var item = queueItems.get(file);
  return item ? item.state : null;
}

function setQueueState(file, state) {
  var item = queueItems.get(file);
  if (!item) return null;
  item.state = state;
  return item;
}

/// 队列里此刻该处理的文件 = state 为 pending 的那些，按队列顺序。
/// `requestedPaths` 给"只处理这几个"用（单文件重试）。
function getPendingQueuePaths(requestedPaths) {
  var wanted = Array.isArray(requestedPaths) ? new Set(requestedPaths) : null;
  return files.filter(function(file) {
    if (wanted && !wanted.has(file)) return false;
    return queueState(file) === 'pending';
  });
}

/// 队列里全部结果（按队列顺序）。压缩产物的一切操作（另存为/对比/恢复/导出）
/// 都从队列读，不再维护一个平行的 results 数组。
function queueResults() {
  var out = [];
  files.forEach(function(file) {
    var item = queueItems.get(file);
    if (item && item.result) out.push(item.result);
  });
  return out;
}

/// 用户看到的总进度。**这是队列的属性，不是某一轮的属性**：
/// 停止后重新开始一轮时，这一轮的目标会变小（只含 pending），但这里的 total
/// 一直是整个队列（100 → 100），所以进度绝不会从 40/100 掉回 0/60。
function getQueueProgress() {
  var progress = { total: 0, processed: 0, done: 0, failed: 0, running: 0, pending: 0 };
  files.forEach(function(file) {
    var item = queueItems.get(file);
    if (!item) return;
    // removed / restored 已经离开了这套账：一个被移出队列，一个被用户撤销了。
    if (item.state === 'removed' || item.state === 'restored') return;
    progress.total += 1;
    if (item.state === 'done') progress.done += 1;
    else if (item.state === 'failed') progress.failed += 1;
    else if (item.state === 'running') progress.running += 1;
    else if (item.state === 'pending') progress.pending += 1;
  });
  // failed 也算"处理过"：它确实跑完了一次，只是没成。否则 98 成功 + 2 失败
  // 的队列会永远停在 98%，那条进度条就成了假的。
  progress.processed = progress.done + progress.failed;
  return progress;
}

function updateQueueSummary() {
  var summary = document.getElementById('queueSummary');
  if (!summary) return;
  var p = getQueueProgress();
  if (p.total === 0) {
    summary.textContent = files.length + localizeUiText(' 个文件');
  } else {
    // 「已处理」而不是「已完成」：失败的文件也算处理过，说"完成"会把失败藏起来。
    summary.textContent = p.processed + ' / ' + p.total + localizeUiText(' 已处理')
      + (p.failed > 0 ? localizeUiText(' · 失败 ') + p.failed : '')
      + compressionStateSuffix()
      + cpuLimitText();
  }
  updateBulkActionButtons();
  applyQueueView();
  renderQueueProgressFill();
  renderRunBar(p);
  renderStartButton();
}

/// 摘要里跟着阶段走的那一小段。只有「已暂停 / 正在停止」两种，
/// 不往里塞"还有几个在跑"的数字：那个数字在事件之后就没有下一个事件来更新它了。
function compressionStateSuffix() {
  if (!isCompressing) return '';
  if (compressionState === COMPRESSION_PAUSED) return localizeUiText(' · 已暂停');
  if (compressionState === COMPRESSION_STOPPING) return localizeUiText(' · 正在停止');
  return '';
}

/// 进度条填充宽度 = 已处理 / 总数。不靠 CSS 动画假装有进度：
/// 停止之后它停在真实比例上，继续时从这个比例接着长。
function renderQueueProgressFill() {
  var fill = document.getElementById('compressBtnFill');
  if (!fill) return;
  var p = getQueueProgress();
  fill.style.width = p.total === 0 ? '0%' : (p.processed / p.total * 100) + '%';
}

function updateBulkActionButtons() {
  var successes = queueResults().filter(function(r) { return r && r.success; });
  var restoreBtn = document.getElementById('restoreAllBtn');
  // 「恢复全部原图」只对**覆盖模式**落过备份的条目有意义，但服务层自己会跳过
  // 没有备份的那些；按钮这里只管"有没有可撤销的产物"。
  if (restoreBtn) restoreBtn.hidden = successes.length === 0;
  var exportBtn = document.getElementById('exportAllBtn');
  if (exportBtn) exportBtn.hidden = successes.length === 0;
}

// ─── 队列渲染（排序 / 视图 / 每一行的画法）──────────────────────

// CPU 上限的展示口径：后端负责算生效值，前端只渲染"上限 / 可用并行数"。
// 这里没有任何绑核语义 —— 数字只是"同时允许几份 CPU 并行压缩工作"。
var cpuStatus = null;

function cpuCeiling() {
  return Math.max(1, (cpuStatus && cpuStatus.availableParallelism) || 1);
}

function cpuLimitText() {
  if (!cpuStatus) return '';
  if (cpuStatus.configuredLimit === null || cpuStatus.configuredLimit === undefined) {
    return localizeUiText(' · CPU 自动');
  }
  var ceiling = cpuCeiling();
  var limit = Math.min(Math.max(1, cpuStatus.effectiveLimit || 1), ceiling);
  return ' · CPU ' + limit + '/' + ceiling;
}

function toggleQueueSortDirection() {
  queueSortDescending = !queueSortDescending;
  applyQueueView();
}

function applyQueueView() {
  var list = document.getElementById('fileQueueList');
  if (!list) return;
  var filter = document.getElementById('queueFailedOnly');
  var failedOnly = !!(filter && filter.checked);
  var sort = document.getElementById('queueSortKey');
  var key = sort ? sort.value : 'import';
  var direction = document.getElementById('queueSortDirection');
  var mediaView = typeof mediaFilter === 'string' ? mediaFilter : 'all';
  if (direction) {
    direction.innerHTML = iconMarkup(queueSortDescending ? 'sort-desc' : 'sort-asc', true) + (queueSortDescending ? localizeUiText(' 降序') : localizeUiText(' 升序'));
    direction.setAttribute('aria-label', queueSortDescending ? localizeUiText('当前降序，点击切换升序') : localizeUiText('当前升序，点击切换降序'));
  }
  // 排序键也全部从 queueItems 读：DOM class 只负责画，不参与任何判断。
  var states = ['failed', 'running', 'pending', 'done', 'restored', 'removed'];
  var entries = files.map(function(file, index) {
    var row = fileRows[file];
    var item = queueItems.get(file);
    var result = item && item.result;
    var value = index;
    if (key === 'name') value = basename(file);
    if (key === 'original') value = result ? result.originalSize : item && item.originalSize;
    if (key === 'compressed') value = result && result.success ? result.compressedSize : null;
    if (key === 'ratio') value = result && result.success ? result.savings : null;
    if (key === 'status') value = states.indexOf(item ? item.state : 'pending');
    return { file: file, row: row, index: index, value: value };
  });
  entries.sort(function(a, b) {
    // Unknown sizes and unfinished compression ratios stay last in either direction.
    var aMissing = a.value == null || (typeof a.value === 'number' && !Number.isFinite(a.value));
    var bMissing = b.value == null || (typeof b.value === 'number' && !Number.isFinite(b.value));
    if (aMissing !== bMissing) return aMissing ? 1 : -1;
    var compared = aMissing ? 0 : typeof a.value === 'string'
      ? a.value.localeCompare(b.value, 'zh-CN', { numeric: true, sensitivity: 'base' })
      : a.value - b.value;
    return (queueSortDescending ? -compared : compared) || a.index - b.index;
  });
  var visible = 0;
  Object.keys(fileRows).forEach(function(file) {
    // 「只看失败」也按 queueItems 判，不按 DOM class：类名只是画出来的结果，
    // 拿它当判据，一旦某处忘了同步 class，筛出来的东西就是错的。
    var failedRow = queueState(file) === 'failed';
    var item = queueItems.get(file);
    var mediaVisible = mediaView === 'all' || !item || !item.mediaKind || item.mediaKind === mediaView;
    fileRows[file].hidden = !files.includes(file) || !mediaVisible || (failedOnly && !failedRow);
  });
  entries.forEach(function(entry, index) {
    if (!entry.row) return;
    if (!entry.row.hidden) visible++;
    if (list.children[index] !== entry.row) list.insertBefore(entry.row, list.children[index] || null);
  });
  var empty = document.getElementById('queueFilterEmpty');
  if (empty) empty.hidden = !failedOnly || visible > 0;
}

async function renderFileQueue() {
  var list = document.getElementById('fileQueueList');
  if (!list) return;
  var wanted = new Set(files);

  Object.keys(fileRows).forEach(function(filePath) {
    if (!wanted.has(filePath)) {
      var staleRow = fileRows[filePath];
      if (staleRow) staleRow.remove();
      delete fileRows[filePath];
    }
  });

  var newFiles = [];
  for (var i = 0; i < files.length; i++) {
    if (!fileRows[files[i]]) {
      var row = createQueueRow(files[i]);
      fileRows[files[i]] = row;
      newFiles.push(files[i]);
    }
    // appendChild also moves an existing row, preserving the queue order.
    list.appendChild(fileRows[files[i]]);
  }
  applyQueueView();
  // Fetch file sizes for newly added rows only (preserve existing row state)
  if (newFiles.length > 0) {
    try {
      const sizes = await invoke('get_file_sizes', { filePaths: newFiles });
      for (var j = 0; j < newFiles.length; j++) {
        var item = queueItems.get(newFiles[j]);
        var row = fileRows[newFiles[j]];
        if (item && sizes[j] !== undefined) item.originalSize = sizes[j];
        if (row && sizes[j] !== undefined && (!item || item.state === 'pending')) {
          var sizeEl = row.querySelector('.media-size');
          if (sizeEl) sizeEl.textContent = formatBytes(sizes[j]);
        }
      }
    } catch (e) { /* ignore */ }
    applyQueueView();
  }
}

/// 把一个队列项此刻的状态**画**到它那一行上：缩略图、来源、结论、尺寸、能做的动作。
/// 唯一的方向是 queueItems → DOM；DOM class 不许反过来影响任何判断。
///
/// 五列各有各的分工，不许互相顶：
///   缩略图 = 素材本身；来源行 = 它原来是什么；结论行 = 这一轮对它做了什么；
///   尺寸 = 现在多大；最右 = 状态图标 + 此刻真能点的动作。
function paintQueueRow(filePath) {
  var row = fileRows[filePath];
  var item = queueItems.get(filePath);
  if (!row || !item) return;
  var state = item.state;
  ['pending', 'running', 'done', 'failed', 'restored', 'removed'].forEach(function(name) {
    row.classList.toggle(name, name === state);
  });
  row.classList.toggle('selected', selectedQueueFile === filePath);

  var actionsEl = row.querySelector('.media-actions');
  var metaEl = row.querySelector('.media-meta');
  var sizeEl = row.querySelector('.media-size');
  var subEl = row.querySelector('.media-sub');
  var removeBtn = row.querySelector('.media-remove');
  if (removeBtn) removeBtn.hidden = state !== 'pending';
  if (actionsEl) actionsEl.innerHTML = '';

  if (item.mediaKind === 'video') {
    paintVideoQueueRow(filePath, row, item);
    return;
  }

  var result = item.result;
  if (subEl) subEl.textContent = imageSourceLine(item, result);
  if (subEl) subEl.title = filePath;

  if (state === 'done' && result) {
    setStateIcon(row, result.error ? 'warning' : 'check', result.error ? 'failed' : 'done');
    if (sizeEl) sizeEl.textContent = formatBytes(result.compressedSize);
    if (sizeEl) sizeEl.title = formatBytes(result.originalSize) + ' → ' + formatBytes(result.compressedSize);
    if (metaEl) {
      metaEl.textContent = imageOutcomeLine(result);
      metaEl.title = result.error || '';
    }
    renderQueueResultActions(row, result);
    return;
  }
  if (state === 'failed' && result) {
    setStateIcon(row, 'error', 'failed');
    if (sizeEl) sizeEl.textContent = item.originalSize ? formatBytes(item.originalSize) : '';
    if (metaEl) metaEl.textContent = localizeUiText('失败');
    renderQueueResultActions(row, result);
    return;
  }
  if (state === 'restored') {
    setStateIcon(row, 'restore', 'restored');
    if (sizeEl) sizeEl.textContent = item.originalSize ? formatBytes(item.originalSize) : '';
    if (metaEl) metaEl.textContent = localizeUiText('已恢复');
    renderRestoredActions(row, filePath);
    return;
  }
  if (state === 'removed') {
    setStateIcon(row, 'minus', 'removed');
    if (metaEl) metaEl.textContent = localizeUiText('已移除');
    return;
  }

  // pending / running：这一列只写**这个文件此刻的处境**（等待中 / 已暂停 / 收尾中…）。
  // 本轮要做什么（意图摘要）是批次级别的事实，写在运行条和详情列里，不重复 40 遍。
  renderTaskStatus(row, state === 'pending' ? waitingRowStatus() : state);
  if (sizeEl) sizeEl.textContent = item.originalSize ? formatBytes(item.originalSize) : '';
}

function setStateIcon(row, iconName, tone) {
  var el = row.querySelector('.media-state-icon');
  if (!el) return;
  el.className = 'media-state-icon' + (tone ? ' ' + tone : '');
  el.innerHTML = iconName === 'spinner' ? progressSpinnerMarkup()
    : iconName ? iconMarkup(iconName, true) : '';
}

/// 行内那条 2px 的进度：只在真的转的时候存在。图片没有单文件进度事件，
/// 所以图片行永远没有这条 —— 宁可没有，也不画一条不知道由谁驱动的条。
function setRowProgressBar(row, fraction) {
  var bar = row.querySelector('.progress-file-bar');
  if (!bar) return;
  var known = Number.isFinite(fraction);
  bar.hidden = !known;
  if (known) bar.firstChild.style.width = (Math.max(0, Math.min(1, fraction)) * 100) + '%';
}

function imageTypeLabel(filePath) {
  var ext = extname(filePath).replace('.', '').toUpperCase();
  return ext === 'JPEG' ? 'JPG' : ext;
}

/// 来源行只写**已经知道的事实**：尺寸未知就只写格式，不猜、不留空占位符。
function imageSourceLine(item, result) {
  var width = (result && result.sourceWidth) || (item && item.width);
  var height = (result && result.sourceHeight) || (item && item.height);
  var dims = width && height ? width + ' × ' + height : '';
  var type = (result && result.type) || imageTypeLabel(item && item.path);
  return [dims, type].filter(Boolean).join(' · ');
}

/// 结论行：算法 + 这次真正的降幅；指定体积时把"达标没有"写在最前面，
/// 因为那是这一轮用户对它的唯一期待（AGENTS.md：达不到的承诺不能报成成功）。
function imageOutcomeLine(result) {
  var parts = [];
  if (result.targetBytes) {
    var want = formatBytes(result.targetBytes);
    parts.push(result.targetMet === true
      ? localizeUiText('达标 ≤ ') + want
      : result.targetMet === false
        ? localizeUiText('未达 ') + want
        : localizeUiText('目标 ') + want);
  }
  if (Number.isFinite(result.savings)) {
    parts.push((result.savings >= 0 ? '-' : '+') + Math.abs(result.savings).toFixed(1) + '%');
  }
  if (result.algorithm) parts.push(result.algorithm);
  return parts.join(' · ');
}

/// 失败行上那个小三角：鼠标悬停有 title，点一下把完整错误说出来。
/// （此前这里调的 showErrorDetail 根本没定义过，点一下就是抛异常。）
function showErrorDetail(filePath, message) {
  showToast(basename(filePath) + ': ' + message);
}

function appendErrorIcon(actionsEl, result) {
  if (!actionsEl || !result.error) return;
  var errIcon = document.createElement('button');
  errIcon.type = 'button';
  errIcon.className = 'error-info-btn';
  errIcon.title = result.error;
  errIcon.setAttribute('aria-label', localizeUiText('查看失败原因'));
  errIcon.innerHTML = iconMarkup('warning', true);
  errIcon.onclick = function(e) { e.stopPropagation(); showErrorDetail(result.file, result.error); };
  actionsEl.appendChild(errIcon);
}

function createQueueRow(filePath) {
  var row = document.createElement('div');
  var item = queueItems.get(filePath);
  var isVideo = !!(item && item.mediaKind === 'video');
  row.className = 'media-item' + (isVideo ? ' media-kind-video' : '');
  row.dataset.mediaKind = isVideo ? 'video' : 'image';
  row.dataset.file = filePath;
  row.innerHTML =
    '<span class="media-thumb">' + iconMarkup(isVideo ? 'video' : 'image', true) + '</span>' +
    '<span class="media-title">' +
      '<span class="media-name"></span>' +
      '<span class="media-sub"></span>' +
    '</span>' +
    '<span class="media-meta"></span>' +
    '<span class="media-size"></span>' +
    '<span class="media-status">' +
      '<span class="media-state-icon"></span>' +
      '<span class="media-actions"></span>' +
      '<button class="media-remove" title="' + localizeUiText('移出队列') + '" aria-label="' + localizeUiText('移出队列') + '">' + iconMarkup('close', true) + '</button>' +
    '</span>' +
    '<span class="progress-file-bar"' + (isVideo ? '' : ' hidden') + '><i></i></span>';
  var nameEl = row.querySelector('.media-name');
  if (nameEl) nameEl.textContent = basename(filePath);
  var removeBtn = row.querySelector('.media-remove');
  removeBtn.addEventListener('click', function(e) {
    e.stopPropagation();
    removeQueueFile(filePath);
  });
  // 点一行就是选中它：详情列画的正是这一项，行本身不藏着任何二级展开。
  row.addEventListener('click', function() { selectQueueFile(filePath); });
  paintQueueRow(filePath);
  queueRowThumb(filePath);
  return row;
}

/// 缩略图取的是**原图**，走 convertFileSrc；取不到就保留类型图标，不占位空白。
function queueRowThumb(filePath) {
  var item = queueItems.get(filePath);
  if (!item || item.mediaKind === 'video') return;
  var row = fileRows[filePath];
  if (!row || item.thumbRequested) return;
  item.thumbRequested = true;
  var img = new Image();
  img.alt = '';
  img.draggable = false;
  img.onerror = function() { /* 缩略图画不出来不是错误：类型图标还在。 */ };
  img.onload = function() {
    var thumb = row.querySelector('.media-thumb');
    if (!thumb) return;
    thumb.innerHTML = '';
    thumb.appendChild(img);
  };
  setImageSource(img, imageFileSrc(filePath), 4000);
}

function videoAudioSummary(probe) {
  var streams = probe && Array.isArray(probe.streams) ? probe.streams : [];
  var audio = streams.filter(function(stream) { return stream && (stream.codecType === 'audio' || stream.codec_type === 'audio'); });
  if (!audio.length) return localizeUiText('无音轨');
  var names = audio.map(function(stream) { return stream.codecName || stream.codec_name || 'audio'; });
  return audio.length + localizeUiText(' 条音轨') + ' · ' + Array.from(new Set(names)).join('/');
}

function videoWarningText(item) {
  var warnings = (item.probeWarnings || []).concat(item.probeWarning ? [item.probeWarning] : []);
  var hdr = item.videoProbe && item.videoProbe.hdrKind;
  if (hdr && String(hdr).toLowerCase() !== 'sdr' && String(hdr).toLowerCase() !== 'none') {
    warnings.unshift(localizeUiText('检测到 ') + hdr + localizeUiText('；须确认 HDR 色彩和动态元数据保真'));
  }
  if (!videoCapabilities.available && videoCapabilities.reason) warnings.unshift(videoCapabilities.reason);
  return Array.from(new Set(warnings.filter(Boolean))).join(' · ');
}

function videoProgressText(item) {
  var statusText = videoStatusText(item);
  var numbers = videoProgressNumbers(item);
  return [statusText, numbers].filter(Boolean).join(' · ');
}

/// 进度数字只报**正在发生**的那几项：百分比、倍速、帧率、剩余时间。
/// 没有实时进度时给的是体积估算，并且把"估算"两个字写在里面 —— 它不是承诺。
function videoProgressNumbers(item) {
  var progress = item.videoProgress;
  if (!progress) {
    var estimate = window.SizqoVideo.estimateOutputBytes(item.videoProbe, currentVideoOptions);
    if (estimate) return localizeUiText('预计输出约 ') + formatBytes(estimate.bytes) + localizeUiText('（估算）');
    return '';
  }
  if (item.state !== 'running') return '';
  var parts = [];
  if (Number.isFinite(progress.fraction)) parts.push(Math.round(Math.max(0, Math.min(1, progress.fraction)) * 100) + '%');
  if (progress.speed) parts.push(String(progress.speed));
  if (Number.isFinite(progress.fps)) parts.push(progress.fps.toFixed(1) + ' fps');
  if (Number.isFinite(progress.etaSeconds) && progress.etaSeconds >= 0) {
    parts.push(localizeUiText('剩余 ') + window.SizqoVideo.formatDurationUs(progress.etaSeconds * 1000000));
  }
  return parts.join(' · ');
}

/// 状态词。暂停 / 收尾中必须能在文字里读到，所以 running 与 pending 走的是
/// 与图片行**同一张**状态表（taskStatusMarkup）。
function videoStatusText(item) {
  if (item.state === 'running') return taskStatusMarkup('running').text;
  if (item.state === 'pending') return taskStatusMarkup(waitingRowStatus()).text;
  var result = item.result || {};
  if (item.state === 'done' && result.skippedLarger) return localizeUiText('体积更大，已跳过');
  if (item.state === 'done' && result.success) {
    return localizeUiText('已压缩 ') + (Number.isFinite(result.savings)
      ? (result.savings >= 0 ? '-' : '+') + Math.abs(result.savings).toFixed(1) + '%' : '');
  }
  if (item.state === 'done') return localizeUiText('已完成');
  if (item.state === 'failed') return localizeUiText('失败');
  if (item.state === 'restored') return localizeUiText('已恢复');
  if (item.state === 'removed') return localizeUiText('已移除');
  return '';
}

function paintVideoQueueRow(filePath, row, item) {
  var thumbEl = row.querySelector('.media-thumb');
  var subEl = row.querySelector('.media-sub');
  var metaEl = row.querySelector('.media-meta');
  var sizeEl = row.querySelector('.media-size');
  var actions = row.querySelector('.media-actions');
  var result = item.result;

  if (thumbEl && item.coverUrl) {
    var existing = thumbEl.querySelector('img');
    if (!existing) {
      thumbEl.innerHTML = '<img alt="" draggable="false">';
      thumbEl.firstChild.src = item.coverUrl;
    } else if (existing.src !== item.coverUrl) {
      existing.src = item.coverUrl;
    }
  }

  var summary = window.SizqoVideo.videoSummary(item.videoProbe || {});
  var hdr = item.videoProbe && item.videoProbe.hdrKind;
  var color = hdr && String(hdr).toLowerCase() !== 'none' ? hdr : localizeUiText('SDR');
  var details = [summary.duration, summary.dimensions, summary.frameRate, color, summary.codec, videoAudioSummary(item.videoProbe)];
  var warning = videoWarningText(item);
  if (subEl) {
    subEl.textContent = details.filter(Boolean).join(' · ');
    // 轨道/HDR 这些细节一屏放不下，但**不许藏**：悬停看全，检测到的限制同时
    // 会出现在详情列里（video-capability-note），并在真正会出问题时被后端拒绝。
    subEl.title = details.filter(Boolean).join('\n') + (warning ? '\n' + warning : '');
  }
  if (metaEl) {
    metaEl.textContent = videoProgressText(item);
    metaEl.classList.toggle('media-warning', !!warning && item.state === 'pending');
  }
  if (sizeEl) {
    sizeEl.textContent = result && result.success
      ? formatBytes(result.outputBytes)
      : (item.originalSize ? formatBytes(item.originalSize) : '');
    sizeEl.title = result && result.success
      ? formatBytes(result.originalBytes) + ' → ' + formatBytes(result.outputBytes) : '';
  }
  setStateIcon(row,
    item.state === 'running' ? 'spinner' : result && result.skippedLarger ? 'warning'
      : item.state === 'done' ? 'check' : item.state === 'failed' ? 'error'
      : item.state === 'restored' ? 'restore' : item.state === 'removed' ? 'minus' : 'queue',
    item.state === 'running' ? 'running' : result && result.skippedLarger ? 'warning' : item.state);
  setRowProgressBar(row, item.state === 'running' ? item.videoProgress && item.videoProgress.fraction : null);

  if (!actions) return;
  actions.innerHTML = '';
  if (item.state === 'running') {
    var cancelButton = appendVideoAction(actions, 'cancel', item.cancelRequested ? localizeUiText('正在取消…') : localizeUiText('取消当前视频'), 'stop', function() { cancelCurrentVideo(item.taskId); });
    cancelButton.disabled = !!item.cancelRequested;
  } else if (item.state === 'done' && item.result && item.result.success) {
    var original = result.outputMode === 'replace' ? result.backupPath : result.sourcePath;
    if (original) appendVideoAction(actions, 'play-original', localizeUiText('播放原视频'), 'play', function() { openMediaFile(original); });
    if (result.outputPath) appendVideoAction(actions, 'play-output', localizeUiText('播放输出视频'), 'play', function() { openMediaFile(result.outputPath); });
    if (original && result.outputPath) appendVideoAction(actions, 'compare', localizeUiText('同帧对比'), 'compare', function() { openVideoCompareForResult(result); });
    appendVideoAction(actions, 'finder', localizeUiText('在访达中显示'), 'finder', function() { openInFinder(filePath); });
    appendVideoAction(actions, 'log', localizeUiText('查看日志'), 'copy', function() { copyVideoLog(result); });
    var canUndo = result.outputMode === 'replace' ? !!result.backupPath : !!result.outputPath;
    if (result.historyId && canUndo) appendVideoAction(actions, 'restore',
      localizeUiText(result.outputMode === 'replace' ? '恢复原视频' : '删除这次压缩结果'),
      result.outputMode === 'replace' ? 'restore' : 'trash', function() { restoreVideoResult(result); });
  } else if (item.state === 'failed') {
    appendVideoAction(actions, 'retry', localizeUiText('重试'), 'recompress', function() { compressOneFile(filePath); });
    if (item.result) appendVideoAction(actions, 'log', localizeUiText('查看日志'), 'copy', function() { copyVideoLog(item.result); });
  } else if (item.state === 'done' && item.result && item.result.skippedLarger) {
    appendVideoAction(actions, 'log', localizeUiText('查看日志'), 'copy', function() { copyVideoLog(item.result); });
  }
}

function appendVideoAction(actions, action, title, icon, callback) {
  var button = document.createElement('button');
  button.type = 'button';
  button.className = 'queue-action-btn' + (action === 'cancel' ? ' danger' : '');
  button.title = title;
  button.setAttribute('aria-label', title);
  button.innerHTML = iconMarkup(icon, true);
  button.addEventListener('click', function(event) { event.stopPropagation(); callback(); });
  actions.appendChild(button);
  return button;
}

function queueVideoCover(item) {
  if (!item || item.mediaKind !== 'video' || item.coverRequested) return;
  item.coverRequested = true;
  videoCoverQueue.push(item);
  if (!videoCoverBusy) drainVideoCoverQueue();
}

async function drainVideoCoverQueue() {
  videoCoverBusy = true;
  while (videoCoverQueue.length) {
    var item = videoCoverQueue.shift();
    if (!item || queueItems.get(item.path) !== item) continue;
    try {
      var frame = await invoke('preview_video_frame', { path: item.path, timeUs: 0 });
      var dataUrl = typeof frame === 'string' ? frame : frame && (frame.dataUrl || frame.frame);
      if (typeof dataUrl === 'string' && dataUrl.startsWith('data:image/')) {
        item.coverUrl = dataUrl;
        paintQueueRow(item.path);
      }
    } catch (_) { /* A missing thumbnail never blocks importing or encoding. */ }
  }
  videoCoverBusy = false;
}

async function openMediaFile(filePath) {
  if (!filePath) return;
  try { await invoke('open_media_file', { path: filePath }); }
  catch (error) { showToast(localizeUiText('无法打开媒体文件: ') + (error.message || error)); }
}

async function cancelCurrentVideo(taskId) {
  if (!taskId || !executionSession) return;
  var target = null;
  queueItems.forEach(function(item) { if (item && item.taskId === taskId) target = item; });
  if (!target || target.state !== 'running' || target.cancelRequested) return;
  target.cancelRequested = true;
  paintQueueRow(target.path);
  try {
    await invoke('cancel_video_task', { taskId: taskId, sessionId: executionSession.id });
  } catch (error) {
    target.cancelRequested = false;
    paintQueueRow(target.path);
    showToast(localizeUiText('取消视频失败: ') + (error.message || error));
  }
}

async function restoreVideoResult(result, force) {
  if (!result || !result.historyId) return;
  if (result.outputMode !== 'replace' && !confirm(localizeUiText('删除这次压缩结果？\n将删除 ') + (result.outputPath || '')
    + localizeUiText('，并移除这条历史记录。原图未被覆盖，不受影响。'))) return;
  var outcome;
  try { outcome = await invoke('restore_history_entry', { historyId: result.historyId, force: !!force }); }
  catch (error) { showToast(localizeUiText('恢复失败: ') + (error.message || error)); return; }
  if (outcome.conflict) {
    if (!confirm(RESTORE_CONFLICT_TEXT)) return;
    await restoreVideoResult(result, true);
    return;
  }
  if (!outcome.success) { showToast(outcome.error || localizeUiText('恢复失败')); return; }
  showToast(result.outputMode === 'replace'
    ? localizeUiText('已恢复原视频: ') + basename(result.sourcePath)
    : localizeUiText('已删除这次压缩结果: ') + basename(result.outputPath));
  afterRestore(outcome.filePath || result.sourcePath);
}

function copyVideoLog(result) {
  if (!result) return;
  var options = result.videoOptions || result.compressOptions || {};
  var lines = [
    '=== Sizqo video task ===',
    'source: ' + (result.sourcePath || ''),
    'output: ' + (result.outputPath || ''),
    'status: ' + (result.skippedLarger ? 'skippedLarger' : result.success ? 'success' : 'failed'),
    'reason: ' + (result.reason || ''),
    'size: ' + (result.originalBytes || 0) + ' -> ' + (result.outputBytes || 0) + ' bytes',
    'savings: ' + (Number.isFinite(result.savings) ? result.savings.toFixed(2) + '%' : 'unknown'),
    'elapsedMs: ' + (result.elapsedMs == null ? 'unknown' : result.elapsedMs),
    'encoderActual: ' + (result.encoderActual || 'unknown'),
    'backendActual: ' + (result.backendActual || 'unknown'),
    'options: ' + JSON.stringify(options),
  ];
  (result.warnings || []).forEach(function(warning) { lines.push('warning: ' + warning); });
  if (result.diagnosticsLogPath) lines.push('diagnosticsLogPath: ' + result.diagnosticsLogPath);
  var ok = copyTextToClipboard(lines.join('\n'));
  showToast(ok ? localizeUiText('视频日志已复制到剪贴板') : localizeUiText('复制失败，请手动选中日志文本'));
}

function openVideoCompareForResult(result) {
  if (!result) return;
  var originalPath = result.outputMode === 'replace' ? result.backupPath : result.sourcePath;
  if (!originalPath || !result.outputPath) {
    showToast(localizeUiText('原视频或压缩结果已不存在'));
    return;
  }
  openVideoCompare(originalPath, result.outputPath, result.videoProbe || null);
}

function findProbedVideo(records, path) {
  var record = (records || []).find(function(item) { return item && item.path === path && item.mediaKind === 'video'; });
  return record && record.videoProbe || null;
}

async function openVideoCompare(sourcePath, outputPath, sourceProbeHint) {
  if (!sourcePath || !outputPath) return;
  var backdrop = document.getElementById('videoCompareBackdrop');
  var dialog = document.getElementById('videoCompareDialog');
  var message = document.getElementById('videoCompareMessage');
  if (!backdrop || !dialog) return;
  var state = {
    sourcePath: sourcePath, outputPath: outputPath, sourceProbe: sourceProbeHint || null,
    outputProbe: null, durationUs: 0, timeUs: 0, split: 50, zoom: 1, panX: 0, panY: 0,
    requestId: 0, timer: null, drag: null,
  };
  videoCompareState = state;
  backdrop.style.display = 'block';
  dialog.style.display = 'flex';
  document.body.style.overflow = 'hidden';
  if (message) { message.hidden = false; message.textContent = localizeUiText('正在读取视频信息…'); }
  var sourceLabel = document.getElementById('videoCompareSourceLabel');
  var outputLabel = document.getElementById('videoCompareOutputLabel');
  if (sourceLabel) sourceLabel.textContent = localizeUiText('原视频: ') + basename(sourcePath);
  if (outputLabel) outputLabel.textContent = localizeUiText('输出视频: ') + basename(outputPath);
  try {
    var records = await invoke('probe_media', { paths: [sourcePath, outputPath] });
    if (videoCompareState !== state) return;
    state.sourceProbe = findProbedVideo(records, sourcePath) || state.sourceProbe;
    state.outputProbe = findProbedVideo(records, outputPath);
    var sourceDuration = Number(state.sourceProbe && state.sourceProbe.durationUs) || 0;
    var outputDuration = Number(state.outputProbe && state.outputProbe.durationUs) || 0;
    state.durationUs = Math.min(sourceDuration, outputDuration);
    if (!state.sourceProbe || !state.outputProbe || state.durationUs <= 0) throw new Error(localizeUiText('无法读取两段视频的有效时长'));
    var timeline = document.getElementById('videoCompareTime');
    if (timeline) { timeline.min = '0'; timeline.max = String(state.durationUs); timeline.value = '0'; }
    renderVideoCompareMetadata(state);
    setVideoCompareSplit(50);
    setVideoCompareZoom(1);
    await requestVideoCompareFrames(state);
  } catch (error) {
    if (videoCompareState === state && message) {
      message.hidden = false;
      message.textContent = (error && error.message) || localizeUiText('无法生成同帧预览');
    }
  }
}

function renderVideoCompareMetadata(state) {
  var target = document.getElementById('videoCompareMetadata');
  if (!target) return;
  var source = state.sourceProbe || {};
  var output = state.outputProbe || {};
  var sourceSummary = window.SizqoVideo.videoSummary(source);
  var outputSummary = window.SizqoVideo.videoSummary(output);
  var sourceSize = Number(source.sizeBytes) || 0;
  var outputSize = Number(output.sizeBytes) || 0;
  target.textContent = sourceSummary.dimensions + ' · ' + formatBytes(sourceSize) + ' · ' + (sourceSummary.codec || '')
    + '  →  ' + outputSummary.dimensions + ' · ' + formatBytes(outputSize) + ' · ' + (outputSummary.codec || '');
}

function setVideoCompareSplit(percent) {
  if (!videoCompareState) return;
  videoCompareState.split = Math.max(0, Math.min(100, Number(percent) || 0));
  var stage = document.getElementById('videoCompareStage');
  if (stage) stage.style.setProperty('--video-compare-split', videoCompareState.split + '%');
}

function setVideoCompareZoom(value) {
  if (!videoCompareState) return;
  videoCompareState.zoom = Math.max(.5, Math.min(2.5, Number(value) || 1));
  var stage = document.getElementById('videoCompareStage');
  if (stage) stage.style.setProperty('--video-compare-zoom', String(videoCompareState.zoom));
  var label = document.getElementById('videoCompareZoomLabel');
  if (label) label.textContent = Math.round(videoCompareState.zoom * 100) + '%';
}

function requestVideoCompareFrames(state) {
  if (!state || videoCompareState !== state) return Promise.resolve();
  state.requestId += 1;
  var requestId = state.requestId;
  var message = document.getElementById('videoCompareMessage');
  if (message) { message.hidden = false; message.textContent = localizeUiText('正在解码所选时间点…'); }
  var label = document.getElementById('videoCompareTimeLabel');
  if (label) label.textContent = window.SizqoVideo.formatDurationUs(state.timeUs);
  return invoke('compare_video_frames', {
    sourcePath: state.sourcePath, outputPath: state.outputPath, timeUs: state.timeUs,
  }).then(function(frames) {
    if (videoCompareState !== state || state.requestId !== requestId) return;
    var sourceFrame = frames && frames.sourceFrame;
    var outputFrame = frames && frames.outputFrame;
    if (typeof sourceFrame !== 'string' || !sourceFrame.startsWith('data:image/')) throw new Error(localizeUiText('原视频帧解码失败'));
    if (typeof outputFrame !== 'string' || !outputFrame.startsWith('data:image/')) throw new Error(localizeUiText('输出视频帧解码失败'));
    var sourceImage = document.getElementById('videoCompareSourceFrame');
    var outputImage = document.getElementById('videoCompareOutputFrame');
    if (sourceImage) sourceImage.src = sourceFrame;
    if (outputImage) outputImage.src = outputFrame;
    if (message) message.hidden = true;
  }).catch(function(error) {
    if (videoCompareState !== state || state.requestId !== requestId || !message) return;
    message.hidden = false;
    message.textContent = (error && error.message) || localizeUiText('无法生成同帧预览');
  });
}

function scheduleVideoCompareFrames() {
  var state = videoCompareState;
  if (!state) return;
  if (state.timer) clearTimeout(state.timer);
  state.timer = setTimeout(function() { state.timer = null; requestVideoCompareFrames(state); }, 180);
}

function closeVideoCompare() {
  if (videoCompareState && videoCompareState.timer) clearTimeout(videoCompareState.timer);
  videoCompareState = null;
  var backdrop = document.getElementById('videoCompareBackdrop');
  var dialog = document.getElementById('videoCompareDialog');
  if (backdrop) backdrop.style.display = 'none';
  if (dialog) dialog.style.display = 'none';
  document.body.style.overflow = '';
}

function bindVideoCompareControls() {
  var timeline = document.getElementById('videoCompareTime');
  if (timeline) timeline.addEventListener('input', function() {
    if (!videoCompareState) return;
    videoCompareState.timeUs = Math.max(0, Math.min(videoCompareState.durationUs, Number(timeline.value) || 0));
    var label = document.getElementById('videoCompareTimeLabel');
    if (label) label.textContent = window.SizqoVideo.formatDurationUs(videoCompareState.timeUs);
    scheduleVideoCompareFrames();
  });
  var zoom = document.getElementById('videoCompareZoom');
  if (zoom) zoom.addEventListener('input', function() { setVideoCompareZoom(Number(zoom.value) / 100); });
  var stage = document.getElementById('videoCompareStage');
  if (stage) {
    stage.addEventListener('pointerdown', function(event) {
      if (!videoCompareState) return;
      var rect = stage.getBoundingClientRect();
      if (event.target.closest && event.target.closest('#videoCompareDivider')) {
        videoCompareState.drag = { type: 'split', rect: rect };
      } else {
        videoCompareState.drag = { type: 'pan', x: event.clientX, y: event.clientY, panX: videoCompareState.panX, panY: videoCompareState.panY };
      }
      stage.setPointerCapture(event.pointerId);
    });
    stage.addEventListener('pointermove', function(event) {
      var state = videoCompareState;
      if (!state || !state.drag) return;
      if (state.drag.type === 'split') {
        setVideoCompareSplit((event.clientX - state.drag.rect.left) / state.drag.rect.width * 100);
      } else {
        state.panX = state.drag.panX + event.clientX - state.drag.x;
        state.panY = state.drag.panY + event.clientY - state.drag.y;
        stage.style.setProperty('--video-compare-pan-x', state.panX + 'px');
        stage.style.setProperty('--video-compare-pan-y', state.panY + 'px');
      }
    });
    ['pointerup', 'pointercancel', 'lostpointercapture'].forEach(function(name) {
      stage.addEventListener(name, function() { if (videoCompareState) videoCompareState.drag = null; });
    });
  }
}

/// 把一个文件移出队列。**只有等待中（还没开始）的可以移**：已经在压的那一行
/// 没有 × 按钮，因为半路扔掉一个正在跑的编码器会留下写了一半的产物。
function removeQueueFile(filePath) {
  if (queueState(filePath) !== 'pending') return;
  if (isCompressing) {
    // 后端也要知道：它可能正堵在闸门上等着，得让那个 worker 自己退出。
    invoke('cancel_file', { filePath: filePath }).catch(function() {});
  }
  setQueueState(filePath, 'removed');
  var idx = files.indexOf(filePath);
  if (idx >= 0) files.splice(idx, 1);
  var row = fileRows[filePath];
  if (row) {
    if (row.parentNode) row.parentNode.removeChild(row);
    delete fileRows[filePath];
  }
  updateQueueSummary();
}

function renderQueueResultActions(row, result) {
  var actions = row.querySelector('.media-actions');
  if (!actions) return;
  actions.innerHTML = '';
  if (!result) return;

  var actionDefs = [];
  if (result.success) {
    // 后缀 / 目录模式的原图从没被覆盖过：那一行按下去删的是这次生成的产物，
    // 按钮就不能写着「恢复原图」。历史页同一套判据（historyRowActionDefs）。
    var undo = result.outputMode === 'replace'
      ? { action: 'restore', title: localizeUiText('恢复原图'), icon: iconMarkup('restore', true) }
      : { action: 'restore', title: localizeUiText('删除这次压缩结果'), icon: iconMarkup('trash', true), danger: true };
    actionDefs = [
      { action: 'save', title: localizeUiText('另存为'), icon: iconMarkup('save', true) },
      { action: 'compare', title: localizeUiText('对比查看'), icon: iconMarkup('compare', true) },
      undo,
      { action: 'finder', title: localizeUiText('在访达中显示'), icon: iconMarkup('finder', true) },
    ];
  } else {
    actionDefs = [
      { action: 'retry', title: localizeUiText('重试'), icon: iconMarkup('recompress', true) },
    ];
  }
  actionDefs.push({ action: 'log', title: localizeUiText('复制日志'), icon: iconMarkup('copy', true) });

  actionDefs.forEach(function(def) {
    var btn = document.createElement('button');
    btn.className = 'queue-action-btn' + (def.danger ? ' danger' : '');
    btn.type = 'button';
    btn.title = def.title;
    btn.innerHTML = def.icon;
    btn.addEventListener('click', function(e) {
      e.stopPropagation();
      if (def.action === 'save') saveResult(result.file);
      else if (def.action === 'compare') openCompareByFile(result.file);
      else if (def.action === 'restore') restoreOriginal(result.file);
      else if (def.action === 'finder') openInFinder(result.file);
      else if (def.action === 'retry') compressOneFile(result.file);
      else if (def.action === 'log') copyCompressLog(result);
    });
    actions.appendChild(btn);
  });
  // 失败的原因不靠 hover 才能发现：给一个真能点的按钮，点了就把错误说出来。
  appendErrorIcon(actions, result);
}

function copyCompressLog(result) {
  var opts = result.compressOptions || {};
  var lines = [];
  lines.push(localizeUiText('版本: ') + BUILD_VARIANT);
  lines.push(localizeUiText('=== Sizqo \u538b\u7f29\u65e5\u5fd7 ==='));
  lines.push('');
  lines.push(localizeUiText('\u6587\u4ef6: ') + (result.file || ''));
  lines.push(localizeUiText('\u72b6\u6001: ') + (result.success ? localizeUiText('\u6210\u529f') : localizeUiText('\u5931\u8d25')));
  lines.push(localizeUiText('意图: ') + intentLabelForRun());
  lines.push('');
  lines.push(localizeUiText('--- 这一轮的请求 ---'));
  lines.push('quality: ' + (opts.quality !== undefined ? opts.quality : localizeUiText('(未设置)')));
  lines.push('outputFormat: ' + (opts.outputFormat || localizeUiText('(未设置)')));
  lines.push('maxDimensionPx: ' + (opts.maxDimensionPx || localizeUiText('(不限)')));
  lines.push('maxOutputBytes: ' + (opts.maxOutputBytes || localizeUiText('(不限)')));
  lines.push('processingMode: ' + (opts.processingMode || localizeUiText('(未设置)')));
  lines.push('outputMode: ' + (opts.outputMode || localizeUiText('(未设置)')));
  lines.push('outputSuffix: ' + (opts.outputSuffix || localizeUiText('(未设置)')));
  lines.push('');
  lines.push(localizeUiText('--- \u538b\u7f29\u7ed3\u679c ---'));
  if (result.success) {
    lines.push(localizeUiText('\u539f\u59cb\u5927\u5c0f: ') + formatBytes(result.originalSize) + ' (' + result.originalSize + ' bytes)');
    lines.push(localizeUiText('\u538b\u7f29\u540e\u5927\u5c0f: ') + formatBytes(result.compressedSize) + ' (' + result.compressedSize + ' bytes)');
    lines.push(localizeUiText('\u538b\u7f29\u7387: ') + (result.savings >= 0 ? '-' : '+') + Math.abs(result.savings).toFixed(1) + '%');
    lines.push(localizeUiText('\u8f93\u51fa\u683c\u5f0f: ') + (result.type || localizeUiText('(\u672a\u77e5)')));
    lines.push(localizeUiText('\u7b97\u6cd5: ') + (result.algorithm || localizeUiText('(\u672a\u77e5)')));
  } else {
    lines.push(localizeUiText('\u538b\u7f29\u5931\u8d25'));
  }
  lines.push('');
  lines.push(localizeUiText('--- \u9519\u8bef\u4fe1\u606f ---'));
  lines.push(result.error ? result.error : localizeUiText('(\u65e0)'));
  var ok = copyTextToClipboard(lines.join('\n'));
  showToast(ok ? localizeUiText('\u538b\u7f29\u65e5\u5fd7\u5df2\u590d\u5236\u5230\u526a\u8d34\u677f') : localizeUiText('\u590d\u5236\u5931\u8d25\uff0c\u8bf7\u624b\u52a8\u9009\u4e2d\u65e5\u5fd7\u6587\u672c'));
}

/// \u590d\u5236\u6587\u672c\u5230\u526a\u8d34\u677f\u3002execCommand \u662f\u6c99\u76d2 WebKit \u91cc\u552f\u4e00\u7a33\u7684\u8def\u5f84\uff1a
/// navigator.clipboard \u8981\u7528\u6237\u624b\u52bf + \u6743\u9650\uff0cApp Store \u7248\u62ff\u4e0d\u5230\u3002\u8fd4\u56de\u662f\u5426\u6210\u529f\uff0c\u6587\u6848\u7531\u8c03\u7528\u65b9\u51b3\u5b9a\u3002
function copyTextToClipboard(text) {
  var ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.top = '0';
  ta.style.left = '0';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  var ok = false;
  try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
  document.body.removeChild(ta);
  return ok;
}

function renderRestoredActions(row, filePath) {
  var actions = row.querySelector('.media-actions');
  if (!actions) return;
  actions.innerHTML = '';

  var btn = document.createElement('button');
  btn.className = 'queue-action-btn';
  btn.type = 'button';
  btn.title = localizeUiText('重新压缩');
  btn.innerHTML = iconMarkup('recompress', true);
  btn.addEventListener('click', function(e) {
    e.stopPropagation();
    compressOneFile(filePath);
  });
  actions.appendChild(btn);
}

/// 图片这一轮真正发给引擎的东西 = 意图编译出的参数 + 输出侧的落盘方式。
/// 字段集合与 src-tauri/src/engine.rs 的 CompressOptions（camelCase）一一对应：
/// 界面上不再提供、引擎也不再读的 backend / effort / smartMode / convertToWebp
/// 已经从这条请求里消失了 —— smart 路由的真实载体是命令参数 use_smart。
function getCurrentCompressionConfig() {
  const compiled = imageRequestFromIntent();
  const options = compiled.options;
  const outputMode = currentOutputMode();
  if (outputMode === 'folder' && !outputDir) {
    return { error: localizeUiText('请先选择输出目录') };
  }
  return {
    useSmartIpc: compiled.useSmart && options.processingMode === 'advanced',
    options: {
      processingMode: options.processingMode,
      systemImageSize: options.systemImageSize || 'actual',
      preserveMetadata: options.processingMode === 'system' ? options.preserveMetadata !== false : false,
      quality: options.quality,
      outputFormat: options.outputFormat,
      maxDimensionPx: options.maxDimensionPx || null,
      maxOutputBytes: options.maxOutputBytes || null,
      outputMode,
      outputSuffix: getOutputSuffix(),
      outputDir: outputMode === 'folder' ? outputDir : null,
      sourceRoots: inputPaths.slice(),
    },
  };
}

/// 当前意图是不是系统转换。按钮文案、错误文案、输出目录权限都读这一个判据，
/// 不再有一个游离的 `processingMode` 变量（它以前既不随意图更新，也不落盘）。
function systemModeActive() {
  return imageRequestFromIntent().options.processingMode === 'system';
}

/// 开跑时拍下的这一轮请求；没拍过就用当前意图现编译。
function requestForRun(kind) {
  var snapshot = runIntentSnapshot ? runIntentSnapshot[kind] : null;
  return snapshot || (kind === 'video' ? videoRequestFromIntent() : imageRequestFromIntent());
}

/// 还没开始的行回答的是「接下来会对它做什么」：跑动中的这一批仍按开跑时冻结的请求算，
/// 空闲时按当前 chips 算 —— 改了意图，排队的行就该跟着改口。
function requestForNext(kind) {
  if (isCompressing && runIntentSnapshot && runIntentSnapshot[kind]) return runIntentSnapshot[kind];
  return kind === 'video' ? videoRequestFromIntent() : imageRequestFromIntent();
}

/// 这一轮（或刚跑完的那一轮）走的是不是系统转换：收尾文案要跟着实际跑的意图说，
/// 不能因为用户在最后一刻点了别的 chip 就改口。
function systemModeForRun() {
  var request = requestForRun('image');
  return !!(request.options && request.options.processingMode === 'system');
}

function intentLabelForRun() {
  return localizeUiText(requestForRun('image').summary || '');
}

function clearAllFiles() {
  if (files.length === 0 && !isCompressing) return;
  if (!confirm(localizeUiText('确定要清空全部 ') + files.length + localizeUiText(' 个文件吗？'))) return;

  // 正在跑的那次 invoke 不能提前作废：这里只把队列**标记**成不要了，
  // 真正把它收回来的仍然是发起它的那次调用（它会在 finally 里收尾）。
  var wasCompressing = isCompressing;
  queueRevision++;
  if (wasCompressing && executionSession) {
    // 一次调用取消整批：逐个 invoke 会让每个请求都顺手动一次暂停闸门，
    // 队列会在清空过程中被重新放行。
    invoke('cancel_batch', { filePaths: executionSession.targetPaths.slice() }).catch(function() {});
  }

  files = [];
  inputPaths = [];
  queueItems = new Map();
  fileRows = {};
  pendingAutoCompress = false;
  selectedQueueFile = null;
  runIntentSnapshot = null;
  var list = document.getElementById('fileQueueList');
  if (list) list.innerHTML = '';
  updateQueueSummary();
  renderDetailPane();
  emitCompareResultsChanged();
}

// ─── Compression ────────────────────────────────────────────────

// ─── 执行会话（一轮）─────────────────────────────────────────────
// 一次执行会话**不是队列**：队列是长期存在的（导入 100 张就在那儿），
// 会话是"用户按下开始 / 继续压缩"到"这一轮结束"之间那一段。
//
// 两者分开是这套行为稳定的前提：
//  - 队列（queueItems）决定"这个文件还需不需要处理"；
//  - 会话决定"当前这一轮在不在跑、跑的是哪些文件"；
//  - 每一轮的目标是开始时拍下的快照（targetPaths），中途导入不会塞进来；
//  - 界面的总进度属于**队列**，所以停止后重新开始一轮，进度绝不会归零。

var executionSession = null;
var sessionSeq = 0;

/// 开一轮：目标 = 调用方给的这批（只会是 pending），并记下当时的队列版本。
function beginExecutionSession(targetPaths) {
  sessionSeq += 1;
  executionSession = {
    id: 'session-' + sessionSeq + '-' + Date.now(),
    queueRevision: queueRevision,
    targetPaths: targetPaths.slice(),
    phase: COMPRESSION_RUNNING,
  };
  return executionSession;
}

function endExecutionSession() {
  executionSession = null;
}

/// 这条事件属不属于当前这一轮。带上会话号之后，"上一轮迟到的收尾事件"
/// 再也污染不了下一轮 —— 光靠 queueRevision 挡不住"同一队列里的两轮"。
function eventBelongsToCurrentSession(data) {
  return !!(executionSession && data && data.sessionId === executionSession.id);
}

// ─── 压缩主流程 ─────────────────────────────────────────────────

function handlePrimaryAction() {
  if (compressionState === COMPRESSION_PAUSED) return toggleCompressionPause();
  if (compressionState !== COMPRESSION_IDLE) return;
  return startCompression(false);
}

async function startCompression(isIncrement) {
  return startCompressionForPaths(isIncrement, null);
}

async function startCompressionForPaths(isIncrement, requestedPaths) {
  if (isCompressing) return;
  // A media view starts its visible type only; All snapshots every runnable item.
  var pendingPaths = getPendingQueuePaths(requestedPaths);
  var batchPaths = getRunnablePendingPaths(requestedPaths);
  var view = typeof mediaFilter === 'string' ? mediaFilter : 'all';
  var blockedVideos = pendingPaths.filter(function(path) {
    var item = queueItems.get(path);
    return item && item.mediaKind === 'video' && (view === 'all' || view === 'video') && !videoCapabilities.available;
  });
  if (batchPaths.length === 0) {
    if (blockedVideos.length) showToast(videoCapabilities.reason || localizeUiText('此版本当前不支持视频编码'));
    return;
  }
  if (blockedVideos.length) showToast(videoCapabilities.reason || localizeUiText('视频编码不可用；本轮仅处理可运行的图片任务'));
  var imagePaths = batchPaths.filter(function(path) { var item = queueItems.get(path); return !item || item.mediaKind !== 'video'; });
  var videoPaths = batchPaths.filter(function(path) { var item = queueItems.get(path); return item && item.mediaKind === 'video'; });

  // 先占住批次位（静默）：真正开跑之前还有几步可能提前 return，
  // 那几步里不该亮出"暂停/停止"按钮和「压缩中…」。
  setCompressionState(COMPRESSION_RUNNING, true);
  var runRevision = queueRevision;

  var hasOutputAccess = true;
  if (imagePaths.length > 0 && systemModeActive()) {
    try {
      hasOutputAccess = await ensureSystemOutputAccess();
    } catch (error) {
      console.error('Output access check failed:', error);
      showToast(localizeUiText('无法确认输出目录，请重试'));
    }
  }
  if (!hasOutputAccess) {
    setCompressionState(COMPRESSION_IDLE, true);
    updateQueueSummary();
    return;
  }
  if (runRevision !== queueRevision) {
    setCompressionState(COMPRESSION_IDLE, true);
    updateQueueSummary();
    return;
  }

  setCompressionState(COMPRESSION_RUNNING, true);
  currentCompressOptions = null;

  const config = imagePaths.length > 0 ? getCurrentCompressionConfig() : { options: {}, useSmartIpc: false };
  if (config.error) {
    showToast(config.error);
    setCompressionState(COMPRESSION_IDLE, true);
    updateQueueSummary();
    return;
  }
  // 队列在等待这几步的工夫被清空了：这一轮没有目标，直接收场。
  batchPaths = batchPaths.filter(function(filePath) { return queueState(filePath) === 'pending'; });
  if (batchPaths.length === 0) {
    setCompressionState(COMPRESSION_IDLE, true);
    updateQueueSummary();
    return;
  }

  imagePaths = batchPaths.filter(function(path) { var item = queueItems.get(path); return !item || item.mediaKind !== 'video'; });
  videoPaths = batchPaths.filter(function(path) { var item = queueItems.get(path); return item && item.mediaKind === 'video'; });
  var batchOptions = config.options || {};
  var videoOptions = videoPaths.length > 0 ? videoOptionsFromControls() : null;
  if (videoPaths.length > 0) {
    var videoVerdict = await validateVideoBatch(videoPaths, videoOptions);
    if (!videoVerdict.valid) {
      var validationMessage = (videoVerdict.errors || []).join(' · ') || localizeUiText('视频设置无效');
      var validationNote = document.getElementById('videoValidationNote');
      if (validationNote) {
        validationNote.textContent = validationMessage;
        validationNote.classList.add('visible', 'warning');
      }
      showToast(validationMessage);
      setCompressionState(COMPRESSION_IDLE, true);
      updateQueueSummary();
      return;
    }
    var warningText = (videoVerdict.warnings || []).join(' · ');
    var warningNote = document.getElementById('videoValidationNote');
    if (warningNote) {
      warningNote.textContent = warningText;
      warningNote.classList.toggle('visible', !!warningText);
      warningNote.classList.toggle('warning', !!warningText);
    }
  }
  if (videoOptions) currentVideoOptions = videoOptions;
  currentCompressOptions = batchOptions;
  // 拍下这一轮的意图：中途用户改 chips 时，正在跑的这一批仍然按开跑时的说法汇报。
  runIntentSnapshot = {
    image: imageRequestFromIntent(),
    video: videoPaths.length > 0 ? videoRequestFromIntent() : null,
  };
  var session = beginExecutionSession(batchPaths);
  var targetItems = new Map(batchPaths.map(function(filePath) { return [filePath, queueItems.get(filePath)]; }));
  var settled = new Set();

  updateQueueSummary();

  // 一轮开始 = 未暂停、未停止；按钮与文案全部由状态机说了算。
  setCompressionState(COMPRESSION_RUNNING);
  setPauseButtonVisible(true);
  session.phase = COMPRESSION_RUNNING;

  renderFileQueue();

  /// 事件 → 队列状态。**每一层判据都来自 queueItems**，不看 DOM class：
  /// 从前"这一行是不是被取消过"要靠 row.classList.contains('cancelled')，
  /// 结果停止一轮再继续时，新事件全被那行旧 class 挡在门外。
  const progressHandler = (data) => {
    if (!data) return;
    // 会话身份先对齐：上一轮迟到的 deferred / cancelled 事件绝不许碰这一轮。
    if (!executionSession || data.sessionId !== executionSession.id) return;
    if (runRevision !== queueRevision || data.queueRevision !== runRevision) return;
    var file = data.file;
    if (session.targetPaths.indexOf(file) < 0) return;
    var item = queueItems.get(file);
    // 同一路径在这一轮执行时可能被移除又重新导入；新项不能接旧 worker 的结果。
    if (!item || item !== targetItems.get(file)) return;

    if (data.status === 'starting') {
      if (item.state !== 'pending') return;
      item.state = 'running';
      settled.delete(file);
      paintQueueRow(file);
      applyQueueView();
      updateQueueSummary();
      return;
    }

    if (data.status === 'completed' || data.status === 'failed') {
      var result = data.result;
      if (!result || result.file !== file || settled.has(file)) return;
      // invoke 的收尾结果能补偿丢失的 starting/completed 事件；实时事件则
      // 必须先见过 starting，避免已移除或已恢复的行被迟到结果写回 done。
      if (item.state !== 'running' && !(data.fromSessionResult && item.state === 'pending')) return;
      settled.add(file);
      result.compressOptions = batchOptions;
      item.result = result;
      item.state = result.success ? 'done' : 'failed';
      paintQueueRow(file);
      if (file === selectedQueueFile) renderDetailPane();
      updateQueueSummary();
      emitCompareResultsChanged();
      return;
    }

    if (data.status === 'cancelled') {
      // 用户明确把它移出了队列：它本轮到此为止，不回到 pending。
      if (settled.has(file) || (item.state !== 'pending' && item.state !== 'running')) return;
      settled.add(file);
      item.state = 'removed';
      var idx = files.indexOf(file);
      if (idx >= 0) files.splice(idx, 1);
      var row = fileRows[file];
      if (row) {
        if (row.parentNode) row.parentNode.removeChild(row);
        delete fileRows[file];
      }
      updateQueueSummary();
      return;
    }

    if (data.status === 'deferred') {
      // 整批被停止，这一轮没轮到它：**状态保持 pending**，
      // 下一次「继续压缩」自然还会带上它。行上照旧写「等待中」。
      if (item.state === 'running') return;
      paintQueueRow(file);
      updateQueueSummary();
      return;
    }

    // queued：只是告诉大家它排上了，状态仍然是 pending。
    paintQueueRow(file);
  };

  var settledVideoTasks = new Set();
  const videoProgressHandler = (data) => {
    if (!data || !executionSession || data.sessionId !== executionSession.id) return;
    if (runRevision !== queueRevision || data.queueRevision !== runRevision) return;
    var taskId = data.taskId;
    if (!taskId) return;
    var path = null;
    targetItems.forEach(function(target, targetPath) { if (target.taskId === taskId) path = targetPath; });
    if (!path || (data.path && data.path !== path)) return;
    var item = queueItems.get(path);
    if (!item || item !== targetItems.get(path) || item.taskId !== taskId) return;
    var stage = data.stage || 'queued';
    item.videoProgress = Object.assign({}, data, { stage: stage });
    if (stage === 'starting' || stage === 'encoding') {
      if (item.state === 'pending') item.state = 'running';
      if (item.state !== 'running') return;
      paintQueueRow(path);
      updateQueueSummary();
      return;
    }
    if (stage === 'completed' || stage === 'failed' || stage === 'skippedLarger') {
      if (settledVideoTasks.has(taskId)) return;
      var rawResult = data.result;
      if (!rawResult) return;
      if (item.state !== 'running' && !(data.fromSessionResult && item.state === 'pending')) return;
      settledVideoTasks.add(taskId);
      if (stage === 'skippedLarger') rawResult = Object.assign({}, rawResult, { stage: 'skippedLarger', success: false, skippedLarger: true });
      var result = window.SizqoVideo.normalizeResult(rawResult, path, item.videoProbe, videoOptions);
      result.taskId = taskId;
      result.historyId = rawResult.historyId || result.historyId || null;
      result.backupPath = rawResult.backupPath || result.backupPath || null;
      result.diagnosticsLogPath = rawResult.diagnosticsLogPath || result.diagnosticsLogPath || null;
      result.videoOptions = videoOptions;
      result.videoProbe = item.videoProbe;
      result.compressOptions = videoOptions;
      item.result = result;
      item.state = stage === 'failed' || (!result.success && !result.skippedLarger) ? 'failed' : 'done';
      paintQueueRow(path);
      if (path === selectedQueueFile) renderDetailPane();
      updateQueueSummary();
      emitCompareResultsChanged();
      return;
    }
    if (stage === 'cancelled') {
      if (item.state !== 'running') return;
      settledVideoTasks.add(taskId);
      item.result = null;
      item.videoProgress = null;
      item.state = 'pending';
      pendingAutoCompress = false;
      paintQueueRow(path);
      updateQueueSummary();
      showToast(localizeUiText('当前视频已取消，输入文件仍留在队列中'));
      return;
    }
    if (stage === 'deferred') {
      if (item.state === 'running') return;
      item.state = 'pending';
      paintQueueRow(path);
      updateQueueSummary();
      return;
    }
    paintQueueRow(path);
  };

  var unlisten = function() {};
  var unlistenVideo = function() {};

  try {
    unlisten = await listen('compress-progress', function(event) {
      progressHandler(event.payload);
    });
    var sessionResult;
    if (videoPaths.length > 0) {
      unlistenVideo = await listen('video-progress', function(event) { videoProgressHandler(event.payload); });
      sessionResult = await invoke('start_media_session', {
        images: imagePaths,
        videoTasks: videoPaths.map(function(path) { return { taskId: targetItems.get(path).taskId, path: path }; }),
        imageOptions: batchOptions,
        videoOptions: videoOptions,
        sessionId: session.id,
        queueRevision: session.queueRevision,
        sourceRoots: inputPaths.slice(),
        useSmart: !!config.useSmartIpc,
      });
    } else {
      sessionResult = await invoke(config.useSmartIpc ? 'compress_smart' : 'compress_files', {
        sessionId: session.id,
        queueRevision: session.queueRevision,
        filePaths: imagePaths,
        options: batchOptions,
      });
    }
    // 事件是实时路径，返回值是补救路径：某一轮结束了却没有把事件送到时，
    // 靠它把结果补齐（会话号对不上就直接不认）。
    var resultSessionId = sessionResult && sessionResult.sessionId || session.id;
    var results = sessionResult && Array.isArray(sessionResult.imageResults)
      ? sessionResult.imageResults
      : (sessionResult && Array.isArray(sessionResult.results) ? sessionResult.results : []);
    results.forEach(function(result) {
      progressHandler({
        sessionId: resultSessionId,
        queueRevision: session.queueRevision,
        file: result.file,
        status: result.success ? 'completed' : 'failed',
        result: result,
        fromSessionResult: true,
      });
    });
    var videoResults = sessionResult && Array.isArray(sessionResult.videoResults) ? sessionResult.videoResults : [];
    videoResults.forEach(function(result) {
      videoProgressHandler({
        sessionId: resultSessionId,
        queueRevision: session.queueRevision,
        taskId: result.taskId,
        stage: result.stage || (result.skippedLarger ? 'skippedLarger' : (result.success ? 'completed' : 'failed')),
        result: result,
        fromSessionResult: true,
      });
    });
    if (runRevision === queueRevision) {
      // 收尾结果可能补偿丢失的事件：把每一行和详情列照最终状态重画一遍。
      renderFileQueue();
      renderDetailPane();
    }
  } catch (err) {
    console.error('Compression error:', err);
    showToast((systemModeForRun() ? localizeUiText('转换出错: ') : localizeUiText('压缩出错: ')) + (err.message || err));
  } finally {
    try { unlisten(); } catch (e) {}
    try { unlistenVideo(); } catch (e) {}
    if (executionSession === session) endExecutionSession();
    // 用户按了停止：这一轮就地结束，绝不被"自动压缩"再拉起来跑下一批。
    var stoppedByUser = compressionState === COMPRESSION_STOPPING;
    setCompressionState(COMPRESSION_IDLE);
    setPauseButtonVisible(false);
    updateQueueSummary();
    var shouldContinue = !stoppedByUser && pendingAutoCompress
      && getRunnablePendingPaths().length > 0;
    pendingAutoCompress = false;
    if (shouldContinue) {
      startCompression(true);
    } else {
      updateQueueSummary();
    }
    refreshHistoryIfOpen();
  }
}

// ─── 运行条 + 详情列（只从队列、状态机与结果派生）─────────────────

/// 队列的体积账：只统计**真的落盘成功**的那些项。
/// 图片与视频的结果字段名不同（compressedSize / outputBytes），在这里归成同一本账。
function queueTotals() {
  var totals = { original: 0, output: 0, count: 0 };
  queueResults().forEach(function(result) {
    if (!result || !result.success) return;
    var before = Number(result.originalSize !== undefined ? result.originalSize : result.originalBytes) || 0;
    var after = Number(result.compressedSize !== undefined ? result.compressedSize : result.outputBytes) || 0;
    totals.original += before;
    totals.output += after;
    totals.count += 1;
  });
  totals.saved = totals.original - totals.output;
  totals.rate = totals.original > 0 ? (totals.saved / totals.original) * 100 : 0;
  return totals;
}

/// 阶段文案。**每条都有出处**：数字来自队列快照，词来自状态机，
/// 没有一个字是"看起来应该在跑"猜出来的。
function runPhaseText(p) {
  if (p.total === 0) return localizeUiText('队列为空');
  if (compressionState === COMPRESSION_PAUSED) return localizeUiText('已暂停');
  if (compressionState === COMPRESSION_STOPPING) return localizeUiText('正在停止');
  if (compressionState === COMPRESSION_RUNNING) return processingActionText('progress');
  // 空闲：这一轮结束了，剩下的取决于队列里还有没有活。
  if (p.pending > 0 && p.processed > 0) return localizeUiText('已停止');
  if (p.pending > 0) return localizeUiText('未开始');
  if (p.failed > 0) return localizeUiText('结束');
  return processingActionText('done');
}

function renderRunBar(p) {
  var progress = p || getQueueProgress();
  var fill = document.getElementById('runProgressFill');
  if (fill) fill.style.width = progress.total === 0 ? '0%' : (progress.processed / progress.total * 100) + '%';

  var phase = document.getElementById('runPhase');
  if (phase) {
    phase.textContent = runPhaseText(progress)
      + (progress.total > 0 ? ' · ' + progress.processed + ' / ' + progress.total : '');
  }

  var gain = document.getElementById('runGain');
  if (gain) {
    var totals = queueTotals();
    gain.className = 'run-gain' + (totals.saved > 0 ? ' is-good' : totals.saved < 0 ? ' is-bad' : '');
    gain.textContent = totals.count === 0 ? ''
      : (totals.saved >= 0 ? localizeUiText('省 ') : localizeUiText('多 ')) + formatBytes(Math.abs(totals.saved))
        + ' · ' + Math.abs(totals.rate).toFixed(1) + '%'
        + (totals.count > 0 ? localizeUiText(' · ') + totals.count + localizeUiText(' 个产物') : '');
  }

  var cpu = document.getElementById('runCpu');
  if (cpu) cpu.textContent = cpuLimitText().replace(/^ · /, '');

  var dot = document.getElementById('railRunningDot');
  if (dot) {
    dot.hidden = !isCompressing;
    dot.classList.toggle('is-paused', compressionState === COMPRESSION_PAUSED);
  }
}

/// 选中一项：行上只有一条选中线，详情列写的就是这一项的全部真相。
/// 重新画旧行 + 新行，而不是直接改 class —— 行的画法永远只有 paintQueueRow 一处。
function selectQueueFile(filePath) {
  var previous = selectedQueueFile;
  selectedQueueFile = filePath || null;
  if (previous && previous !== selectedQueueFile) paintQueueRow(previous);
  if (selectedQueueFile) paintQueueRow(selectedQueueFile);
  renderDetailPane();
}

function detailSection(parent, className) {
  var el = document.createElement('div');
  el.className = className;
  parent.appendChild(el);
  return el;
}

function detailText(parent, className, text, title) {
  var el = document.createElement('div');
  el.className = className;
  el.textContent = text;
  if (title) el.title = title;
  parent.appendChild(el);
  return el;
}

/// 键值表。**没有值的键一律不写**：宁可少一行，也不放一个"（未提供）"进去，
/// 因为那既不是用户能改的东西，也不是这一版引擎会上报的东西。
function detailSpecs(parent, pairs) {
  var rows = pairs.filter(function(pair) { return pair[1] !== null && pair[1] !== undefined && pair[1] !== ''; });
  if (!rows.length) return;
  var grid = document.createElement('div');
  grid.className = 'spec-grid';
  rows.forEach(function(pair) {
    var key = document.createElement('span');
    key.className = 'spec-key';
    key.textContent = pair[0];
    var value = document.createElement('span');
    value.className = 'spec-value';
    value.textContent = pair[1];
    grid.appendChild(key);
    grid.appendChild(value);
  });
  parent.appendChild(grid);
}

function detailCallout(parent, tone, text) {
  if (!text) return;
  var box = detailSection(parent, 'detail-callout' + (tone ? ' ' + tone : ''));
  var icon = document.createElement('span');
  icon.innerHTML = iconMarkup(tone === 'is-error' ? 'error' : tone === 'is-ok' ? 'check' : 'warning', true);
  box.appendChild(icon);
  var body = document.createElement('span');
  body.textContent = text;
  box.appendChild(body);
}

/// 详情列的动作**带文字**：行上那一排图标是快捷方式，这里才是把"按下去会发生什么"说清楚的地方。
function detailActions(parent, defs) {
  var rows = defs.filter(function(def) { return !!def; });
  if (!rows.length) return;
  var box = detailSection(parent, 'detail-actions');
  rows.forEach(function(def) {
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'btn btn-small ' + (def.ghost ? 'btn-ghost' : 'btn-secondary');
    btn.innerHTML = iconMarkup(def.icon, true) + ' ' + def.label;
    if (def.title) btn.title = def.title;
    btn.disabled = !!def.disabled;
    btn.addEventListener('click', def.onClick);
    box.appendChild(btn);
  });
}

/// 预览画的是**原素材**：那是用户认得出的一张图。压缩后的差别交给对比窗口。
function detailPreviewFile(container, filePath) {
  var box = detailSection(container, 'detail-preview');
  var img = document.createElement('img');
  img.alt = '';
  img.draggable = false;
  box.appendChild(img);
  loadOriginalImage(img, filePath);
}

/// 视频没有"原图"可读，能拿到的就是抽出来的封面帧：直接给它一个 src。
function detailPreviewSrc(container, src) {
  if (!src) return;
  var box = detailSection(container, 'detail-preview');
  var img = document.createElement('img');
  img.alt = '';
  img.draggable = false;
  img.src = src;
  box.appendChild(img);
}

var DETAIL_EMPTY_ART = '<svg class="detail-empty-art" viewBox="0 0 96 82" aria-hidden="true">'
  + '<rect x="26.5" y="9.5" width="44" height="34" rx="7"/>'
  + '<circle cx="38" cy="20" r="3"/>'
  + '<path d="M31 37l9-10 6 6 5-5 10 11"/>'
  + '<path d="M26 55h44 M26 65h28"/>'
  + '<path class="art-accent" d="M17 44c-7 4-11 10-13 18" stroke-dasharray="3 4"/>'
  + '<path class="art-accent" d="M79 48l1.8 4.2 4.2 1.8-4.2 1.8-1.8 4.2-1.8-4.2-4.2-1.8 4.2-1.8Z"/>'
  + '</svg>';

/// 空着的详情列不是"没有内容"，而是这一栏还没被点亮：给它线稿和一句产品话，
/// 而不是孤零零一行灰色提示。
function detailEmptyState(parent, title, sub) {
  var box = detailSection(parent, 'detail-empty');
  box.innerHTML = DETAIL_EMPTY_ART;
  detailText(box, 'detail-hint', title);
  detailText(box, 'detail-empty-sub', sub);
}

/// 详情列画什么，全看选中项**此刻**在队列里的状态。
function renderDetailPane() {
  var body = document.getElementById('detailBody');
  if (!body) return;
  body.innerHTML = '';
  var filePath = selectedQueueFile;
  var item = filePath ? queueItems.get(filePath) : null;
  if (!item) {
    detailEmptyState(body, localizeUiText('选择左侧的文件看详情'),
      localizeUiText('更轻的文件，让创作更自由'));
    return;
  }
  if (item.mediaKind === 'video') buildVideoDetail(body, filePath, item);
  else buildImageDetail(body, filePath, item);
}

function statusWord(state, result) {
  if (state === 'running') return taskStatusMarkup('running').text;
  if (state === 'pending') return taskStatusMarkup(waitingRowStatus()).text;
  if (state === 'failed') return localizeUiText('失败');
  if (state === 'restored') return localizeUiText('已恢复');
  if (state === 'removed') return localizeUiText('已移除');
  if (state === 'done') return result && result.skippedLarger ? localizeUiText('已跳过（更大）') : localizeUiText('已完成');
  return '';
}

function buildImageDetail(body, filePath, item) {
  var result = item.result;
  var state = item.state;
  detailText(body, 'detail-eyebrow', localizeUiText('图片 · ') + statusWord(state, result));
  detailText(body, 'detail-title', basename(filePath), filePath);
  detailPreviewFile(body, filePath);

  if (result && result.success) {
    var hero = detailSection(body, 'detail-hero' + (result.savings > 0 ? ' is-good' : ''));
    detailText(hero, 'detail-hero-number',
      (result.savings >= 0 ? '-' : '+') + Math.abs(result.savings).toFixed(1) + '%');
    detailText(hero, 'detail-hero-note',
      formatBytes(result.originalSize) + ' → ' + formatBytes(result.compressedSize));
    var chain = detailSection(body, 'detail-chain');
    [
      result.sourceWidth ? result.sourceWidth + ' × ' + result.sourceHeight : '',
      imageTypeLabel(filePath),
      '→',
      result.outputWidth ? result.outputWidth + ' × ' + result.outputHeight : '',
      result.type,
    ].filter(Boolean).forEach(function(part) {
      var span = document.createElement('span');
      span.className = part === '→' ? 'chain-arrow' : '';
      span.textContent = part;
      chain.appendChild(span);
    });
    detailSpecs(body, [
      [localizeUiText('算法'), result.algorithm],
      [localizeUiText('输出格式'), result.type],
      [localizeUiText('耗时'), Number.isFinite(result.elapsedMs) ? (result.elapsedMs / 1000).toFixed(2) + ' s' : ''],
      [localizeUiText('体积目标'), result.targetBytes ? formatBytes(result.targetBytes) : ''],
      [localizeUiText('是否达标'), result.targetMet === true ? localizeUiText('达标')
        : result.targetMet === false ? localizeUiText('未达标') : ''],
      [localizeUiText('输出位置'), result.outputPath || ''],
    ]);
    // 未达标必须单独说，不能埋在数字里：省了多少不是用户在这轮唯一的期待。
    var warn = (result.warnings || []).join('\n');
    detailCallout(body, result.targetMet === false ? '' : warn ? '' : 'is-ok',
      result.targetMet === false
        ? localizeUiText('没能在预算内达标：给出的是这一轮最小的有效结果。') + (warn ? '\n' + warn : '')
        : warn || localizeUiText('有效且更小，已按默认规则采用。'));
    detailActions(body, [
      { label: localizeUiText('另存为'), icon: 'save', onClick: function() { saveResult(filePath); } },
      { label: localizeUiText('对比查看'), icon: 'compare', onClick: function() { openCompareByFile(filePath); } },
      { label: localizeUiText(result.outputMode === 'replace' ? '恢复原图' : '删除这次结果'),
        icon: result.outputMode === 'replace' ? 'restore' : 'trash',
        onClick: function() { restoreOriginal(filePath); } },
      { label: localizeUiText('在访达中显示'), icon: 'finder', onClick: function() { openInFinder(filePath); } },
      { label: localizeUiText('复制日志'), icon: 'copy', ghost: true, onClick: function() { copyCompressLog(result); } },
    ]);
  } else if (state === 'failed') {
    detailCallout(body, 'is-error', (result && result.error) || localizeUiText('这次压缩没有成功'));
    detailSpecs(body, [[localizeUiText('原始大小'), item.originalSize ? formatBytes(item.originalSize) : '']]);
    detailActions(body, [
      { label: localizeUiText('重试这一个'), icon: 'recompress', onClick: function() { compressOneFile(filePath); } },
      result ? { label: localizeUiText('复制日志'), icon: 'copy', ghost: true, onClick: function() { copyCompressLog(result); } } : null,
    ]);
  } else if (state === 'restored') {
    detailCallout(body, 'is-ok', localizeUiText('原图已恢复，压缩产物已按记录处理。'));
    detailActions(body, [
      { label: localizeUiText('重新压缩这一个'), icon: 'recompress', onClick: function() { compressOneFile(filePath); } },
    ]);
  } else {
    // 还没开始：详情列说清楚"接下来会对它做什么"，以及现在已知的事实。
    var next = requestForNext('image');
    detailText(body, 'detail-chain', localizeUiText(next.summary || ''));
    detailSpecs(body, [
      [localizeUiText('原始大小'), item.originalSize ? formatBytes(item.originalSize) : ''],
      [localizeUiText('输出方式'), outputModeLabel()],
      [localizeUiText('元数据'), next.options.processingMode === 'system'
        ? (next.options.preserveMetadata !== false ? localizeUiText('尽量保留（系统转换）') : localizeUiText('不保留'))
        : localizeUiText('高级路径重编码后不保留')],
    ]);
    detailCallout(body, '', localizeUiText(next.warning || ''));
    detailActions(body, [
      { label: localizeUiText('立即压缩这一个'), icon: 'recompress', disabled: isCompressing,
        onClick: function() { compressOneFile(filePath); } },
      { label: localizeUiText('移出队列'), icon: 'minus', ghost: true, disabled: state !== 'pending',
        onClick: function() { removeQueueFile(filePath); } },
    ]);
  }
  detailText(body, 'detail-path', dirname(filePath));
}

function outputModeLabel() {
  var mode = currentOutputMode();
  if (mode === 'replace') return localizeUiText('覆盖原文件（保留可恢复的备份）');
  if (mode === 'folder') return localizeUiText('输出到 ') + (outputDir || localizeUiText('（还没选目录）'));
  return localizeUiText('同目录 · ') + getOutputSuffix() + localizeUiText(' 后缀');
}

function buildVideoDetail(body, filePath, item) {
  var result = item.result;
  var probe = item.videoProbe || {};
  var summary = window.SizqoVideo.videoSummary(probe);
  detailText(body, 'detail-eyebrow', localizeUiText('视频 · ') + statusWord(item.state, result));
  detailText(body, 'detail-title', basename(filePath), filePath);
  if (item.coverUrl) detailPreviewSrc(body, item.coverUrl);

  if (result && result.success) {
    var savings = Number.isFinite(result.savings) ? result.savings
      : (result.originalBytes > 0 && result.outputBytes ? (result.originalBytes - result.outputBytes) / result.originalBytes * 100 : 0);
    var hero = detailSection(body, 'detail-hero' + (savings > 0 ? ' is-good' : ''));
    detailText(hero, 'detail-hero-number', (savings >= 0 ? '-' : '+') + Math.abs(savings).toFixed(1) + '%');
    detailText(hero, 'detail-hero-note',
      formatBytes(result.originalBytes) + ' → ' + formatBytes(result.outputBytes || 0));
    var chain = detailSection(body, 'detail-chain');
    [summary.duration, summary.dimensions, summary.frameRate, '→', result.encoderActual]
      .filter(Boolean).forEach(function(part) {
        var span = document.createElement('span');
        span.className = part === '→' ? 'chain-arrow' : '';
        span.textContent = part;
        chain.appendChild(span);
      });
    detailSpecs(body, [
      [localizeUiText('编码器'), result.encoderActual],
      [localizeUiText('后端'), result.backendActual],
      [localizeUiText('耗时'), window.SizqoVideo.formatDurationUs((result.elapsedMs || 0) * 1000)],
      [localizeUiText('音轨策略'), (result.compressOptions || {}).audioPolicy || ''],
      [localizeUiText('输出位置'), result.outputPath || ''],
    ]);
    detailCallout(body, (result.warnings || []).length ? '' : 'is-ok',
      (result.warnings || []).join('\n') || localizeUiText('已重新探测并完整解码校验后才提交。'));
    detailActions(body, [
      result.outputPath ? { label: localizeUiText('播放结果'), icon: 'play', onClick: function() { openMediaFile(result.outputPath); } } : null,
      { label: localizeUiText('同帧对比'), icon: 'compare',
        onClick: function() { openVideoCompareForResult(result); } },
      { label: localizeUiText(result.outputMode === 'replace' ? '恢复原视频' : '删除这次结果'),
        icon: result.outputMode === 'replace' ? 'restore' : 'trash',
        onClick: function() { restoreVideoResult(result); } },
      { label: localizeUiText('在访达中显示'), icon: 'finder', onClick: function() { openInFinder(filePath); } },
      { label: localizeUiText('查看日志'), icon: 'copy', ghost: true, onClick: function() { copyVideoLog(result); } },
    ]);
  } else if (item.state === 'failed') {
    detailCallout(body, 'is-error', (result && (result.reason || result.error)) || videoWarningText(item));
    detailActions(body, [
      { label: localizeUiText('重试这一个'), icon: 'recompress', disabled: !videoCapabilities.available,
        onClick: function() { compressOneFile(filePath); } },
      result ? { label: localizeUiText('查看日志'), icon: 'copy', ghost: true, onClick: function() { copyVideoLog(result); } } : null,
    ]);
  } else {
    detailSpecs(body, [
      [localizeUiText('时长'), summary.duration],
      [localizeUiText('尺寸'), summary.dimensions],
      [localizeUiText('帧率'), summary.frameRate],
      [localizeUiText('编码'), summary.codec],
      [localizeUiText('音轨'), videoAudioSummary(probe)],
      [localizeUiText('原始大小'), item.originalSize ? formatBytes(item.originalSize) : ''],
    ]);
    var estimate = videoProgressNumbers(item);
    detailText(body, 'detail-chain', localizeUiText(requestForNext('video').summary || '')
      + (estimate ? ' · ' + estimate : ''));
    detailCallout(body, '', videoWarningText(item)
      || (!estimate && item.state === 'pending' ? localizeUiText('这个素材还没有可靠的体积估算。') : ''));
    detailActions(body, [
      { label: localizeUiText('立即压缩这一个'), icon: 'recompress',
        disabled: isCompressing || !videoCapabilities.available,
        onClick: function() { compressOneFile(filePath); } },
      { label: localizeUiText('移出队列'), icon: 'minus', ghost: true, disabled: item.state !== 'pending',
        onClick: function() { removeQueueFile(filePath); } },
    ]);
  }
  detailText(body, 'detail-path', dirname(filePath));
}

/// 单个文件重新压一次 = 把它放回 pending，然后开一轮只含它的会话。
/// 失败文件走的就是这条路（显式「重试」），普通「继续压缩」不碰失败项。
async function compressOneFile(filePath) {
  if (isCompressing) return;
  var item = queueItems.get(filePath);
  if (!item) return;
  item.result = null;
  item.state = 'pending';
  paintQueueRow(filePath);
  emitCompareResultsChanged();
  await startCompressionForPaths(true, [filePath]);
  var after = queueItems.get(filePath);
  if (after && after.state === 'done') {
    showToast(localizeUiText('已重新压缩: ') + basename(filePath));
  }
}

async function saveResult(filePath) {
  const item = queueItems.get(filePath);
  const result = item && item.result;
  if (!result || !result.outputPath) {
    showToast(localizeUiText('无法保存：找不到压缩文件'));
    return;
  }
  const savedPath = await invoke('save_file', { sourcePath: result.outputPath });
  if (savedPath) {
    showToast(localizeUiText('已保存到: ') + basename(savedPath));
  }
}

function openInFinder(filePath) {
  // Reveal the compressed output if available, else the original
  const item = queueItems.get(filePath);
  const result = item && item.result;
  const target = (result && result.outputPath) ? result.outputPath : filePath;
  invoke('open_in_finder', { filePath: target });
}

// 压缩后又用别的 App 改过图：恢复会覆盖那个新版本，必须先问。
var RESTORE_CONFLICT_TEXT = localizeUiText('这个文件在压缩后又被修改过。\n恢复原图会覆盖当前版本。');

/// 恢复成功后的统一收尾：主队列、历史页、对比窗口都只走这里。
/// skipRefresh 供批量恢复使用，避免每个文件重绘一次结果列表。
function afterRestore(filePath, skipRefresh) {
  markQueueRowRestored(filePath);
  if (skipRefresh) return;
  updateQueueSummary();
  emitCompareResultsChanged();
  refreshHistoryIfOpen();
}

async function restoreOriginal(filePath, force) {
  var outcome;
  try {
    outcome = await invoke('restore_original', { filePath: filePath, force: !!force });
  } catch (error) {
    showToast(localizeUiText('恢复失败: ') + (error.message || error));
    return;
  }
  if (outcome.conflict) {
    if (!confirm(RESTORE_CONFLICT_TEXT)) return;
    await restoreOriginal(filePath, true);
    return;
  }
  if (!outcome.success) {
    showToast(outcome.error || localizeUiText('恢复失败'));
    return;
  }
  // 「这条记录到底是什么模式」以后端说的为准：缓存里的 result 可能已经不是这一批的了。
  var cachedItem = queueItems.get(filePath);
  var mode = outcome.outputMode
    || (cachedItem && cachedItem.result ? cachedItem.result.outputMode : null);
  showToast(mode === 'replace'
    ? localizeUiText('已恢复原图: ') + basename(filePath)
    : localizeUiText('已删除这次压缩结果: ') + basename(filePath));
  afterRestore(outcome.filePath || filePath);
}

function markQueueRowRestored(filePath) {
  var item = queueItems.get(filePath);
  if (item && item.state === 'done') {
    item.result = null;
    item.state = 'restored';
    paintQueueRow(filePath);
  }
}

async function restoreAllOriginals() {
  var stash = queueResults();
  if (stash.length === 0) return;
  if (!confirm(localizeUiText('确定要恢复全部已压缩成功的原图吗？'))) return;
  var outcome;
  try {
    outcome = await invoke('restore_all', { results: stash.slice() });
  } catch (error) {
    showToast(localizeUiText('恢复失败: ') + (error.message || error));
    return;
  }
  showToast(outcome.message);
  (outcome.restoredFiles || []).forEach(function(filePath) {
    afterRestore(filePath, true);
  });
  renderDetailPane();
  updateQueueSummary();
  emitCompareResultsChanged();
}

async function exportAll() {
  var stash = queueResults();
  if (stash.length === 0) return;
  const suffix = getResultOutputSuffix(stash[0]);
  const count = await invoke('export_all', { results: stash, outputSuffix: suffix });
  showToast(localizeUiText('已导出 ') + count + localizeUiText(' 个文件到原目录（') + suffix + localizeUiText(' 后缀）'));
}

function formatBytes(bytes) {
  if (bytes === 0) return '0B';
  if (bytes < 1024) return bytes.toFixed(1) + 'B';
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + 'KB';
  return (bytes / (1024 * 1024)).toFixed(1) + 'MB';
}

// ─── 页面导航（本轮 / 历史 / 设置）───────────────────────────────
// 只切换 pane 的 hidden：文件队列 DOM 与正在跑的压缩任务都不动，所以压缩中途也能进历史/设置。
var VIEWS = ['main', 'history', 'settings'];
var RAIL_KEYS = { main: 'workspace', history: 'history', settings: 'settings' };
var VIEW_TITLES = { main: '媒体工作台', history: '压缩历史', settings: '设置' };
// 水印是装饰性展示字，不参与本地化：三种语言下都用同一组英文大字。
var VIEW_WATERMARKS = { main: 'Compress', history: 'History', settings: 'Settings' };
var currentView = 'main';

function showView(name) {
  var view = VIEWS.indexOf(name) >= 0 ? name : 'main';
  currentView = view;
  VIEWS.forEach(function(id) {
    var el = document.getElementById(id + 'View');
    if (el) el.hidden = id !== view;
  });
  // 高亮只从 currentView 派生，绝不去读 CSS class 反推"现在在哪个页"。
  document.querySelectorAll('.rail-button').forEach(function(button) {
    var active = button.dataset.rail === RAIL_KEYS[view];
    button.classList.toggle('active', active);
    if (active) button.setAttribute('aria-current', 'page');
    else button.removeAttribute('aria-current');
  });
  var context = document.getElementById('titlebarContext');
  if (context) context.textContent = localizeUiText(VIEW_TITLES[view]);
  var watermark = document.getElementById('decorWord');
  if (watermark) watermark.textContent = VIEW_WATERMARKS[view];
  // 每次进入都重新读盘：历史是后端状态，不能只信启动时那份快照。
  if (view === 'history') refreshHistory();
  if (view === 'settings') { loadRetentionSetting(); loadCpuSetting(); initUpdatePanel(); }
}

// ─── 压缩状态机（idle / running / paused / stopping）───────────────
// 值域与后端 `CompressionScheduler::state()` 逐字一致：后端每次真的变化都发
// compression-state-change，前端只负责照着画。
//
// 从前只有 `isCompressing` 一个布尔值，于是「暂停中」和「压缩中」在界面上没有任何区别，
// loading 照转、文案照写 —— 用户点了暂停却看不出暂停了，这是那一堆体验问题的根因。
var COMPRESSION_IDLE = 'idle';
var COMPRESSION_RUNNING = 'running';
var COMPRESSION_PAUSED = 'paused';
var COMPRESSION_STOPPING = 'stopping';
var COMPRESSION_STATES = [COMPRESSION_IDLE, COMPRESSION_RUNNING, COMPRESSION_PAUSED, COMPRESSION_STOPPING];

var compressionState = COMPRESSION_IDLE;
// 下面两个是 compressionState 的派生镜像，供既有读点使用。
// **只有 setCompressionState 能写它们**（tests/pause-resume.cjs 钉住这条），
// 别处一律只读，否则又会出现"两个真相"。
var isCompressing = false;
var compressionPaused = false;

/// 唯一的写入口。silent 供"先占住批次、UI 稍后再画"的中间态使用
/// （与旧实现里那些提前 return 的路径一一对应）。
function setCompressionState(next, silent) {
  if (COMPRESSION_STATES.indexOf(next) < 0) return false;
  compressionState = next;
  isCompressing = next !== COMPRESSION_IDLE;
  compressionPaused = next === COMPRESSION_PAUSED;
  if (!silent) renderPauseControls();
  return true;
}

/// 后端阶段变化的确认。批次的收尾由发起它的那次 invoke 负责，所以：
/// - 非 idle 的状态只在本地确实有批次时采纳（否则是上一批的迟到事件）；
/// - idle **永远不采纳**，绝不把一个刚起跑的新批次打回空闲。
function applyCompressionStateEvent(payload) {
  if (!payload || COMPRESSION_STATES.indexOf(payload.state) < 0) return;
  if (payload.state === COMPRESSION_IDLE || !isCompressing) return;
  setCompressionState(payload.state);
}

// ─── 暂停 / 继续 / 停止 ──────────────────────────────────────────
// 三者都只决定"要不要再启动新文件"：暂停时正在压的那张继续跑完，
// 停止时也一样 —— 绝不 kill 已经在跑的子进程（那会留下写了一半的临时文件、
// 悬空的覆盖事务和对不上账的历史）。区别在闸门关多久：暂停关到用户点继续，
// 停止是永久关闭，等已经开跑的那几个收尾就结束整批。

function pauseButtonText(paused) {
  return paused
    ? localizeUiText('<svg class="symbol-icon symbol-icon-small"><use href="#icon-play"/></svg> 继续')
    : localizeUiText('<svg class="symbol-icon symbol-icon-small"><use href="#icon-pause"/></svg> 暂停');
}

function stopButtonText(stopping) {
  return '<svg class="symbol-icon symbol-icon-small"><use href="#icon-stop"/></svg> '
    + (stopping ? localizeUiText('正在停止…') : localizeUiText('停止'));
}

/// 转圈图标只在这一处拼 HTML：队列行、视频行、进度按钮都从这里拿。
function progressSpinnerMarkup() {
  return '<span class="progress-file-spinner"></span>';
}

/// 队列行的图标 + 文案，一处说了算 —— 不许再散落 `innerHTML = '<span class="spinner">'`。
/// 入参是**队列状态**（pending / running / done / failed / removed / restored）
/// 或会话状态（paused / stopping）。停止不改变队列状态：被延后的文件仍然是
/// pending，界面上照旧写「等待中」，因为它真的还在等下一轮。
function taskStatusMarkup(status) {
  switch (status) {
    case 'running':
      // 会话已经暂停 / 正在停止，但它真的还在跑：照实写「收尾中…」并留着 spinner。
      // 把一张确实在压缩的图假装成停下来，比让它多转一会儿圈更糟。
      if (compressionState === COMPRESSION_PAUSED || compressionState === COMPRESSION_STOPPING) {
        return { icon: progressSpinnerMarkup(), text: localizeUiText('收尾中…') };
      }
      return { icon: progressSpinnerMarkup(), text: processingActionText('progress') };
    case 'paused':
      // 暂停不用"停住的转圈"：一个静止的圆环看着像卡死，暂停图标才是它的意思。
      return { icon: iconMarkup('pause', true), text: localizeUiText('已暂停') };
    case 'removed':
      return { icon: iconMarkup('minus', true), text: localizeUiText('已移除') };
    case 'restored':
      return { icon: iconMarkup('restore', true), text: localizeUiText('已恢复') };
    default:
      return { icon: iconMarkup('queue', true), text: localizeUiText('等待中') };
  }
}

/// 状态写进两处：最右的状态图标 + 「结论」那一列的文字。
/// 图标只是提示，**文字才是承诺**：暂停必须能在文字里读出来，不能只靠换一张图。
function renderTaskStatus(row, status) {
  if (!row) return;
  var markup = taskStatusMarkup(status);
  var icon = row.querySelector('.media-state-icon');
  if (icon) {
    icon.className = 'media-state-icon ' + (status === 'paused' ? 'pending' : status);
    icon.innerHTML = markup.icon;
  }
  var text = row.querySelector('.media-meta');
  if (text) text.textContent = markup.text;
}

/// 还没开始的行此刻该显示什么：暂停时不该还写着「等待中」。
/// **停止时它仍然是 pending**（下一轮还会带上），所以照旧是「等待中」——
/// 「已跳过」那种写法是把"这一轮没轮到"说成了"这个文件出局了"。
function waitingRowStatus() {
  return compressionPaused ? 'paused' : 'pending';
}

function repaintWaitingRows() {
  files.forEach(function(filePath) {
    if (queueState(filePath) === 'running') {
      // 正在跑的行在暂停 / 停止时写「收尾中…」，那才是它此刻的真面目。
      var runningRow = fileRows[filePath];
      if (runningRow) renderTaskStatus(runningRow, 'running');
      return;
    }
    if (queueState(filePath) !== 'pending') return;
    var row = fileRows[filePath];
    if (row) renderTaskStatus(row, waitingRowStatus());
  });
}

/// 进度按钮上那段"图标 + 文案"。暂停时换掉转圈的圆环改用静态暂停图标 ——
/// "动画还在转"本身就是用户判断"到底暂停了没有"的依据。
function progressButtonMarkup() {
  if (compressionState === COMPRESSION_PAUSED) {
    return iconMarkup('pause', true) + localizeUiText(' 暂停中…');
  }
  if (compressionState === COMPRESSION_STOPPING) {
    return progressSpinnerMarkup() + localizeUiText(' 正在停止…');
  }
  return progressSpinnerMarkup() + ' ' + processingActionText('progress');
}

function renderPauseControls() {
  var stopping = compressionState === COMPRESSION_STOPPING;
  var btn = document.getElementById('pauseCompressBtn');
  var text = document.getElementById('pauseBtnText');
  if (text) text.innerHTML = pauseButtonText(compressionPaused);
  if (btn) {
    btn.title = compressionPaused ? localizeUiText('继续压缩剩余文件') : localizeUiText('暂停：不再启动新文件');
    btn.disabled = stopping;
  }
  var stopBtn = document.getElementById('stopCompressBtn');
  var stopText = document.getElementById('stopBtnText');
  if (stopText) stopText.innerHTML = stopButtonText(stopping);
  if (stopBtn) {
    stopBtn.disabled = !isCompressing || stopping;
    stopBtn.title = localizeUiText('停止：正在处理的文件会先完成，其余文件保留在队列中');
  }
  repaintWaitingRows();
  updateQueueSummary();
}

/// 主按钮。三件事一起看：**会话在不在跑**（compressionState）、
/// **还有没有要处理的**（队列 pending）、**有没有失败的**（队列 failed）。
/// 从前的 finally 一律写死「压缩完成 ✓」，停止之后明明还剩 58 张也照写不误。
function renderStartButton() {
  var startBtn = document.getElementById('startCompressBtn');
  if (!startBtn) return;
  var btnText = document.getElementById('compressBtnText');
  var icon = '<svg class="symbol-icon"><use href="#icon-compress"/></svg> ';
  startBtn.classList.toggle('compressing', isCompressing);
  startBtn.classList.toggle('paused', compressionState === COMPRESSION_PAUSED);
  startBtn.classList.toggle('stopping', compressionState === COMPRESSION_STOPPING);
  startBtn.classList.remove('done');

  if (isCompressing) {
    startBtn.disabled = true;
    if (btnText) btnText.innerHTML = progressButtonMarkup();
    return;
  }
  // 这一轮结束了：按钮该说什么，全看队列还剩什么。
  startBtn.disabled = false;
  var p = getQueueProgress();
  if (!btnText) return;
  if (p.pending > 0 && p.processed > 0) {
    // 停止 / 中途收场之后：还有活没干完，主按钮就是「继续压缩」。
    btnText.innerHTML = icon + processingActionText('continue');
    return;
  }
  if (p.pending > 0) {
    btnText.innerHTML = icon + processingActionText('idle');
    return;
  }
  if (p.total > 0 && p.failed > 0) {
    btnText.innerHTML = iconMarkup('warning', true) + localizeUiText(' 处理完成 · ') + p.failed + localizeUiText(' 个失败');
    return;
  }
  if (p.total > 0) {
    startBtn.classList.add('done');
    btnText.innerHTML = '<svg class="symbol-icon"><use href="#icon-check"/></svg> ' + processingActionText('done');
    return;
  }
  btnText.innerHTML = icon + processingActionText('idle');
}

function setPauseButtonVisible(visible) {
  ['pauseCompressBtn', 'stopCompressBtn'].forEach(function(id) {
    var btn = document.getElementById(id);
    if (btn) btn.style.display = visible ? 'inline-flex' : 'none';
  });
}

async function toggleCompressionPause() {
  // 停止在收尾途中不许再切换暂停：闸门已经焊死，按下去只会让文案和真相分叉。
  if (!isCompressing || compressionState === COMPRESSION_STOPPING) return;
  var previous = compressionState;
  var next = previous === COMPRESSION_PAUSED ? COMPRESSION_RUNNING : COMPRESSION_PAUSED;
  setCompressionState(next);
  try {
    await invoke(next === COMPRESSION_PAUSED ? 'pause_compression' : 'resume_compression');
  } catch (error) {
    console.error('Pause toggle failed:', error);
    setCompressionState(previous);
    showToast(next === COMPRESSION_PAUSED ? localizeUiText('暂停失败，请重试') : localizeUiText('继续失败，请重试'));
  }
}

/// 停止**这一轮**：还没轮到的文件不再启动，已经在压的几个跑完各自收尾。
///
/// 停止不是取消：没轮到的文件留在队列里（仍然是 pending），主按钮随后变成
/// 「继续压缩」。停止之后**不许**自动续跑下一轮（pendingAutoCompress 在这里就清掉）：
/// 用户刚说"停下"，再被"自动压缩"拉起来就是没听他说话。
async function stopCompression() {
  if (!isCompressing || compressionState === COMPRESSION_STOPPING) return;
  var previous = compressionState;
  pendingAutoCompress = false;
  setCompressionState(COMPRESSION_STOPPING);
  showToast(localizeUiText('正在停止：正在处理的文件会先完成，其余文件保留在队列中'));
  try {
    // 闸门在后端焊死，不需要把路径一条条传过去。
    await invoke('stop_compression');
  } catch (error) {
    console.error('Stop failed:', error);
    setCompressionState(previous);
    showToast(localizeUiText('停止失败，请重试'));
  }
}

// ─── 历史记录页 ─────────────────────────────────────────────────
var historyEntries = [];
var retentionDays = 0;   // 与后端默认一致：不保留（本次退出时清理）

function pad2(value) {
  return String(value).length < 2 ? '0' + value : String(value);
}

function historyTime(millis) {
  if (!millis) return '';
  var date = new Date(millis);
  var today = new Date();
  var yesterday = new Date(today.getTime() - 86400000);
  var clock = pad2(date.getHours()) + ':' + pad2(date.getMinutes());
  if (date.toDateString() === today.toDateString()) return localizeUiText('今天 ') + clock;
  if (date.toDateString() === yesterday.toDateString()) return localizeUiText('昨天 ') + clock;
  return date.getFullYear() + '-' + pad2(date.getMonth() + 1) + '-' + pad2(date.getDate()) + ' ' + clock;
}

function canRestoreHistory(entry) {
  // recoveryAvailable = history.json 损坏后从备份目录重建出来的条目：
  // 压缩明细已经无从得知，但备份确实还在，原图仍然可以一键恢复。
  if (entry.status === 'recoveryAvailable') {
    return !!entry.backupExists && !!entry.sourceExists;
  }
  return entry.status === 'compressed'
    && entry.outputMode === 'replace'
    && !!entry.backupExists
    && !!entry.sourceExists;
}

function historyStatusText(entry) {
  if (entry.status === 'restored') {
    return localizeUiText('已恢复') + (entry.restoredAt ? ' · ' + historyTime(entry.restoredAt) : '');
  }
  if (entry.status === 'recoveryAvailable') {
    return entry.backupExists ? localizeUiText('检测到可恢复的原图备份') : localizeUiText('原图备份已清理');
  }
  if (entry.outputMode !== 'replace') return localizeUiText('原图未覆盖');
  if (!entry.sourceExists) return localizeUiText('原文件位置不存在');
  if (!entry.backupExists) return localizeUiText('原图备份已清理');
  return localizeUiText('已压缩');
}

/// 历史行的按钮，与队列「压缩完成」那一行同一套动作 —— 判据必须和 Swift 的
/// `historyRowActions` 逐条一致：按下去不成立的按钮一个都不许出现。
/// 后缀模式的原图从没被覆盖过，给它「恢复原图」是假的；对等的反悔是删掉产物。
function historyRowActionDefs(entry) {
  var isVideo = entry.mediaKind === 'video';
  var isReplace = entry.outputMode === 'replace';
  // 「原图还摸得着吗」按模式判：覆盖模式只有备份算数（源位置此刻躺着的是压缩结果），
  // 后缀 / 目录模式的源文件本身就没被动过。
  var originalAvailable = isReplace ? !!entry.backupExists : !!entry.sourceExists;
  var defs = [];
  if (entry.outputExists) {
    defs.push({ action: 'save', title: localizeUiText('另存为'), icon: 'save' });
  }
  var originalPath = isReplace ? (entry.backupExists ? entry.backupPath : null) : (entry.sourceExists ? entry.sourcePath : null);
  if (isVideo && originalPath) defs.push({ action: 'playOriginal', title: localizeUiText('播放原视频'), icon: 'play' });
  if (isVideo && entry.outputExists) defs.push({ action: 'playOutput', title: localizeUiText('播放输出视频'), icon: 'play' });
  // 两边都真实存在才比得出差别 —— 备份没了还挂一个「对比」，比的是那张压缩图和它自己。
  if (entry.outputExists && originalAvailable) {
    defs.push({ action: 'compare', title: localizeUiText(isVideo ? '同帧对比' : '对比查看'), icon: 'compare' });
  }
  if (canRestoreHistory(entry)) {
    defs.push({ action: 'restore', title: localizeUiText(isVideo ? '恢复原视频' : '恢复原图'), icon: 'restore' });
  } else if (entry.status === 'compressed' && !isReplace && entry.outputExists) {
    defs.push({ action: 'deleteOutput', title: localizeUiText('删除这次压缩结果'), icon: 'trash' });
  }
  defs.push({ action: 'finder', title: localizeUiText('在访达中显示'), icon: 'finder' });
  defs.push({ action: 'log', title: localizeUiText('复制日志'), icon: 'copy' });
  return defs;
}

/// 历史条目 → 对比窗口的载荷。字段名必须对齐 Rust 的 `CompressResult`
/// （`type` 而不是 `outType`，尺寸还要给已经格式化好的字符串），
/// 且只有备份真在的时候才填 `backupPath` —— 否则 compare_window.js 会把
/// "已被压缩结果占着的源文件"当成原图来比。
function historyCompareResult(entry) {
  return {
    file: entry.sourcePath,
    success: true,
    originalSize: entry.originalSize,
    compressedSize: entry.compressedSize,
    originalSizeFormatted: formatBytes(entry.originalSize),
    compressedSizeFormatted: formatBytes(entry.compressedSize),
    savings: entry.savings,
    type: entry.outType,
    algorithm: entry.algorithm,
    outputMode: entry.outputMode,
    outputPath: entry.outputPath || entry.sourcePath,
    backupPath: entry.backupExists ? entry.backupPath : null,
  };
}

function openCompareFromHistory(entry) {
  if (!entry.outputExists) {
    showToast(localizeUiText('压缩结果已不存在'));
    refreshHistory();
    return;
  }
  if (entry.mediaKind === 'video') {
    var source = entry.outputMode === 'replace'
      ? (entry.backupExists ? entry.backupPath : null)
      : (entry.sourceExists ? entry.sourcePath : null);
    if (!source || !entry.outputPath) {
      showToast(localizeUiText('原视频或压缩结果已不存在'));
      refreshHistory();
      return;
    }
    openVideoCompare(source, entry.outputPath, entry.videoMetadata || null);
    return;
  }
  invoke('open_compare_window', {
    payload: { results: [historyCompareResult(entry)], index: 0 },
  }).catch(function(err) {
    showToast(localizeUiText('打开对比窗口失败: ') + (err.message || err));
  });
}

async function saveHistoryOutput(entry) {
  if (!entry.outputExists || !entry.outputPath) {
    showToast(localizeUiText('压缩结果已不存在'));
    refreshHistory();
    return;
  }
  const savedPath = await invoke('save_file', { sourcePath: entry.outputPath });
  if (savedPath) showToast(localizeUiText('已保存到: ') + basename(savedPath));
}

/// 后缀 / 目录模式的对等「反悔」= 删掉这次生成的压缩结果。
/// 删的是用户目录里的真实文件，所以必须二次确认；原图从头到尾没动过。
async function deleteHistoryOutput(entry) {
  if (!confirm(localizeUiText('删除这次压缩结果？\n将删除 ') + entry.outputPath
    + localizeUiText('，并移除这条历史记录。原图未被覆盖，不受影响。'))) return;
  // 后缀 / 目录模式没有"原图被改过"这回事，force 在这儿没有意义，照默认走同一个服务。
  await restoreFromHistory(entry);
}

/// 历史行的复制日志：记录里没有本批次的压缩参数快照，只报历史上记下来的那些事实。
function copyHistoryLog(entry) {
  var lines = [];
  lines.push(localizeUiText('版本: ') + BUILD_VARIANT);
  lines.push(localizeUiText('=== Sizqo 压缩历史 ==='));
  lines.push('');
  lines.push(localizeUiText('文件: ') + entry.sourcePath);
  lines.push(localizeUiText('时间: ') + historyTime(entry.createdAt));
  lines.push(localizeUiText('状态: ') + historyStatusText(entry));
  lines.push('');
  lines.push(localizeUiText('--- 压缩结果 ---'));
  lines.push(localizeUiText('输出: ') + (entry.outputPath || entry.sourcePath));
  lines.push(localizeUiText('输出方式: ') + entry.outputMode);
  if (entry.mediaKind === 'video') {
    var meta = entry.videoMetadata || {};
    lines.push('mediaKind: video');
    lines.push('encoder: ' + (entry.algorithm || meta.encoderActual || localizeUiText('(未知)')));
    lines.push('metadata: ' + JSON.stringify(meta));
  }
  if (entry.status === 'recoveryAvailable') {
    lines.push(localizeUiText('明细: 已丢失（这条记录是按原图备份重建出来的）'));
  } else {
    lines.push(localizeUiText('原始大小: ') + formatBytes(entry.originalSize) + ' (' + entry.originalSize + ' bytes)');
    lines.push(localizeUiText('压缩后大小: ') + formatBytes(entry.compressedSize) + ' (' + entry.compressedSize + ' bytes)');
    lines.push(localizeUiText('压缩率: ') + (entry.savings >= 0 ? '-' : '+') + Math.abs(entry.savings).toFixed(1) + '%');
    lines.push(localizeUiText('输出格式: ') + (entry.outType || localizeUiText('(未知)')));
    lines.push(localizeUiText('算法: ') + (entry.algorithm || localizeUiText('(未知)')));
  }
  lines.push('');
  lines.push(localizeUiText('--- 原图备份 ---'));
  lines.push(entry.backupPath || localizeUiText('(无)'));
  var ok = copyTextToClipboard(lines.join('\n'));
  showToast(ok ? localizeUiText('压缩日志已复制到剪贴板') : localizeUiText('复制失败，请手动选中日志文本'));
}

/// 历史行与本轮队列行**共用同一套格子**：同一 kinds 的事实放在同一列，
/// 用户不必在两页之间重新学一遍布局。
function historyRow(entry) {
  var isRecovery = entry.status === 'recoveryAvailable';
  var isVideo = entry.mediaKind === 'video';
  var row = document.createElement('div');
  row.className = 'media-item' + (isRecovery ? ' failed' : entry.status === 'restored' ? '' : ' done');
  row.dataset.historyId = entry.id;
  row.innerHTML =
    '<span class="media-thumb"></span>' +
    '<span class="media-title"><span class="media-name"></span><span class="media-sub"></span></span>' +
    '<span class="media-meta"></span>' +
    '<span class="media-size"></span>' +
    '<span class="media-status"><span class="media-state-icon"></span><span class="media-actions"></span></span>';

  row.querySelector('.media-thumb').innerHTML = iconMarkup(isVideo ? 'video' : 'image', true);
  var stateIcon = row.querySelector('.media-state-icon');
  stateIcon.className = 'media-state-icon ' + (isRecovery ? 'failed' : entry.status === 'restored' ? 'restored' : 'done');
  stateIcon.innerHTML = iconMarkup(isRecovery ? 'warning' : entry.status === 'restored' ? 'restore' : 'check', true);

  var name = row.querySelector('.media-name');
  name.textContent = entry.fileName;
  name.title = entry.sourcePath;
  var sub = row.querySelector('.media-sub');
  sub.textContent = [historyTime(entry.createdAt), dirname(entry.sourcePath)].filter(Boolean).join(' · ');
  sub.title = entry.sourcePath;

  var meta = row.querySelector('.media-meta');
  // 重建条目没有这次压缩的明细，报一个算出来的 0.0% 节省率是假数字。
  meta.textContent = isRecovery ? localizeUiText('明细已丢失（按原图备份重建）')
    : historyStatusText(entry) + ' · ' + (entry.algorithm || entry.outType || '');

  var size = row.querySelector('.media-size');
  size.textContent = formatBytes(entry.compressedSize);
  size.title = formatBytes(entry.originalSize) + ' → ' + formatBytes(entry.compressedSize);

  var actions = row.querySelector('.media-actions');
  historyRowActionDefs(entry).forEach(function(def) {
    var btn = document.createElement('button');
    btn.className = 'queue-action-btn';
    btn.type = 'button';
    btn.title = def.title;
    btn.setAttribute('aria-label', def.title);
    btn.dataset.historyAction = def.action;
    btn.innerHTML = iconMarkup(def.icon, true);
    btn.addEventListener('click', function() { runHistoryAction(entry, def.action); });
    actions.appendChild(btn);
  });

  row.classList.toggle('selected', selectedHistoryKey === entry.id);
  row.addEventListener('click', function() { selectHistoryEntry(entry.id); });
  return row;
}

/// 选中一条历史：详情列写的就是这一条，包括"原图还在不在"这种此刻才成立的事实。
function selectHistoryEntry(id) {
  var previous = selectedHistoryKey;
  selectedHistoryKey = id || null;
  if (previous && previous !== id) {
    var oldRow = document.querySelector('[data-history-id="' + cssEscape(previous) + '"]');
    if (oldRow) oldRow.classList.remove('selected');
  }
  var row = document.querySelector('[data-history-id="' + cssEscape(id) + '"]');
  if (row) row.classList.add('selected');
  renderHistoryDetail();
}

function cssEscape(value) {
  return window.CSS && CSS.escape ? CSS.escape(String(value)) : String(value).replace(/[^a-zA-Z0-9_-]/g, '\\$&');
}

/// 历史详情列：**这条记录此刻在磁盘上还剩什么**，以及因此还能做什么。
/// 备份被清理、产物被手动删掉，都在这里如实反映，不靠状态字段猜测。
function renderHistoryDetail() {
  var body = document.getElementById('historyDetailBody');
  if (!body) return;
  body.innerHTML = '';
  var entry = null;
  for (var i = 0; i < historyEntries.length; i++) {
    if (historyEntries[i].id === selectedHistoryKey) { entry = historyEntries[i]; break; }
  }
  if (!entry) {
    detailEmptyState(body, localizeUiText('选择一条记录，看它此刻在磁盘上还剩什么'),
      localizeUiText('每一次压缩都留了回来的路'));
    return;
  }
  var isVideo = entry.mediaKind === 'video';
  var isRecovery = entry.status === 'recoveryAvailable';
  var isReplace = entry.outputMode === 'replace';
  var originalPath = isReplace ? (entry.backupExists ? entry.backupPath : null)
    : (entry.sourceExists ? entry.sourcePath : null);

  detailText(body, 'detail-eyebrow',
    localizeUiText('历史记录 · ') + localizeUiText(isVideo ? '视频 · ' : '图片 · ') + historyStatusText(entry));
  detailText(body, 'detail-title', entry.fileName, entry.sourcePath);
  if (originalPath) detailPreviewFile(body, originalPath);
  else if (entry.outputExists) detailPreviewFile(body, entry.outputPath || entry.sourcePath);

  if (!isRecovery) {
    var hero = detailSection(body, 'detail-hero' + (entry.savings > 0 ? ' is-good' : ''));
    detailText(hero, 'detail-hero-number',
      (entry.savings >= 0 ? '-' : '+') + Math.abs(entry.savings).toFixed(1) + '%');
    detailText(hero, 'detail-hero-note',
      formatBytes(entry.originalSize) + ' → ' + formatBytes(entry.compressedSize));
    var chain = detailSection(body, 'detail-chain');
    [entry.algorithm, entry.outType, isVideo ? localizeUiText('视频') : imageTypeLabel(entry.sourcePath)]
      .filter(Boolean).forEach(function(part) {
        var span = document.createElement('span');
        span.textContent = part;
        chain.appendChild(span);
      });
  }
  detailSpecs(body, [
    [localizeUiText('时间'), historyTime(entry.createdAt)],
    [localizeUiText('保留至'), historyTime(entry.expiresAt)],
    [localizeUiText('输出方式'), entry.outputMode],
    [localizeUiText('压缩结果'), entry.outputExists
      ? (entry.outputPath || entry.sourcePath) : localizeUiText('已不在磁盘上')],
    [localizeUiText('原图备份'), isReplace
      ? (entry.backupExists ? entry.backupPath : localizeUiText('已清理，无法恢复原图'))
      : localizeUiText('原图从未被覆盖')],
    [localizeUiText('原文件'), entry.sourceExists ? entry.sourcePath : localizeUiText('位置已不存在')],
  ]);

  if (isRecovery) {
    detailCallout(body, '', localizeUiText('这条记录是从备份目录重建的：这次压缩的明细已无从得知，但备份还在，原图仍能恢复。'));
  } else if (entry.status === 'restored') {
    detailCallout(body, 'is-ok', localizeUiText('原图已在 ') + historyTime(entry.restoredAt) + localizeUiText(' 写回。'));
  } else if (!entry.outputExists) {
    detailCallout(body, '', localizeUiText('压缩结果已不在磁盘上（可能被手动删除）。'));
  }
  detailActions(body, historyRowActionDefs(entry).map(function(def) {
    return { label: def.title, icon: def.icon, onClick: function() { runHistoryAction(entry, def.action); } };
  }));
  detailText(body, 'detail-path', entry.sourcePath);
}

/// 历史页那一行按下去要做的事。恢复和「删除这次压缩结果」共用 restore_history_entry
/// 这一个服务：后端按 outputMode 决定是写回原图还是删掉产物，前端不自己判断文件该怎么动。
function runHistoryAction(entry, action) {
  if (action === 'save') { saveHistoryOutput(entry); return; }
  if (action === 'playOriginal') { openMediaFile(entry.outputMode === 'replace' ? entry.backupPath : entry.sourcePath); return; }
  if (action === 'playOutput') { openMediaFile(entry.outputPath || entry.sourcePath); return; }
  if (action === 'compare') { openCompareFromHistory(entry); return; }
  if (action === 'restore') { restoreFromHistory(entry); return; }
  if (action === 'deleteOutput') { deleteHistoryOutput(entry); return; }
  if (action === 'log') { copyHistoryLog(entry); return; }
  var target = entry.outputExists ? (entry.outputPath || entry.sourcePath) : entry.sourcePath;
  invoke('open_in_finder', { filePath: target }).catch(function() {});
}

function renderHistory() {
  var list = document.getElementById('historyList');
  if (!list) return;
  list.innerHTML = '';
  var visibleEntries = historyEntries.filter(function(entry) {
    var kind = entry.mediaKind === 'video' ? 'video' : 'image';
    return historyFilter === 'all' || historyFilter === kind;
  });
  visibleEntries.forEach(function(entry) { list.appendChild(historyRow(entry)); });
  // 选中项可能被保留策略或用户在别处的操作清掉了：只跟着真实存在的记录走。
  var selectedStillExists = historyEntries.some(function(e) { return e.id === selectedHistoryKey; });
  if (!selectedStillExists) selectedHistoryKey = null;
  if (!selectedHistoryKey && visibleEntries.length > 0) selectedHistoryKey = visibleEntries[0].id;
  renderHistoryDetail();
  var empty = document.getElementById('historyEmpty');
  if (empty) {
    empty.hidden = visibleEntries.length > 0;
    empty.textContent = historyEntries.length > 0 ? localizeUiText('此类别还没有压缩记录') : localizeUiText('还没有压缩记录');
  }
  var meta = document.getElementById('historyMeta');
  if (meta) meta.textContent = visibleEntries.length + localizeUiText(' 条');
  // 后端在批次/事务进行中会拒绝清空，这里提前收成不可点，别让用户撞上报错。
  var clearBtn = document.getElementById('historyClearBtn');
  if (clearBtn) {
    clearBtn.disabled = isCompressing;
    clearBtn.title = isCompressing ? localizeUiText('压缩进行中，这一批结束后才能清空历史') : localizeUiText('清空历史记录');
  }
}

async function refreshHistory() {
  try {
    historyEntries = await invoke('list_history');
  } catch (error) {
    console.error('History load failed:', error);
    historyEntries = [];
    showToast(localizeUiText('读取历史记录失败'));
  }
  renderHistory();
}

function refreshHistoryIfOpen() {
  if (currentView === 'history') refreshHistory();
}

async function restoreFromHistory(entry, force) {
  var outcome;
  try {
    outcome = await invoke('restore_history_entry', { historyId: entry.id, force: !!force });
  } catch (error) {
    showToast(localizeUiText('恢复失败: ') + (error.message || error));
    return;
  }
  if (outcome.conflict) {
    if (!confirm(RESTORE_CONFLICT_TEXT)) return;
    await restoreFromHistory(entry, true);
    return;
  }
  if (!outcome.success) {
    showToast(outcome.error || localizeUiText('恢复失败'));
    return;
  }
  // 非 replace 模式的原图从没被盖过，后端做的是"删掉这次的压缩产物" ——
  // 报「已恢复原图」等于把一件没发生过的事说给用户听。
  showToast(entry.outputMode === 'replace'
    ? localizeUiText('已恢复原图: ') + basename(entry.fileName)
    : localizeUiText('已删除这次压缩结果: ') + basename(entry.fileName));
  afterRestore(outcome.filePath || entry.sourcePath);
}

async function clearHistory() {
  if (historyEntries.length === 0) return;
  // 按钮通常已被收成不可点；这里兜住"渲染时机没赶上"的那一次点击。
  if (isCompressing) {
    showToast(localizeUiText('压缩进行中，这一批结束后才能清空历史'));
    return;
  }
  // 清空就是手动到期：备份立刻删掉、不必等保留期。要数清楚这一次带走几份，
  // 也要说清楚"不会删除你的任何图片文件"——那是事实，不是安抚。
  var pendingBackups = historyEntries.filter(function(e) { return e.backupExists; }).length;
  var question = localizeUiText('确定要清空 ') + historyEntries.length + localizeUiText(' 条历史记录吗？\n');
  question += pendingBackups > 0
    ? localizeUiText('同时立即删除 Sizqo 保存的 ') + pendingBackups + localizeUiText(' 份原图备份，不必等保留期到期。不会删除你的任何图片文件。')
    : localizeUiText('Sizqo 目前没有保存原图备份，这次只清记录。不会删除你的任何图片文件。');
  if (!confirm(question)) return;
  try {
    var removed = await invoke('clear_history');
    showToast(localizeUiText('已清空 ') + removed + localizeUiText(' 条历史记录'));
  } catch (error) {
    showToast(localizeUiText('清空失败: ') + (error.message || error));
    return;
  }
  await refreshHistory();
}

// ─── 设置页：原图备份保留时间 ───────────────────────────────────
async function loadRetentionSetting() {
  try {
    var settings = await invoke('get_app_settings');
    // 0 是真实档位（不保留），不能用真值判断，否则会被当成"没设置"回落成 3 天。
    if (settings && settings.originalRetentionDays != null) {
      retentionDays = settings.originalRetentionDays;
    }
    applyRetentionSetting();
  } catch (error) {
    console.error('Settings load failed:', error);
  }
}

async function saveRetentionDays(days) {
  try {
    var settings = await invoke('set_original_retention_days', { days: days });
    retentionDays = settings.originalRetentionDays;
    showToast(retentionDays === 0
      ? localizeUiText('原图备份改为不保留，关闭应用时清理记录和备份')
      : localizeUiText('原图备份保留 ') + retentionDays + localizeUiText(' 天，关闭应用时清理过期项'));
  } catch (error) {
    showToast(localizeUiText('设置失败: ') + (error.message || error));
  }
  // 失败也要回到 retentionDays 那份真值，不能把选错的档位停在半路上。
  applyRetentionSetting();
}

// 「不保留」不是"关掉备份"：覆盖前照旧备份，只是寿命到本次退出为止。
// 清理只有一个时机 —— 关闭应用，所以文案不许再承诺"下次启动"。
// 文案必须把这件事说清楚，也不能写成吓人的措辞。
function applyRetentionSetting() {
  var select = document.getElementById('retentionDays');
  if (select) select.value = String(retentionDays);
  var copy = document.getElementById('retentionCopy');
  if (copy) {
    copy.textContent = retentionDays === 0
      ? localizeUiText('本次运行的压缩记录和原图备份都会在关闭应用时清理；期间可以随时恢复原图。')
      : localizeUiText('过期的历史记录和原图备份将在关闭应用时自动清理。');
  }
}

(function wireRetentionSelect() {
  var select = document.getElementById('retentionDays');
  if (!select) return;
  select.addEventListener('change', function() {
    var days = parseInt(select.value, 10);
    // || 3 会把"不保留"吞成 3 天 —— 0 必须原样传下去。
    saveRetentionDays(Number.isNaN(days) ? 0 : days);
  });
})();

// ─── 设置页：CPU 使用上限 ───────────────────────────────────────
// 后端负责检测与 clamp（含"换到核更少的机器"），前端只呈现与回传用户选择。

function cpuArchitectureLabel(architecture) {
  return architecture === 'aarch64' ? 'ARM64' : (architecture || '');
}

function cpuDeviceText(status) {
  if (!status) return localizeUiText('检测中…');
  return [status.modelName, cpuArchitectureLabel(status.architecture)]
    .filter(function(part) { return !!part; }).join(' · ');
}

// 核心构成。Apple Silicon 才报 P/E 核，其他平台不能假装知道。
function cpuCoreText(status) {
  if (!status) return '';
  var ceiling = Math.max(1, status.availableParallelism || 1);
  if (status.appleSilicon && status.performanceCpus && status.efficiencyCpus) {
    return (status.physicalCpus || ceiling) + localizeUiText(' 核 CPU（')
      + status.performanceCpus + localizeUiText(' 性能核 + ') + status.efficiencyCpus + localizeUiText(' 能效核）');
  }
  if (status.physicalCpus && status.logicalCpus > status.physicalCpus) {
    return status.physicalCpus + localizeUiText(' 个物理核心 · ') + status.logicalCpus + localizeUiText(' 个逻辑处理器');
  }
  if (status.logicalCpus) return status.logicalCpus + localizeUiText(' 个逻辑处理器');
  return localizeUiText('最多 ') + ceiling + localizeUiText(' 份并行计算');
}

function cpuSliderLabel() {
  var ceiling = cpuCeiling();
  if (!cpuStatus) return '';
  var configured = cpuStatus.configuredLimit;
  var limit = Math.min(Math.max(1, cpuStatus.effectiveLimit || 1), ceiling);
  if (configured === null || configured === undefined) return localizeUiText('自动（') + limit + '）';
  return limit + ' / ' + ceiling + (limit >= ceiling ? localizeUiText('（全部）') : '');
}

function renderCpuSetting(status) {
  if (status) cpuStatus = status;
  var device = document.getElementById('cpuDevice');
  if (device) device.textContent = cpuDeviceText(cpuStatus);
  var cores = document.getElementById('cpuCoreInfo');
  if (cores) cores.textContent = cpuCoreText(cpuStatus);
  // 「系统会自动在性能核与能效核之间调度任务」只对大小核架构成立。
  var note = document.getElementById('cpuSchedulingNote');
  if (note) note.style.display = cpuStatus && cpuStatus.appleSilicon ? '' : 'none';
  var slider = document.getElementById('cpuLimitSlider');
  if (slider && cpuStatus) {
    var ceiling = cpuCeiling();
    slider.min = '1';
    slider.max = String(ceiling);
    slider.disabled = ceiling <= 1;
    var configured = cpuStatus.configuredLimit;
    slider.value = String(Math.min(Math.max(1,
      configured == null ? cpuStatus.effectiveLimit : configured), ceiling));
  }
  var label = document.getElementById('cpuLimitValue');
  if (label) label.textContent = cpuSliderLabel();
  var autoBtn = document.getElementById('cpuLimitAuto');
  if (autoBtn) {
    var isAuto = !cpuStatus || cpuStatus.configuredLimit == null;
    autoBtn.classList.toggle('active', isAuto);
    autoBtn.disabled = isAuto;
  }
}

async function loadCpuSetting() {
  try {
    renderCpuSetting(await invoke('get_cpu_info'));
  } catch (error) {
    console.error('CPU info load failed:', error);
  }
}

async function saveCpuThreadLimit(limit) {
  try {
    renderCpuSetting(await invoke('set_cpu_thread_limit', { limit: limit }));
    updateQueueSummary();
    showToast(limit == null
      ? localizeUiText('CPU 上限改为自动（') + cpuStatus.effectiveLimit + '）'
      : localizeUiText('CPU 上限改为 ') + cpuStatus.effectiveLimit + ' / ' + cpuCeiling());
  } catch (error) {
    showToast(localizeUiText('设置失败: ') + (error.message || error));
    // 失败就回到后端那份真值，不能把预览值留在滑杆上骗用户。
    await loadCpuSetting();
  }
}

(function wireCpuLimitControls() {
  var slider = document.getElementById('cpuLimitSlider');
  if (slider) {
    slider.addEventListener('input', function() {
      // 拖动过程中只改本地预览，松手（change）才写盘 + 下发给调度器。
      if (!cpuStatus) return;
      var picked = Math.min(Math.max(1, parseInt(slider.value, 10) || 1), cpuCeiling());
      cpuStatus = Object.assign({}, cpuStatus, { configuredLimit: picked, effectiveLimit: picked });
      var label = document.getElementById('cpuLimitValue');
      if (label) label.textContent = cpuSliderLabel();
      var autoBtn = document.getElementById('cpuLimitAuto');
      if (autoBtn) { autoBtn.classList.toggle('active', false); autoBtn.disabled = false; }
    });
    slider.addEventListener('change', function() {
      saveCpuThreadLimit(parseInt(slider.value, 10));
    });
  }
  var autoBtn = document.getElementById('cpuLimitAuto');
  if (autoBtn) autoBtn.addEventListener('click', function() { saveCpuThreadLimit(null); });
})();

// ─── Comparison（独立原生窗口）─────────────────────────────────
// 对比视图已迁移到独立窗口 compare.html + compare_window.js：
// 可自由缩放（可大于主窗口）、拥有自己的红绿灯，避免误关主窗口。
// 本侧只负责组装载荷、打开/聚焦窗口，并把结果集变化推送给该窗口。

function comparableResults() {
  return queueResults().filter(function(r) { return r && r.success; });
}

function openCompare(result) {
  const okResults = comparableResults();
  if (!okResults.length) {
    showToast(localizeUiText('没有可对比的结果'));
    return;
  }
  let index = okResults.findIndex(function(r) { return r.file === result.file; });
  if (index < 0) index = 0;
  invoke('open_compare_window', { payload: { results: okResults, index: index } })
    .catch(function(err) {
      showToast(localizeUiText('打开对比窗口失败: ') + (err.message || err));
    });
}

function openCompareByFile(filePath) {
  const item = queueItems.get(filePath);
  if (item && item.result) openCompare(item.result);
}

// 结果集变化（恢复/清空/压缩完成等）时推送快照，对比窗口据此刷新或置空
function emitCompareResultsChanged() {
  try {
    window.__TAURI__.event.emit('compare-results-changed', { results: comparableResults() });
  } catch (_) {}
}

listen('compare-recompressed', function(event) {
  const payload = event.payload || {};
  const updated = payload.result;
  if (!payload.filePath || !updated) return;
  const item = queueItems.get(payload.filePath);
  if (!item || !item.result) return;
  const existing = item.result;
  item.result = Object.assign({}, existing, updated, {
    // compress_single writes a temporary preview. Keep the real output
    // and backup metadata so Restore still targets the original result.
    outputPath: existing.outputPath,
    backupPath: existing.backupPath,
    outputMode: existing.outputMode,
  });
});

listen('compare-restored', function(event) {
  const filePath = event.payload && event.payload.filePath;
  if (!filePath) return;
  afterRestore(filePath);
  showToast(localizeUiText('已恢复原图: ') + basename(filePath));
});

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Escape') return;
  var systemInfoPanel = document.getElementById('systemInfoPanel');
  if (systemInfoPanel && systemInfoPanel.style.display !== 'none') {
    closeSystemConversionInfo();
    return;
  }
  // Esc = 离开历史/设置页，回到主视图；压缩任务不受影响。
  if (currentView !== 'main') showView('main');
});

function showToast(message) {
  const existing = document.querySelector('.toast');
  if (existing) existing.remove();

  const toast = document.createElement('div');
  toast.className = 'toast';
  toast.textContent = message;
  document.body.appendChild(toast);

  setTimeout(() => {
    toast.style.opacity = '0';
    toast.style.transition = 'opacity 0.3s';
    setTimeout(() => toast.remove(), 300);
  }, 2500);
}

/// 更新面板在设置页里，但启动时的静默检查可能先于用户进设置页就发现了新版本，
/// 所以元素一律先按产物线摆好，谁先来谁写。
/// App Store 版不加载 updater 插件 —— 那一行直接不出现，改说一句实话，
/// 而不是留一个按下去只会失败的按钮。
function initUpdatePanel() {
  var version = document.getElementById('updateVersion');
  // 版本号是异步取的：没取到之前宁可留 HTML 里的「—」，别显示半截「 Direct」。
  if (version && window.appVersion) version.textContent = 'v' + window.appVersion + ' ' + BUILD_VARIANT;
  // 「关于」小节里的版本行（从标题栏气泡搬进设置页）
  var aboutVersion = document.getElementById('aboutVersion');
  if (aboutVersion && window.appVersion) aboutVersion.textContent = 'v' + window.appVersion + ' ' + BUILD_VARIANT;
  var isDirect = BUILD_VARIANT === 'Direct';
  var checkRow = document.getElementById('updateCheckRow');
  var directNote = document.getElementById('updateDirectNote');
  var appStoreNote = document.getElementById('updateAppStoreNote');
  if (checkRow) checkRow.style.display = isDirect ? '' : 'none';
  if (directNote) directNote.style.display = isDirect ? '' : 'none';
  if (appStoreNote) appStoreNote.style.display = isDirect ? 'none' : '';
}

function getUpdateStatusEl() {
  return document.getElementById('updateStatus');
}

async function manualCheckUpdate() {
  var btn = document.getElementById('updateCheckBtn');
  if (!btn || btn.dataset.downloading === '1') return;
  var statusEl = getUpdateStatusEl();
  btn.disabled = true;
  btn.textContent = localizeUiText('检查中');
  if (statusEl) { statusEl.textContent = ''; statusEl.classList.remove('has-update'); }
  try {
    const update = await invoke('check_for_update');
    if (update) {
      btn.textContent = localizeUiText('立即更新');
      btn.disabled = false;
      if (statusEl) { statusEl.textContent = 'v' + update.version + localizeUiText(' 可用'); statusEl.classList.add('has-update'); }
      btn.onclick = function() { startUpdateDownload(btn, statusEl, update.version); };
    } else {
      btn.textContent = localizeUiText('检查更新');
      btn.disabled = false;
      if (statusEl) statusEl.textContent = localizeUiText('已是最新版本');
    }
  } catch (error) {
    btn.textContent = localizeUiText('检查更新');
    btn.disabled = false;
    if (statusEl) statusEl.textContent = localizeUiText('检查失败');
  }
}

/// 更新下载的进度监听是**全局唯一**的一份：下载任务跑在后端，页面重载
/// （「还原默认设置」走 location.reload）不会中断它——重载后第一条进度
/// 事件就把进度条"接管"回来；后端有单飞守卫，并发下载不存在，所以进度条
/// 永远只有一条数据源。取消 / 失败收摊之后置 true，避免迟到的进度事件
/// 把刚收起的进度条又弹回来。
var updateRejoinSuppressed = false;

function applyUpdateProgress(pct) {
  var row = document.getElementById('updateDownloadRow');
  var text = document.getElementById('updateProgressText');
  var fill = document.getElementById('updateProgressFill');
  var tbBar = document.getElementById('titlebarProgress');
  var btn = document.getElementById('updateCheckBtn');
  if (row && pct > 0 && row.style.display === 'none' && !updateRejoinSuppressed) {
    // 页面重载后进度 UI 丢了，但后端还在下载：进度事件一到就恢复"下载中"形态
    row.style.display = '';
    if (btn && btn.dataset.downloading !== '1') {
      btn.dataset.downloading = '1';
      btn.disabled = true;
    }
  }
  if (row && row.style.display === 'none') return; // 已收起的进度条不再被写入
  if (tbBar) tbBar.style.width = pct + '%';
  if (fill) fill.style.width = pct + '%';
  if (text) text.textContent = localizeUiText('下载中 ') + pct + '%';
}

function startUpdateDownload(btn, statusEl, version) {
  btn.dataset.downloading = '1';
  updateRejoinSuppressed = false;
  var row = document.getElementById('updateDownloadRow');
  var text = document.getElementById('updateProgressText');
  var fill = document.getElementById('updateProgressFill');
  // 标题栏那根窗口级进度条照旧推：下载中途切回队列页也看得见动静。
  var tbBar = document.getElementById('titlebarProgress');
  if (row) row.style.display = '';
  if (fill) fill.style.width = '0%';
  if (tbBar) tbBar.style.width = '0%';
  if (text) text.textContent = localizeUiText('下载中 0%');
  if (statusEl) statusEl.textContent = '';
  btn.disabled = true;

  invoke('install_update')
    .then(function() {
      if (text) text.textContent = localizeUiText('安装中…');
      if (fill) fill.style.width = '100%';
      if (tbBar) tbBar.style.width = '100%';
    })
    .catch(function(err) {
      if (String(err).indexOf('进行中') >= 0) {
        // 后端单飞守卫拒绝并发下载：已有一个下载在跑（进度 UI 曾被页面重载
        // 丢掉）。保持"下载中"形态即可——仍在播报的进度事件会立刻把进度条
        // 接管回来，装完之后应用自行重启。
        if (text) text.textContent = localizeUiText('下载中') + '…';
        return;
      }
      if (btn.dataset.downloading !== '1') return;
      btn.dataset.downloading = '';
      endUpdateDownload();
      btn.disabled = false;
      btn.textContent = localizeUiText('立即更新');
      var cancelled = String(err).indexOf('取消') >= 0;
      if (statusEl) statusEl.textContent = cancelled ? 'v' + version + localizeUiText(' 可用') : localizeUiText('更新失败');
    });
}

/// 下载停下来了（取消、失败都算）：面板那一行收起来，窗口进度条归零。
/// 成功安装时不调用它 —— 那一刻界面正等着被替换掉。
/// 同时打开"抑制接管"：迟到的进度事件不许把刚收起的进度条弹回来。
function endUpdateDownload() {
  updateRejoinSuppressed = true;
  var row = document.getElementById('updateDownloadRow');
  var fill = document.getElementById('updateProgressFill');
  var tbBar = document.getElementById('titlebarProgress');
  if (row) row.style.display = 'none';
  if (fill) fill.style.width = '0%';
  if (tbBar) tbBar.style.width = '0%';
}

function cancelUpdateDownload() {
  invoke('cancel_update').catch(function(){});
  endUpdateDownload();
  var btn = document.getElementById('updateCheckBtn');
  var statusEl = getUpdateStatusEl();
  if (btn && btn.dataset.downloading === '1') {
    btn.dataset.downloading = '';
    btn.disabled = false;
    btn.textContent = localizeUiText('立即更新');
    if (statusEl) statusEl.textContent = localizeUiText('已取消');
  }
}

async function checkDirectUpdate() {
  if (BUILD_VARIANT !== 'Direct' || window.updateCheckStarted) return;
  window.updateCheckStarted = true;
  try {
    const update = await invoke('check_for_update');
    if (!update) return;
    var btn = document.getElementById('updateCheckBtn');
    if (btn) {
      var statusEl = getUpdateStatusEl();
      btn.textContent = localizeUiText('立即更新');
      if (statusEl) { statusEl.textContent = 'v' + update.version + localizeUiText(' 可用'); statusEl.classList.add('has-update'); }
      btn.onclick = function() { startUpdateDownload(btn, statusEl, update.version); };
    }
  } catch (error) {
    console.warn(localizeUiText('在线更新检查失败:'), error);
  }
}
/// 落盘的是「意图 + 后果」：用户选的是要多小、文件落在哪。参数永远是 SizqoIntents
/// 从意图现编译的（core/intent-model.md）—— 把编译结果存下来，等于让上一轮的残留
/// 参数冒充这一轮的选择。
function saveCompressSettings() {
  var auto = document.getElementById('autoCompress');
  var data = {
    version: 2,
    imageIntent: imageIntent,
    videoIntent: videoIntent,
    autoCompress: !!(auto && auto.checked),
    outputMode: currentOutputMode(),
    outputSuffix: outputSuffixInput ? outputSuffixInput.value : '',
  };
  try { localStorage.setItem('sizqo-settings', JSON.stringify(data)); } catch (e) {}
}

/// 读回来的意图 id 必须在**当前平台**的可选列表里：systemConvert 只在 macOS 存在，
/// 在别的系统恢复它就等于一个用户看不见、却驱动着整条请求的意图。
function loadCompressSettings() {
  var data = null;
  try { data = JSON.parse(localStorage.getItem('sizqo-settings') || 'null'); } catch (e) {}
  if (!data || typeof data !== 'object') return;

  var savedImage = data.imageIntent;
  if (savedImage && typeof savedImage === 'object') {
    var allowedImages = window.SizqoIntents.imageIntentList(isMacOS());
    if (allowedImages.some(function (intent) { return intent.id === savedImage.intent; })) {
      imageIntent.intent = savedImage.intent;
    }
    ['quality', 'outputFormat', 'systemOutputFormat', 'systemImageSize', 'preserveMetadata',
     'maxDimensionPx', 'maxOutputBytes'].forEach(function (key) {
      if (savedImage[key] !== undefined) imageIntent[key] = savedImage[key];
    });
  }
  var savedVideo = data.videoIntent;
  if (savedVideo && typeof savedVideo === 'object') {
    if (window.SizqoIntents.VIDEO_INTENTS.some(function (intent) { return intent.id === savedVideo.intent; })) {
      videoIntent.intent = savedVideo.intent;
    }
    var miB = parseFloat(savedVideo.targetMiB);
    if (Number.isFinite(miB) && miB > 0) videoIntent.targetMiB = miB;
  }

  var auto = document.getElementById('autoCompress');
  if (auto) auto.checked = !!data.autoCompress;
  if (['replace', 'suffix', 'folder'].indexOf(data.outputMode) >= 0) {
    var radio = document.querySelector('input[name="outputMode"][value="' + data.outputMode + '"]');
    if (radio) radio.checked = true;
  }
  if (outputSuffixInput && typeof data.outputSuffix === 'string') outputSuffixInput.value = data.outputSuffix;

  // 意图回来了，控件得跟着铺成它真正会跑的那套值。
  if (imageIntent.intent === 'custom' || imageIntent.intent === 'systemConvert') paintImageStateControls();
  else paintImageRawControls(imageRequestFromIntent());
}

/// 设置页的「还原默认设置」清掉两套落盘：只清图片那份会留下一个
/// "看着恢复了、视频参数却还是上次的"的半新状态。
function resetSettings() {
  try { localStorage.removeItem('sizqo-settings'); } catch (e) {}
  try { localStorage.removeItem('sizqo-video-settings'); } catch (e) {}
  location.reload();
}

// Init
(function() {
  // 后端是阶段状态的权威：暂停 / 继续 / 停止的真实结果由它播报，
  // 前端点按钮时只是先乐观地画一遍（失败会回滚）。
  listen('compression-state-change', function(event) {
    applyCompressionStateEvent(event.payload);
  }).catch(function() {});
  // 更新进度全局唯一监听：重载后靠它把仍在跑的下载"接管"回界面
  // （详见 applyUpdateProgress 的说明）。
  listen('update-progress', function(event) {
    applyUpdateProgress(event.payload || 0);
  }).catch(function() {});

  loadCompressSettings();
  loadVideoSettings();
  // 自定义之外，视频控件只是意图的展开显示：存回来的意图要把参数铺成这一套。
  if (videoIntent.intent !== 'videoCustom') applyVideoOptions(videoRequestFromIntent().options);
  bindIntentControls();
  bindVideoSettings();

  var auto = document.getElementById('autoCompress');
  if (auto) auto.addEventListener('change', function() {
    saveCompressSettings();
    updateAutoCompressLabel();
  });
  if (outputSuffixInput) outputSuffixInput.addEventListener('input', function() {
    saveCompressSettings();
    renderDetailPane();
  });

  renderVideoCapabilities();
  loadVideoCapabilities();
  // 队列摘要里的「CPU 4/10」需要在进入设置页之前就拿到，所以启动即检测一次。
  loadCpuSetting().then(function() { updateQueueSummary(); }).catch(function() {});
  // 启动一次画齐：chips、目标行、高级列、输出方式、运行条、详情列。
  updateMediaWorkspace();
  updateOutputModeControls();
  updateQueueSummary();
  renderDetailPane();

  invoke('get_app_version').then(v => {
    window.appVersion = v;
    initUpdatePanel();
    setTimeout(checkDirectUpdate, 1500);
  }).catch(() => {});
})();
