# Validation record

This file records the observed build/test/runtime status of the three Sizqo
tracks. Rules: a feature is "supported" only with a command and its real
outcome recorded here; pending or blocked work is never reported as passed.
Update this file whenever a track's capability changes.

Last updated: 2026-10-31

## Automated checks

| Check | Command | Result |
|-------|---------|--------|
| Rust tests, Direct features | `cargo test` (in `src-tauri`) | ✅ 237 unit + 11 integration passed (2026-10-31, twice for stability) |
| Rust process-level crash injection (SIGKILL × 6 phases + idempotent re-recovery) | `cargo test --test crash_injection` | ✅ passed (2026-10-31; also green under both inproc feature sets) |
| Rust real-media matrix (bundled ffmpeg) | `cargo test --lib real -- --ignored` (in `src-tauri`) | ✅ 2 suites passed (2026-10-31) |
| Rust tests, shared in-process engines | `cargo test --features inproc-backends` | ✅ all passed (2026-10-31) |
| Rust tests, App Store feature set | `cargo test --no-default-features --features inproc-backends` | ✅ all passed (2026-10-31) |
| Dual-engine contract vectors (`tests/contracts/*.json`, executed by Rust AND Swift) | `cargo test --lib rust_executes_every_contract` + `bash scripts/test_swift_history.sh` [41] | ✅ 29 vectors × 2 engines identical (2026-10-31) |
| Frontend suites incl. virtualization + perf benchmarks | `npm run test:frontend` (29 suites) | ✅ all pass (2026-10-31) |
| Queue perf benchmark 1k/5k/10k | `node tests/perf-benchmark.cjs` | ✅ 10k: first=11.7ms, clean path=3.4ms, window rows=20, resort=10.9ms |
| Swift history / backup / pause / queue / settings / result-contract / contract-vectors self-checks | `bash scripts/test_swift_history.sh` | ✅ all pass (2026-10-31) |
| Swift full-source typecheck | `xcrun swiftc -typecheck …` all sources except SizqoApp.swift | ✅ EXIT=0 (2026-10-31). SizqoApp.swift carries 4 pre-existing strict-concurrency errors identical at HEAD `3891103`; the AppDelegate `@MainActor` fix added in this round makes the real build pass |

## 0.3.6 file-safety overhaul (working state, unreleased)

Baseline `3891103`. Implementation of the 0.3.6 file-safety / transaction /
video-integrity plan. Items below are recorded with the tests that prove them;
anything not listed as passed is **not** passed.

### Passed (with evidence)

- **统一身份采集（§2.1）**：`SourceIdentity::read_with_hash` 改为 fd 基采集
  （打开的文件描述符上 fstat → 哈希 → 再 fstat，前后不一致报 `SourceError::Unstable`）。
  测试 `read_with_hash_captures_identity_through_one_fd`。
- **视频覆盖线接入内容指纹（§3.2）**：replace 模式视频用 `read_with_hash`，
  备份提前到编码前（失败即任务失败，不烧几小时 CPU 才发现），commit 前完整
  verify + 备份五态复核；视频事务 `originalSha256` 不再为空。
- **备份五态复用判定（§2.3）**：`backup_reuse_decision`（Verified /
  LegacyUnverified / ContentMismatch / Missing / Unreadable）。legacy 备份
  无证据不再静默复用；凭历史 `sourceSha256` 证据自动升级为 v4 来历记录；
  复用判定拒绝的旧槽原样保留。测试
  `a_legacy_backup_without_any_evidence_is_not_silently_reused`、
  `a_backup_whose_content_no_longer_matches_its_record_is_refused`、
  `a_backup_made_under_the_legacy_hash_key_is_still_reused`（升级路径）。
- **备份写入后源复核（§2.2）**：快照复制完成后整份重读源文件复核身份，
  复制期间被改则快照作废、目录清除、覆盖被拦。故障注入测试
  `a_snapshot_is_aborted_when_the_source_changes_during_backup`
  （`TEST_FAULT_EDIT_SOURCE_AFTER_COPY`，仅测试可见）。
- **事务 V2（§3.1）**：`schemaVersion=2` + `backupSha256`；prepare 前凭证
  完整性门（覆盖缺三指纹之一 / 新建缺产物指纹 → 拒绝落账，legacy 不受限）。
  测试 `a_v2_journal_without_full_credentials_is_refused_at_prepare`。
- **原子交换提交（§3.3）**：`publish_replace_if_identity_matches` ——
  macOS `renameatx_np(RENAME_SWAP)` / Linux `renameat2(RENAME_EXCHANGE)`，
  交换后验证**被换出的**字节，不一致换回；换回失败保留现场并改名
  `*.sizqo-conflict-N`。不支持的平台回退 verify+rename（与旧两道门同级）。
  图片同格式 replace 与视频 replace 均已接线。测试
  `publish_replace_swaps_and_verifies_the_swapped_out_bytes`、
  `publish_replace_swaps_back_when_the_target_is_not_the_expected_original`。
- **备份凭证回滚（§3.1）**：v2 事务回滚前验备份字节与凭证一致，不符拒绝
  自动恢复、保留现场。测试 `rollback_verifies_the_backup_voucher_before_writing_back`。
- **legacy 事务恢复语义（§3.4）**：无凭证老日志不再无条件回滚（旧契约会盖掉
  用户崩溃后写入的内容）。尽力识别：源==备份字节 → 只清产物；其余 →
  `NEEDS_REVIEW` 前缀错误，recover 计入 `needs_review` 并保留现场。
  测试 `legacy_logs_are_identified_as_far_as_possible_and_never_clobber_unknown_content`、
  `recovery_is_idempotent_when_run_twice_on_the_same_scene`。
- **跨格式产物清理收紧**：legacy 无指纹日志只删"大小恰为承诺产出"的文件，
  不再无条件删。图片线 / Swift 线同步。
- **退出清理引用集（§2.3）**：`sweep_unreferenced_backups` 引用核对补入
  未完成事务的备份 key（`pending_backup_keys`，Rust 与 Swift 同步）。
- **恢复五态验证（§2.3/§4.2）**：Rust `restore` 与 Swift `restore` 在写回前
  按五态验备份；legacy 需显式确认（Rust `accept_legacy` 命令参数 +
  前端确认弹窗 + i18n；Swift 弹窗同语义）；`force` 不绕过备份验证。
  测试 `restore_requires_confirmation_for_legacy_backups`（Rust）、
  Swift 自检全套、`tests/history-view.cjs`（含 acceptLegacy 契约断言）。
- **Swift 对齐（§4.1–§4.4）**：HistoryEntry 增 `outputSha256/backupSha256/
  sourceSha256`（decode-if-present）；`conflictState` 五态（有指纹按内容判，
  时间戳不再单独定罪）；Maintenance Lease（`beginMaintenance` RAII，
  restore/restoreAll/clearHistory 持租约，与 tryBeginBatch 同锁仲裁）；
  占位 `(dev,ino)` 凭证落账并在 retire 时对账（空占位无凭证不删）；
  事务 V2 字段与凭证门；legacy 恢复语义与 Rust 相同；
  记账时写入产物/备份指纹。Swift 自检全部通过。
- **运行中禁止移除（§7.3）**：Swift `removeItem` 对 running 显示
  「当前文件正在压缩，无法移除」，不再把"从队列隐藏"当成移除。
