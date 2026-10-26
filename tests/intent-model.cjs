// 意图模型契约（core/intent-model.md）。
//
// 意图是这台产品唯一暴露给用户的压缩语言：表上写的每一档都必须
//   ① 真的被编译器兑现（表 ↔ compileImage 不许分叉）；
//   ② 真的被界面渲染出来（chip 来自这张表，不在别处再列一遍字面量）；
//   ③ 真的被引擎执行（做不到的请求不许出现在表里 —— AGENTS.md 不许假控件）；
//   ④ 在英文界面下说得出来（每一条文本都有英文映射）。
// 任何一条断了，用户看到的就是一个他点不动、或者点了不生效的按钮。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const intents = require('../frontend/intents.js');
const { translateText } = require('../frontend/i18n.js');

const root = path.join(__dirname, '..');
const read = relative => fs.readFileSync(path.join(root, relative), 'utf8');
const html = read('frontend/index.html');
const appJs = read('frontend/app.js');

const hasCjk = value => /[㐀-\u9fff]/.test(value);
const neutral = {
  quality: 75, outputFormat: 'original', systemOutputFormat: 'original',
  systemImageSize: 'actual', preserveMetadata: true,
  maxDimensionPx: null, maxOutputBytes: null,
};
const compile = (intent, extra) =>
  intents.compileImage(Object.assign({}, neutral, extra, { intent }), true);

// ── ① 表就是编译器：每一档声明什么常量，就发什么请求 ──────────────────
const tableIds = intents.IMAGE_INTENTS.map(i => i.id);
assert.equal(new Set(tableIds).size, tableIds.length, '意图 id 不许重复');
assert.equal(intents.IMAGE_INTENTS.filter(i => i.isDefault).length, 1);
assert.equal(intents.defaultImageIntentId(), 'balanced');
assert.equal(intents.VIDEO_INTENTS.filter(i => i.isDefault).length, 1);
assert.equal(intents.defaultVideoIntentId(), 'videoSmall');

for (const intent of intents.IMAGE_INTENTS) {
  if (intent.id === 'custom') continue; // 自定义不发常量
  const compiled = compile(intent.id);
  assert.equal(compiled.intent, intent.id, `${intent.id} 编译失败，落到了别的意图`);
  const spec = intent.request || {};
  if (spec.processingMode === 'system') {
    assert.equal(compiled.options.processingMode, 'system', '系统转换必须真的切处理模式');
    assert.equal(compiled.useSmart, false, '系统转换走的是转换，不是候选搜索');
    continue;
  }
  assert.equal(compiled.options.processingMode, 'advanced');
  for (const key of ['quality', 'outputFormat', 'maxDimensionPx']) {
    if (spec[key] === undefined) continue;
    assert.equal(compiled.options[key], spec[key], `${intent.id}: ${key} 与表上写的不一致`);
  }
  // 预算档由阶梯决定质量，表上的 0 就是"我不预设质量"。
  if (intent.editableTarget === 'maxOutputBytes') {
    assert.equal(compiled.options.quality, 0, `${intent.id} 不许替求解器预设质量`);
    assert.equal(compiled.options.maxOutputBytes, spec.maxOutputBytes);
  }
  if (intent.editableTarget === 'maxDimensionPx') {
    // 可编辑目标：用户填了就以用户为准，没填才落到表上的默认档。
    assert.equal(compile(intent.id, { maxDimensionPx: 1200 }).options.maxDimensionPx, 1200);
  }
  assert.ok(intent.label && intent.note, `${intent.id} 必须有名字和说明`);
  // 诚实规则 3：凡是重编码像素的意图，都要把 EXIF/GPS 说不带进输出讲出来。
  assert.match(intent.warning || '', /EXIF/, `${intent.id} 重编码像素却不交代元数据去向`);
}

// ── macOnly 门禁：看不见的开关不许编译出请求 ──────────────────────────
assert.equal(intents.imageIntentList(true).some(i => i.id === 'systemConvert'), true);
assert.equal(intents.imageIntentList(false).some(i => i.id === 'systemConvert'), false,
  '非 macOS 不许出现系统转换这一档');
assert.equal(intents.compileImage(Object.assign({}, neutral, { intent: 'systemConvert' }), false).intent,
  'balanced', '非 macOS 上即使状态里存着系统转换，也必须落回默认意图');

// 未知/失效的意图 id 回落默认档，而不是发一份空请求。
assert.equal(compile('noSuchIntent').intent, 'balanced');
assert.equal(compile(null).intent, 'balanced');

