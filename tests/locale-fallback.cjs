const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {
  applyUiLanguage,
  normalizeLocale,
  normalizePreference,
  readPreference,
  resolveUiLanguage,
  translateText,
  translateMessage,
} = require('../frontend/i18n.js');

const frontend = path.join(__dirname, '..', 'frontend');
const html = fs.readFileSync(path.join(frontend, 'index.html'), 'utf8');
const compareHtml = fs.readFileSync(path.join(frontend, 'compare.html'), 'utf8');
const appJs = fs.readFileSync(path.join(frontend, 'app.js'), 'utf8');

function assertEnglishStaticMarkup(markup, filename) {
  const visibleMarkup = markup
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/<script\b[\s\S]*?<\/script>/gi, '')
    .replace(/<style\b[\s\S]*?<\/style>/gi, '');
  const textNodes = [...visibleMarkup.matchAll(/>([^<>]+)</g)].map(match => match[1].trim()).filter(Boolean);
  const attributes = [...visibleMarkup.matchAll(/\b(?:title|aria-label|aria-description|placeholder|alt)="([^"]*)"/g)].map(match => match[1]);
  const untranslated = [...textNodes, ...attributes]
    .map(value => translateText(value, 'en'))
    .filter(value => /[\u3400-\u9fff]/.test(value));
  assert.deepEqual(untranslated, [], `${filename} has visible static Chinese strings without English translations`);
}

assertEnglishStaticMarkup(html, 'index.html');
assertEnglishStaticMarkup(compareHtml, 'compare.html');

assert.match(html, /<html lang="en">/, 'English remains a safe initial document language');
assert.match(html, /<script src="i18n\.js"><\/script>/);
assert.match(html, /id="languagePreference" class="settings-select"/);
assert.match(html, /<option value="auto">自动（跟随系统）<\/option>/);
assert.match(html, /<option value="zh">简体中文<\/option>/);
assert.match(html, /<option value="en">英语<\/option>/);
assert.match(html, /SizqoI18n\.applyToDocument\(document\)/);
assert.match(html, /<h2 data-i18n-zh="拖拽图片、视频或文件夹到此处">Drop images, videos, or folders here<\/h2>/);
assert.match(html, /<p data-i18n-zh="自动识别媒体类型 · 文件夹递归扫描 · 大文件分析不阻塞界面">Media types detected automatically · folders scanned recursively<\/p>/);
assert.match(html, /<span data-i18n-zh="选择媒体文件">Choose Media<\/span>/);
assert.match(html, /<span data-i18n-zh="选择文件夹">Choose Folder<\/span>/);
assert.match(compareHtml, /<script src="i18n\.js"><\/script>/, 'the comparison window uses the same language preference');
assert.match(compareHtml, /SizqoI18n\.applyToDocument\(document\)/);
assert.match(appJs, /localizeUiText\('压缩中…'\)/, 'runtime UI messages go through the translation catalog');

assert.equal(normalizeLocale('C.UTF-8'), '');
assert.equal(normalizeLocale('en_US.UTF-8'), 'en-us');
assert.equal(normalizePreference('invalid'), 'auto');
assert.equal(resolveUiLanguage(['C', '']), 'en');
assert.equal(resolveUiLanguage(['en-US']), 'en');
assert.equal(resolveUiLanguage(['fr-FR']), 'en');
assert.equal(resolveUiLanguage(['zh-CN', 'en-US']), 'zh');
assert.equal(resolveUiLanguage(['zh-CN'], 'en'), 'en', 'manual English overrides the system locale');
assert.equal(resolveUiLanguage(['en-US'], 'zh'), 'zh', 'manual Chinese overrides the system locale');
assert.equal(readPreference({ getItem: () => 'zh' }), 'zh');
assert.equal(readPreference({ getItem: () => 'invalid' }), 'auto');
assert.equal(translateText('压缩设置', 'en'), 'Compression settings');
assert.equal(translateText('原图对比', 'en'), 'Image comparison');
assert.equal(translateText('压缩设置', 'zh'), '压缩设置');
assert.equal(translateText('已保存到: ', 'en') + '原格式.png', 'Saved to: 原格式.png', 'user file names remain untouched');

/// 意图摘要是新界面里"当前会跑什么"的唯一读数，英文界面下它不许夹汉字。
/// 逐个意图过真实编译器，而不是抄一份字符串清单 —— 清单会漂，编译器不会。
const intents = require('../frontend/intents.js');
function summaryFor(intentId, language, extra) {
  const state = Object.assign({
    intent: intentId, quality: 75, outputFormat: 'original', systemOutputFormat: 'original',
    systemImageSize: 'actual', preserveMetadata: true, maxDimensionPx: null, maxOutputBytes: null,
  }, extra || {});
  return translateText(intents.compileImage(state, true).summary, language);
}

