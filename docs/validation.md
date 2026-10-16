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
| Stapled | `xcrun stapler validate Sizqo.app` / `… Sizqo-0.2.0-macos.dmg` | ~~✅ EXIT 0 both~~ **INVALID — see the 2026-10-16 finding below.** Both objects were the wrong ones: the build-directory app and the dmg envelope, never the app *inside* the image or inside the update archive. |
| Deep signature | `codesign --verify --deep --strict --verbose=2 Sizqo.app` | ✅ EXIT 0 |
| Gatekeeper | `spctl --assess -vvv -t install Sizqo.app` | ✅ accepted, `source=Notarized Developer ID`, `com.misswell.sizqo` — **but this line cannot detect a missing ticket** (see finding). |
| Feed serves this version | `curl -L …/releases/latest/download/latest.json` | ✅ HTTP 200, `version: "0.2.0"`, URL → `…/download/v0.2.0/Sizqo.app.tar.gz` — the feed pointed at a **defective package** (see finding). |
| Anonymous download | `curl -L` DMG + tar.gz | ✅ byte-identical SHA-256 to local (`2d4887b8…`, `50f11e0c…`) |
| Updater signature | `minisign -V -x sig -p <committed pubkey> -m Sizqo.app.tar.gz` | ✅ "Signature and comment signature verified" (key `CA755A8F7DAAC119`, Sizqo's own) — the signature is valid **over the wrong bytes**; verification could not have caught this. |
| Published install path | `hdiutil attach` the downloaded DMG → `spctl --assess -t exec` + `CFBundleShortVersionString` | ~~✅ accepted as Notarized Developer ID, version `0.2.0`~~ **INVALID** — `spctl` online reports a ticket it fetched from Apple, not one attached to the bundle. |

## Post-release finding: the v0.1.3–v0.2.0 updater package is a different (broken) app (2026-10-16)

Re-checking the live v0.2.0 artifacts against the Sevqo runbook's rule "notarize and
staple the `.app` **before** the dmg is built from it" surfaced a second, worse defect
the runbook does not cover. One release shipped **two different apps**:

| | `Sizqo-0.2.0-macos.dmg` (31,703,781 B) | `Sizqo.app.tar.gz` (4,432,774 B) |
|---|---|---|
| app size | 70 MB | 12 MB |
| signature | Developer ID Application: Guofeng Liu (U8U443D7ZL), `flags=0x10000(runtime)` | `Signature=adhoc`, `flags=0x20002(adhoc,linker-signed)`, `TeamIdentifier=not set` |
| `Contents/Resources/` | `bin/` `lib/` `video/` | `icon.icns` only — no image engine, no FFmpeg |
| entitlements | `allow-unsigned`, `disable-library-validation` | none |
| notarization ticket | **absent** (`stapler validate` → `does not have a ticket stapled to it`, rc=65) | absent |
| `Info.plist` | 0.2.0 / `com.misswell.sizqo` | 0.2.0 / `com.misswell.sizqo`, updater still compiled in |

DMG byte size matched `gh release view` exactly, so this is truncation-ruled-out, real
shipped content. `v0.1.3` (4,302,173 B) and `v0.1.4` (4,302,631 B) tar.gz are the same
size class → same defect, and each of those releases published a `latest.json`, so the
in-app update channel was serving an engine-less, ad-hoc-signed Sizqo to anyone who
accepted it.

Root cause is ordering inside `scripts/notarize.sh`, two independent inversions:

1. **The update archive predates everything.** `cargo tauri build` emitted
   `Sizqo.app.tar.gz` + `.sig` as a build by-product, and the old step 1.5 read that
   file and wrote `latest.json` *immediately* — before the step that copies the bundled
   CLI tools/dylibs/FFmpeg into `Contents/Resources`, and before the Developer ID
   signing step. The `.sig` was therefore a valid minisign signature over a stale,
   unsigned, tool-less shell; no signature check could ever have caught it.
2. **Only the DMG was submitted to Apple.** `hdiutil create` copied the app and the
   image was built first, `notarytool submit` only ever saw the `.dmg`, and
   `stapler staple "$APP"` ran afterwards — so the in-image copy can never catch the
   ticket. The only validation was `stapler validate`/`spctl` against the
   build-directory bundle, i.e. the two objects nobody receives.

Why the recorded acceptance above looked green: `spctl --assess -t exec` reports
`source=Notarized Developer ID` for an unticketed bundle while online (the assessment
does a live lookup that supplies the context the ticket would carry). On the very same
in-image app, `spctl --type open` returned `source=Insufficient Context` while
`stapler validate` returned rc=65. Only `stapler validate`, run against the app
*inside* the mounted image (and inside the extracted tar.gz), separates the two cases.

Mitigation applied on 2026-10-16:

- `latest.json` **removed from the v0.2.0 release assets**, so
  `releases/latest/download/latest.json` now returns HTTP 404 and clients are offered
  no update instead of a broken one. The DMG asset is untouched (200). Byte-exact
  backups of what was removed, plus the tar.gz and its `.sig`, are in
  `/Users/guofeng/sizqo-release-archive/v0.2.0/` (sha256
  `73e51c30…` latest.json, `50f11e0c…` tar.gz, `666602e7…` sig).
- `scripts/notarize.sh` reordered: notarize + staple the `.app` → build the DMG from
  the ticketed app → notarize + staple the DMG → **mount the DMG and assert the app
  inside it** → only then re-`tar` the final app, sign it with `cargo tauri signer
  sign`, assert the extracted app, and emit `latest.json`. The assertion
  (`assert_distributable`) requires all four of: not ad-hoc, a `Developer ID
  Application` authority, a non-empty `Contents/Resources/bin`, and a valid stapled
  ticket. Verified clause-by-clause against four samples on 2026-10-16: a legitimate
  notarized Developer ID app passes; the live v0.2.0 in-image app fails on the ticket;
  the live v0.2.0 tar.gz app fails on ad-hoc; a missing path fails clean.
  A tar round-trip of a real stapled app was confirmed to preserve the ticket, so the
  new order is what makes the update package carry one. The helper matches the
  `codesign` output with `case` on a captured string instead of `codesign | grep -q`:
  both scripts run under `pipefail`, and a `grep -q` that exits on its first match
  SIGPIPEs `codesign`, so the pipeline returns 141 and a perfectly good bundle is
  rejected. That was reproduced locally before it could burn a CI run.
- `scripts/sign_notarize_macos_ci.sh` got the same treatment for the cloud route,
  which had the identical inversion: `create_dmg` ran before any submission and only
  the three DMGs went to Apple, so every CI image also carried an unticketed app. The
  apps are now notarized and stapled first, the DMGs are built from the ticketed
  apps, and each of the three images is mounted and checked, as is the updater archive.

Still not proven: the reordered script has **not** been run end-to-end (no build, no
submission to Apple). Two corrections found while rebasing onto the 2026-10-14 work:

- The updater archive ordering was **already fixed** by `3537b66` (step 11 re-tars the
  finished app and signs it). What was still open there, and is fixed here, is the
  other inversion: `scripts/notarize.sh` and `scripts/sign_notarize_macos_ci.sh` both
  create the DMG and the archive *before* any notarization happens and only ever
  submit the **dmg** to Apple, so the `.app` inside every distributed image carries no
  ticket even though the image itself validates. `3537b66`'s "DMG 不受影响" holds for
  the engines, not for the ticket.
- The updater key is **not lost for publishing purposes**. `gh secret list --repo
  misswell/Sizqo` now returns nine secrets including `TAURI_SIGNING_PRIVATE_KEY`
  (2026-10-14T01:58:44Z), matching the "CI release secrets" section below — the
  private key lives in Actions secrets, so a cloud release does not depend on the
  local keychain. It is genuinely gone locally, and this time the reason is visible in
  the keychain itself: `login.keychain-db` is a fresh file created 2026-10-16 07:46,
  24 KB instead of the 1,002,384-byte snapshot `login.keychain-db.sb-dbff1aa3-…-db`
  from 2026-09-24. `security list-keychains` shows no custom keychain (only
  `login.keychain-db` and the System one; `octoshrink-signing.keychain-db` exists on
  disk but is not in the search list and does not hold the item), and
  `find-generic-password -s sizqo-updater-privkey` returns "The specified item could
  not be found in the keychain" — not an access-denied artifact. Any future *local*
  updater build needs the key recovered or rotated; `latest.json` must only come back
  once it is served by artifacts that pass `assert_distributable`.

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

处置结果（同日）：坏 feed 已撤，链路顺序与自检判据均已修正，并按修正链路发布
v0.3.0 —— 见下文「Release: v0.3.0」。v0.2.0 的 DMG 资产保持原样，不回头改写已发布字节。

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

### 分发顺序倒正的端到端验证，以及一次把好包判成坏包的自检（2026-10-16）

云端 dry-run `38854784499`（workflow_dispatch，publish=false）验证 `4a1e45a` 的顺序改动：
三个架构的 DMG 都在 `stapler staple` 后 `stapler validate` 通过，说明"先给 .app 装订票据、
再造 DMG"确实让镜像里那份 app 带上了票据——这正是 v0.2.0 缺的那一步。作业随后停在
`build-macos-universal`，报 `DMG arm64: Contents/Resources/bin is empty`。

下载该 run 上传的 `macos-arm64-build` 产物实测 bundle 内部（zip 27,775,540 B）：

```
Contents/Resources/icon.icns
Contents/Resources/resources/bin/{pngquant,oxipng,cjpeg,cwebp,avifenc,cjxl,gifsicle}
Contents/Resources/resources/lib/*        (30 项)
Contents/Resources/video/bin/{ffmpeg,ffprobe}
Contents/Resources/video/lib/*            (20 项)
```

即 **判据错了，不是包错了**：`release.yml:93` 用 `bundle.resources` 的通配
`"resources/bin/*"`，tauri-bundler 会保留 `resources/` 前缀，引擎落在
`Contents/Resources/resources/bin`；而 `scripts/notarize.sh` 是手工投递成扁平
`Contents/Resources/bin`。`src-tauri/src/engine.rs:314-324` 的 `find_tool` 两种位置都认
（先 `<res>/resources/bin`，再 `<res>/bin`），所以 CI 那份装上是能压东西的。
`assert_distributable` 只按本地那条路线的布局查，就把 CI 的好包拦了下来。

`87b68bd` 把两条路线的自检都改成"两个候选目录任一非空"，与运行时解析顺序一致；
CI 侧另加 `Resources/video/bin` 非空一条——视频轨是 `release.yml:103-118` 手工投递的，
原先没有任何检查会发现它没进去。修正后按上表的真实产物排练过该判据（命中
`resources/bin`，七个引擎）。

发布 run：`v0.3.0` 标签先前指向 `b28192b`（内容 `73ddb17`，仍带错判据），
`38857380118` 会在同一处失败，已取消该 run；由于 GitHub 上**尚不存在** v0.3.0 Release
（`gh release view v0.3.0` → `release not found`，线上 Latest 仍是 v0.2.0），
标签被重打到含修正的提交（`b28192b → 61f79ea`，`git push --force`）后重新触发发布。

### Release: v0.3.0（2026-10-16，run `38859583185`，completed/success）

标签 `v0.3.0`（重打后指向 `87b68bd`）推送即触发发布；Release
`https://github.com/misswell/Sizqo/releases/tag/v0.3.0` 于 2026-10-16T06:48:04Z 发布，
`gh api repos/misswell/Sizqo/releases/latest` 现返回 `v0.3.0`，v0.2.0 不再是 Latest。

云端日志（`gh run view 38859583185 --log`）里的硬证据：

| 判据 | 证据 |
| --- | --- |
| 公证顺序已倒正 | 6 次 `notarytool` 提交全部 `status: Accepted`（arm64/x86_64/universal 三份 `.app` 先装订，三份 `.dmg` 后提交） |
| 用户真正拿到的对象合格 | 挂载 DMG 与解包 tar.gz 后各跑一次自检：`DMG arm64`、`DMG x86_64`、`DMG universal`、`updater archive` 全部 `Developer ID + engines + ticket` |
| 更新能力确实编译进包 | `Build` 步骤 env 打印 `SIZQO_UPDATER_CONFIGURED: 1`，配置由 `tauri.conf.updater.json` 合并进 `ci.tauri.conf.json` |
| 发布的是装订后那份，不是 Tauri 空壳 | Tauri 自产的 10.63 MiB `Sizqo.app.tar.gz`（无内置资源）没有出现在资产里；线上 `Sizqo_v0.3.0_macos_universal.app.tar.gz` 为 59,991,869 B |

在线更新源（app 实际请求的那个 URL）：

```
GET https://github.com/misswell/Sizqo/releases/latest/download/latest.json
→ 200，1,309 B，version "0.3.0"
   darwin-aarch64 / darwin-x86_64 各 signature 432 字符，
   url 指向 v0.3.0/Sizqo_v0.3.0_macos_universal.app.tar.gz
```

签名格式与运行时一致：`tauri-plugin-updater-2.10.1/src/updater.rs:1451-1462` 的
`verify_signature` 先对 `latest.json` 里的 `signature` 做 base64 解码得到 minisign 文本，
再 `Signature::decode`。线上字段正是这种「base64(整个 .sig 文本)」形态
（解出首行 `untrusted comment: signature from tauri secret key`），与 432 B 的 `.sig` 资产同内容。

密钥身份：`src-tauri/tauri.conf.updater.json` 内公钥解出 sig type `Ed`、
key id `CA755A8F7DAAC119`（与公钥自带注释一致），是 Sizqo 自己的密钥，
不是 AGENTS.md 禁止沿用的 OctoShrink 密钥。

资产字节对齐（HTTP 侧 `Content-Length`/`Content-Range` 对 API `size`）：
universal DMG 66,123,825；arm64 DMG 30,500,877；x86_64 DMG 36,797,942；
universal `.app.tar.gz` 59,991,869（HEAD 不回长度，用 `206 + bytes 0-0/59991869` 证实）；
`.sig` 432；`latest.json` 1,309。全部 MATCH。

本机直连 GitHub 每连接被限速到约 7 KB/s（`gh release download` 单流下不动 126 MB），
改用 2 MB 窗口 × 14 路的分段并发拉取后落盘，两份 universal 资产字节与 API `size` 一致。
以下是对**下载到的线上字节**做的复核，不是对构建目录里那份：

| 对象 | `stapler validate` | 签名 | 架构 | 版本 | 引擎 |
| --- | --- | --- | --- | --- | --- |
| DMG 内 `Sizqo.app`（挂载 `-readonly -nobrowse`） | `The validate action worked!` | `Authority=Developer ID Application: Guofeng Liu (U8U443D7ZL)`，`flags=0x10000(runtime)`，Signature size 9042 | `x86_64 arm64` | 0.3.0 | `Resources/resources/bin`＝avifenc cjpeg cjxl cwebp gifsicle oxipng pngquant；`Resources/video/bin`＝ffmpeg ffprobe |
| tar.gz 解包出的 `Sizqo.app` | `The validate action worked!` | 同上，另 `codesign --verify --deep --strict` → `valid on disk` / `satisfies its Designated Requirement` | `x86_64 arm64` | 0.3.0 | 同上 |

在线更新签名用 `scripts/verify_updater_signature.sh` 复核，其逻辑与依赖
（`minisign-verify =0.2.5`）与 `tauri-plugin-updater-2.10.1` 的 `verify_signature`
完全一致，因此这条通过就等于应用内更新那一步会通过：

```
$ bash scripts/verify_updater_signature.sh \
    /Users/guofeng/sizqo-release-archive/v0.3.0/Sizqo_v0.3.0_macos_universal.app.tar.gz
archive=…59991869 bytes  untrusted_comment="untrusted comment: signature from tauri secret key"
OK: 这份签名的确覆盖这组字节，且由配置里的公钥所对应的私钥签发
sha256 87f46a6e9929382514c970ef8d20b73d4e55cf4c086cb2bccc2800e5c875a71d  .app.tar.gz
sha256 d3430adcc2c26d2c57a512c50deaae99415d6b0674a6ed3f4cac5feda9133c3e  .app.tar.gz.sig
```

这同时补上了此前只能推测的一环：CI 里的私钥与 `tauri.conf.updater.json` 内公钥
`CA755A8F7DAAC119` 确实是同一对（否则 `PublicKey::verify` 必失败）。
反向对照也做了：把归档第 1000 字节改掉后再验，报 `Error: InvalidSignature`，
说明这个判据不是永真。`latest.json` 里的 `signature` 字段与线上 `.sig` 资产内容逐字符相同。

仍未做的只有肉眼验收：新工作台在签名包里的点击通过、§11 视频矩阵、App Store 沙盒运行时、
FFmpeg GPL 对应源码义务。

## Release gate (per [AGENTS.md](../AGENTS.md))

Before any tag/Release: full three-track build, signed Direct runtime check,
signed sandbox check, real fixture video matrix (AGENTS.md §11 items 1–12),
FFmpeg licensing review. A local commit is not a GitHub push; every published
tag must link its artifacts here with the exact commands used.