- **视频输入能力分类器（§5.1/§5.2）**：新增 `video/format_classifier.rs`，
  严格白名单（8-bit SDR 精确位深；9/10/12/16-bit、alpha、float、未知格式
  一律拒绝并说明）；HDR 按传递函数（PQ/HLG/DOVI/HDR10+）判定，
  BT.2020 原色单独拒绝并说明色彩空间原因。probe 不再把 BT.2020 原色当 HDR。
- **VFR 保留源 PTS（§5.3）**：默认 `fps_mode passthrough`，不再自动归一化为
  平均帧率 CFR；只有用户明确指定帧率才 CFR。
  `core/media-task-protocol.md` 已同步。真实夹具测试（v0.3.2 真实文件链）
  改为证 passthrough 语义。
- **输入严格解码（§5.5）**：编码 argv 输入侧加 `-err_detect explode` +
  全局 `-xerror`；损坏输入在解码层报错，不能被容错吞成"压缩成功"。
- **输出验证契约（§5.4）**：时长容差按帧间隔（2 帧，250ms 下限）计算，
  百分比阈值删除（2 小时丢 60 秒必失败）；输出视频流数必须恰为 1；
  源里明确写过的 color transfer/primaries 输出不许丢。
- **元数据隐私全作用域（§6）**：probe 增加 `-show_programs` 与
  `programs` 字段；StripAll argv 增加流级/章节级/program 级清空
  （`-map_metadata:s/c/p -1`）；StripPrivate 与 StripAll 的**验证**遍历
  全局+每流+每章节+每 program；muxer 结构标签豁免清单扩展（DURATION 等）。
  真实编码夹具测试 `metadata_policies_hold_on_real_encoded_fixtures` 通过。
- **批量视频预检（§7.2）**：新命令 `validate_video_batch`（一次 IPC 整批、
  逐文件独立作答）；前端本地判据先筛 + 批量结果标记失败。
- **前端预检竞态（§7.1）**：`preflightToken` 代次 + 授权弹窗/批量校验每个
  await 后复核 + `beginExecutionSession` 前重筛"仍在队列且 pending"；
  clearAllFiles/startNextRound 作废预检。测试 `tests/video-batch-isolation.cjs`
  （批量语义）、`tests/queue-progress.cjs` 等全套通过。
- **导入扫描加固（§8.1）**：目录递归不跟随越出根的符号链接；探测 worker
  panic 时该块文件按"探测失败"入队（带原因），其余块照常回收，整批不再
  静默消失。

### Round 2 completions (2026-10-31, same working state)

- **万级队列虚拟列表 + 排序脏标记**：`applyQueueView` 拆为"排序（脏检查）"与
  "附着（窗口化）"两段；>200 项且存在真实滚动容器时只附着一屏 ±20 行
  （垫片撑滚动高度），行 DOM 与业务状态彻底分离；进度事件连 O(N) map 都
  不跑。专项测试 `tests/queue-virtualization.cjs`（10,000 项 → 窗口 ≤150 行）
  与 `tests/perf-benchmark.cjs`（1k/5k/10k：10k 干净路径 3.4ms、首次 11.7ms、
  全量重排 10.9ms；对照旧语义全量渲染 250ms）已进入 `npm run test:frontend`。
- **tests/contracts/ 双引擎契约向量**：6 个 JSON 文件 29 条场景
  （file_identity / backup_verification / history_conflict /
  transaction_recovery / output_retirement / lifecycle_gate），Rust 执行器
  `src/contract_vectors.rs` 与 Swift 执行器
  `swift/Tests/HistoryStoreCheck/ContractVectors.swift` 对同一份期望作答。
  上线即抓到并修复一处真实引擎分歧：Swift `BackupMeta` 合成 Codable 读不了
  v1 legacy 记录（Rust serde defaults 可以）→ 补 decodeIfPresent 解码 +
  显式成员构造器。含 manual_restore 两条向量。
- **进程级崩溃注入**：`src-tauri/tests/crash_injection.rs`
  （`harness = false`）——真实测试二进制在 6 个阶段
  （备份后 / 记账后 / 临时产物后 / rename 后 / 历史落盘后 / legacy rename 后）
  被 SIGKILL，父进程核对终态不变量并**二次恢复**证明幂等；三个特性矩阵全过。
- **人工恢复 GUI**：Rust `list_pending_recoveries` /
  `resolve_pending_recovery`（维护租约 + `confirmOverwrite` 确认门），
  `output_transaction::describe_pending` / `manual_restore`；前端启动拉取、
  横幅列出"当前文件 vs 原图备份"双方状态，恢复需确认弹窗（凭证不符时警告
  加倍），i18n 全配；Swift 侧 `pendingRecoveryItems` /
  `manualRestore` + `ManualRecoveryBanner`（SwiftUI 横幅 + 确认 alert），
  全源 typecheck（除既有 app 入口问题）0 诊断。
- **三轨真实构建（2026-10-31，不签名构建 + 开发签名核验）**：
  - Direct：`scripts/build_all.sh` → `Sizqo_direct.app`（70M，ffmpeg/ffprobe
    与 CLI 工具/dylib 齐备），Apple Development 身份签名，
    `codesign --verify --deep --strict` 通过。
  - App Store：`cargo tauri build --features appstore --config
    tauri.conf.appstore.json` → `Sizqo.app`；bundle 自检通过（无 bin/lib/video
    违规夹带）；**Apple Distribution**（U8U443D7ZL）+ app-sandbox entitlements
    签名，`--verify --deep --strict` 通过，entitlements 含 app-sandbox /
    user-selected.read-write / bookmarks.app-scope / network.client。
  - Swift：`scripts/build_swift.sh` → `Sizqo_swift.app`（24M）。
    修复既有 MainActor 严格并发错误（AppDelegate 标注 `@MainActor`，
    与 HEAD `3891103` 对比确认是工具链更新暴露的既有问题）。
  - 构建事故记录：App Store 线两次 E0463 `equator_macro`——既有的
    Cargo 1.90 panic=abort 规避不完整，真正根因是**指纹与产物错配的陈旧
    proc-macro 缓存**；定向清除 `target/release/{deps,.fingerprint}/equator*`
    后重建即通过（脚本注释补充在
    `docs/validation.md` 本节，供后续参考）。
- **FFmpeg GPL 复核（0.3.6）**：`THIRD_PARTY_NOTICES.md` 已记录当日实测
  `ffmpeg -version` 配置行；结论不变（libx264/libx265 → GPL-2.0-or-later，
  源义务随发布页提供）；本轮代码改动只影响调用方式，不改变捆绑内容。

### Review round (2026-10-31 晚, rc.2)：外部审查修复

上一轮的 rc.1 tag 触发了远端 CI；`compression-parity` 失败根因是版本门禁
（tag v0.3.6-rc.1 vs app version 0.3.5）——本轮把版本统一到 `0.3.6-rc.2`。
审查确认的问题与修复（全部带回归测试）：

- **P0 虚拟列表 spacer 顺序**：垫片先双双 append、行再 append 到底部之后
  → 真实 DOM 顺序错乱。修复：摘除窗口外行后按「top → 行 → bottom」
  重新落位（appendChild 移动语义），`ensureQueueSpacer` 对已存在的垫片
  也执行移动；测试断言真实子节点顺序与滚动定位数学。
- **P0 快照基线绑定**：`write_verified_snapshot` 此前不比对调用方
  （压缩流程）的基线指纹 → A→B→A 场景下备份留住的可能是中间字节 B。
  修复：快照开始时 `pre.sha256 == baseline` 否则终止（Rust+Swift）；
  契约向量 `snapshot_refuses_when_baseline_differs_from_current_bytes`。
