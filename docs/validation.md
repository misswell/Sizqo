# Validation record

This file records the observed build/test/runtime status of the three Sizqo
tracks. Rules: a feature is "supported" only with a command and its real
outcome recorded here; pending or blocked work is never reported as passed.
Update this file whenever a track's capability changes.

Last updated: 2026-10-12

## Automated checks

| Check | Command | Result |
|-------|---------|--------|
| Rust unit tests, Direct features | `cargo test --lib` (in `src-tauri`) | ✅ 111 unit passed (2026-10-12, v0.2.0 change set) |
| Rust unit tests, shared in-process engines | `cargo test --lib --features inproc-backends` | ✅ 112 unit passed (2026-10-12) |
| Rust unit tests, App Store feature set | `cargo test --lib --no-default-features --features inproc-backends` | ✅ 112 unit passed (2026-10-12) |
| Frontend suites (queue, sessions, media import, video contract, i18n, history, CPU limit, settings, compare, update panel, parity, intent model) | `npm run test:frontend` (17 suites, incl. `tests/intent-model.cjs`, `tests/app-slices.cjs`) | ✅ all pass (2026-10-12) |
| Swift history / backup / pause / queue / settings self-checks | `bash scripts/test_swift_history.sh` | ✅ all pass |

## Track: Direct (`com.misswell.sizqo`)

- Image pipeline: inherited OctoShrink engine, covered by the Rust suites and
  frontend suites above. No regression observed.
- Video pipeline (`src-tauri/src/video/`, `video_commands.rs`):
  - Probe / capability trials / EncodePlan / streaming encode / progress /
    cancellation / validation / transaction are unit-tested (87 unit tests).
  - Software encoder set: x264, x265, SVT-AV1, VP9; containers MP4, MOV,
    MKV, WebM, TS; audio policies copy/AAC/Opus/MP3/FLAC/none. Real runtime
    validation (2026-10-11): VP9+WebM+Opus, x264+TS+MP3, and x264+MKV+FLAC
    encoded with the collected binary, probed correct (format/codec), and
    passed full-decode checks.
  - Real encode end-to-end with the collected runtime (2026-10-11, arm64):
    generated a 1280x720/30fps H.264+AAC 5s MP4 (4.7 MB), re-encoded it with
    the plan-shaped argv (libx264 CRF 28, AAC 160k, `+faststart`) through the
    collected bundled binary; output 937 KB (80% smaller), `ffprobe` reports
    exactly 5.000000 s, and a full `-xerror` decode of the output passed.
  - Full acceptance matrix (AGENTS.md §11, items 1–12 incl. HDR/VFR/multi-track
    fixtures, kill-during-commit recovery, 20 GB sources): **pending** —
    needs real user-accepted fixtures and signed-runtime runs.
- Bundled runtime: `scripts/collect_macos_tools.sh` collects `ffmpeg` 9.0.2 +
  `ffprobe` (Homebrew build, GPL configuration: `--enable-gpl --enable-libx264
  --enable-libx265 --enable-libsvtav1`; encoders verified present in the
  collected copy) into `src-tauri/resources/video/{bin,lib}` (19 dylibs),
  ad-hoc signed so they execute; `scripts/notarize.sh`, `scripts/package.sh`
  and `scripts/video_collect_ffmpeg.sh` stage/verify them. The GPL
  configuration is **not publishable** until the review in
  [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) is done.
- Unsigned build (2026-10-11): `bash scripts/build_all.sh` produced
  `src-tauri/target/release/bundle/macos/Sizqo_direct.app` (69 MB); the
  bundled `ffmpeg`/`ffprobe`/`pngquant` were executed from inside the built
  `.app` successfully. Signed-runtime check and GUI launch check: **pending**.

## Track: App Store (`com.misswell.sizqo.appstore`)

- Image pipeline: in-process engines build and test under
  `--no-default-features --features inproc-backends`.
- Video: intentionally unavailable at runtime; the capability matrix reports
  the reason instead of showing fake controls
  (`video::capabilities::NATIVE_UNAVAILABLE`). This is the documented,
  accurate runtime-disabled state; no native bridge exists yet.
