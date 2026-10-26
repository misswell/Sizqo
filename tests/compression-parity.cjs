// Guard the compression settings and encoder profiles shared by Direct,
// App Store, and the native Swift app. Update this contract with every codec,
// quality, intent, or fallback change in any implementation.
//
// 参数不再是界面上的控件，而是意图的编译结果，所以三轨对齐的"源"也从
// `<select>`/`<input>` 的默认值挪到了这张意图表：同一档意图在两轨上必须
// 编译出同一份引擎请求。这里逐档比对 frontend/intents.js 与 Swift 的
// ImageIntent，而不是比对两边"看起来都挺均衡"。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const intents = require('../frontend/intents.js');

const root = path.join(__dirname, '..');
const read = relative => fs.readFileSync(path.join(root, relative), 'utf8');
const html = read('frontend/index.html');
const swiftIntents = read('swift/Sources/SizqoSwift/Models/ImageIntent.swift');
const swiftOptions = read('swift/Sources/SizqoSwift/Models/CompressOptions.swift');
const swiftUI = read('swift/Sources/SizqoSwift/Views/ContentView.swift');
const swiftState = read('swift/Sources/SizqoSwift/ViewModels/AppState.swift');
const swiftEngine = read('swift/Sources/SizqoSwift/Engine/CompressionEngine.swift');
const swiftRunner = read('swift/Sources/SizqoSwift/Engine/CLIRunner.swift');
const directEngine = read('src-tauri/src/engine.rs');
const appStoreEngine = read('src-tauri/src/engine_inproc.rs');
const swiftBuild = read('scripts/build_swift.sh');
const buildAll = read('scripts/build_all.sh');

function functionBody(source, start, end) {
  const from = source.indexOf(start);
  assert.notEqual(from, -1, `missing function marker: ${start}`);
  const to = source.indexOf(end, from + start.length);
  assert.notEqual(to, -1, `missing function end marker: ${end}`);
  return source.slice(from, to);
}

// ── Swift 意图表解析 ───────────────────────────────────────────────────
const defaultBudget = Number(functionBody(swiftIntents, 'static let defaultBudgetBytes', 'func apply')
  .match(/=\s*(\d+)\s*\*\s*1024/)[1]) * 1024;

// ImageIntent 的 case 顺序即屏幕上的意图顺序。
const swiftIds = [...swiftIntents
  .slice(swiftIntents.indexOf('enum ImageIntent'), swiftIntents.indexOf('var label: String'))
  .matchAll(/^\s*case (\w+)$/gm)].map(m => m[1]);

// usesSmartRouting 里 return true 的那些档位 = 会走候选搜索的意图。
const smartBody = functionBody(swiftIntents, 'var usesSmartRouting: Bool', 'static let defaultBudgetBytes');
const smartLine = smartBody.match(/case ([\w., ]+): return true/);
assert.ok(smartLine, 'usesSmartRouting 的 true 分支必须是一张明确的意图清单');
const swiftSmart = new Set(smartLine[1].split(',').map(s => s.trim().replace(/^\./, '')).filter(Boolean));

// apply(to:) 每一档编译出的原始参数；没写出的字段就是"继承 base"。
function swiftCompiled(id) {
  const branches = {};
  const body = functionBody(swiftIntents, 'func apply(to base: CompressOptions)', 'func requestSummary');
  const lines = body.split('\n');
  let current = null;
  for (const line of lines) {
    const open = line.match(/^\s*case \.(\w+):$/);
    if (open) { current = open[1]; branches[current] = ''; continue; }
    if (current) branches[current] += line + '\n';
  }
  const src = branches[id];
  assert.ok(src !== undefined, `Swift 意图表没有 ${id} 分支`);
  const pick = (name, pattern) => {
    const m = src.match(new RegExp(`o\\.${name} = ${pattern}`));
    return m ? m[1] : null;
  };
  const dimension = pick('maxDimensionPx', '(nil|\\d+)');
  const budget = pick('maxOutputBytes', '(nil|base\\.maxOutputBytes \\?\\? ImageIntent\\.defaultBudgetBytes)');
  return {
    quality: pick('quality', '(\\d+)'),
    outputFormat: pick('outputFormat', '\\.(\\w+)'),
    maxDimensionPx: dimension === null || dimension === 'nil' ? null : Number(dimension),
    maxOutputBytes: budget === null || budget === 'nil' ? null : defaultBudget,
    touchesAnything: /\bo\.\w+ =/.test(src),
  };
}