- **P0 目录 fsync 链**：备份 rename 后/meta 写入后/恢复写回后/覆盖提交后/
  journal 写入后补齐父目录 fsync（Rust `sync_dir` + Swift `syncDirectory`）。
- **P1 legacy 备份可信度**：`ReplaceTransaction.backupLegacyUnverified`
  标记随事务落账；自动回滚对 legacy 备份一律转人工（两个写回分支），
  现场哈希不再能把它洗成可信原图；契约向量
  `legacy_backup_voucher_not_auto_trusted`。
- **P1 Swift 占位所有权**：`publishIfPlaceholderMatches` ——
  `renameatx_np(RENAME_SWAP)` 先交换、再验证被换出的 inode 是我们认领的
  占位，不一致换回原状；`releaseOutputPlaceholder` 不再盲删；导出路径
  同款门；行为测试覆盖"外部顶掉占位绝不被盖"。
- **P1 Rust 冲突检测五态**：`conflict_state` 与 Swift 同契约，
  **Unreadable 一律拦截**（旧逻辑退回 size+mtime 会放行"读不出来"的文件）；
  chmod-000 回归测试。
- **P1 产物删除身份门**：`remove_generated_output` 删除前按产物指纹对账
  （legacy 按尺寸有界），`already_restored` 幂等分支同样受门保护；
  回归测试"幂等恢复不删用户编辑过的跨格式产物"。
- **P1 人工恢复三问题**：备份读不出来 → `VoucherMismatch`（进 GUI 列表）；
  凭证不符分两层确认（`confirmVoucherMismatch`，读不出来永远拒绝）；
  `resolve_pending_recovery` 接入 FileAccess 沙盒授权并按 `txn.id` 销账；
  前端警告文案写明"继续即接受风险"。
- **P1 色彩一致性**：transfer/primaries/space 三个字段的值必须逐字相等
  （bt709 → smpte2084 的"换标签"判失败，新增回归测试）。
- **P2 虚拟列表复杂度**：可见序列缓存（滚动路径复用，排序/筛选变化才重算）
  + `fileRows` LRU（上限 600，detached 行逐出交 GC）；基准新增
  "滚动重附不随 N 线性恶化"断言；10k 干净路径 3.4ms → 0.63ms。
- **P1 崩溃注入生产路径**：备份改走 `ensure_backup_verified` 真实快照链、
  覆盖改走 `publish_replace_if_identity_matches` 真实原子交换；
  补齐 legacy+源缺失（自动恢复，无内容可覆盖）与备份读不出来
  （VoucherMismatch + 回滚拒绝）两个缺失场景。
- **并行 flake 根因**：`TEST_FAULT_EDIT_SOURCE_AFTER_COPY` 全局静态会在
  并行测试间互相注入（真实踩到：25 字节 `user-edited-during-backup` 写进
  邻测试的源文件）→ 改 thread-local，5 连绿确认根除。

### Not done (never report as passed)

- **Developer ID 签名与公证**：本机钥匙串没有
  「Developer ID Application」证书（仅有 Apple Development / Apple
  Distribution），Direct 分发签名与 notarization 无法执行——凭据级阻塞，
  需要用户在装有该证书的环境执行 `scripts/notarize.sh`。
- **GUI 实际运行验收**（三轨界面点击、App Store 沙盒实际运行）：项目规则
  禁止静默启动 GUI，未执行——留给用户按验收矩阵操作。
- **真实旧版升级实测、真实大容量视频矩阵在慢卷/外置盘上的运行**：未验证。
- **Windows / Linux 构建**：未执行（本机为 macOS arm64；两平台的
  cross-build 需要各自环境）。
- **发布动作本身**（tag/Release/上传）：门禁未齐前禁止，未执行。

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

### Release: v0.3.1（2026-10-20，run `39417008495`，completed/success）

内容：图片与视频输出方式分开配置（`c543137`），版本提交 `e369506`。标签 `v0.3.1`
推送即发布；Release `https://github.com/misswell/Sizqo/releases/tag/v0.3.1`
于 2026-10-20T12:26:44Z 创建（非 draft / 非 prerelease，Latest）。

本地门禁（全部 EXIT=0）：

| 命令 | 结果 |
| --- | --- |
| `npm run test:frontend` | 18 个脚本全过，含新增 `tests/output-mode-split.cjs` |
| `cargo test` | 6 个集成测试 + 单测/文档测试全过 |
| `cargo test --features inproc-backends` | 全过 |
| `cargo test --no-default-features --features inproc-backends` | 全过 |

云端（`gh run view 39417008495 --log`）：

- 8 个 job 全部 success；`SIZQO_UPDATER_CONFIGURED: 1`。
- 6 次 notarytool 提交全部 `status: Accepted`；3 份 `.app` 与 3 份 DMG 的
  staple/validate 均 worked。
- 自检四行全过：`DMG arm64 / DMG x86_64 / DMG universal / updater archive: Developer ID + engines + ticket`。

线上资产（`gh api repos/misswell/Sizqo/releases/latest`，单位 B）：
universal DMG 66,738,662；arm64 30,499,972；x86_64 36,797,003；
universal `.app.tar.gz` 59,990,809；`.sig` 432；`latest.json` 1,309；
Linux AppImage 87,898,616；deb 5,094,136；Windows exe 2,949,501；msi 3,796,992。
`latest.json` 的 `version` 为 `0.3.1`，两个 darwin 平台的 `url` 都指向
`v0.3.1/Sizqo_v0.3.1_macos_universal.app.tar.gz`，`signature` 字段与 432 B
的 `.sig` 资产逐字符相同（432 字符的 base64）。

下载线上字节复核：

| 对象 | 结果 |
| --- | --- |
| universal DMG（挂载） | 校验和通过；`Sizqo.app` 0.3.1、`x86_64 arm64`、Developer ID + runtime、`stapler validate` worked（DMG 自身也 worked） |
| DMG 内引擎 | `Resources/resources/bin` 7 个图片 CLI；`Resources/video/bin` 2 个（ffmpeg/ffprobe） |
| universal 更新包 | `tar -xzf` 后同样 0.3.1/universal/Developer ID + ticket + 9 个引擎 |
| 更新签名 | `scripts/verify_updater_signature.sh <绝对路径>` → OK；改第 1000 字节后 → `Error: InvalidSignature` |

```
sha256 9b0ee8d4159256cd7283ec988155d39301ad6059e5fdc4cc9123736cb591feab  .app.tar.gz
sha256 65074217eba733c45d1c425612fb210fe665400657b9bdcedd71305ae4d65481  .app.tar.gz.sig
```

仍未做（与 v0.3.0 相同）：签名包内新工作台的肉眼点击验收、§11 fixture 视频矩阵、
App Store 沙盒运行时、FFmpeg GPL 对应源码义务。本轮按用户「做完就发版」的常驻规则
发布补丁版本，未把上述未验证项标成已通过。

### Release: v0.3.4（2026-10-21）

起因：用户提供的 `video-logs/1792550965656402000-0-21b3272905bf3349.log`。
源 `录制于 2026-10-13 19.59.33.mp4`（10,002,379 B）探得 `duration=30.636667`、
`avg_frame_rate=130500/9191`（14.199 fps）、`r_frame_rate=89/6`（14.833 fps）、
435 帧；最后一个视频包 PTS 30.010、duration 0.626667。FFmpeg 编码本身 exit 0，
但 passthrough 输出止于 30.067 s；`validation.rs` 的时长容差是
`max(1%, 250 ms)=306 ms`，差 569 ms 被拒，任务按失败清理临时文件。

