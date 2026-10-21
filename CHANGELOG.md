# Changelog

Sizqo's versioning starts at 0.1.0. This file records user-visible changes;
engineering rules live in [AGENTS.md](AGENTS.md).

## 0.3.2 — 可变帧率视频保住时长 (2026-10-21)

- 未指定帧率时，VFR 源按源平均帧率做 CFR 归一化，输出时长与源一致；修复录屏类
  素材编码成功却被「输出时长与源不符」校验拒绝、临时文件被清理的问题。
- 帧率选项文案由「保持源时间戳 / VFR」改为「自动（保持时长）/ VFR」，与真实行为一致。
- 探测层新增结构化 VFR 识别，编码计划新增对应 argv 回归测试。

## 0.3.1 — 图片与视频输出方式分开配置 (2026-10-20)

- 图片组与视频组各自带一套「覆盖原文件 / 加后缀 / 输出到目录」，分别落盘、互不覆盖；
  视频的输出方式不再藏在「更多参数」里，默认加后缀。
- 输出方式与意图保持正交：切换图片或视频意图只更换编码参数，不重置已选好的落盘位置。
- 「拖入后自动压缩」仍是全局项，单独占一行。

## 0.3.0 — 浅薄荷纸面工作台 + 分发链路倒正 (2026-10-16)

### 视觉语言

- 主窗口从深色工具软件气质改为浅色纸面工作台：`#f5f7f7` 纸面、220px 水印字标
  （Compress / History / Settings）、薄荷模糊圆装饰、72px 左侧导航条。
- 意图组改成无边框分组；能力芯片 32px 胶囊，选中态 `#bce9df`；更多参数折叠行 44px 描边。
- 工作区 75%:25% 浮起白卡；320px 拖放区与 300px 详情列改用线描插画（不是 3D 渲染），
  空状态居中、副标题按 `text-wrap: balance` 断行。
- 底部运行条 64px 透明；主按钮薄荷 `#8ad5c7` 160×44 圆角 22。
- 动效统一 Apple 缓动 `cubic-bezier(0.25,0.1,0.25,1)`，悬停 150ms、面板 250ms + 8px 位移。
- 暗色模式保留，整套 token 重投为墨绿薄荷系；窄窗（≤720px 高）落区与 CTA 自动退让一档。

### 分发链路（重要，影响已发布版本）

- **在线更新包改在最终 app 上生成**：构建期 `createUpdaterArtifacts` 的 tar.gz 里没有
  内置压缩引擎，此前 v0.1.3–v0.2.0 的 `latest.json` 一直把这份空壳发给用户。
- **.app 先公证装订，再拿它造 DMG**；并新增"挂载 DMG 后验包内 app""解包更新包后验包内
  app"两条断言。此前只有 dmg 外壳被提交公证，包内 app 没有票据。
- 新增 `assert_distributable`：非 ad-hoc、Developer ID 签名链、内置引擎非空、票据在——
  全过才算可分发。引擎位置按运行时的两种解析布局都认（本地脚本投递成扁平
  `Resources/bin`，CI 的 `bundle.resources` 通配保留前缀落在 `Resources/resources/bin`），
  否则会把好包判成坏包。
- 新增 `scripts/verify_updater_signature.sh`：对**已下载到本地的更新包**用与
  `tauri-plugin-updater` 同一套 `minisign-verify` 验签。签名曾覆盖过错误字节而全程绿灯，
  只有"下载什么就验什么"才能证明线上那 60 MB 真的能被应用接受。
- v0.2.0 的 `latest.json` 已从 Release 资产撤下（更新通道返回 404），避免继续派发坏包；
  原文件与哈希备份在 `/Users/guofeng/sizqo-release-archive/v0.2.0/`。

## 0.2.0 — intent-first redesign (2026-10-12)

- **The main window is a two-column workspace**: left rail (本轮 / 历史 / 设置),
  a media list with thumbnails plus sort/filter, and a detail pane for the
  selected item that holds its comparison, stats and actions. The run bar at
  the bottom keeps progress, phase, savings, CPU, and the start/pause/stop
  controls visible at all times.