- Unsigned build (2026-10-11): `bash scripts/build_all.sh` produced
  `src-tauri/target/release/bundle/macos/Sizqo.app` (17 MB); verified the
  bundle contains **zero** third-party binaries/dylibs (`Resources/bin`,
  `Resources/lib`, `Resources/video` all absent).
- Signed sandbox runtime check: **pending** (release engineering).

## Track: Swift native (`com.misswell.sizqo.swift`)

- Module renamed `OctoShrinkSwift` → `SizqoSwift`; independent Application
  Support root with `sizqo.`-prefixed settings keys.
- Image functionality: inherited, verified by `scripts/test_swift_history.sh`.
- Build (2026-10-11): `bash scripts/build_swift.sh` produced
  `swift/.build/Sizqo_swift.app` (22 MB, 6 CLI tools + 28 dylibs).
- Video: **not implemented**. No VideoProbe/VideoEncoder exists in
  `swift/Sources/SizqoSwift`; do not advertise video for this track.

## Redesign: intent-first workspace (2026-10-12)

Scope of this change set (all three tracks): the page shell and the operation
model were rebuilt around **intents instead of raw parameters** — left rail
(本轮 / 历史 / 设置), list + detail two-column workspace, persistent run bar;
image intents 肉眼无差优先 / 均衡 / 尽量变小 / 网页尺寸 / 指定体积 / 系统转换 /
自定义, video intents 通用更小 / 高画质·保留轨道 / 指定体积 / 1080p 网页 / 自定义.
The intent → parameter contract lives in
[core/intent-model.md](../core/intent-model.md); the design board in
`docs/UI_DESIGN_MACOS26.html` is marked superseded.

Also fixed in this change set: a compression that converted formats but produced
no smaller valid file used to hand back the **source bytes tagged with the
requested format**, so replace mode could store PNG bytes as `photo.avif` or
write WebP bytes into `photo.png`. The engine now labels such results with the
source format (`kept_original` in `engine.rs` / `engine_inproc.rs`, and the
resized-branch relabel in `settle_working_file`), and replace mode names the
output after the extension its bytes actually have
(`available_converted_path` + the `!same_format` branch in `commands.rs`),
never clobbering an existing file and always after backup + journal.

| Gate | Command | Result |
|------|---------|--------|
| Rust tests, Direct | `cargo test --lib` (in `src-tauri`) | ✅ EXIT=0 — 111 passed; 0 failed; 0 ignored (2026-10-12) |
| Rust tests, shared in-process | `cargo test --lib --features inproc-backends` | ✅ EXIT=0 — 112 passed; 0 failed |
| Rust tests, App Store feature set | `cargo test --lib --no-default-features --features inproc-backends` | ✅ EXIT=0 — 112 passed; 0 failed |
| New format-label regression tests actually ran | `cargo test --lib -- --exact` per name | ✅ all six ran and passed in all three gates (`commands::tests::{replace_mode_renames_the_file_to_the_format_its_bytes_actually_are, a_format_conversion_never_clobbers_an_existing_file_with_the_target_name, jpg_bytes_overwrite_a_jpeg_source_in_place, a_converted_target_never_overwrites_an_existing_file}`, `engine::intent_tests::{kept_original_labels_the_bytes_with_the_format_they_actually_are, a_settled_run_that_keeps_the_original_reports_the_source_format}`) |
| Rust integration tests | `cargo test` (full, incl. `src-tauri/tests/`) | ⏳ pending — run as part of the release build gate below |
| Frontend suites | `npm run test:frontend` | ✅ EXIT=0 — 17/17 suites |
| Swift module compiles | `xcrun swiftc -typecheck -parse-as-library -target arm64-apple-macos13.0 -module-name SizqoSwift` over the 20 tracked sources (incl. new `Models/ImageIntent.swift`) | ✅ EXIT=0, zero diagnostics; full `-O` codegen also EXIT=0. Note: this track has **no `Package.swift`** — `swift build` cannot be used; the real build entry point is `scripts/build_swift.sh` |
| Swift history / backup / pause / queue / settings self-checks | `bash scripts/test_swift_history.sh` | ✅ EXIT=0 — 32 sections, all `ok`. It compiles a **subset** (not `ImageIntent.swift`, `AppState.swift` or any `Views/`), so it does not gate the intent UI — only the full-app build does |
| Swift `.app` rebuilt this session | `bash scripts/build_swift.sh` | ⏳ not run — that script `rm -rf`s the existing `swift/.build/Sizqo_swift.app` (stale since 2026-10-11) and needs `src-tauri/resources/bin`; the Swift track is not part of this Release |
| GUI / signed-runtime acceptance of the new workspace | launch the built `.app` and drive 本轮/历史/设置 + image/video intents | ⏳ pending — recorded as pending, not passed |
| Three-track build / signing / notarization / §11 video matrix | `bash scripts/build_all.sh`, `bash scripts/notarize.sh` | ⏳ pending per release run below |