本地复现（bundled FFmpeg，libx265 CRF 28、4K、threads 3、音频 copy）：

| 时间戳策略 | 输出 format duration | 帧数 | 与源差 |
| --- | --- | --- | --- |
| `-fps_mode passthrough` | 30.067416 | 435 | −0.569251（超 306 ms 容差） |
| `-fps_mode cfr -r 130500/9191` | 30.636667 | 435 | 0 |
| `hevc_videotoolbox` + 同一 CFR 参数 | 30.636667 | 435 | 0（dup=15/drop=15） |

修复（`17e1ab8`）：probe 增加结构化 `variableFrameRate`；build 在用户未选帧率且源为
VFR 时记录 `vfrRate`；argv 使用 `-fps_mode cfr -r <avg_frame_rate>`。显式帧率与 CFR 源
仍走原路径（`-r` / passthrough）。前端选项改为「自动（保持时长）/ VFR」。

本地门禁（全部 EXIT=0）：

| 命令 | 结果 |
| --- | --- |
| `npm run test:frontend` | 18 个脚本全过 |
| `cargo test` | 单测 113 + 集成 6 全过 |
| `cargo test --features inproc-backends` | 单测 114 + 集成 6 全过 |
| `cargo test --no-default-features --features inproc-backends` | 单测 114 + 集成 6 全过 |
| `bash scripts/test_swift_history.sh` | 全过 |
| `bash scripts/test_sign_notarize_macos_ci.sh` | 桩环境成功/失败两路径全过 |

新增回归：probe 的 VFR/CFR 识别两例；plan 的
`variable_frame_rate_is_normalized_only_without_explicit_fps`
（VFR→`cfr`+有理帧率、CFR→`passthrough`、显式 24 fps→仅 `-r`）。

发布插曲（两次 macOS universal 作业失败，标签均不复用）：

- v0.3.2（run `39536720265`）：x86_64 的 `hdiutil create` 瞬时返回 `Resource busy`，
  而 `create_dmg` 的失败又被命令替换末尾的 `echo` 吞掉，脚本继续把不存在的 DMG 送公证。
  `a2ac6c8` 增加三次重试、文件存在性检查和显式失败传播。
- v0.3.3（run `39538762236`）：DMG 全部创建成功，但 `hdiutil create` 的
  `created: …` 进度行写在 stdout，被 `ARM_DMG="$(create_dmg …)"` 一起捕获，路径变成
  两行；公证前检查因此报
  `Missing DMG before notarization: created: …/Sizqo_v0.3.3_macos_arm64.dmg`。
  其余 6 个作业全绿，`release` 跳过。修复：`hdiutil create … >&2`，并新增
  `scripts/test_sign_notarize_macos_ci.sh` 用桩命令跑完整发布脚本。
  旧脚本（`a2ac6c8`）在该测试下稳定复现上述报错并以 1 退出；新脚本成功路径产出
  三个 DMG 与更新包，失败路径（`hdiutil` 恒失败）在三次重试后以非零退出，日志含
  `DMG was not created`，且不会进入公证。

最终以 0.3.4 发布：tag `v0.3.4`（annotated `bdfce9e`，peeled commit `b680892`），
run `39541875940` 的 8 个作业全部 success。Release 于 2026-10-21T04:38:02Z 创建，
非 draft / 非 prerelease，为 Latest。

云端签名证据（`gh run view 39541875940 --log`）：

- 6 次 notarytool 提交全部 `status: Accepted`：3 个 `.app` + 3 个 DMG。
- 3 个 `.app` 与 3 个 DMG 的 staple/validate 均 worked。
- 自检四行全过：`DMG arm64 / DMG x86_64 / DMG universal / updater archive:
  Developer ID + engines + ticket`。
- `Create signed DMGs from the ticketed apps` 之后出现三条 `created: …`，没有
  `Missing DMG before notarization`，说明 `hdiutil` 进度行已与产物路径分离。

线上资产（`gh api repos/misswell/Sizqo/releases/latest`，单位 B）：
universal DMG 66,122,998；arm64 30,500,450；x86_64 36,798,031；
universal `.app.tar.gz` 59,991,983；`.sig` 432；`latest.json` 1,309；
Linux AppImage 87,902,712；deb 5,093,550；Windows exe 2,950,080；msi 3,796,992。
`latest.json` 的 `version` 为 `0.3.4`，两个 darwin 平台的 `url` 都指向
`v0.3.4/Sizqo_v0.3.4_macos_universal.app.tar.gz`，`signature` 字段与 432 B
的 `.sig` 资产逐字符相同（432 字符的 base64）。

下载线上字节复核：

| 对象 | 结果 |
| --- | --- |
| universal DMG（挂载） | `hdiutil verify` 校验和 VALID；`Sizqo.app` 0.3.4、`x86_64 arm64`、Developer ID + runtime、`stapler validate` app 与 DMG 自身都 worked |
| DMG 内引擎 | `Resources/resources/bin` 7 个图片 CLI；`Resources/video/bin` 2 个（ffmpeg/ffprobe） |
| universal 更新包 | `tar -xzf` 后同样 0.3.4/universal/Developer ID + ticket + 9 个引擎 |
| 更新签名 | `scripts/verify_updater_signature.sh <绝对路径>` → OK；翻转第 1000 字节后 → `Error: InvalidSignature` |

```
sha256 a6c080c31d7397994dd7068d411a44161824c19937769ac6f17de7bdef7fff92  .app.tar.gz
sha256 92fa8ab358a71537e8f9b133cab5cdb6f358d0a1b7c96284a43921dc1ac4d6c2  .app.tar.gz.sig
```

仍未做（与 v0.3.1 相同）：签名包内新工作台的肉眼点击验收、§11 fixture 视频矩阵、
App Store 沙盒运行时、FFmpeg GPL 对应源码义务。本轮按用户「做完就发版」的常驻规则
发布补丁版本，未把上述未验证项标成已通过。
## 压缩事务重构：结果契约与提交边界（2026-10-21，Phase 1–4.5）

按「全链路状态、压缩事务与处理逻辑重构方案」（§138/§139）执行到第四阶段。
本轮只动结果契约、提交事务与会话语义；视频、历史/恢复、UI 与集成阶段尚未开始。

### 这一轮改了什么

- **结果契约唯一化**：Rust / Swift / 前端都只读 `outcome` + `reasonCode`；
  「有没有产物」只由 `has_product()` / `hasProduct` 一处回答。
  `CompressResult.success` 已删除（§139 第四阶段）：历史从未持久化它、三轨同包发布，
  不存在跨版本兼容需求。`EngineResult.success` 保留为引擎内部判据，
  恢复命令的 `RestoreOutcome.success` 是命令响应旗标，二者都不属于结果契约。
- **四步分离**：Encode → Evaluate → Commit → Publish。`Evaluation::evaluate`
  是唯一回答「这些字节值不值得落盘」的地方；没有落盘就不许预先写结论。
- **提交事务**：随机临时文件名；统一 `OutputNameAllocator`（后缀/目录/转换共用，
  绝不覆盖既有文件）；`SourceIdentity` 提前备份 + 改名前 SHA-256 复核；
  取消在 Encode 前与 Commit 前两处阻断；失败回滚保留日志与备份。
- **会话语义**：图片/视频/混合入口统一 `try_begin_batch`；`sourceRoot` /
  `relativePath` 在入队时定死；重试保持队列项身份且进度不倒退；
  会话期间禁用恢复/删除历史产物。