- **Parameters became intents.** Image goals are now 肉眼无差优先 / 均衡 /
  尽量变小 / 网页尺寸 / 指定体积 / 系统转换 / 自定义; video goals are 通用更小 /
  高画质·保留轨道 / 指定体积 / 1080p 网页 / 自定义. The app compiles the chosen
  intent into quality, format, dimension cap and candidate search, shows the
  exact settings it will send, and asks for a number only where the intent
  needs one (longest edge, or a size budget). Raw parameters stay editable in
  the advanced panel; the removed controls (engine picker, effort,
  "smart mode" toggle, "convert to WebP" checkbox) were settings the product
  itself did not act on.
- **Honesty notes per intent**: re-encoding pixels drops EXIF/GPS and only
  System Conversion preserves metadata; 尽量变小 keeps the source format;
  a size budget reports the best size actually reached when it cannot hit the
  target.
- **Format label now matches the bytes it names.** When a conversion could not
  produce a smaller valid file, the engine hands back the source image but used
  to keep the *requested* format tag, so replace mode could write PNG bytes as
  `photo.avif` (or WebP bytes into `photo.png`). Such results are now labelled
  with the source format, and a real format change in replace mode lands on a
  name with the extension its bytes actually have — never clobbering an
  existing file, always after the backup and journal.
- **Swift track aligned**: same intent table, same size-budget ladder
  (quality 80/65/50, then 2560/1920/1280 px, max 6 encodes), same candidate
  rules, and the same honesty notes; a Swift-only extra WebP candidate was
  removed so all three tracks produce the same file for the same intent.
- Frontend regression contracts: `tests/intent-model.cjs` (table ↔ compiler ↔
  rendered chips ↔ English translations) and a rewritten
  `tests/compression-parity.cjs` (JavaScript ↔ Rust ↔ Swift intent tables,
  budget ladder, candidate sets, dead-field guards).

## 0.1.4 — video dropdown fix

- Fixed the video settings panel's 预设 and 视频编码器 dropdowns being empty:
  `loadVideoCapabilities()` was defined but never called, so runtime
  capability detection and preset loading never ran. The startup flow now
  renders a "detecting…" placeholder and populates both dropdowns when the
  probe settles; the capability note explains the current runtime either way.
- Regression assertions added to `tests/video-contract.cjs` (startup must
  call the loader; detection must show a placeholder, not a fake
  "unavailable").

## 0.1.3 — in-app updates