Compile and build operations are delegated per [AGENTS.md](../AGENTS.md); the
rows above are filled in only with the exit codes actually observed.

## Release: v0.1.0 (2026-10-11)

- Direct DMG `Sizqo-0.1.0-macos.dmg` (31 MB, arm64): Developer ID Application
  "Guofeng Liu (U8U443D7ZL)" signed, notarized (Accepted), stapled, spctl
  accepted. Bundled ffmpeg 9.0.2 configuration recorded in
  [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) with corresponding-source
  links (GPL-2.0-or-later configuration: libx264/libx265 enabled).
- App Store and Swift-native tracks are not part of this release.

## In-app updates (2026-10-11, v0.1.3)

- Independent minisign keypair generated for Sizqo; private key + password
  stored only in the local login keychain (`sizqo-updater-privkey` /
  `sizqo-updater-key-password`), pubkey committed in
  `src-tauri/tauri.conf.updater.json`. No OctoShrink key or feed reused.
- `notarize.sh` enables the updater only when the keychain entry exists
  (build merge config + `SIZQO_UPDATER_CONFIGURED=1` + signed
  `Sizqo.app.tar.gz` + `latest.json`); without it the build stays
  updater-disabled.
- Verified: app boots with the plugin registered (config-present +
  registered combination), settings page shows 当前版本 v0.1.3 Direct with
  the 在线更新 row, the public feed endpoint
  `releases/latest/download/latest.json` returns the v0.1.3 feed
  (HTTP 200), and release assets are anonymously downloadable. The actual
  download-and-install path engages from the next release onward.

## Release: v0.2.0 (2026-10-12)

Tag `v0.2.0` → commit `6c50b92` (on top of `dc718d5` credential fix and
`e91b95c` the intent-first redesign). Release:
https://github.com/misswell/Sizqo/releases/tag/v0.2.0. Working tree was clean
and equal to `origin/main` at build time (`git status --porcelain` empty,
`git rev-parse HEAD` = `git rev-parse origin/main`), so the shipped binary is
this commit's sources.

- Built, signed and notarized with `bash scripts/notarize.sh` (ASC API-key route,
  see `dc718d5`): `NOTARIZE_EXIT=0`, notary `status: Accepted`.
  Keychain profile `sizqo-notary` does **not** exist on this machine and
  `store-credentials` cannot write from a non-interactive shell, so the run
  used `--key/--key-id/--issuer` extracted into a `umask 077` temp file.
- Artifact set (all four uploaded, sizes verified against the local files):
  `Sizqo-0.2.0-macos.dmg` 31,703,781 B · `Sizqo.app.tar.gz` 4,432,774 B ·
  `Sizqo.app.tar.gz.sig` 404 B · `latest.json` 659 B.