- **收紧后的缺省读法**：没有 `outcome` 的载荷 = 没有产物、队列生命周期 `failed`、
  日志状态 `unknown`。不再按 `outputPath` 反推成功（同时满足 §140 的禁令）。

### 本轮实测（全部 EXIT=0，2026-10-21）

| 命令 | 结果 |
| --- | --- |
| `cargo test`（`src-tauri`，Direct 默认特性） | ✅ 157 个单测 + 6 个集成测试 |
| `cargo test --features inproc-backends` | ✅ 158 个单测 + 6 个集成测试 |
| `cargo test --no-default-features --features inproc-backends` | ✅ 158 个单测 + 6 个集成测试 |
| `npm run test:frontend` | ✅ 23 个脚本全过 |
| `bash scripts/test_swift_history.sh` | ✅ 自检 [1]–[44] 全过，含结果契约 [33]–[36]、随机临时名 [37]、输出命名/事务 [38]–[39]、源身份 [40]、会话唯一 [41]、来源根 [42]、重试 [43]、会话期恢复禁令 [44] |

新增/强化的关键回归：

- Rust `new_result_fields_are_camel_case_and_optional`：序列化不再出现 `success`，
  旧载荷缺少 `outcome` 时 `has_product() == false`。
- 前端 `tests/no-product-status.cjs`：六种结论的呈现、无结论不当完成，
  并加源码守卫禁止契约文件再读 `result.success` 或保留 `legacyHasProduct`。
- Swift 自检 [34]–[36]：无结论的载荷不产出、不冒充完成；
  三轨字面量必须同源；视图不得另立 `success` 判据。

### 变异验证（故意改坏，确认测试真的会响）

- Rust：把 `CompressResult::has_product` 的 `None => false` 改成
  `None => self.output_path.is_some()`，
  `new_result_fields_are_camel_case_and_optional` 失败并报出
  「没有结论的旧载荷不许拿 outputPath 猜产物」；恢复后 `diff` 字节一致。
- JS / Swift：同样把「没有结论」分支改成按 `outputPath` 猜产物，
  各自触发对应断言失败；恢复后 `diff` 字节一致。

### 仍未跑的门禁

三轨真实构建与签名运行时、App Store 沙盒运行时、AGENTS.md §11 的 fixture 视频矩阵、
FFmpeg GPL 对应源码义务均未在本轮执行，不能据此认为发布门禁已通过。
本轮改动未提交、未推送、未发版。

## 压缩事务重构：历史 / 恢复 / UI / 集成（2026-10-22，Phase 5–9）

按同一份方案执行完 Phase 5–9：图片能力与 IO 错误、视频元数据与校验、
历史/恢复/批量、UI 契约、全量回归。本轮仍**未提交、未推送、未发版**。

### A. 修改文件（按层）

- **Rust 结果与提交**：`result_model.rs`、`engine.rs`、`engine_inproc.rs`、
  `commands.rs`、`output_transaction.rs`、`source_identity.rs`、`history.rs`、
  `image_capabilities.rs`、`image_memory.rs`、`source_root.rs`、`system_image.rs`、
  `video/{mod,plan,probe,validation,transaction,ffmpeg_backend}.rs`、
  `video_commands.rs`、`lib.rs`。
- **前端**：`app.js`、`result_state.js`（新增，结果契约唯一解释器）、`i18n.js`、
  `intents.js`、`video-settings.js`、`compare_window.js`、`index.html`、
  `compare.html`、`style.css`。
- **Swift**：`ResultContract.swift`、`ImageCapabilities.swift`、
  `SourceIdentity.swift`、`ReplaceBaseline.swift`（新增），以及
  `CompressionEngine`、`SystemImageConverter`、`CompressOptions`、`CompressResult`、
  `CompressionScheduler`、`HistoryStore`、`OutputTransactionStore`、`AppState`、
  `CompareView`、`ContentView`、`HistoryView`。
- **测试**：新增 `tests/bulk-operations.cjs`、`tests/ui-contract.cjs`、
  `tests/round-exit.cjs`、`tests/source-root.cjs`、`tests/output-dest.cjs`；
  强化 history / no-product-status / compression-session / video-contract 等既有套件。

### B. 状态机

| 状态 | 进入条件 | 允许的下一步 |
| --- | --- | --- |
| `pending` | 导入成功、或从 `restored` 重新压缩 | 会话开始 → `running`；移出队列 → `removed` |
| `running` | 后端 `starting` 事件（同一 `sessionId` + `queueRevision` + 队列对象身份） | `completed` / `failed` 事件 → `done` 或 `failed` |
| `done` | 结论是 `Committed` / `TargetUnmet` / `Skipped*` / `Cancelled` | 恢复成功 → `restored`；重试 → `running`（`attemptCount++`） |
| `failed` | 结论是 `Failed` | 重试 → `running`；`attemptCount++`，`everProcessed` 不回退 |
| `restored` | 后端确认恢复/删除成功 | 重新压缩 → `pending`（新的一次尝试） |
| `removed` | 用户移出队列（取消） | 无；不写历史 |

压缩控制状态独立于行状态：`idle → running ↔ paused → stopping → idle`。
暂停只拦尚未开工的任务，在跑的任务结束；停止不可逆，未开工项保持 `pending`；
取消当前视频是单独动作。`idle` 之外任何会话状态都禁用恢复/删除历史产物。
重试复用同一个队列项与 `taskId`，不删旧行/不新建身份；进度分子不倒退。

### C. ResultOutcome

| 结论 | 产物 | 历史 | 语义与按钮 |
| --- | --- | --- | --- |
| `Committed` | 有 | 有 | 已完成；保存 / 对比 / 恢复 / 访达 / 日志 |
| `TargetUnmet` | 有 | 有 | 已完成 · 未达到目标；在 Committed 基础上多一个「重试」 |
| `SkippedNoBenefit` | 无 | 无 | 已跳过；只给重试 / 日志，不出现任何指向产物的按钮 |
| `SkippedAlreadyUnderTarget` | 无 | 无 | 原文件已经小于目标体积；同上 |
| `Cancelled` | 无 | 无 | 已取消；只给重试 / 日志 |
| `Failed` | 无 | 无 | 处理失败；按 `reasonCode` 说明原因，给重试 / 日志 |

「有没有产物」只由 `has_product()` / `hasProduct` 回答；前端不再读 `success`、
不再按 `outputPath` 反推。产物被外部删除时结论不变，但 `outputMissing` 收走
保存/对比/访达/重试，只留恢复与日志。

### D. Transaction

| 场景 | 保证 |
| --- | --- |
| Replace | 备份 → 持久化预提交日志 → 临时文件写满 fsync → 源指纹与备份 SHA-256 复核 → 原子 rename → 历史落盘 → 销账。历史写失败用备份回滚；任何一步失败都不在目标位置留半份文件 |
| Suffix / Folder | `OutputNameAllocator` 独占一个不存在的新名字，绝不覆盖既有文件；不产生备份；历史在 publish 之后才写；撤销 = 只删这次产物 |
| Export | 重新检查产物在盘 → `OutputNameAllocator` 领名 → `StagedOutput::copy_from` 原子复制；失败释放占位名；上次同名导出不会被覆盖 |
| Save As | 单文件另存为同样走暂存：目标同目录临时文件 → fsync → 原子改名；复制失败不留下半个文件，也不先删用户原来的目标文件（Rust `StagedOutput::copy_from` / Swift `atomicCopyFile`） |
| Restore | 只接受 `historyId`，统一走 `HistoryStore::restore`；SHA-256 冲突需明确 force；重复恢复返回 `AlreadyRestored`；产物清理失败返回 warning 而恢复仍算成功；备份也不在则 `BackupGone` |
| 取消 / 停止 | Encode 前与 Commit 前两处闸门；取消不会产生 `Committed`，也不会留下 orphan |