- In-app (online) updates are live for the Direct edition, following the
  OctoShrink/MacPilot pipeline but with an **independent Sizqo signing key**
  (generated fresh; the private key and its password live only in this
  machine's login keychain — never in the repo) and an independent feed:
  `releases/latest/download/latest.json`.
- `scripts/notarize.sh` enables the updater only when the keychain holds
  `sizqo-updater-privkey`: the build merges `tauri.conf.updater.json`
  (pubkey + endpoint + `createUpdaterArtifacts`), sets
  `SIZQO_UPDATER_CONFIGURED=1` (the same gate `lib.rs` uses for plugin
  registration), and emits the signed `Sizqo.app.tar.gz` + `.sig` +
  `latest.json`. Without the key the build stays updater-disabled, exactly
  as [AGENTS.md](AGENTS.md) requires.
- Existing installs update automatically from v0.1.3 onward; the v0.1.2 and
  earlier builds have no updater registered and need one manual install of
  v0.1.3.

## 0.1.2 — media tab switching fix

- Fixed the 全部/图片/视频/历史 media tabs doing nothing when clicked:
  `setMediaFilter` only updated the internal filter without triggering any
  redraw, so the nav highlight, the image/video settings panels, and the
  queue filtering never followed. Switching now runs the workspace redraw;
  `showView` also refreshes the media nav highlight (fixes the 历史 tab
  highlight too). Regression test: `tests/media-tabs.cjs` drives the real
  functions in a VM and asserts highlight + panel visibility + queue redraw.

## 0.1.1 — launch crash fix

- Fixed a startup abort on every build: the updater plugin was registered
  unconditionally while `tauri.conf.json` intentionally ships no
  `plugins.updater` section. Registration now follows the same build-time
  gate as the update commands (`SIZQO_UPDATER_CONFIGURED=1`); without it the
  app starts with updates disabled, exactly as [AGENTS.md](AGENTS.md) requires.

## 0.1.0 — initial Sizqo release

Sizqo is an independent derivative of OctoShrink (inherited MIT license and
commit history intact; see [docs/UPSTREAM_ENGINEERING_RULES.md](docs/UPSTREAM_ENGINEERING_RULES.md)).
It keeps the complete image compression feature set and adds a unified media
workbench with video compression.

### Product identity

- Independent product name, icon (light/dark variants generated by
  `scripts/generate_sizqo_icons.py`), and brand copy (`README.md`,
  `README.zh-CN.md`, `promotion-kit.md`).
- Independent bundle IDs: `com.misswell.sizqo` (Direct),
  `com.misswell.sizqo.appstore` (App Store), `com.misswell.sizqo.swift`
  (Swift native). Independent app-data roots for history, backups, settings,
  and video diagnostics; Sizqo never reads or migrates OctoShrink data.
- Version reset to 0.1.0. Updater keys/feed are intentionally unconfigured;
  update operations stay disabled until `SIZQO_UPDATER_CONFIGURED` is set.
- English/Chinese bilingual UI throughout (new i18n catalog entries for every
  video control).

### Video compression (Direct edition)

- One queue for images and videos; media kind detected from file contents,
  folders expand recursively into a frozen snapshot per session.
- `ffprobe`-based import probing: container, streams, duration, rotation,
  frame rate, HDR/SDR, audio/subtitle/attachment/chapter inventory.
- Runtime capability matrix: every encoder (hardware VideoToolbox
  H.264/HEVC, software x264, x265, SVT-AV1) must pass a real trial encode
  before it is offered; unsupported combinations are hidden or explained.
- Presets: fast (hardware), balanced, high quality (CRF), target size
  (two-pass), compatibility-first, plus fully custom encoder controls with
  per-encoder validation.
- Software encoders cover x264, x265, SVT-AV1, and VP9; output containers
  are MP4, MOV, MKV, WebM, and TS; audio can be copied or converted to
  AAC, Opus, MP3, or FLAC (lossless in MKV). Unsupported combinations
  (VP9 in MP4, FLAC outside MKV, AV1 in TS, chapters in TS) are rejected
  with an explanation instead of producing unplayable files.
- Typed `EncodePlan` argv (no shell, no frontend-supplied commands); rotation
  metadata, odd-dimension scaling, frame-rate and audio policies handled
  explicitly.
- Honest result policy: outputs that are not smaller are reported as
  `skippedLarger`; HDR / Dolby Vision / 10-bit / alpha / interlaced inputs are
  refused with an explanation instead of silent tone mapping.
- Streaming encode with `-progress pipe:1`, per-task speed/ETA, bounded
  diagnostics log, independent cancellation of the current video.
- Output verification: re-probe of the encoded file plus a full decode pass
  before anything is committed; duration/track/pixel-format checks.
- Safe commit: suffix/directory modes claim an atomically free name; replace
  mode reuses the inherited persistent-backup → pre-commit journal → fsync →
  atomic rename → history service, with crash recovery on next launch.
- Video frame preview and source-vs-output same-timestamp comparison.

### Images (inherited, unchanged semantics)

- Full OctoShrink image pipeline: PNG/JPEG/WebP/AVIF/GIF, smart mode,
  system conversion on macOS, batch queue with pause/stop/resume, compare
  window, history with restore, backup retention, CPU limit.

### Distribution tracks

- Direct (`cli-backends`): full video pipeline as above.
- App Store (`inproc-backends`): in-process image engines; video is
  explicitly unavailable at runtime with an explanation — no third-party CLI
  is bundled or spawned in the sandbox.
- Swift native: image functionality inherited; video is not implemented yet
  (see [docs/validation.md](docs/validation.md)).

### Known limitations

- HDR / Dolby Vision / HDR10+, >8-bit, alpha, and interlaced inputs are not
  transcoded; they are detected and rejected with a reason.
- Target-size mode is an estimate; audio, container overhead, and rate
  control cause deviation from the requested size.
- FFmpeg bundling and its licensing review are pending release-owner sign-off
  (see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)); do not publish an
  installer before that review.
