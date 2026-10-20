// 输出方式必须按媒体分开：图片与视频各有一组“覆盖原文件 / 加后缀 / 输出到目录”，
// 各自落盘、互不覆盖。旧版把图片那一组放在两组意图下面，看起来像全局开关，
// 而视频真正的输出方式藏在「更多参数」里，默认还是加后缀 —— 视觉与行为对不上。
//
// 两条不变量：
//   ① 两组开关分属各自的意图组，视频那份不再出现在高级参数里；
//   ② 输出方式是正交项：视频切意图只换编码参数，不重置用户单独选的落盘位置。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { source, slice } = require('./app-slices.cjs');
const video = require('../frontend/video-settings.js');
const intents = require('../frontend/intents.js');

const root = path.join(__dirname, '..');
const html = fs.readFileSync(path.join(root, 'frontend', 'index.html'), 'utf8');

// ── ① 结构：一组在图片组、一组在视频组，高级参数里不再有输出方式 ──────
const imageGroup = html.slice(html.indexOf('id="imageIntentGroup"'), html.indexOf('id="videoIntentGroup"'));
const videoGroup = html.slice(html.indexOf('id="videoIntentGroup"'), html.indexOf('id="intentTargetRow"'));
assert.ok(imageGroup.includes('name="outputMode"'), '图片组必须有独立的输出方式开关');
assert.ok(!imageGroup.includes('name="videoOutputMode"'), '视频开关不许混进图片组');
assert.ok(videoGroup.includes('name="videoOutputMode"'), '视频组必须有独立的输出方式开关');
assert.ok(!videoGroup.includes('name="outputMode"'), '图片开关不许混进视频组');
assert.match(imageGroup, /name="outputMode" value="replace" checked/,
  '图片默认覆盖原文件（与 Swift 默认档一致）');
assert.match(videoGroup, /name="videoOutputMode" value="suffix" checked/,
  '视频默认同目录加后缀，与视频引擎的默认值一致');

const advanced = html.slice(html.indexOf('id="videoAdvancedRows"'), html.indexOf('</details>'));
for (const id of ['videoOutputMode', 'videoOutputSuffix', 'videoOutputDirDisplay']) {
  assert.ok(!advanced.includes(id), `视频输出控件 ${id} 不该再留在「更多参数」里`);
}
assert.ok(!source.includes("getElementById('videoOutputMode')"),
  '视频输出方式已改为单选组，不许再按旧下拉框 id 读取');
assert.ok(!source.includes("setValue('videoOutputMode'"),
  '视频输出方式已改为单选组，不许再按旧下拉框 id 回填');
assert.match(source, /var outputMode = currentVideoOutputMode\(\);/,
  'videoOptionsFromControls 必须从视频单选组读输出方式');

// ── ② 行为：两套开关互不影响，切意图保留输出设置 ──────────────────────
function field(value, checked) {
  return { value: value === undefined || value === null ? '' : String(value), checked: !!checked };
}
const fields = {
  videoEncoder: field('auto'), videoMode: field('crf'), videoCrf: field(28),
  videoBitrate: field(4000), videoTargetMiBInput: field(100), videoSpeed: field('medium'),
  videoResolution: field(0), videoResolutionCustom: field(1080), videoFrameRate: field(''),
  videoAudioPolicy: field('copy'), videoSubtitlePolicy: field('copy'), videoMetadataPolicy: field('preserve'),
  videoOutputSuffix: field('_compressed'), videoAllowLarger: field('', false),
  videoContainer: field('mp4'), videoThreads: field(0),
};
const radios = {
  image: { value: 'replace' },
  video: { value: 'suffix' },
};
const context = vm.createContext({
  console, parseInt, parseFloat, Number, String, Math, Object, Array,
  window: { SizqoVideo: video, SizqoIntents: intents },
  document: {
    getElementById: id => fields[id] || null,
    querySelector: selector => selector.includes('videoOutputMode') ? radios.video
      : (selector.includes('outputMode') ? radios.image : null),
    querySelectorAll: () => [],
  },
  videoIntent: { intent: 'videoSmall', targetMiB: 100 },
  currentVideoOptions: video.normalizeOptions(),
  videoOutputDir: null,
});
vm.runInContext(slice('function currentVideoOutputMode(', 'function getResultOutputSuffix('), context);
vm.runInContext(slice('function videoOptionsFromControls(', 'function applyVideoOptions('), context);
vm.runInContext(slice('function videoRequestFromIntent(', 'function processingActionText('), context);

assert.equal(context.videoOptionsFromControls().outputMode, 'suffix', '视频默认加后缀');
radios.video.value = 'replace';
assert.equal(context.videoOptionsFromControls().outputMode, 'replace', '视频开关独立生效');
assert.equal(context.videoOptionsFromControls().outputDir, null, '非目录模式不携带输出目录');

context.videoOutputDir = '/tmp/sizqo-video-out';
radios.video.value = 'directory';
const directoryOptions = context.videoOptionsFromControls();
assert.equal(directoryOptions.outputMode, 'directory');
assert.equal(directoryOptions.outputDir, '/tmp/sizqo-video-out', '目录模式使用视频自己的目录');

radios.image.value = 'folder';
context.videoOutputDir = '/tmp/other';
assert.equal(context.videoOptionsFromControls().outputDir, '/tmp/other',
  '改图片开关不影响视频选项');
assert.equal(context.videoOptionsFromControls().outputMode, 'directory');

// 非自定义意图：编码参数来自意图，输出参数仍然来自用户单独配置。
context.currentVideoOptions = video.normalizeOptions({
  outputMode: 'replace', outputDir: '/tmp/kept', outputSuffix: '_v',
});
const compiled = context.videoRequestFromIntent();
assert.equal(compiled.intent, 'videoSmall');
assert.equal(compiled.options.crf, 28, '意图仍然决定编码参数');
assert.equal(compiled.options.outputMode, 'replace', '切意图不许重置视频输出方式');
assert.equal(compiled.options.outputDir, '/tmp/kept', '切意图不许丢掉视频输出目录');
assert.equal(compiled.options.outputSuffix, '_v', '切意图不许重置视频后缀');

console.log('PASS: 图片与视频输出方式各配各的、互不覆盖，视频切意图保留输出设置');