// ── 自定义 = 原始参数说话；越界值先夹紧再发 ────────────────────────────
assert.equal(compile('custom', { quality: 500 }).options.quality, 100);
assert.equal(compile('custom', { quality: 3 }).options.quality, 10);
assert.equal(compile('custom', { quality: 0 }).options.quality, 75, '非法值回落默认，不许发 0');
assert.equal(compile('custom', { outputFormat: 'gif' }).options.outputFormat, 'original',
  '引擎不支持的目标格式不许当成请求发出去');
assert.equal(compile('custom', { outputFormat: 'avif' }).options.outputFormat, 'avif');
assert.equal(compile('custom', { maxDimensionPx: 10 }).options.maxDimensionPx, 144);
assert.equal(compile('custom', { maxDimensionPx: 99999 }).options.maxDimensionPx, 16384);
assert.equal(compile('custom', { maxOutputBytes: 10 }).options.maxOutputBytes, 10 * 1024);
assert.equal(compile('custom', { maxOutputBytes: 1e12 }).options.maxOutputBytes, 2 * 1024 * 1024 * 1024);
assert.equal(compile('custom', { maxDimensionPx: 'abc' }).options.maxDimensionPx, null);

// ── 系统转换分支：可保留元数据，尺寸档位照实带上 ───────────────────────
const system = compile('systemConvert', { systemOutputFormat: 'heif', systemImageSize: 'medium', preserveMetadata: false });
assert.equal(system.options.processingMode, 'system');
assert.equal(system.options.outputFormat, 'heif');
assert.equal(system.options.systemImageSize, 'medium');
assert.equal(system.options.preserveMetadata, false);
assert.equal(system.useSmart, false);
assert.match(system.summary, /^系统转换/);
// 系统转换是唯一"保留元数据"的路径，说明里必须这么说（诚实规则 3）。
assert.match(intents.IMAGE_INTENTS.filter(i => i.id === 'systemConvert')[0].note, /元数据/);

// 界面上给得出的格式，编译器必须照收 —— 下拉里有但被 normalize 悄悄丢掉，
// 用户看到的设置和他发出去的请求就不是同一件事。
function optionValues(selectId) {
  const from = html.indexOf(`id="${selectId}"`);
  assert.ok(from > 0, `找不到下拉 ${selectId}`);
  const block = html.slice(from, html.indexOf('</select>', from));
  return [...block.matchAll(/<option value="([^"]+)"/g)].map(m => m[1]);
}
for (const value of optionValues('outputFormat')) {
  assert.equal(compile('custom', { outputFormat: value }).options.outputFormat, value,
    `高级路径的下拉值 ${value} 被编译丢掉了`);
}
for (const value of optionValues('systemOutputFormat')) {
  assert.equal(compile('systemConvert', { systemOutputFormat: value }).options.outputFormat, value,
    `系统转换的下拉值 ${value} 被编译丢掉了`);
}

// ── 反推意图：只有逐字相等才算，"看起来像"一律 custom ──────────────────
assert.equal(intents.classifyImage({ raw: compile('balanced').options }, true), 'balanced');
assert.equal(intents.classifyImage({ raw: compile('losslessFirst').options }, true), 'losslessFirst');
assert.equal(intents.classifyImage({ raw: { processingMode: 'advanced', quality: 88, outputFormat: 'original', maxDimensionPx: null, maxOutputBytes: null } }, true), 'custom');
assert.equal(intents.classifyImage({ raw: { processingMode: 'advanced', quality: 75, outputFormat: 'jpg', maxDimensionPx: null, maxOutputBytes: null } }, true), 'custom');
// 带可编辑目标的档位（网页尺寸/指定体积）不许被反推认领：那格数字是用户填的，
// 认领成意图就等于把他的输入偷偷换成另一档默认值。
assert.equal(intents.classifyImage({ raw: compile('webSize').options }, true), 'custom');

// ── 切到自定义时接手的是"此刻真实生效的值"，不是空白默认表单 ───────────
for (const id of ['losslessFirst', 'balanced', 'asSmallAsPossible', 'webSize']) {
  const before = compile(id);
  const after = compile('custom', intents.seedCustom(before));
  assert.deepEqual(after.options, before.options, `${id} → 自定义之后请求变了`);
}
// 预算档例外：质量归阶梯管，接手时把预算原样带走、质量还回可编辑的默认值。
const budgetSeed = intents.seedCustom(compile('targetSize', { maxOutputBytes: 300 * 1024 }));
assert.equal(budgetSeed.maxOutputBytes, 300 * 1024);
assert.equal(compile('custom', budgetSeed).options.maxOutputBytes, 300 * 1024);
assert.equal(compile('custom', budgetSeed).options.quality, 75);