// ── 逐档比对：同一意图 → 同一份引擎请求 ────────────────────────────────
const neutralState = {
  quality: 75, outputFormat: 'original', systemOutputFormat: 'original',
  systemImageSize: 'actual', preserveMetadata: true,
  maxDimensionPx: null, maxOutputBytes: null,
};
// JS 表里独有的那一档是「系统转换」：Swift 把它做成 processingMode，不是意图 case。
assert.equal(intents.IMAGE_INTENTS.filter(i => i.macOnly).map(i => i.id).join(), 'systemConvert');
assert.match(swiftOptions, /case system = "system"/);
assert.equal(swiftIds.includes('systemConvert'), false,
  '系统转换在 Swift 是处理模式；两档都做成意图就会有两处开关');
const sharedIds = intents.IMAGE_INTENTS.map(i => i.id).filter(id => id !== 'systemConvert');
assert.deepEqual(swiftIds, sharedIds, '两轨的意图档位必须一字不差（顺序即屏幕顺序）');

for (const id of sharedIds) {
  if (id === 'custom') continue; // 自定义 = 原始参数原样发送，单独守
  const js = intents.compileImage({ ...neutralState, intent: id }, true);
  const swift = swiftCompiled(id);
  assert.equal(js.intent, id);
  assert.equal(String(js.options.quality), swift.quality, `${id}: 质量档位分叉`);
  assert.equal(js.options.outputFormat, swift.outputFormat, `${id}: 输出格式分叉`);
  assert.equal(js.options.maxDimensionPx, swift.maxDimensionPx, `${id}: 尺寸上限分叉`);
  assert.equal(js.options.maxOutputBytes, swift.maxOutputBytes, `${id}: 体积预算分叉`);
  assert.equal(js.useSmart, swiftSmart.has(id), `${id}: 候选搜索开关分叉`);
}

// targetSize 的预算由引擎阶梯决定，两轨都不许自己预设一个质量。
const budgetJs = intents.compileImage({ ...neutralState, intent: 'targetSize' }, true);
assert.equal(budgetJs.options.quality, 0, '预算模式由阶梯决定质量');
assert.equal(budgetJs.options.maxOutputBytes, defaultBudget, `默认预算要和 Swift 的 ${defaultBudget} 字节一致`);

// 自定义 = 意图不再介入：JS 读编辑值，Swift 的分支必须什么都不写（继承 base）。
assert.equal(swiftCompiled('custom').touchesAnything, false,
  '自定义不许偷偷编译常量，否则"原始参数怎么编辑就怎么发送"就成了假话');
const customJs = intents.compileImage({ ...neutralState, intent: 'custom', quality: 63, outputFormat: 'avif', maxDimensionPx: 800 }, true);
assert.equal(customJs.options.quality, 63);
assert.equal(customJs.options.outputFormat, 'avif');
assert.equal(customJs.options.maxDimensionPx, 800);

// 默认意图 = 两轨起手那一档。
assert.equal(intents.defaultImageIntentId(), 'balanced');
assert.equal(swiftIds[swiftIds.indexOf('balanced')], 'balanced');
assert.match(swiftState, /@Published private\(set\) var intent: ImageIntent = \.balanced/);
assert.match(swiftState, /intent = \.balanced/, '重置默认也回到均衡档');
const balanced = intents.compileImage({ ...neutralState, intent: 'balanced' }, true);
assert.equal(balanced.options.quality, 75);
assert.equal(balanced.useSmart, true, '默认意图必须开候选搜索');

// 裸参数控件的起手值 = 均衡档编译出来的值（折叠区里看到的即发出去的）。
assert.match(html, /id="qualitySlider" min="10" max="100" value="75"/);
assert.match(swiftOptions, /var quality: Int = 75/);
assert.match(html, /name="outputMode" value="replace" checked/,
  '默认输出方式与 Swift 一致；回落到 suffix 会让三轨默认行为分叉');
assert.match(swiftOptions, /var outputMode: OutputMode = \.replace/);

