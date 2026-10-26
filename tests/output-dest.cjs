// 落盘方式的归属：图片一份、视频一份，各摆在自家意图组里，切意图不许动它。
//
// 报上来的现象是「选了覆盖原文件，跑出来却是 _compressed 后缀」。根因不在后端：
// 切视频意图/预设会把编译出来的那套参数铺回控件，其中就带着 outputMode:'suffix'，
// 于是用户在顶上的选择在无人知情时被换掉。三条不变量：
//   ① 两组 radio 都可见，且分别随图片/视频分组一起收放，不再藏在「更多参数」里；
//   ② 意图/预设编译出的参数不写回落盘控件，只有读档（loadVideoSettings）才铺回去；
//   ③ 发出去的请求里的 outputMode 永远等于界面上选中的那一项。
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { source, slice } = require('./app-slices.cjs');

const html = fs.readFileSync(path.join(__dirname, '..', 'frontend', 'index.html'), 'utf8');
const i18n = fs.readFileSync(path.join(__dirname, '..', 'frontend', 'i18n.js'), 'utf8');

function region(from, to, label) {
  const start = html.indexOf(from);
  const end = html.indexOf(to, start + from.length);
  assert.ok(start >= 0 && end > start, `HTML 里找不到${label}`);
  return html.slice(start, end);
}

// ── ① 可见且分组：radio 摆在意图组内，不在高级参数里 ────────────────────
assert.ok(region('id="imageIntentGroup"', 'id="videoIntentGroup"', '图片意图组')
  .includes('name="outputMode"'), '图片输出方式必须住在 #imageIntentGroup 内，才跟着图片一起收放');
assert.ok(region('id="videoIntentGroup"', 'id="intentTargetRow"', '视频意图组')
  .includes('name="videoOutputMode"'), '视频输出方式必须住在 #videoIntentGroup 内，才跟着视频一起收放');
assert.ok(region('id="videoIntentGroup"', 'id="intentTargetRow"', '视频意图组')
  .includes('id="videoSuffixRow"'), '视频的后缀格也要跟着可见，不能留在更多参数里');
assert.ok(!html.slice(html.indexOf('id="intentAdvanced"')).includes('videoOutputMode'),
  '落盘方式不许再藏回「更多参数」');
assert.ok(!region('class="intent-output intent-output-global"', 'id="intentAdvanced"', '会话级输出行')
  .includes('output-mode-switch'), '顶上的输出方式已经分家到两组，会话行不能再留一份');
assert.doesNotMatch(html, /<select id="videoOutputMode"/, '视频输出方式的下拉已下线，只留 radio 一个真相源');
// 默认值：图片覆盖、视频后缀，各自与三轨契约 / DEFAULT_OPTIONS 对齐。
assert.match(html, /name="outputMode" value="replace" checked/);
assert.match(html, /name="videoOutputMode" value="suffix" checked/);
assert.match(i18n, /'图片输出方式': /);
assert.match(i18n, /'视频输出方式': /);

// ── ② 切意图不写回落盘控件（跑真代码，不是读源码猜）─────────────────────
const suffixInput = { value: '_mine' };
// radio 组的语义：勾上一个，同组其余自动落勾。测试必须复刻这条，
// 否则「铺回了 directory」会因为 replace 没被清掉而看不出来。
let checkedMode = 'replace';
const radios = {};
['replace', 'suffix', 'directory'].forEach(value => {
  radios[value] = {
    value,
    get checked() { return checkedMode === value; },
    set checked(on) { if (on) checkedMode = value; },
  };
});
const byId = { videoOutputSuffix: suffixInput };
const context = vm.createContext({
  console, Object, Number, String, Array, Boolean, document: {
    getElementById: id => byId[id] || (byId[id] = { value: '', checked: false, textContent: '', title: '', hidden: false }),
    querySelector(selector) {
      if (selector === 'input[name="videoOutputMode"]:checked') {
        return Object.keys(radios).map(key => radios[key]).find(radio => radio.checked) || null;
      }
      const match = /^input\[name="videoOutputMode"\]\[value="([^"]+)"\]$/.exec(selector);
      return match ? radios[match[1]] || null : null;
    },
    querySelectorAll: () => [],
  },
  window: {
    // 意图编译不带输出方式，normalizeOptions 兜的就是 video-settings.js 的那份默认。
    SizqoVideo: {
      normalizeOptions: value => Object.assign({ outputMode: 'suffix', outputSuffix: '_compressed', outputDir: null }, value || {}),
    },
  },
  localizeUiText: value => value,
  videoOutputDir: null,
  currentVideoOptions: {},
  // 真的那行第一句：派生状态从界面重读。其余部分与本节无关，桩掉。
  syncVideoSettingsControls() { context.currentVideoOptions = context.videoOptionsFromControls(); },
});
vm.runInContext(slice('function currentVideoOutputMode(', 'function syncVideoSettingsControls('), context);

// 界面上选中的就是发出去的那一套。
assert.equal(context.videoOptionsFromControls().outputMode, 'replace');

// 铺意图（setVideoIntent / 启动时那句）：编码参数照铺，落盘那三样一个字都不许改。
context.applyVideoOptions({ encoder: 'libx264', outputMode: 'suffix', outputSuffix: '_compressed' });
assert.equal(radios.replace.checked, true, '切意图把「覆盖原文件」换成了「加后缀」——就是报上来的那个 bug');
assert.equal(radios.suffix.checked, false);
assert.equal(suffixInput.value, '_mine', '后缀被意图默认值顶掉了');
assert.equal(context.currentVideoOptions.outputMode, 'replace', '派生状态也必须跟着界面，不是跟着意图');
assert.equal(context.videoOptionsFromControls().encoder, 'libx264', '编码参数照常铺下去了，只挡住落盘');

// 读档是唯一允许铺落盘控件的路径：这时就该照存档铺。
context.applyVideoOptions({ outputMode: 'directory', outputSuffix: '_v2', outputDir: '/Movies/Out' }, true);
assert.equal(radios.directory.checked, true);
assert.equal(radios.replace.checked, false);
assert.equal(suffixInput.value, '_v2');
assert.equal(context.videoOutputDir, '/Movies/Out');
assert.equal(context.videoOptionsFromControls().outputDir, '/Movies/Out');

// 覆盖/后缀模式下不许带着一份陈旧的目录上路：那是后端会照做的第二个决定。
context.setVideoOutputModeRadio('replace');
assert.equal(context.videoOptionsFromControls().outputDir, null,
  '选了覆盖却还把上次的输出目录发出去，文件会落在用户没选的那边');

// ── ③ 只有读档传 pushOutput，其余调用点一律不传 ─────────────────────────
assert.match(source, /function applyVideoOptions\(options, pushOutput\)/);
const withoutLoad = source.replace(/function loadVideoSettings\([\s\S]*?\n}\n/, '');
assert.doesNotMatch(withoutLoad, /applyVideoOptions\([^)]*,\s*true/,
  '除 loadVideoSettings 之外任何调用点都不许把落盘控件铺回去');
assert.equal((withoutLoad.match(/applyVideoOptions\(/g) || []).length,
  (source.match(/applyVideoOptions\(/g) || []).length - 2,
  'loadVideoSettings 的两个分支就是仅有的两处铺回控件');

console.log('PASS: 图片/视频落盘各自可见、切意图不再改写它、请求里的输出方式恒等于界面');