// ── 摘要说得懂：每档都要报出意图名 + 真实生效的关键参数 ─────────────────
for (const intent of intents.IMAGE_INTENTS) {
  const compiled = compile(intent.id);
  assert.ok(compiled.summary.length > 0, `${intent.id} 没有摘要`);
  assert.ok(!/undefined|NaN|null/.test(compiled.summary), `${intent.id} 摘要里漏了值：${compiled.summary}`);
}
assert.equal(compile('balanced').summary, '均衡 · Q75 · 原格式');
assert.equal(compile('webSize').summary, '网页尺寸 · Q72 · 原格式 · 最长边 2560 px');
assert.equal(compile('targetSize').summary, '指定体积 · 预算 512 KB · 原格式');

// ── 视频意图：只有 Direct 有视频引擎，估算必须说成估算 ─────────────────
const videoIds = intents.VIDEO_INTENTS.map(i => i.id);
assert.equal(new Set(videoIds).size, videoIds.length);
const compiledVideo = intents.compileVideo({ intent: 'videoTarget', targetMiB: 64 }, null, null);
assert.equal(compiledVideo.options.mode, 'targetSize');
assert.equal(compiledVideo.options.targetMiB, 64, '用户填的目标体积必须被带走');
assert.match(compiledVideo.summary, /估算/, '目标体积是估算，摘要不许说成保证');
assert.equal(intents.compileVideo({ intent: 'nope' }, null, null).intent, 'videoSmall');
const customVideo = intents.compileVideo({ intent: 'videoCustom', options: { mode: 'crf', crf: 24, container: 'mkv' } }, { crf: 28, container: 'mp4' }, null);
assert.equal(customVideo.options.crf, 24, '视频自定义必须原样发送编辑值');
assert.equal(customVideo.options.container, 'mkv');
// Swift 没有视频引擎：这一档不许出现在 Swift 界面（AGENTS.md：不许假控件）。
assert.doesNotMatch(read('swift/Sources/SizqoSwift/Views/ContentView.swift'), /videoSmall|VideoIntent/,
  'Swift 不许渲染视频意图');

// ── ③′ Swift 说的是同一批事实：意图表在两轨各写一遍，说明就得逐档比对 ────
// 表的结构由 tests/compression-parity.cjs 守；这里守的是"用户读到的那句话"
// 有没有把同一件事（哪一档质量、动不动尺寸、候选搜索会不会换格式）说清楚。
const swiftIntentSrc = read('swift/Sources/SizqoSwift/Models/ImageIntent.swift');
const swiftUI = read('swift/Sources/SizqoSwift/Views/ContentView.swift');
const swiftFacts = id => [...swiftIntentSrc
  .matchAll(new RegExp(`case \\.${id}: return "([^"]+)"`, 'g'))].map(m => m[1]);