// 引擎从不读的字段不许回来（AGENTS.md：不许有假控件）。
// 智能路由是真实引擎行为（compress_smart 的候选搜索），由意图决定，不是用户开关。
for (const dead of ['backend', 'effort', 'smartMode', 'convertToWebp', 'lossless']) {
  assert.doesNotMatch(swiftOptions, new RegExp(`var ${dead}`), `Swift 不许复活引擎不读的 ${dead}`);
  assert.doesNotMatch(html, new RegExp(`id="${dead}"`), `Direct 不许复活引擎不读的 ${dead}`);
  assert.doesNotMatch(swiftUI, new RegExp(`options\\.${dead}`), `Swift 界面不许绑引擎不读的 ${dead}`);
}
assert.match(swiftState, /"sizqo\.setting-backend", "sizqo\.setting-effort",/,
  '旧版本写到磁盘的死开关，重置时必须一起从磁盘清掉');
assert.match(swiftState, /let useSmart = usesSmartRouting/);
assert.match(swiftState, /CompressionEngine\.compressSmart\(file: path, options: options\)/);
assert.match(swiftUI, /appState\.selectIntent\(intent\)/);

// 预算求解器是一张跨语言的表：档位错了，同一档意图在 Swift 上就会压出和 Direct
// 不同体积的文件。这里解析两边的常量表逐档比对，而不是相信注释里的"同一阶梯"。
const rustLadderRaw = functionBody(directEngine, 'fn budget_ladder', 'fn merge_cap');
const rustLadder = [...rustLadderRaw.matchAll(/\((None|Some\(\d+\)),\s*(\d+)\)/g)]
  .map(m => [m[1] === 'None' ? null : Number(m[1].match(/\d+/)[0]), Number(m[2])]);
const swiftLadder = [...functionBody(swiftEngine, 'static let targetLadder', 'static let budgetAttemptLimit')
  .matchAll(/\((nil|\d+),\s*(\d+)\)/g)].map(m => [m[1] === 'nil' ? null : Number(m[1]), Number(m[2])]);
assert.ok(rustLadder.length >= 2 && swiftLadder.length === rustLadder.length,
  `阶梯档数分叉：Rust ${rustLadder.length} / Swift ${swiftLadder.length}`);
assert.deepEqual(swiftLadder, rustLadder, '指定体积阶梯的每一档都必须一致（先降质量，再动尺寸）');
assert.equal(swiftEngine.match(/static let budgetAttemptLimit = (\d+)/)[1],
  directEngine.match(/const BUDGET_ATTEMPT_LIMIT[^0-9]*(\d+)/)[1],
  '最多几次编码，两轨必须说同一个数');

// 智能候选集合由引擎决定，两轨一致：PNG 同时试 PNG 与 WebP 取更小。
const directSmart = functionBody(directEngine, 'pub async fn compress_smart', '#[cfg(test)]');
const appStoreSmart = functionBody(appStoreEngine, 'pub async fn compress_smart', '#[cfg(test)]');
const swiftSmart2 = functionBody(swiftEngine, 'static func compressSmart', 'private static func analyzeQuality');
for (const [name, body, expected] of [
  ['Direct', directSmart, [/compress_png\(file, &opts, OutputRule::BenefitOnly\)/, /compress_to_webp\(file, &opts, OutputRule::BenefitOnly\)/]],
  ['App Store', appStoreSmart, [/compress_png\(file, &opts, rule\)/, /compress_to_webp\(file, &opts, rule\)/]],
  ['Swift', swiftSmart2, [/compressPNG\(file: file, options: opts\)/, /compressToWebP\(file: file, options: opts\)/]],
]) {
  for (const candidate of expected) {
    assert.match(body, candidate, `${name} smart PNG mode must compare both PNG and WebP candidates`);
  }
}

// App Store uses in-process equivalents; pin their quality and effort mapping to
// the same values Swift passes to Direct's bundled encoders.
for (const [name, token] of [
  ['PNG quantization quality range', 'attr.set_quality((q - 10) as u8, q as u8)'],
  ['PNG quantization speed', 'attr.set_speed(3)'],
  ['oxipng effort preset', 'oxipng::Options::from_preset(level)'],
  ['JPEG quality', 'cinfo.set_quality(quality as f32)'],
  ['JPEG optimization', 'cinfo.set_optimize_coding(true)'],
  ['JPEG progressive mode', 'cinfo.set_progressive_mode()'],
  ['WebP quality', 'encoder.encode(quality as f32)'],
  ['AVIF quality', 'with_quality(quality as f32)'],
  ['AVIF speed', 'with_speed(6)'],
]) {
  assert.ok(appStoreEngine.includes(token), `App Store encoder profile missing ${name}`);
}

