// CFG-01：Intent / Preset / Options 三态一致（跑真代码，不是读源码猜）。
//
// 任务书点名的四类缺陷，每类都要有回归证据：
//   ① 首次启动：参数 = 默认意图编译结果，预设下拉不再预选 balanced
//      （旧版 chip 显示 CRF28/MP4、预设框却指 balanced/auto-abr）；
//   ② frameRate 从数值改回 null 必须真的清空控件 —— 旧版清不掉，
//      陈旧 fps 从控件读回漏进下一次提交；
//   ③ 应用预设后必须同步预设 ID 和意图状态 —— 参数不再匹配任何意图时
//      如实切到自定义；切回意图时预设选择跟着真实参数走；
//   ④ 保存后重启结果一致：读档铺回的参数就是控件里的参数。
// 另加一条跨语言契约：前端 fallback 预设名与后端 presets.rs 的 id 同表。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { slice } = require('./app-slices.cjs');

const root = path.join(__dirname, '..');
const read = relative => fs.readFileSync(path.join(root, relative), 'utf8');

// ── 跨语言契约：fallback 标签表的键就是后端预设 id ──────────────────────
const rustPresets = read('src-tauri/src/video/presets.rs');
const settingsSource = read('frontend/video-settings.js');
const backendIds = [...rustPresets.matchAll(/\("([a-z]+)","[^"]*"/g)].map(m => m[1]);
assert.ok(backendIds.length >= 5, `后端预设 id 应至少 5 个，实际 ${JSON.stringify(backendIds)}`);
const labelTable = settingsSource.slice(settingsSource.indexOf('PRESET_LABELS'), settingsSource.indexOf('function normalizeOptions'));
for (const id of backendIds) {
  assert.ok(labelTable.includes(`${id}:`), `前端 PRESET_LABELS 缺少后端预设 id "${id}"（CFG-01 三态对齐）`);
}

// ── 通用沙盒 ────────────────────────────────────────────────────────────
function makeElement(value) {
  return { value: value === undefined ? '' : value, checked: false, textContent: '', title: '', hidden: false, disabled: false,
    classList: { toggle() {}, add() {}, remove() {} },
    appendChild() {}, addEventListener() {}, options: [], setAttribute() {}, getAttribute: () => null };
}

function makeContext() {
  const byId = {};
  const storage = new Map();
  const context = vm.createContext({
    console, Object, Number, String, Array, Boolean, JSON, Math, Promise,
    document: {
      getElementById: id => byId[id] || (byId[id] = makeElement()),
      querySelectorAll: () => [],
      querySelector: () => null,
      createElement: () => makeElement(),
    },
    localStorage: {
      getItem: key => storage.has(key) ? storage.get(key) : null,
      setItem: (key, value) => storage.set(key, String(value)),
      removeItem: key => storage.delete(key),
    },
    localizeUiText: text => text,
    renderVideoQueueRows() {}, updateVideoOutputModeControls() {}, updateVideoSettingsSummary() {},
    renderVideoCapabilities() {}, renderIntentBar() {}, renderDetailPane() {},
    setVideoOutputModeRadio() {}, currentVideoOutputMode: () => 'suffix',
    intentNoteFor: () => '',
    videoOutputDir: null,
    videoIntent: { intent: 'videoSmall', targetMiB: 100 },
    videoPresets: [],
    selectedVideoPresetId: '',
    videoCapabilities: { available: true, backend: 'ffmpeg', encoders: [], loading: false },
  });
  vm.runInContext(read('frontend/video-settings.js'), context);
  vm.runInContext(read('frontend/intents.js'), context);
  // app.js 里统一走 window.SizqoVideo / window.SizqoIntents：沙盒里把 window 指回全局。
  vm.runInContext('window = globalThis', context);
  return { context, byId, storage };
}

function installVideoConfig(context) {
  // setVideoIntent 在文件前段，与 match 帮手同段；其余配置函数连成一段。
  vm.runInContext(slice('function setVideoIntent(', '/// 把意图编译出来的参数写回原始控件'), context);
  vm.runInContext(slice('function videoRequestFromIntent(', 'function processingActionText('), context);
  vm.runInContext(slice('function videoOptionsFromControls(', 'function renderVideoCapabilities('), context);
}

// ── ① 首次启动 ──────────────────────────────────────────────────────────
{
  const { context } = makeContext();
  installVideoConfig(context);
  const loaded = vm.runInContext('loadVideoSettings()', context);
  assert.equal(loaded, false, '没有存档时 loadVideoSettings 返回 false');
  assert.equal(context.selectedVideoPresetId, '', '首次启动不预选 balanced（预设框与意图条必须说同一句话）');
  assert.equal(context.currentVideoOptions.mode, 'crf');
  assert.equal(context.currentVideoOptions.crf, 28, '首次启动的参数就是默认意图（videoSmall）的编译结果');
  assert.equal(context.currentVideoOptions.container, 'mp4');
}

// ── ② frameRate null 清空控件 ───────────────────────────────────────────
{
  const { context, byId } = makeContext();
  installVideoConfig(context);
  vm.runInContext('applyVideoOptions({ frameRate: 30 })', context);
  assert.equal(byId.videoFrameRate.value, '30', '数值照铺');
  vm.runInContext('applyVideoOptions({ frameRate: null })', context);
  assert.equal(byId.videoFrameRate.value, '', 'null 必须清空控件（旧版跳过 null，陈旧 fps 残留）');
  assert.equal(context.currentVideoOptions.frameRate, null,
    '读回的状态也必须是 null —— syncVideoSettingsControls 不许从控件里捡回旧值');
}

// ── ③ 应用预设同步意图状态；切意图同步预设选择 ──────────────────────────
{
  const { context } = makeContext();
  installVideoConfig(context);
  // 一个与任何意图都不匹配的预设（auto/abr）。
  context.videoPresets = [
    { id: 'balanced', name: '均衡', available: true,
      options: { encoder: 'auto', mode: 'abr', bitrateKbps: 4000, crf: null, container: 'mp4', audioPolicy: 'copy' } },
    // 一个恰好等于 videoTarget 编译结果的预设（targetMiB 100）。
    { id: 'target', name: '目标体积', available: true,
      options: { encoder: 'auto', mode: 'targetSize', targetMiB: 100, container: 'mp4', audioPolicy: 'copy', subtitlePolicy: 'copy', metadataPolicy: 'preserve' } },
  ];
  vm.runInContext('saveVideoSettings()', context);
  const applied = vm.runInContext('applyVideoPreset("balanced")', context);
  assert.equal(applied, true);
  assert.equal(context.selectedVideoPresetId, 'balanced', '预设 ID 必须同步');
  assert.equal(context.videoIntent.intent, 'videoCustom',
    'balanced（auto/abr）不等于任何意图编译结果 —— 意图条必须如实切到自定义，不能继续宣称 videoSmall');
  // 自定义意图下，摘要/提交读的就是控件参数（同一份快照）。
  const compiled = vm.runInContext('videoRequestFromIntent()', context);
  assert.equal(compiled.intent, 'videoCustom');
  assert.equal(compiled.options.mode, 'abr', '自定义意图编译出来的就是控件里的参数');
  assert.equal(compiled.options.bitrateKbps, 4000);

  // 切回 videoSmall：预设选择清空（编译结果不等于 balanced）。
  vm.runInContext('setVideoIntent("videoSmall")', context);
  assert.equal(context.videoIntent.intent, 'videoSmall');
  assert.equal(context.selectedVideoPresetId, '', '切意图后预设选择跟着真实参数走，不再留着旧预设的名字');
  assert.equal(context.currentVideoOptions.mode, 'crf', '切意图把控件摆成意图参数');

  // 与意图完全一致的预设 → 意图状态同步回 videoTarget（editableTarget 照活）。
  vm.runInContext('applyVideoPreset("target")', context);
  assert.equal(context.videoIntent.intent, 'videoTarget', '参数恰好等于 videoTarget 编译结果时回到那个意图');
  assert.equal(context.selectedVideoPresetId, 'target');
}

// ── ④ 保存 → 重启一致 ───────────────────────────────────────────────────
{
  const { context, storage } = makeContext();
  installVideoConfig(context);
  vm.runInContext('applyVideoOptions({ mode: "crf", crf: 23, frameRate: 24, container: "mkv", audioPolicy: "aac" })', context);
  vm.runInContext('saveVideoSettings()', context);
  assert.ok(storage.has('sizqo-video-settings'), '设置已落盘');

  // 新会话（重启）：同样的沙盒，从 localStorage 读档。
  const reboot = makeContext();
  // 把上一个会话的存档搬进新沙盒。
  reboot.storage.set('sizqo-video-settings', storage.get('sizqo-video-settings'));
  installVideoConfig(reboot.context);
  const loaded = vm.runInContext('loadVideoSettings()', reboot.context);
  assert.equal(loaded, true);
  assert.equal(reboot.context.currentVideoOptions.crf, 23, '重启后 CRF 与保存一致');
  assert.equal(reboot.context.currentVideoOptions.frameRate, 24, '重启后帧率与保存一致');
  assert.equal(reboot.context.currentVideoOptions.container, 'mkv');
  assert.equal(reboot.byId.videoFrameRate.value, '24', '控件里铺的就是保存的值');
  // 保存时的预设选择也回来。
  assert.equal(reboot.context.selectedVideoPresetId, '', '没选预设就不预选');
}

// ── ⑤ 具名场景：保存 balanced 后重启（任务书 3.1 回归清单）────────────
{
  const presets = [
    // 与后端 presets.rs 的 balanced 同参：auto/abr。
    { id: 'balanced', name: '均衡', available: true,
      options: { encoder: 'auto', mode: 'abr', bitrateKbps: 4000, crf: null, container: 'mp4', audioPolicy: 'copy' } },
    // 与后端 quality 同参：libx265 / CRF 28。
    { id: 'quality', name: '高质量压缩', available: true,
      options: { encoder: 'libx265', mode: 'crf', crf: 28, container: 'mp4', audioPolicy: 'copy' } },
  ];
  const first = makeContext();
  first.context.videoPresets = presets;
  installVideoConfig(first.context);
  assert.equal(vm.runInContext('applyVideoPreset("balanced")', first.context), true, 'balanced 预设必须可用');
  assert.equal(first.context.videoIntent.intent, 'videoCustom', 'balanced（auto/abr）不匹配任何意图 —— 如实切自定义');
  vm.runInContext('saveVideoSettings()', first.context);
  const saved = first.storage.get('sizqo-video-settings');
  assert.ok(saved, 'balanced 存档已落盘');

  // 重启：同样的存储；意图按 loadCompressSettings 的恢复顺序先回来。
  const reboot = makeContext();
  reboot.context.videoPresets = presets;
  reboot.context.videoIntent = { intent: 'videoCustom', targetMiB: 100 };
  reboot.storage.set('sizqo-video-settings', saved);
  installVideoConfig(reboot.context);
  assert.equal(vm.runInContext('loadVideoSettings()', reboot.context), true);
  assert.equal(reboot.context.selectedVideoPresetId, 'balanced', '重启后预设选择恢复');
  assert.equal(reboot.context.currentVideoOptions.mode, 'abr', '重启后 mode 与保存一致');
  // 启动尾部的意图重铺对 videoCustom 是空操作 —— 存档参数不许被意图默认值覆盖。
  vm.runInContext(
    "if (videoIntent.intent !== 'videoCustom') applyVideoOptions(videoRequestFromIntent().options);",
    reboot.context);
  assert.equal(reboot.context.currentVideoOptions.mode, 'abr', 'init 重铺后参数仍是保存的那套');
  // 三方一致：意图编译（自定义=控件）、控件、请求同源。
  const request = vm.runInContext('videoRequestFromIntent()', reboot.context);
  assert.equal(request.intent, 'videoCustom');
  assert.equal(request.options.mode, 'abr');
  assert.equal(String(request.options.bitrateKbps), String(reboot.context.currentVideoOptions.bitrateKbps));
  console.log('PASS: 保存 balanced 后重启 —— 预设选择、参数、请求三方一致');
}

// ── ⑥ 具名场景：保存 quality 后重启 ────────────────────────────────────
{
  const presets = [
    { id: 'quality', name: '高质量压缩', available: true,
      options: { encoder: 'libx265', mode: 'crf', crf: 28, container: 'mp4', audioPolicy: 'copy' } },
  ];
  const first = makeContext();
  first.context.videoPresets = presets;
  installVideoConfig(first.context);
  assert.equal(vm.runInContext('applyVideoPreset("quality")', first.context), true);
  assert.equal(first.context.videoIntent.intent, 'videoCustom');
  assert.equal(first.context.currentVideoOptions.encoder, 'libx265');
  assert.equal(first.context.currentVideoOptions.crf, 28);
  vm.runInContext('saveVideoSettings()', first.context);
  const saved = first.storage.get('sizqo-video-settings');

  const reboot = makeContext();
  reboot.context.videoPresets = presets;
  reboot.context.videoIntent = { intent: 'videoCustom', targetMiB: 100 };
  reboot.storage.set('sizqo-video-settings', saved);
  installVideoConfig(reboot.context);
  assert.equal(vm.runInContext('loadVideoSettings()', reboot.context), true);
  assert.equal(reboot.context.selectedVideoPresetId, 'quality', '重启后 quality 预设恢复');
  assert.equal(reboot.context.currentVideoOptions.encoder, 'libx265', '重启后编码器与保存一致');
  assert.equal(reboot.context.currentVideoOptions.crf, 28, '重启后 CRF 与保存一致');
  vm.runInContext(
    "if (videoIntent.intent !== 'videoCustom') applyVideoOptions(videoRequestFromIntent().options);",
    reboot.context);
  assert.equal(reboot.context.currentVideoOptions.crf, 28, 'init 重铺（videoCustom 下跳过）不丢参数');
  const request = vm.runInContext('videoRequestFromIntent()', reboot.context);
  assert.equal(request.options.encoder, 'libx265');
  assert.equal(request.options.crf, 28);
  console.log('PASS: 保存 quality 后重启 —— 编码器/CRF/预设选择原样恢复');
}

console.log('PASS: CFG-01 Intent/Preset/Options 三态一致（首启、null 清空、预设同步意图、重启一致、跨语言预设 id）');