### E. 实测结果（2026-10-22，§144 审计修复后重跑，全部 EXIT=0）

| 命令 | 结果 |
| --- | --- |
| `cargo test`（Direct） | ✅ 196 unit + 6 integration，1 ignored |
| `cargo test --features inproc-backends` | ✅ 198 unit + 6 integration |
| `cargo test --no-default-features --features inproc-backends` | ✅ 198 unit + 6 integration |
| `npm run test:frontend` | ✅ 25 个脚本全过，含 `bulk-operations` / `ui-contract` |
| `bash scripts/test_swift_history.sh` | ✅ [1]–[44] 全过 |
| Swift 全源码 typecheck（24 文件） | ✅ EXIT=0，无诊断 |
| `node --check frontend/*.js tests/*.cjs` | ✅ 无语法错误 |

新增回归重点：`an_externally_deleted_output_still_restores_from_the_backup`（§76）、
`a_missing_backup_with_a_missing_output_reports_an_incomplete_record`（§76）、
`a_committed_result_and_its_history_point_at_a_file_that_really_exists`（§71/§72）、
`tests/bulk-operations.cjs`（§49/§52/§75/§77/§78/§79）、
`tests/ui-contract.cjs`（§67–§69/§87/§88）。

### §144 遗留模式审计与修复（2026-10-22）

对 `success` / `skippedLarger` / `outputPath` / 被吞掉的 IO 错误 / `fs::copy` /
`File::create` 做了一次全仓审计，逐条核实后修掉这些真实遗留：

- **已恢复 / 重建的历史记录不再冒充产物**：`historyRowActionDefs` 与 Swift
  `historyRowActions` 现在只在 `compressed` / `missing` 状态且产物在盘时给
  「另存为 / 对比 / 播放输出」；已恢复记录里 `outputPath` 指向的是写回后的原图，
  重建条目没有压缩明细，都不能再被当成压缩结果。历史行同时识别 Rust 的
  `restoredWithWarning`，文案写着「已恢复 · 压缩结果未能删除」。
- **fsync 失败不再被忽略**：Swift `syncFile` 返回成功与否；备份暂存、写回原图、
  暂存写入、覆盖事务日志的目录 fsync 全部检查结果，失败按"没落盘"处理。
- **跨格式恢复的清理失败可见**：Swift `removeGeneratedOutput` 失败时返回 warning，
  `RestoreOutcome.warning` 进入 toast / 恢复全部摘要；恢复本身仍是成功（§48）。
- **视频非覆盖提交的失败清理由吞变报**：删不掉未提交产物时，错误里写明目标路径，
  不再留下一个没人认领的文件。
- **非 macOS 转换读源失败归因正确**：改用 `read_source`，报 `source_read_failed`，
  不再把空字节当成"格式不支持"。
- **另存为原子化**：Rust `save_file` 改用 `StagedOutput::copy_from` + `settle`；
  Swift 新增 `atomicCopyFile`，两个"另存为"入口都改走它。
- 另核实：Swift `recompressForCompare` 的 `success` 只表示"引擎交回了可用字节"
  （`keptOriginal` 分支也是 `success = true`），没有把"跳过"误判成失败，无需改动；
  `skippedLarger` 已无任何运行时消费方。

### F. 手工验收路径（真实 GUI 点击，待执行）

当前环境未启动 Tauri GUI，以下路径是发布前的点击清单；自动化已覆盖其判据。

1. **正常图片压缩**：导入一张 JPG → 均衡意图 → 开始 → 行从等待到完成，
   大小列写「原 → 新」，详情省率 `-xx%`，保存/对比/恢复/访达都可用。
2. **压缩后更大**：选一张已优化的小图，输出 WebP 高质量 → 行显示「已跳过」，
   无产物大小、保存/对比/恢复按钮不出现，重试可见；历史页新增 0 条。
3. **target-size 达标**：设定一个可达预算 → 结论 `Committed`，体积 ≤ 预算。
4. **target-size 未达标**：设定不可达预算 → 结论「已完成 · 未达到目标」，
   仍有产物与保存/对比/恢复，并出现「重试」；日志里 `reasonCode=target_unmet`。
5. **原图已小于目标**：预算高于原图 → 「已跳过」+ 原文件已经小于目标体积，无历史。
6. **格式转换**：PNG → JPG 显式转换 → 即使更大也提交，最终磁盘文件格式必须是 JPG；
   覆盖率与轨道不被静默丢弃。
7. **视频压缩**：导入 MP4 → 目标体积 → 进度到完成；播放结果、同帧对比可用；
   产物能完整解码，音轨策略与界面一致。
8. **Stop**：混合队列运行中点停止 → 在跑项完成，未开工项保持 `pending`；
   继续只处理 `pending`，已完成/已跳过不被重跑。
9. **Cancel（视频）**：运行中取消当前视频 → 该项 `removed`，下一项不自动开始，
   没有 orphan 临时文件。
10. **Clear**：清空历史 → 历史行与备份清理，原图/已导出产物不受影响；
    压缩进行中清空按钮被拒绝。
11. **Retry**：让一项失败（如源文件不可读）→ 修复后点重试 → 同一行、`attemptCount`
    加一、进度不倒退；重试成功后写一条新历史。
12. **Restore**：覆盖模式下恢复 → 原图字节写回、行变 `restored`、历史 `Restored`；
    再点一次不重复改动（`AlreadyRestored`）；压缩后手改过的文件先弹冲突确认。
13. **Restore All / Export All（混合图片视频）**：只提交有产物的项；
    成功/失败/文件不存在/跳过分别计数；产物被外部删掉的项标回 `outputMissing`。
14. **外部删除产物**：在访达里删掉压缩结果 → 历史行显示「输出文件不存在」，
    保存/对比/访达/重试消失；备份还在时恢复仍可把原图写回；备份也没了显示记录不完整。
15. **恢复后重新压缩**：恢复后点重新压缩 → 回到 `pending`、旧产物路径与旧
    `historyId` 不被复用，完成后产生新的历史 id。
16. **压缩中退出并重启**：在覆盖提交窗口内强退 → 重启后事务恢复要么完成历史、
    要么用备份回滚，不留下半写的目标文件或 orphan 临时文件。

## 审查方案落地：恢复身份门 / 调度竞态 / 批次隔离 / 队列与扫描（2026-10-26）

基线：本地 HEAD `97af890`（v0.3.1）+ 未提交工作区改动。外部审查基线 fd68b35
（GitHub v0.3.4）在本仓库不是有效对象，无法直接 diff。本轮先逐项复核八阶段
实施方案在当前工作区的真实状态，再只修确认缺口。

### 复核结论（未在本轮重复实现的条目）

以下条目经代码核实**已在工作区（未提交）实现**：输出不覆盖认领（§28/§54）、
临时文件唯一（§30）、恢复会话闸门、Swift 跨格式扩展名、Alpha 判定补全、
元数据三策略分立、指定体积停止语义（§61/§62）、事务日志持久化与临时路径
先记账（§2.4）、历史写入失败处理与创建型事务（§2.6）、移除/重试/清空事件
隔离（§3.3）、Swift 历史失败处理与 Rust 一致（§4.2）、视频编码前磁盘空间
分项检查（§7.3）。这些修复随工作区一并提交前不得丢弃。

### 本轮新修复