// Swift invokes the same Direct CLI tools with the same quality parameters.
// These paired assertions are the source-level contract for future codec edits.
const cliProfiles = [
  ['pngquant quality range', String.raw`format!("--quality={}-{}", q_low, q_high)`, String.raw`"--quality=\(qLow)-\(qHigh)"`],
  ['pngquant speed', '"--speed=3"', '"--speed=3"'],
  ['pngquant metadata stripping', '"--strip"', '"--strip"'],
  ['oxipng quality level', String.raw`format!("-o{}", level)`, String.raw`"-o\(level)"`],
  ['oxipng safe metadata stripping', '"safe"', '"safe"'],
  ['mozjpeg quality option', '"-quality"', '"-quality"'],
  ['mozjpeg optimization', '"-optimize"', '"-optimize"'],
  ['mozjpeg progressive encoding', '"-progressive"', '"-progressive"'],
  ['gifsicle optimization', '"--optimize=3"', '"--optimize=3"'],
  ['gifsicle color count', String.raw`format!("--colors={}", colors)`, String.raw`"--colors=\(colors)"`],
  ['gifsicle comment stripping', '"--no-comments"', '"--no-comments"'],
  ['cwebp quality', '"-q"', '"-q"'],
  ['cwebp method', '"-m".into(),', '"-m", "6"'],
  ['cwebp passes', '"-pass".into(),', '"-pass", "10"'],
  ['avifenc speed', '"--speed".into(),', '"--speed", "6"'],
  ['avifenc minimum quality', '"--min".into(),', '"--min", "0"'],
  ['avifenc quality maximum', '"--max".into(),', '"--max", "\\(quality)"'],
];
for (const [name, directToken, swiftToken] of cliProfiles) {
  assert.ok(directEngine.includes(directToken), `Direct encoder profile missing ${name}`);
  assert.ok(swiftEngine.includes(swiftToken), `Swift encoder profile differs for ${name}`);
}

// The Swift bundle must contain every CLI backend used by Direct. Missing tools
// used to silently select a different ImageIO fallback and produce different files.
assert.match(swiftBuild, /ENCODER_TOOLS=\(pngquant oxipng cjpeg cwebp avifenc gifsicle\)/);
assert.ok(swiftRunner.includes('case "avifenc": return ["--jobs", "\\(threadLimit)"]'));
assert.ok(swiftRunner.includes('case "oxipng": return ["--threads", "\\(threadLimit)"]'));
assert.match(buildAll, /bash "\$PROJECT_DIR\/scripts\/build_swift\.sh" \|\| fail "Swift 版构建失败"/);

// §57 Replace 的源文件身份只允许一份实现：图片与视频都必须用同一个
// SourceIdentity（编码前钉指纹、覆盖前复核），不许各自写一套更弱的判断。
const commandsSource = read('src-tauri/src/commands.rs');
const videoTransaction = read('src-tauri/src/video/transaction.rs');
const videoBackend = read('src-tauri/src/video/ffmpeg_backend.rs');
assert.ok(commandsSource.includes('use crate::source_identity::{SourceCheck, SourceIdentity}'),
  '图片线必须使用共享的 SourceIdentity');
assert.ok(videoTransaction.includes('pub use crate::source_identity::{SourceCheck, SourceIdentity}'),
  '视频线必须复用同一份 SourceIdentity，而不是另写一套');
assert.ok(commandsSource.includes('SourceIdentity::read_with_hash(path)'),
  '图片 Replace 必须在编码前钉下内容指纹');
assert.ok(videoBackend.includes('transaction::SourceIdentity::read(&source)'),
  '视频 Replace 必须在编码前钉下源身份');
assert.ok(videoTransaction.includes('identity.verify(source)'),
  '视频覆盖前必须复核源身份');
assert.ok(commandsSource.includes('identity.verify('),
  '图片覆盖前必须复核源身份');

console.log('Compression parity contract checks passed');
