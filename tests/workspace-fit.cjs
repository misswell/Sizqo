// 工作台不许有"看运气"的那一屏：滚动条与底部死白必须同时为零。
//
// 旧版把落区钉成 min-height: 320px，于是同一个空队列会随窗口高度换病症——
// 740–780 之间顶出滚动条，800 以上又在落区下面留一条随高度线性增长的死白；
// 而「更多参数」一展开，flex-shrink:0 的意图区直接清单挤成 0 高，被 body 的
// overflow:hidden 裁到永远滚不回来。三条不变量：
//   ① 落区跟随容器伸缩，不写死高度；
//   ② 意图区可收缩，清单有存活线，参数展开只让参数卡自己滚；
//   ③ 退让的判据是面板自身高度（容器查询），不是视口高度。
const assert = require('node:assert/strict');
const fs = require('node:fs');

const css = fs.readFileSync('frontend/style.css', 'utf8');
function rule(sel) {
  const m = css.match(new RegExp('(^|\\n)' + sel.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '\\s*\\{([^}]*)\\}'));
  assert.ok(m, '找不到规则 ' + sel);
  return m[2];
}

// ──  落区不钉死 ─────────────────────────────────────────────
const dropzone = rule('.dropzone');
assert.doesNotMatch(dropzone, /min-height\s*:/,
  '.dropzone 不许再写 min-height：钉死的高度矮一档顶出滚动条、高一档留死白');
assert.match(dropzone, /flex\s*:\s*1/, '.dropzone 必须 flex: 1 铺满清单可用高度');
assert.ok(css.includes('.list-scroll:has(> .dropzone:not([hidden]))'),
  '落区要铺满，.list-scroll 就得在它可见时切成 flex 列');

// ──  意图区可收缩 + 清单存活线 ──────────────────────────────
const intentBar = rule('.intent-bar');
assert.doesNotMatch(intentBar, /flex-shrink\s*:\s*0/,
  '.intent-bar 写死 flex-shrink:0 = 参数一展开清单就被挤出视口，且再也回不来');
assert.match(intentBar, /min-height\s*:\s*0/, '.intent-bar 必须允许收缩到内容以下');
assert.match(rule('.work-split'), /min-height\s*:\s*200px/, '.work-split 要有 200px 存活线');
// 收起态必须保持 overflow: visible，否则卡片向下的投影会被滚动框裁掉。
assert.doesNotMatch(intentBar, /overflow/, '收起态的 .intent-bar 不许是滚动容器（会裁投影）');
assert.ok(css.includes('.intent-bar:has(> .intent-advanced[open])'),
  '只有展开参数那一档才给意图区内滚兜底');
assert.match(rule('.advanced-body'), /max-height\s*:/,
  '参数卡自己内滚：滚动条落在有边有角的卡里，而不是整块顶栏在晃');

// ── ③ 退让按容器量，不按视口量 ───────────────────────────────
assert.ok(css.includes('@container list-col') && css.includes('@container detail-col'),
  '落区/空状态的退让必须按面板自身高度触发：展开参数时窗口哪怕 800 高，清单也只剩 200px');
assert.doesNotMatch(css, /@media\s*\(max-height:\s*720px\)/,
  '视口高度判据已废：它看不见面板被挤到多高');
for (const sel of ['.list-column', '.detail-column']) {
  assert.match(rule(sel), /container-type\s*:\s*size/, sel + ' 要先声明成尺寸容器');
}
// 居中一旦内容高出容器就会把上半截推到滚不回来的地方，只能用 auto 外边距。
assert.match(rule('.detail-inner > .detail-empty:only-child'), /margin\s*:\s*auto\s+0/,
  '详情空状态用 auto 外边距居中，不用 justify-content: center');
assert.doesNotMatch(css, /\.detail-inner:has\([^)]*\)\s*\{[^}]*justify-content\s*:\s*center/,
  'justify-content: center 的溢出上半截不可达');

console.log('PASS: 空队列随窗口高度既不蹦滚动条也不留死白，参数展开不再挤没清单');