1. **§3.4 调度器暂停/上限竞态（Rust）**：`pause()` 与 `set_max_parallelism()`
   改为持有 usage 锁置位，`try_acquire_with_memory` 在同一临界区内读
   `paused`；`acquire_with_memory` 拿到 permit 后补一次暂停复核（还回名额
   继续等，不误当停止）。Swift 侧经核实本就同临界区（`acquireOutcome`），
   无改动。
2. **§5.3 视频批次单文件错误隔离**：后端 `resolve_video_tasks` 把
   canonicalize 失败就地记成 failed（`SourceNotFound`）并当场播事件，不再用
   `?` 让一张坏卡没收整批；前端 `validateVideoBatch` 逐项收集 `failures`
   （不再第一个就短路），`markVideoPreflightFailures` 只标记坏行，其余视频
   照常进入会话；能力级失效仍是会话级失败。
3. **§2.5 启动恢复身份门（Rust + Swift 同构）**：`ReplaceTransaction` 新增
   `original_sha256` / `output_sha256`（serde/Codable default，向后兼容）。
   图片线在记账前以内存产物字节计算、视频线以 partial 文件计算；回滚前先
   证明"源位置字节是本次事务的产物"才允许用备份覆盖；证明仍是原图则只清
   残骸不重写；证明不了归属（外部修改）→ 保留备份与日志并报警告，绝不自动
   覆盖。**无哈希的老日志维持升级前"一律回滚"契约**，不另造启发式。
4. **§6.1 队列刷新 O(N²)**：`applyQueueView` 改用一次构建的 `Set(files)`；
   `starting` 事件去掉双重全量刷新；视频 `encoding` tick（约 0.5s 一条）只
   刷新对应行，不再全量重排——队列摘要/排序/可见性只在状态真正变化时刷新。
5. **§6.4 目录扫描预过滤 + 有界并发**：`plausible_video_header` 容器魔数
   嗅探（EBML/ftyp/RIFF-AVI/OggS/FLV/ASF/MPEG-PS/MPEG-TS），既非图片也无
   媒体特征的文件不再各自起一次 ffprobe；候选视频按可用并行度（≤4）分块
   并发探测，结果按导入序回槽，无扩展名但内容有效的视频仍可识别。

### 本轮新增测试

- Rust：`no_new_permit_outlives_a_returned_pause`（多线程压力，判据取
  usage 锁内的发放计数）、`a_paused_gate_stays_shut_even_when_capacity_frees_up`、
  `a_missing_video_fails_alone_and_never_sinks_the_batch`、
  `video_header_sniffing_recognizes_containers_and_ignores_documents`、
  `rollback_refuses_to_clobber_an_externally_modified_source`、
  `rollback_restores_the_original_when_the_source_is_the_staged_output`、
  `rollback_skips_the_rewrite_when_the_source_is_still_the_pristine_original`、
  `a_cross_format_output_is_only_removed_when_its_content_matches`、
  `legacy_logs_keep_the_pre_upgrade_recovery_contract`。
- 前端：`tests/video-batch-isolation.cjs`（逐项校验、预检失败不连坐、能力级
  失效、二次标记幂等）、`tests/queue-view-scale.cjs`（2000 项顺序语义 +
  `files.includes` 回归绊线）。
- Swift：HistoryStoreCheck `[21b]` 事务身份哈希 9 条断言（产物命中回滚、
  原图未动只清场、外部修改保留现场、跨格式产物指纹判删）。

### 门禁记录（2026-10-26，全部通过）

```text
cargo test                                                → 206 passed / 0 failed
cargo test --features inproc-backends                     → 208 passed / 0 failed
cargo test --no-default-features --features inproc-backends → 208 passed / 0 failed
npm run test:frontend                                     → 25/25 PASS（含 2 个新测试）
bash scripts/test_swift_history.sh                        → 全部通过（含 [21b]）
```

### 与远端 v0.3.2–v0.3.4 的整合（2026-10-27）

本地修复基于 v0.3.1 提交；期间远端已发布 v0.3.2（tag `17e1ab8`，VFR 时长修复）
与 v0.3.3/v0.3.4（发布链路修复）。整合方式：工作区整体固化为提交后
`git rebase origin/main`，冲突手工合并：

- `video/plan.rs`：远端 VFR 归一化（`vfr_rate` 字段与 `-fps_mode cfr` argv）
  与本地元数据三策略（`metadata_policy` + StripPrivate 警告文案）**双方都保留**；
  `validation.rs` 的测试构造器补上 `vfr_rate: None`。
- `docs/validation.md`：双方各自的发布/重构记录按时间序并存。

rebase 后全部门禁重跑（2026-10-27，全部 EXIT=0）：

```text
cargo test                                                 → 208 passed / 0 failed（+集成 6）
cargo test --features inproc-backends                      → 210 passed / 0 failed（+集成 6）
cargo test --no-default-features --features inproc-backends → 210 passed / 0 failed（+集成 6）
npm run test:frontend                                      → 25/25 PASS
bash scripts/test_swift_history.sh                         → 全部通过
```

仓库既有真实视频 fixture 回归（`metadata_policies_hold_on_real_encoded_fixtures`
等，bundled FFmpeg 实编实探）随 cargo 套件一并执行并通过——元数据三策略与
StripPrivate 语义由真实编码输出探测背书。

### 残留与阻塞（不冒充通过）

- **§2.7 文件级操作互斥协调器**：未新建。现有会话级闸门
  （`restore_blocked_by_session`、clear_history 守卫）+ 提交前后
  SourceIdentity 双重复核已覆盖方案列出的实际竞态面；按文件粒度的协调器
  评估为当前无可达场景，暂缓并在此留档。
- **§6.3 视频封面**：串行、失败不阻塞、身份校验已有；未复用导入时 probe
  （每次预览仍重新探测一次），收益有限暂缓。
- **§6.4 扫描取消与进度事件**：未实现。导入是单次 await 调用，需要 UI 层
  决策；预过滤后无关文件的 ffprobe 成本已消除。
- **大批量基准（1000/5000/10000 项）、真实图片/视频 fixture 矩阵、三轨构建
  与签名沙盒运行检查、发布门禁**：本轮未执行，维持"未通过/未验证"状态；
  发布前必须按 AGENTS.md Release gate 补齐。

## Release: v0.3.5（2026-10-27）

