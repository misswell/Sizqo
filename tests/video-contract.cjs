const assert = require('node:assert/strict');
const video = require('../frontend/video-settings.js');

const defaults = video.normalizeOptions();
assert.equal(defaults.encoder, 'auto');
assert.equal(defaults.container, 'mp4');
assert.equal(defaults.resolution, 0);
assert.equal(defaults.frameRate, null, 'source frame timing stays unchanged by default');
assert.equal(defaults.preset, 'medium', 'encoder speed preset is separate from the named quality profile');
assert.equal(defaults.audioPolicy, 'copy');
assert.equal(defaults.allowLargerOutput, false);
assert.equal(Object.hasOwn(defaults, 'quality'), false, 'video settings do not inherit image quality');

const capabilities = {
  backend: 'ffmpeg',
  available: true,
  encoders: [
    {id: 'libx264', label: 'H.264 software', modes: ['crf', 'abr', 'twoPass']},
    {id: 'hevc_videotoolbox', label: 'HEVC hardware', modes: ['abr']},
  ],
};
const sdr = {
  durationUs: 120000000,
  sizeBytes: 50 * 1024 * 1024,
  streams: [{codecType: 'video', codecName: 'h264', width: 1920, height: 1080, avgFrameRate: '30000/1001'}],
};

assert.equal(video.validateOptions(defaults, sdr, capabilities).valid, true,
  'auto mode may use a runtime software encoder that exposes CRF');
const validX264 = video.validateOptions({...defaults, encoder: 'libx264', mode: 'crf'}, sdr, capabilities);
assert.equal(validX264.valid, true);
assert.equal(video.validateOptions({...defaults, encoder: 'hevc_videotoolbox', mode: 'crf'}, sdr, capabilities).valid, false,
  'hardware VideoToolbox does not inherit software CRF modes');
assert.equal(video.validateOptions(defaults, sdr, {
  available: true, encoders: [{id: 'hevc_videotoolbox', modes: ['abr']}],
}).valid, false, 'auto mode cannot expose CRF if every runtime encoder lacks it');
assert.match(video.validateOptions({...defaults, encoder: 'libx264', container: 'webm'}, sdr, capabilities).errors.join(' '), /WebM/);
assert.match(video.validateOptions({...defaults, encoder: 'libx264', audioPolicy: 'opus'}, sdr, capabilities).errors.join(' '), /Opus/);

const hdr = {...sdr, hdrKind: 'dolbyVision'};
assert.match(video.validateOptions({...defaults, encoder: 'libx264'}, hdr, capabilities).errors.join(' '), /HDR/,
  'unknown HDR preservation must stop the job before encoding');
assert.match(video.validateOptions(defaults, sdr, {backend: 'unavailable', available: false, reason: 'App Store 尚未支持视频编码'}).errors.join(' '), /尚未支持/);

assert.equal(video.videoSummary(sdr).dimensions, '1920 × 1080');
assert.equal(video.videoSummary(sdr).frameRate, '29.97 fps');
assert.equal(video.formatDurationUs(3661000000), '1:01:01');
assert.equal(video.estimateOutputBytes(sdr, {...defaults, mode: 'crf'}), null,
  'CRF output estimates stay unknown instead of claiming a compression ratio');
assert.equal(video.estimateOutputBytes(sdr, {...defaults, mode: 'targetSize', targetMiB: 24}).bytes, 24 * 1024 * 1024);

// §139 第三阶段：skippedLarger 这个"既是状态又是原因"的旧旗标已经删掉。
// 有效但没变小 = 结论 skipped_no_benefit + 原因 output_larger_than_source，
// 前端只转述后端交回的结论，不再自己合成第三种事实。
const larger = video.normalizeResult({success: false, sourcePath: '/movie.mov', outputPath: null, originalBytes: 100, outputBytes: 110, outcome: 'skipped_no_benefit', reasonCode: 'output_larger_than_source'}, '/movie.mov', sdr, defaults);
assert.equal(larger.skippedLarger, undefined, '旧旗标不许再被前端造出来');
assert.equal(larger.success, false);
assert.equal(larger.outcome, 'skipped_no_benefit');
assert.equal(larger.reasonCode, 'output_larger_than_source');
assert.equal(larger.savings, -10);
console.log('PASS: video options honor runtime capabilities, source timing, HDR safeguards, and honest size results');
const nodeFs = require('node:fs');
const nodePath = require('node:path');
const appJs = nodeFs.readFileSync(nodePath.join(__dirname, '..', 'frontend', 'app.js'), 'utf8');
// 注释里正是在解释这条禁令，所以扫描前先去掉注释行（JS 与 Rust 都用 //）。
const codeOnly = source => source.split('\n').filter(line => !line.trim().startsWith('//')).join('\n');
for (const name of ['frontend/app.js', 'frontend/video-settings.js', 'frontend/result_state.js',
                    'src-tauri/src/video/ffmpeg_backend.rs']) {
  const source = nodeFs.readFileSync(nodePath.join(__dirname, '..', name), 'utf8');
  assert.doesNotMatch(codeOnly(source), /skippedLarger/, `${name} 又读回或写回了 skippedLarger`);
}
console.log('PASS: skippedLarger 旧旗标已删除，跳过只由结论与原因码说');
const appWithoutLoader = appJs.replace(/async function loadVideoCapabilities\(\)[\s\S]*?\n}/, '');
assert.match(appWithoutLoader, /loadVideoCapabilities\(\)/,
  '启动流程必须调用 loadVideoCapabilities（只定义不调用 = 预设/编码器下拉永远空白）');
assert.match(appJs, /videoCapabilities\.loading[\s\S]*?正在检测视频编码能力/,
  '能力探测期间下拉必须显示「检测中」占位，而不是空白或假不可用');
console.log('video dropdown wiring checked');