| Check | Command | Result |
|-------|---------|--------|
| Stapled | `xcrun stapler validate Sizqo.app` / `… Sizqo-0.2.0-macos.dmg` | ✅ EXIT 0 both |
| Deep signature | `codesign --verify --deep --strict --verbose=2 Sizqo.app` | ✅ EXIT 0 |
| Gatekeeper | `spctl --assess -vvv -t install Sizqo.app` | ✅ accepted, `source=Notarized Developer ID`, `com.misswell.sizqo` |
| Feed serves this version | `curl -L …/releases/latest/download/latest.json` | ✅ HTTP 200, `version: "0.2.0"`, URL → `…/download/v0.2.0/Sizqo.app.tar.gz` |
| Anonymous download | `curl -L` DMG + tar.gz | ✅ byte-identical SHA-256 to local (`2d4887b8…`, `50f11e0c…`) |
| Updater signature | `minisign -V -x sig -p <committed pubkey> -m Sizqo.app.tar.gz` | ✅ "Signature and comment signature verified" (key `CA755A8F7DAAC119`, Sizqo's own) |
| Published install path | `hdiutil attach` the downloaded DMG → `spctl --assess -t exec` + `CFBundleShortVersionString` | ✅ accepted as Notarized Developer ID, version `0.2.0` |

UI verification actually performed for this release:

- The packaged app launches and owns a real 1180×760 window titled "Sizqo"
  (`CGWindowListCopyWindowInfo`), its WebContent process starts, and the log
  shows no resource-load failure; `withGlobalTauri` is still `true` and every
  `index.html` asset reference resolves.
- The shipped frontend (`frontendDist: "../frontend"`, i.e. the exact files
  embedded in the bundle) was rendered in a real browser engine with a stubbed
  `window.__TAURI__`: the left rail (本轮/历史/设置), all 7 image intent chips
  with 均衡 `aria-pressed="true"`, all 5 video chips with 通用更小 pressed, the
  compiled-intent summary line (including the EXIF/GPS honesty note), the
  output-mode row, the queue header, and the run bar (队列为空 / CPU 自动 /
  追加 / 清除全部 / 开始压缩) all paint with their design-system colours. Only
  stub-caused messages appeared (missing `onDragDropEvent`, which the app
  handles by falling back to HTML5 DnD, and the deliberately rejected
  `check_for_update`).
- **Still pending**: clicking through the new workspace in the *signed* bundle
  on this desktop. The window stayed occluded behind pre-existing user windows
  (`window occluded 1`, WebKit discarded its layers, so Computer Use captured
  only the `#f8f6f8` window background), `open` did not take focus away from
  them, and this shell has no Screen Recording permission
  (`screencapture -l… → could not create image from window`). No user window
  was moved, hidden, or closed to work around this. Real-device acceptance of
  the redesign therefore remains a human step.

Gate status: this release followed the v0.1.x precedent — the user asked for
推送/发版, and the outstanding gate items below are **not** marked passed:
signed-sandbox (App Store) runtime check, the §11 video acceptance matrix, the
FFmpeg GPL licensing review, and the signed Direct GUI pass above.

## CI release secrets (jollyfon/Sizqo, 2026-10-13)

Secret names were taken from **this** repository's pipeline, not from the
MacPilot setup note: [.github/workflows/release.yml](../.github/workflows/release.yml)
and `scripts/sign_notarize_macos_ci.sh` consume
`APPLE_DEVELOPER_ID_P12_BASE64`, `APPLE_DEVELOPER_ID_P12_PASSWORD`, `APPLE_ID`,
`APPLE_TEAM_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, `TAURI_SIGNING_PRIVATE_KEY` and
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The `APPLE_CERTIFICATE_*` /
`APPLE_DEVELOPER_ID` names from that note would have been dead config here.

Configured (2026-10-13, values never printed, committed or piped through argv):

```sh
security find-identity -v -p codesigning     # 3 valid; Developer ID Application: Guofeng Liu (U8U443D7ZL)
gh api user --jq .login                     # jollyfon (admin on jollyfon/Sizqo)
zsh /tmp/sizqo_configure_ci_secrets.sh --repo jollyfon/Sizqo \
  --apple-id <Apple ID email> \
  --developer-id 'Developer ID Application: Guofeng Liu (U8U443D7ZL)' --team-id U8U443D7ZL
gh secret list --repo jollyfon/Sizqo        # names + timestamps only
```

`gh secret list` then shows exactly `APPLE_DEVELOPER_ID_P12_BASE64`,
`APPLE_DEVELOPER_ID_P12_PASSWORD`, `APPLE_ID`, `APPLE_TEAM_ID`. The P12 is the
single-identity export (certificate matched to its private key by public-key
SHA-256), imported into a throwaway keychain and asserted to yield **exactly
one** codesigning identity equal to the requested Developer ID *before* upload;
`find` for stray `.p12`/`cert-*.pem`/`key-*.pem`/`verify.keychain-db` under the
temp roots returns nothing and `security list-keychains -d user` no longer
lists the verify keychain. The scratch copy of the skill script was deleted.

Two deviations worth knowing: `gh auth status` exits **1** on this machine
because the suspended `misswell` token is still in the keyring, so the script
was changed to gate on `gh api user` + `repos/<repo> .permissions.admin`
instead; and the pushed `v0.1.0`–`v0.2.0` tags produced **no** Actions runs
(`gh run list --repo jollyfon/Sizqo` → `[]`).

Not done — this section is configured, not end-to-end verified:

- `APPLE_APP_SPECIFIC_PASSWORD` must be typed by the user directly at
  <https://github.com/jollyfon/Sizqo/settings/secrets/actions>; it never goes
  through chat. The alternative that already works locally is the App Store
  Connect API-key trio (`notarytool --key/--key-id/--issuer`), which would mean
  changing the workflow's notarization step rather than adding this secret.
- `TAURI_SIGNING_PRIVATE_KEY`/`_PASSWORD` were deliberately **not** uploaded:
  that minisign key (`sizqo-updater-privkey`, pubkey `CA755A8F7DAAC119`) signs
  the auto-update payload for shipped binaries, so publishing it to a second
  GitHub account needs an explicit decision rather than being done as a
  convenience.

### 2026-10-14 复核：misswell 恢复，上面的 jollyfon 配置失去消费方

The `misswell` suspension is over, so the premise of this whole section
changed. Re-measured: `gh api user` → `misswell` (active again; `jollyfon` is
now the inactive keyring entry, which is why `gh secret list --repo
jollyfon/Sizqo` returns HTTP 403), `ssh -T git@github.com` → `Hi misswell!`,
and `gh api repos/misswell/Sizqo/releases/latest` resolves to **v0.2.0** with
four assets (`latest.json` 659 B, `Sizqo-0.2.0-macos.dmg` 31,703,781 B,
`Sizqo.app.tar.gz` 4,432,774 B, `.sig` 404 B) — the baked updater feed is live
again, so the "feed 404s" caveat above no longer applies. `gh secret list
--repo misswell/Sizqo` is **empty**: the original repository has no CI secrets
at all, and the four just written to `jollyfon/Sizqo` have no consumer there.

Measurement note: plain HTTPS to `github.com:443` from this shell times out
(`curl -w '%{http_code}'` → `000`, "Failed to connect … port 443"), while
`api.github.com` via `gh` and git-over-SSH both work. A `000` here is this
shell's transport, **not** the repository's status — release/feed checks must
go through `gh api`, or they read as an outage that isn't happening.
- The release workflow has since been rewritten for Sizqo (commit `cbb0b23`,
  recorded in "CI 发布链路重做" below); at the time of writing it was still the
  inherited OctoShrink one — 16 `OctoShrink` matches, 0 `Sizqo`, zipping
  `bundle/macos/OctoShrink.app`, which no longer exists because `productName`
  is `Sizqo`.

## CI 发布链路重做与端到端验证（2026-10-14）

### 六个失败 run 的真实原因不是 Secrets

`gh api repos/misswell/Sizqo/actions/runs` 显示 6 次 Release run **全部 failure**，
每一次都停在同一位置。job 114819441433 的日志尾部：

```
Collected 7 tools -> src-tauri/resources/bin (29 dylibs)
Missing required tool: ffmpeg
##[error]Process completed with exit code 1.
```

`scripts/collect_macos_tools.sh` 收两组工具（图片 7 个 + `resources/video/bin` 的
`ffmpeg ffprobe`），而 workflow 的 `brew install pngquant oxipng mozjpeg gifsicle
webp jpeg-xl libavif` 从来没有装 ffmpeg——上游 OctoShrink 没有视频轨。
所以只配 Secrets 一步 CI 都推进不了；这轮改动先解决它。

### 四个必须改的结构问题

1. **视频引擎根本不进 bundle**。`src-tauri/src/video/process.rs` 只在
   `<exe>/../Resources/video/bin` 找 `ffmpeg/ffprobe`，而 Build 步骤的
   `--config '{"bundle":{"resources":["resources/bin/*","resources/lib/*"]}}'`
   只覆盖图片 CLI。新增 "Bundle the video engine" 步骤，在签名前把
   `src-tauri/resources/video/{bin,lib}` 投进 `Contents/Resources/video/`，
   与 `scripts/notarize.sh:148-166` 的本地路线一致。`sign_app()` 遍历
   `Contents` 下所有 Mach-O，因此这些文件照样被正式签名。
2. **`--config` 不可重复**。`cargo tauri build --help` 显示
   `-c, --config <CONFIG>`（没有 `...`），传两次只会留下最后一次。改用
   `jq -n --argjson r '$RESOURCES' --slurpfile u tauri.conf.updater.json '$r * $u[0]'`
   合成 `${RUNNER_TEMP}/ci.tauri.conf.json` 后单次传入。
3. **公证凭据**。`scripts/sign_notarize_macos_ci.sh` 原来硬要求
   `APPLE_ID + APPLE_TEAM_ID + APPLE_APP_SPECIFIC_PASSWORD`。现在与
   `scripts/notarize.sh` 同路线：App Store Connect API key 三件套优先
   （`notarytool --key/--key-id/--issuer`），没有才回落 Apple ID + 专用密码，
   两者都没有则 `exit 2`。临时 `.p8` 由 `trap cleanup_notary_key EXIT` 清理。
   这条改动直接取消了「用户必须手工粘一个 app-specific password」的前置条件。
4. **更新签名门禁**。Build 用 `SIZQO_UPDATER_CONFIGURED: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY != '' && '1' || '0' }}`
   决定是否合并 `tauri.conf.updater.json`，与 `src-tauri/src/lib.rs:65,287,376,514`
   的 `option_env!` 门禁一致；"Sign updater archive" 在没有私钥时**中止本轮**，
   而不是发一个 `latest.json` 里签名为空的 Release。
   其余为去 OctoShrink 化：资产名、临时 keychain `sizqo-signing.keychain-db`、
   `scripts/repair_appimage_permissions.py` 的临时目录前缀、Release 标题与正文
   （正文按三轨真实能力重写，含「视频由运行时探测」「意图优先双栏工作台」）。

### misswell/Sizqo 的 9 个 Actions Secrets

`gh api user --jq .login` → `misswell`，`repos/misswell/Sizqo .permissions.admin` → true。
`gh auth status` 在本机仍退出 1（钥匙串留着失效的 `jollyfon` 条目），所以脚本门禁改成
上面两条断言。所有值一律 keychain → stdin → `gh secret set`，不经 argv、不打印、
不进仓库；`APPLE_APP_SPECIFIC_PASSWORD` 全程没有出现过。

| Secret | 来源 | 上传前的证明 |
| --- | --- | --- |
| `APPLE_DEVELOPER_ID_P12_BASE64` / `_PASSWORD` | 登录钥匙串导出，按公钥 SHA-256 把证书与私钥配对，只保留目标身份 | 导入一次性 keychain 后断言 `find-identity` **恰好一个**身份等于 `Developer ID Application: Guofeng Liu (U8U443D7ZL)` |
| `APPLE_ID` / `APPLE_TEAM_ID` | `misswell@foxmail.com` / `U8U443D7ZL` | — |
| `APPLE_API_KEY`（base64 的 .p8）/ `APPLE_API_KEY_ID` / `APPLE_API_ISSUER` | 钥匙串 `asc:credential:agentspace-notary` | `xcrun notarytool history --key … --key-id … --issuer …` 返回 "Successfully received submission history" |
| `TAURI_SIGNING_PRIVATE_KEY` / `_PASSWORD` | 钥匙串 `sizqo-updater-privkey` / `sizqo-updater-key-password` | 见下 |

**私钥编码坑**：`sizqo-updater-privkey` 存的是**两层 base64**。只解一层得到 348 字节
文本，其内容本身仍是 base64；直接解两层得到 258 字节 `untrusted comment: …` 原文，
而 `cargo tauri signer sign` 对它会报
`Error failed to decode base64 secret key: Invalid symbol 32, offset 9`。
也就是说 `TAURI_SIGNING_PRIVATE_KEY` 要的正是**解一层**的那个值——与
`scripts/notarize.sh:71` 完全一致。用该值签一个探针文件、把 `.sig` 再 base64 解码后
`minisign -V -x … -p <tauri.conf.updater.json 的 pubkey 解码> -m …`：

```
Signature and comment signature verified
Trusted comment: timestamp:1791943123	file:probe.bin
```

### 端到端验证：run 38498689210（publish=false）

`gh workflow run release.yml -f tag=v0.2.1 -f publish=false --ref main`，
落在 `cbb0b23`。7 个 job 全 `success`，`release` 按预期 `skipped`
（`if: github.event_name == 'push' || inputs.publish`），**没有产出任何 Release**。
<https://github.com/misswell/Sizqo/actions/runs/38498689210>

日志证据（arm64 job 115640244219 / universal job 115647722328）：

- `Collected 7 tools -> src-tauri/resources/bin (29 dylibs)`
  + `Collected 2 tools -> src-tauri/resources/video/bin (19 dylibs)` — 曾经致命的那一步过了
- `video/bin:\nffmpeg\nffprobe` — CI 产物里第一次带上视频引擎
- `Bundling …/bundle/macos/Sizqo.app.tar.gz` — `createUpdaterArtifacts` 经合并配置生效
- `==> Notarization credentials: App Store Connect API key (U7LW75MAWP)`
- `verified 58 / 58 / 59 Mach-O files`，`Authority=Developer ID Application: Guofeng Liu (U8U443D7ZL)` ×3
- `Current status: Accepted` ×3，`The staple and validate action worked!` ×6
- `release/`：`Sizqo_v0.2.1_macos_arm64.dmg` 29M、`_x86_64.dmg` 35M、`_universal.dmg` 63M、
  `_universal.app.tar.gz` 57M + `.sig`
- `Universal app created`，`Merge Universal app` 对 `Resources/video` 里的 ffmpeg 做了 lipo

### 顺带查出的线上缺陷：v0.2.0 的更新包没有引擎

对比本地 v0.2.0 的更新产物与按最终 app 重打包的结果：

| | 字节 | 条目 | 含 `Resources/{bin,lib,video}` |
| --- | --- | --- | --- |
| 已发布的 `Sizqo.app.tar.gz` | 4,432,774 | 7 | 0 |
| 装订后的 app 重打包 | 28,731,687 | 71 | 61 |

那 7 个条目只有 `Info.plist`、`Resources/icon.icns`、`MacOS/sizqo`。原因在
`scripts/notarize.sh`：旧的「1.5 生成在线更新产物清单」直接取构建期
`createUpdaterArtifacts` 的 tar.gz，而内置工具是**第 2 步**才拷进 bundle 的。
即 `latest.json` 指向的在线更新会装出一个没有任何压缩引擎的 app——DMG 不受影响。
已改为第 10 步（装订完成后 `tar` + `cargo tauri signer sign` + 生成 `latest.json`），
并按上表验证。**已发布的 v0.2.0 资产仍是坏的**，是否回填替换需要单独决定。

CI 路线本身没有这个缺陷：`sign_notarize_macos_ci.sh:192` 是在 staple 之后打包。

### 仍未做完

- 这轮 run 只证明链路能产出正确资产，**没有**对 CI 产物跑 GUI 验收矩阵；
  AGENTS.md 的发布门槛（三轨真实构建、沙盒运行时、fixture 视频矩阵、
  用户验收）仍未满足，所以 `publish=false`。
- FFmpeg（Homebrew 构建带 x264/x265）的 GPL **对应源码义务**还没有落地方案，
  公开分发前需要发布负责人/法务确认。
- `jollyfon/Sizqo` 上 2026-10-13 写入的 4 个 Secrets 现在没有任何消费方，
  其中包含真实 Developer ID 私钥与口令。建议删除（`gh secret delete`），
  但这是不可逆动作，留给用户执行。

## Release gate (per [AGENTS.md](../AGENTS.md))

Before any tag/Release: full three-track build, signed Direct runtime check,
signed sandbox check, real fixture video matrix (AGENTS.md §11 items 1–12),
FFmpeg licensing review. A local commit is not a GitHub push; every published
tag must link its artifacts here with the exact commands used.