压缩事务与文件安全重构随 0.3.5 发布。提交：`eb24576`（重构主体，rebase 到
v0.3.4 之上）+ `075edcd`（版本 0.3.5）。CI run [`40362381159`](https://github.com/misswell/Sizqo/actions/runs/40362381159)
8 个作业全部 success（compression-parity、build-linux、build-macos arm64 与
x86_64、verify-appstore、build-windows、build-macos-universal、release），
耗时约 42 分钟。Release 于 2026-10-26T17:27:20Z 创建，非 draft / 非
prerelease，为 Latest：
[`v0.3.5`](https://github.com/misswell/Sizqo/releases/tag/v0.3.5)。

云端签名证据（`gh run view 40362381159 --log`）：

- universal 作业内 6 次 notarytool 提交全部 `status: Accepted`（3 个 `.app`
  + 3 个 DMG；日志 Accepted 行 ×12 是每笔提交的双行输出）。
- staple/validate `worked` ×6；签名身份 Developer ID Application: Guofeng Liu，
  hardened runtime。

线上资产（字节）：universal DMG 66,328,208；arm64 DMG 30,571,082；
x86_64 DMG 36,891,672；universal `.app.tar.gz` 60,110,863；`.sig` 432；
`latest.json` 1,309；Linux AppImage 87,947,768；deb 5,157,650；
Windows exe 2,998,527；msi 3,854,336。`latest.json` 的 `version` 为 `0.3.5`，
两个 darwin 平台的 `url` 都指向
`v0.3.5/Sizqo_v0.3.5_macos_universal.app.tar.gz`，`signature` 字段与 432 B
的 `.sig` 资产逐字符相同。

下载线上字节复核（`/tmp/sizqo-v035-verify`）：

| 对象 | 结果 |
| --- | --- |
| universal DMG（挂载） | `hdiutil verify` 校验和 VALID；`Sizqo.app` 0.3.5、`x86_64 arm64`、hardened runtime、TeamIdentifier `U8U443D7ZL`；`codesign --verify --deep --strict` 通过；`spctl` accepted (Notarized Developer ID)；`stapler validate` worked；`Resources/resources/bin` 7 个图片 CLI，`Resources/video/bin` ffmpeg/ffprobe |
| universal 更新包 | `tar -xzf` 后同样 0.3.5/universal/9 个引擎，`stapler validate` worked |
| 更新签名 | `scripts/verify_updater_signature.sh` → OK；翻转第 1000 字节后 → `Error: InvalidSignature` |

```
sha256 2cc4076e21cf4ca45b632fe71ad1dc95c7a5e451b39281a40dfbc72a3394b737  .app.tar.gz
sha256 da8bd8ef4ca6f40881a940839d3366c0e97f7545a70592848acd2b46eafda3c7  .app.tar.gz.sig
```

真实视频 fixture 回归（`metadata_policies_hold_on_real_encoded_fixtures` 等，
bundled FFmpeg 实编实探）随 cargo 套件在发布前通过，覆盖元数据三策略语义；
§11 fixture 矩阵的其余条目、签名包内新工作台的肉眼点击验收、App Store 沙盒
运行时、FFmpeg GPL 对应源码义务仍未做（与 v0.3.1–v0.3.4 相同）。本轮按用户
「做完就发版」的指示发布，未把上述未验证项标成已通过。

## 第二轮审查修复与验收推进（2026-10-27）

针对 v0.3.5（`74debeb3`）的第二轮外部审查逐条核实后的处置。**更正**：上一节
"真实视频 fixture 回归随 cargo 套件通过"表述有误——`metadata_policies_hold_on_
real_encoded_fixtures` 是 `#[ignore]` 的显式测试，常规套件并不运行它；本轮已
显式运行并通过（`cargo test --lib real -- --ignored`）。

### 真实媒体矩阵（§9.3 首批落地）

新增两组真实文件测试并全部通过：

- 视频 `s11_real_media_matrix`（`#[ignore]`，bundled FFmpeg 实编实探）：
  SDR 双音轨+字幕+章节的 copy 全保留并通过 §56 全量校验；VFR 夹具（r=6/1、
  avg=45/7）计划带归一化帧率且产物时长在 §56 容差内；argb/qtrle 的 Alpha
  输入在编码前被拒；10bit+HDR10 VUI 被拒；隔行被拒；魔数被覆盖的损坏文件被
  probe 干净拒绝；物理不可达的目标体积在 plan 被拒。
- 图片 `tests/real_media_matrix.rs`（常规套件）：带 Alpha 的 PNG 压缩后透明
  区保留；损坏 JPEG/空文件干净失败；Unicode+空格路径正常压缩；64 字节的
  物理不可达预算不会被报告为命中。
- **矩阵抓到并修复的真实缺陷**：损坏 JPEG 的解码失败在 `compress_jpg` 被静默
  跳过、最终归类为 KeptOriginal（"原图已是最优压缩"）。修复：新增
  `corrupted_source` 守卫，png/jpg/gif/webp 四条回退路径在"没有任何编码器
  产出"时先验证源文件可解码，解不开 → `DecodeFailed` 明确失败；heic/avif/jxl
  的解码在外部 CLI 里无法就地区分，维持原语义（留档）。

### 第二轮审查 8 条的处置

| # | 审查意见 | 处置 |
| --- | --- | --- |
| 1 | 同大小内容被改的输出被 retire 误删 | **已修**：`retire_generated_output` 非空产物必须内容指纹命中才删，无指纹一律保留；空占位须 `(dev,ino)` 凭证命中（journal 记录 `placeholder_identity`）。Swift 同款指纹门 |
| 2 | 占位文件所有权竞态（A）+ 认领先于记账（B） | **A 已修**：`publish_claimed` 提交前核对目标 inode 仍是认领的占位；`release_output_placeholder` / `ClaimedOutput::release` 同款凭证，外部放入的文件绝不碰。**B 部分缓解**：有 journal 的占位凭 inode 清理；崩在"认领与记账之间"的 0 字节孤儿保守保留（删除它同样需要凭证），下一次同名认领自动顺延 `_1` |
| 3 | 恢复与压缩之间非原子竞争窗口 | **已修**：调度器生命周期改为单一原子状态（0=空闲/1=批次/2=维护）；`restore_entry`/`restore_all`/`clear_history` 在动手前获取维护租约（RAII），租约在场时会话开不起来，会话在场时租约拿不到——两个方向都原子 |
| 4 | 损坏事务日志导致备份被清理 | **已修**（Rust+Swift）：`.unparsed` 计入 `has_pending`，一切依赖该判据的清理都不会把相关备份当孤儿；`recover` 给出明确警告 |
| 5 | 视频输出指纹 `.ok()` 吞错 + 非覆盖无事务 | **已修**：指纹计算失败在记账与覆盖之前终止提交；视频非覆盖模式补齐 `CreateNew` 事务（prepare + 失败 retire），与图片线同构 |
| 6 | 图片终检只查 stat 指纹 | **已修**（Rust+Swift）：改名前最后两道门改用完整 `verify`（内容哈希），与视频线同一标准 |
| 7 | 队列仍有全量重排，缺基准 | **部分**：Set 与 encoding tick 降频已在 v0.3.5；排序脏标记与 1,000/5,000/10,000 实测基准**未完成**，见下方遗留 |
| 8 | 老日志无条件回滚 | **维持原决策**：无身份信息的老日志按升级前契约回滚，否则旧崩溃现场的原图永远滞留在备份里；风险窗口（升级前崩溃+升级后人为改动同一文件）与残留概率已在第一轮记录。重新评估的结论：保留，但已在日志中明示 |

新增 Rust 测试：`retire_never_deletes_a_same_size_file_whose_content_changed`、
`an_unparsable_journal_counts_as_pending_and_warns`、
`restore_is_refused_exactly_while_the_compression_session_is_active`（租约双向）。

### 已知不对称与遗留（不冒充完成）

- ~~Swift 占位 inode 凭证未接线~~ / ~~Swift 调度器维护租约未加~~：
  均已在 0.3.6 轮完成（见上节"Swift 对齐"），以 Swift 自检为证。
- **视频 CreateNew 事务的崩溃恢复测试**：事务与 retire 语义与图片共用同一实现
  （output_transaction 测试覆盖），端到端视频崩溃注入未单独补。
- **1,000/5,000/10,000 性能基准、虚拟列表、排序脏标记、签名应用 GUI 点击验收、
  App Store 沙盒运行**：仍未完成，继续标记为未通过。

## Release gate (per [AGENTS.md](../AGENTS.md))

Before any tag/Release: full three-track build, signed Direct runtime check,
signed sandbox check, real fixture video matrix (AGENTS.md §11 items 1–12),
FFmpeg licensing review. A local commit is not a GitHub push; every published
tag must link its artifacts here with the exact commands used.