for (const intent of intents.IMAGE_INTENTS.filter(i => i.id !== 'systemConvert' && i.id !== 'custom')) {
  const texts = swiftFacts(intent.id);
  assert.ok(texts.length >= 2, `${intent.id} 在 Swift 缺标签或说明`);
  if (intent.id === 'targetSize') {
    assert.ok(texts.some(t => /最多 6 次/.test(t)), 'targetSize 的编码次数上限必须说得出');
  } else {
    const quality = `质量 ${compile(intent.id).options.quality}`;
    assert.ok(texts.some(t => t.includes(quality)), `${intent.id} 的 Swift 说明没报出 ${quality}`);
  }
}
assert.ok(swiftFacts('balanced').some(t => /默认/.test(t)), '默认档必须在两轨都标出来');
assert.ok(swiftFacts('webSize').some(t => t.includes('2560')), '网页尺寸的 2560 px 上限两边都得说');
// 诚实规则 3 在 Swift 上也要在按下开始之前说，而且只在高级路径说（系统转换保留元数据）。
assert.match(swiftIntentSrc, /var metadataNote: String/, 'Swift 意图必须带元数据去向的说明');
assert.match(swiftIntentSrc, /metadataNote[\s\S]{0,120}EXIF/);
assert.match(swiftUI, /if appState\.options\.processingMode == \.advanced \{[\s\S]{0,80}intent\.metadataNote/,
  '元数据说明必须挂在高级模式下：系统转换之下它就是假话');

// ── ② 界面就是这张表：chip 由列表渲染，字面量不在第二处重写 ────────────
assert.match(appJs, /renderIntentChips\('imageIntentChips', window\.SizqoIntents\.imageIntentList\(isMac\)/,
  '图片 chip 必须由意图表渲染');
assert.match(appJs, /renderIntentChips\('videoIntentChips', window\.SizqoIntents\.VIDEO_INTENTS/,
  '视频 chip 必须由意图表渲染');
assert.doesNotMatch(html, /data-intent="/, 'chip 全部由意图表渲染，HTML 里不许有静态 chip');
assert.ok(html.includes('id="imageIntentChips"') && html.includes('id="videoIntentChips"'));

// ── ④ 英文界面说得出每一条意图文本 ────────────────────────────────────
const englishText = value => translateText(value, 'en');
for (const intent of intents.IMAGE_INTENTS.concat(intents.VIDEO_INTENTS)) {
  for (const [field, text] of [['label', intent.label], ['note', intent.note], ['warning', intent.warning]]) {
    if (!text) continue;
    const english = englishText(text);
    assert.ok(!hasCjk(english), `${intent.id}.${field} 在英文界面下仍是中文：${english}`);
    if (field !== 'label') assert.notEqual(english, text, `${intent.id}.${field} 没有英文映射`);
  }
  const compiled = intents.compileImage(Object.assign({}, neutral, { intent: intent.id }), true);
  assert.ok(!hasCjk(englishText(compiled.summary)), `${intent.id} 的摘要英文下仍含中文：${englishText(compiled.summary)}`);
}
assert.equal(englishText('均衡 · Q75 · 原格式'), 'Balanced · Q75 · Original format');

// ── §22 能力矩阵：下拉与请求都由后端能力表决定，前端不再维护第二份白名单 ──
const matrix = {
  backend: 'inproc',
  formats: [
    { id: 'jpg', label: 'JPEG' },
    { id: 'png', label: 'PNG' },
    { id: 'heif', label: 'HEIF' },
  ],
  outputTargets: ['jpg', 'png'],
  systemOutputTargets: ['jpg', 'heif'],
  animationPreservingTargets: ['gif'],
};
intents.setImageCapabilities(matrix);
assert.deepEqual(intents.imageOutputTargets(false), ['jpg', 'png'], '高级目标必须来自矩阵');
assert.deepEqual(intents.imageOutputTargets(true), ['jpg', 'heif'], '系统目标必须来自矩阵');
assert.equal(compile('custom', { outputFormat: 'webp' }).options.outputFormat, 'original',
  '矩阵没给 WebP 时不许把请求发出去');
assert.equal(compile('custom', { outputFormat: 'png' }).options.outputFormat, 'png');
assert.equal(compile('systemConvert', { systemOutputFormat: 'heif' }).options.outputFormat, 'heif');
assert.equal(compile('systemConvert', { systemOutputFormat: 'png' }).options.outputFormat, 'original',
  '系统转换矩阵外的格式必须回落');
assert.equal(intents.formatLabel('heif'), 'HEIF', '标签也来自矩阵');
intents.setImageCapabilities(null);
// 后端还没答复时也必须能编译：回落到保守集合，不抛错。
assert.deepEqual(intents.imageOutputTargets(false), ['jpg', 'png', 'webp', 'avif']);
assert.match(appJs, /invoke\('get_image_capabilities'\)/, '前端必须从后端读能力矩阵');
assert.match(appJs, /fillImageFormatSelect\('outputFormat', matrix\.outputTargets/,
  '高级输出下拉必须由矩阵填充');
// Swift 轨用同一张能力表驱动下拉，不允许再手写第二份格式列表。
const swiftCaps = read('swift/Sources/SizqoSwift/Models/ImageCapabilities.swift');
assert.match(swiftCaps, /static func matrix\(\)/, 'Swift 必须有能力矩阵');
assert.match(swiftCaps, /animatedSupported[\s\S]*systemConvertible/, 'Swift 矩阵字段必须与 Rust 对齐');
assert.match(swiftUI, /ForEach\(OutputFormat\.advancedTargets/, 'Swift 高级下拉必须由矩阵驱动');
assert.match(swiftUI, /ForEach\(OutputFormat\.systemTargets/, 'Swift 系统下拉必须由矩阵驱动');
assert.doesNotMatch(swiftUI, /Text\("WebP"\)\.tag/, 'Swift 下拉不许再手写格式列表');

console.log('PASS: 意图表 ↔ 编译器 ↔ 界面 ↔ 引擎 ↔ 英文文案 五者一致');