assert.equal(summaryFor('balanced', 'en'), 'Balanced · Q75 · Original format');
assert.equal(summaryFor('balanced', 'zh'), '均衡 · Q75 · 原格式');
assert.equal(summaryFor('webSize', 'en'), 'Web size · Q72 · Original format · Longest edge 2560 px');
assert.equal(summaryFor('targetSize', 'en'), 'Target size · Budget 512 KB · Original format');
assert.equal(summaryFor('systemConvert', 'en'), 'System conversion · Original format · Actual size');

const everyImageSummary = intents.imageIntentList(true)
  .filter(intent => intent.id !== 'custom')
  .map(intent => summaryFor(intent.id, 'en'));
assert.deepEqual(everyImageSummary.filter(line => /[\u3400-\u9fff]/.test(line)), [],
  `英文界面下意图摘要残留中文：${everyImageSummary.join(' | ')}`);

// 视频那侧的"目标体积"是同一句话的形状，也一起检查。
const videoSummaries = intents.VIDEO_INTENTS
  .filter(intent => intent.id !== 'videoCustom')
  .map(intent => translateText(intents.compileVideo(
    { intent: intent.id, targetMiB: 64 }, {}, { normalizeOptions: o => Object.assign({}, o), controlSummary: () => '' }).summary, 'en'));
assert.deepEqual(videoSummaries.filter(line => /[\u3400-\u9fff]/.test(line)), [],
  `英文界面下视频意图摘要残留中文：${videoSummaries.join(' | ')}`);

function makeDocument(language, initialText, chineseText) {
  const translated = { textContent: initialText, getAttribute: () => chineseText };
  const plainText = { nodeValue: '均衡 · Q75 · 原格式' };
  const body = { querySelectorAll: selector => selector === '[data-i18n-zh]' ? [translated] : [] };
  const doc = {
    body,
    title: 'Sizqo',
    documentElement: { lang: '', dataset: {} },
    defaultView: null,
    createTreeWalker: () => {
      const nodes = [plainText];
      return { nextNode: () => nodes.shift() || null };
    },
    getElementById: () => null,
  };
  assert.equal(applyUiLanguage(doc, [language], 'auto'), language.startsWith('zh') ? 'zh' : 'en');
  return { doc, translated, plainText };
}

const english = makeDocument('C', 'Drop images, videos, or folders here', '拖拽图片、视频或文件夹到此处');
assert.equal(english.doc.documentElement.lang, 'en');
assert.equal(english.translated.textContent, 'Drop images, videos, or folders here');
assert.equal(english.plainText.nodeValue, 'Balanced · Q75 · Original format');

const chinese = makeDocument('zh-CN', 'Drop images, videos, or folders here', '拖拽图片、视频或文件夹到此处');
assert.equal(chinese.doc.documentElement.lang, 'zh-Hans');
assert.equal(chinese.translated.textContent, '拖拽图片、视频或文件夹到此处');
assert.equal(chinese.plainText.nodeValue, '均衡 · Q75 · 原格式');

/// 后端的原话（result.error / result.reason / outcome.error）要上屏只有一条路：
/// 整句词典 translateMessage。走 translateText 那条子串替换同样"翻"了，但翻成半中半英，
/// 比留原文更难读 —— 这里两头都钉住。主窗口和独立对比窗口共用这条规矩。
const scanWrites = (js, filename) => js.split('\n').map((line, index) => [filename + ':' + (index + 1), line.trim()])
  .filter(([, line]) => /(result\.(error|reason)|outcome\.error)/.test(line)
    && (/\.(?:title|textContent|innerHTML)\s*=/.test(line) || /showToast\(|lines\.push\(|detailCallout\(/.test(line)));
const compareJs = fs.readFileSync(path.join(frontend, 'compare_window.js'), 'utf8');
const contractJs = fs.readFileSync(path.join(frontend, 'result_state.js'), 'utf8');
const pages = [['app.js', appJs], ['compare_window.js', compareJs], ['result_state.js', contractJs]];
const errorWrites = pages.flatMap(([name, js]) => scanWrites(js, name));
const unlocalized = errorWrites.filter(([, line]) => !/(?:localizeBackendText|contractBackendText)\(/.test(line));
assert.deepEqual(unlocalized, [],
  `后端原话没过整句词典就上屏：${unlocalized.map(([n, l]) => `${n}: ${l}`).join(' | ')}`);
assert.ok(errorWrites.length >= 5, `这条扫描本身要覆盖到真实写入点，现在只找到 ${errorWrites.length} 处`);
/// 契约重构后写入点从 10 处降到 5 处：行悬停、失败详情、原因图标都收进了
/// resultDetailTitle 这一个漏斗。这个数字只会因为"收拢"而变小，所以真正的护栏是下面两条：
/// 漏斗必须还在，且任何直接赋给 DOM 属性的后端原话都必须过整句词典。
assert.match(contractJs, /function resultDetailTitle\(result\)[\s\S]{0,320}contractBackendText\(result\.error\)/,
  '后端原话上屏只有 resultDetailTitle 这一个漏斗');
const rawReads = pages
  .flatMap(([name, js]) => js.split('\n').map((line, i) => [name + ':' + (i + 1), line.trim()]))
  .filter(([, line]) => /(?:result|outcome)\.(?:error|reason)\b/.test(line)
    && /\.(?:title|textContent|innerHTML)\s*=/.test(line));
assert.deepEqual(rawReads.filter(([, line]) => !/(?:localizeBackendText|contractBackendText)\(/.test(line)), [],
  `直接把后端原话赋给 DOM 属性：${rawReads.map(([n, l]) => n).join(' | ')}`);

/// §70 状态与原因的句子由前端按 outcome/reasonCode 选，英文界面必须整组有词条。
/// 「原图已是最优」那类后端句子不再是任何状态的载体，所以它连词条都不需要了 ——
/// 这里反过来钉住它不许再回来当判据。注释里允许引用这段历史，代码行不行。
const strip = js => js.split('\n').filter(line => !/^\s*(?:\/\/|\/\*|\*)/.test(line)).join('\n');
const appCode = strip(appJs);
const pageCode = [appJs, compareJs].map(strip).join('\n');
const contractCode = strip(contractJs);
assert.ok(/function (mediaResultFacts|resultStatusText|resultReasonText|resultCapabilities|taskStateFor)\(/.test(contractCode),
  '结果契约的解释器必须住在 result_state.js 里');
for (const name of ['mediaResultFacts', 'resultHasProduct', 'resultCapabilities', 'resultStatusText',
  'resultReasonText', 'taskStateFor', 'resultDetailTitle', 'rowStateVisual']) {
  assert.ok(!new RegExp('function ' + name + '\\(').test(pageCode),
    `${name} 在页面文件里又长出了第二份 —— 两个窗口会各判一次`);
}
assert.ok(/function resultHasProduct\([\s\S]{0,200}mediaResultFacts\(/.test(contractCode),
  '「有没有产物」必须由结论回答，不许退回 outputPath 的旧读法');
for (const dead of ['原图已是最优，无需替换', '已跳过（更大）', '体积更大，已跳过']) {
  assert.ok(![appCode, contractCode].some(code => code.includes(dead)),
    `这句话又被当成状态判据写回了代码：${dead}`);
}
for (const wording of ['已完成 · 未达到目标', '处理失败', '已跳过', '已取消', '压缩后文件更大',
  '原文件已经小于目标体积', '无法读取源文件', '没有产物 · 原文件未改动', '这次没有写出文件：',
  '产物不比原文件小，原文件保持原样。', '原因: ']) {
  assert.doesNotMatch(translateText(wording, 'en'), /[\u3400-\u9fff]/, `${wording} 在英文界面下仍是中文`);
}
assert.doesNotMatch(translateText('未产出文件', 'en'), /[一-鿿]/);
assert.doesNotMatch(translateText('没有产物 · 原图未改动', 'en'), /[一-鿿]/);

/// 没词条的句子必须一字不动。子串替换会把这句切成「解码Failed：文件不是有效Images」，
/// 这个断言就是那条管道不许再被套到后端原话上的理由。
const NO_ENTRY = '解码失败：文件不是有效图片';
assert.equal(translateMessage(NO_ENTRY, 'en'), NO_ENTRY, '没词条的后端原话必须原样给出');
assert.notEqual(translateText(NO_ENTRY, 'en'), NO_ENTRY, '子串替换仍在改动这句，所以后端原话不能走它');
assert.equal(translateMessage(NO_ENTRY, 'zh'), NO_ENTRY);

/// RestoreError::message() 的每一句都会被 Toast 直接念出来，逐条对着 Rust 源码核词条 ——
/// 抄一份字符串清单会漂，源码不会。
const rustHistory = fs.readFileSync(path.join(__dirname, '..', 'src-tauri', 'src', 'history.rs'), 'utf8');
const restoreMessages = [...rustHistory.matchAll(/RestoreError::\w+(?:\([^)]*\))?\s*=>\s*"([^"]+)"/g)].map(m => m[1]);
assert.ok(restoreMessages.length >= 5, `要从 history.rs 抓到恢复失败的那几句，现在只抓到 ${restoreMessages.length} 句`);
const missingEntries = restoreMessages.filter(message => /[\u3400-\u9fff]/.test(translateMessage(message, 'en')));
assert.deepEqual(missingEntries, [], `英文界面下恢复失败会直接念中文：${missingEntries.join(' | ')}`);

console.log('Chinese and English locale checks passed');
