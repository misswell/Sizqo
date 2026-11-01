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

### Review round 3 (2026-10-31 深夜, rc.3)：第三轮审查修复

上一轮审查确认 rc.2 的修复大部分落地，但指出五类残留问题，本轮全部修复：

- **P0 交换失败后外部文件保护（双端）**：换回失败 + 冲突迁移失败时，
  外部内容停在临时路径上——调用方的 discard/remove_temp 会把它删掉。
  修复：失败路径写 `<temp>.protected` 标记，remove_temp / discard
  见标记绝不删（Rust `remove_temp` + Swift `StagedWrite.discard` 身份门）。
- **P0 Windows 目录 fsync**：`sync_dir` 按 `#[cfg]` 分平台——Windows 用
  `OpenOptionsExt::custom_flags(FILE_FLAG_BACKUP_SEANTICS)` 打开目录句柄；
  未知平台如实报错，不静默假装成功。
- **P0 RC.1 事务迁移**：`backupLegacyUnverified` 缺字段 = RC.1 事务，
  保守按 `true`（未验证）处理，哈希一致也不能自动写回；
  迁移测试用 RC.1 原始 JSON 形状（schemaVersion=2、三指纹、无 legacy 字段）。
- **P0 跨格式删除原图前再验证**：落盘流程与删除之间外部可能改掉原图，
  备份保住的是旧字节——删除前用基线身份再 verify 一次，改过 → 保留文件
  留警告，产物照常成立（Rust+Swift）。修复中避免了一个次生错误：
  保留原图时**不能提前 return**——历史照常记、事务照常销账。
- **P1 describe_pending legacy/unreadable**：LegacyUnverified 标记 +
  备份读不出来都进人工恢复状态机（不再返回 AutoResolvable 被过滤掉），
  列表与 rollback 判据同源（Rust+Swift）。
- **P1 SwiftUI 凭证/legacy 确认透传**：banner 回调扩成三参
  `(item, restore, confirmVoucherMismatch)`；legacyUnknown 行加橙色提示；
  确认弹窗对凭证不符写明"继续即接受风险"——与前端/Rust 同一协议。
- **P1 Swift 同格式原子交换**：`publishWithOwnershipVerification` 三态
  （published / refused / publishedButNotDurable）；同格式用基线哈希验证
  被换出字节，占位路径用 inode，fsync 失败按 publishedButNotDurable
  返回、调用方用备份回滚（Rust 提交后 fsync 失败同走回滚而非 withdraw）。
- **P1 releaseOutputPlaceholder 真正比较 inode**：签名带
  placeholderIdentity，只有 (dev,ino) 完全一致的 0 字节文件才删。
- **P1 可见序列纯数据化 + LRU 按窗口**：computeVisibleFiles 不再读 DOM
  的 hidden（数据事实与旧渲染分离，筛选往返不再丢行）；LRU 保护集只含
  当前视口窗口（此前用完整 visibleFiles 当保护集 = 10k 全豁免）；
  回归测试：筛选往返 + 10k 全表往返滚动后 fileRows ≤ 700。
- **P1 崩溃注入真正走生产路径（上轮虚报的更正）**：rc.2 的提交信息与
  validation.md 声称"崩溃注入改走生产路径"，但实际改写因编辑工具链中断
  **没有落盘**——本轮逐段重写并 grep 验证：备份走 `ensure_backup_verified`
  （真实快照链），覆盖走 `publish_replace_if_identity_matches`（真实交换），
  备份真实路径由子进程留档、父进程断言；6 阶段 × 双恢复全过。
- **P2 Windows/Linux 打包**：CI 失败根因是版本 `0.3.6-rc.2` 的 pre-release
  后缀（MSI 要求数字、RPM 禁止 '-'）；release.yml 在 Windows/Linux 打包前
  把 bundle 版本剥成数字（tag 门禁不受影响）。
- **P2 App Store 配置版本同步**：`tauri.conf.appstore.json` 同步到 rc.2。
- **P2 测试工作流**：新增 `.github/workflows/tests.yml`（push/PR 都跑）：
  Rust lib + crash injection + 契约向量 + 真实矩阵、前端 29 套、Swift 自检、
  Windows cargo check、App Store 特性测试——发布工作流不再代替测试门禁。

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

## 第三轮审查修复与平台取证（2026-11-01，0.3.6-rc.4）

基准：远端 `main` = `9ee0394`（`v0.3.6-rc.3`）。本轮先把 RC.3 的提交说明与磁盘上的
源码逐条对照，再补齐没做到的部分，并把取证从"编译通过"推进到"现场可复现"。
**结论：RC.3 提交说明里那四项"已修复"，逐项对源码核对后没有一项在磁盘上完整成立**，
审查的 P0-1 / P0-2 / P0-3 / P1-4 判断全部正确，本轮按这些判断重做。

### 先更正：RC.3 提交说明与 `git show 9ee0394` 的差距

| RC.3 声称 | 磁盘上的真实状态（RC.3 版本） |
| --- | --- |
| 跨格式删除门 | `commands.rs` 复核后只 `log::warn!("…已保留原文件")`，紧接着**无条件** `fs::remove_file(file_path)`（RC.3 第 1478 → 1490 行）——报的是保留、做的是删除 |
| 交换失败外部文件保护 | `remove_temp` 只认 `.protected` 标记，无标记即 `fs::remove_file(temp)`；标记是 `let _ = fs::write(…)` 写的，写失败＝没有保护。外部放进临时路径的零字节文件会被当自家残骸删掉。Swift 侧同构缺陷（RC.3 的 `OutputTransactionStore.swift` 里 `TempOwnership`/`swappedButUnrecovered`/`OriginalRetirement` 出现 0 次） |
| Windows 目录 fsync | Windows 分支以**只读**句柄打开目录再 `sync_all()`；`FlushFileBuffers` 要求句柄具写权限，运行时必然 `ACCESS_DENIED`。RC.3 只有 `cargo check` 通过，`cargo test` 从没在 Windows 上跑过 |
| LRU | `fileRowOrder` 按创建顺序淘汰（FIFO），重新访问旧行不搬序 —— 代码注释称 LRU，行为不是 |
| 标签 | `v0.3.6-rc.3` 指向 `e6e3eae`，而把版本号写成 0.3.6-rc.3 的提交是 `9ee0394` —— 标签与内容错开一个提交 |

### P0-1 跨格式替换的原图删除门（Rust）

新增 `output_transaction::retire_original(file, identity)`，把"删原图"从一个
`verify → remove_file` 的两步（中间塞得进一次外部改写）拆成三步，每一步只删得到
自己证明得了归属的字节：

1. 动手前最后一次完整复核（stat 三项 + 内容哈希）：不是原图就**根本不进删除**；
2. `claim_quarantine_name` 以 `O_EXCL` 独占认领隔离名，`fs::rename` 过去 —— 此后
   外部在该目录里怎么折腾都碰不到即将被删的那份字节；
3. 复验隔离出来的字节：对得上才删；对不上（改名窗口里还有人持 fd 在写）就放回原位，
   原位被外部占了就用原子交换顶回来，外部那份落到隔离名上，一个字节都不动。

三条路都走不通时的结果一律是"字节还在磁盘上"＋一句说实话的警告，绝不会再是
"报了保留却把文件删了"。接线在 [commands.rs:1479](../src-tauri/src/commands.rs#L1479)：
`Err(detail)` 只落警告，跨格式产物照常成立。

### P0-2 临时归属门、保护标记与「不提前销账」（Rust + Swift）

Rust：

- journal 新增 `temp_identity (dev,ino)`（schema v2），`remove_temp` 的第一依据是
  记账时认领的 inode，其次才是内容 sha256；两条都证明不了 → **一律保留**。
- `.protected` 仍然先查，它是"这里的字节属于外部"的显式声明，压过一切归属推断。
- `write_protected_marker` 返回 `Result<(), String>`：以前 `let _ = fs::write(…)` 把
  "没写成"和"写成了"当成同一件事。写不上时日志明说"归属门仍会拒绝清理"，且 inode
  凭证照样拒删 —— 保护不再依赖单一手段成功。
- `SwapPublishError { swapped: bool, detail }` 把"覆盖已发生"与"目标没动"分成两种态；
  ⑥ 交换失败分支回滚成功后才 `finish`，⑦ 收场失败分支返回 `RollbackFailed` 且
  **不 `finish`** —— 日志留在现场，交给下次启动的恢复。

Swift（`OutputTransactionStore.swift` +236/-21、`AppState.swift` +62）：

- `TempOwnership`：`isProtected` / `writeMarker -> String?`（失败必须报出来）/
  `isOurs(claimed:expectedSha256:)` / `protectForeignBytes` / `conflictSibling`。
- `StagedWrite.PublishOutcome` 四态：`published / refused / publishedButNotDurable /
  swappedButUnrecovered(String)` —— `.refused` 与"交换失败且现场未恢复"不再混为一谈。
- `OriginalRetirement`：与 Rust 同构的隔离改名 + 复验，`restoreQuarantined` 三态
  （`inPlace` / `inPlaceForeignKept` / `kept`）。
- `AppState` 的覆盖写回路径先 `recoverCoveredWrite(…)`，收回原图之前不许 `finish`；
  两处交换回滚失败都必须把外部字节交给 `protectForeignBytes` 处置。
- 测试断言的是生产源码本身（无 Swift 故障注入惯例）：`try? TempOwnership.writeMarker`
  必须不存在、`rollback(txn)` 必须排在 `transactions.finish` 之前，等等。

### P0-3 Windows 目录 fsync

`history::sync_dir` 成为唯一一处按平台开句柄的实现：Windows 用
`FILE_FLAG_BACKUP_SEMANTICS + .write(true) + share_mode(READ|WRITE|DELETE)`（写权限是
`FlushFileBuffers` 的硬要求，共享位保证不把并发写入者挡在门外）；unix 沿用
`File::open + sync_all`；未知平台**如实返回 Err**，不假装成功。

调用方一律不再吞错：`commands.rs:1457` fsync 失败 → 用备份回滚（回滚也失败才
`RollbackFailed`，且此时不销账）；`video/transaction.rs:55,88` 直接 `?`／
`map_err(|e|(e,false))`，其本地 `sync_dir` 只转调 `history::sync_dir`。

取证限制（如实记录）：本机交叉编译不可行 ——
`cargo check --lib --tests --target x86_64-pc-windows-msvc` EXIT=101，`ring` 的 cc-rs 报
`fatal error: 'assert.h' file not found`。因此 Windows 运行时行为只能由 CI 的
`windows-tests` 作业取证，本轮**不声称** Windows 已通过。

### P1-4 没有原子交换时的覆盖

`publish_replace_if_identity_matches` 在交换不被支持时走
`publish_replace_via_quarantine`：① 目标改名进独占隔离名；② 复验隔离出来的正是本次
编码所依据的原图，不是就放回原位并拒绝；③ 用**不会覆盖**的方式把候选放进目标位 ——
`install_at_free_path` 先 `fs::hard_link`（原子判空，撞 `AlreadyExists` 即拒绝），
不让硬链接的卷退回"查空 + 改名"。旧写法"复核指纹 → `fs::rename`"会把刚出现在该位置
的任何字节直接盖掉，正是审查报的那次事故。

Windows 上这条路是**唯一**路径而不是降级：`atomic_swap` 在非 unix 直接返回
`ErrorKind::Unsupported`，`swap_unsupported` 认得它，于是 `publish_replace_if_identity_matches`
必定进隔离改名；`restore_quarantined` 在 Windows 上交换不了外部占用的原位，只会落到
`Restored::Kept`（把字节留在隔离名里报路径），也就绝不会用"放回原图"的名义盖掉别人的文件。
`quarantine_publish_*` / `installing_a_candidate_*` 这 5 项测试直接驱动这些原语，
不带平台门，因此同一份断言会随新的 `windows-tests` 作业在 Windows 上真跑。

### 平台分支的测试（不再"编译通过就当通过"）

- `engine.rs`：`system_mode_does_not_pretend_to_have_run_a_budget_search` 加
  `#[cfg(target_os = "macos")]`（ImageIO 真的会跑才有"不许冒充体积搜索"可断言）；
  新增 `#[cfg(not(target_os = "macos"))] system_mode_reports_unsupported_off_macos`，
  断言 Linux/Windows 上系统模式**如实失败**：`UnsupportedFormat` + `Rejected`、
  交回原图字节非空、`target_bytes` 回显但 `target_met == Some(false)`、`error` 里
  看得见"系统转换仅支持 macOS"。测试没有被删掉，只是每个平台断言各自成立的那一半。
- `history.rs`：产物清理失败的现场按平台构造 —— unix 把目录改成 `0o555`，Windows 用
  "另一句柄打开产物"（`PermissionsExt` 在 Windows 上根本编不过，这也是 RC.3 的
  `cargo test` 从未在 Windows 跑起来的原因之一）；两者共用同一组断言
  `restore_survives_a_failed_output_cleanup`：原图写回、产物仍在原位、警告文案、
  `RestoredWithWarning`。

### 前端行缓存：从 FIFO 改成真 LRU

`pruneFileRowCache(list, usedFiles)` 把"本轮窗口用过 / 仍挂在 DOM"的行搬到顺序表的
最近使用端，逐出只从最久未使用端开始；`resetQueue` 清表；全量渲染分支传整张有序表，
走同一套规则。缓存与 DOM 从不分家 —— 仍挂在 list 上的行一律受保护。

`tests/queue-virtualization.cjs` 新增三组场景（+153 行）：① 反复回看的窗口第二次访问
不得重建 DOM（`timesCreated == 1`）；② 早创建但每轮都被重新访问的行必须存活，而
"只看一次的晚创建行"必须已被逐出；③ 单调下滚时缓存条数恒 ≤ 上限且逐出从最久未访问端走。
只断言"滚走再滚回来不重建"分辨不了 FIFO 与 LRU，所以场景按"是否发生过逐出"设计。

### 发布与 CI

- `release.yml`：PowerShell 路径里的 ASCII TAB 全部改为正斜杠（两个工作流现在
  `grep -n $'\t'` 均 0 命中）；版本守卫扩为 `tauri.conf.json` / `Cargo.toml` /
  `package.json` 三方一致，不一致 `exit 1`。
- `tests.yml`：`windows-build-check` → **`windows-tests`**（`cargo check --lib` +
  `cargo test --lib` + 契约向量），Windows 分支从此有运行时取证；
  新增 **`rust-tests-macos`**（default + `--features inproc-backends` + 崩溃注入）——
  此前没有任何作业真的在 macOS 上跑 `cargo test`，平台分支的断言无人守；
  崩溃注入步骤加 `-- --nocapture`，harness 里逐阶段 `println`（这条门禁成功时以前
  一行输出都没有，CI 日志里与"没跑"看不出区别）。
- 顺手去掉 `sanitize_compare_payload` 里对 `&mut Vec` 的 `drop(results)`：no-op，
  且每次构建都产生 `dropping_references` 警告（lib 警告数 7 → 6）。
- 标签决策：`v0.3.6-rc.3` 已随 RC 发布分发，**不覆盖、不改指**；本轮把工作完成态
  标在新的 `v0.3.6-rc.4`（指向 0.3.6-rc.4 的版本提交）。正式版 `v0.3.6` 仍受发布门禁
  约束（见文末）。

### 本轮门禁命令与结果（macOS 本机，2026-11-01）

| 命令 | 结果 |
| --- | --- |
| `cargo test --lib` | 258 passed / 0 failed / 2 ignored（23.63s），EXIT=0 |
| `cargo test --lib --features inproc-backends` | 260 / 0 / 0（43.10s），EXIT=0 |
| `cargo test --no-default-features --features inproc-backends` | lib 260 / 0；`compression_test` 6 / 0；`real_media_matrix` 5 / 0；主程序与 doc-test 0；EXIT=0（43.75s） |
| `cargo test --test crash_injection -- --nocapture` | 6 阶段逐条打印（`after_backup`→backup_only、`after_journal`/`after_partial`→residue_only、`after_rename`→restored、`after_history`→committed、`legacy_after_rename`→preserved），每阶段两次恢复且二次幂等，EXIT=0 |
| `cargo test --lib rust_executes_every_contract` | 1 passed / 259 filtered，32 条契约向量与 Swift 期望一致 |
| `cargo test --lib real -- --ignored` | 2 passed（含 `s11_real_media_matrix`，bundled FFmpeg 实编实探） |
| `npm run test:frontend` | 29 个套件全绿，EXIT=0；基准 N=1000 scroll×100=81.1ms、N=5000=155.4ms、N=10000=164.8ms（windowRows=20），10k 全量渲染参考 first=562.5ms / clean=14.93ms |
| `bash scripts/test_swift_history.sh` | 546 项 `ok`，0 项 FAIL，EXIT=0（`swiftc -O` 编译 12 个产品源文件 + 自检；`[45]` 节约 93 项，其中 4 项为"测试前提"） |

新增测试：**Rust 17 项**（`src-tauri/src` 里 `#[test]`/`#[tokio::test]` 计数
248 → 265，无一项被删除；含 `output_transaction` 的隔离窗口 / 占名 / 发布门、
`commands` 接线 2 项、`history` 的 Windows 现场 1 项、`engine` 的非 macOS 平台 1 项），
**Swift `[45]` 93 项**，**前端 3 组 LRU 场景**。
`cargo test --lib` 的 258 与带 `inproc-backends` 的 260 都在这轮实测复跑过（表见上）。

### 负向对照（本轮亲手拆门，证这些门不是空话）

每次改完立刻从备份逐字节还原并核对 md5，还原后复跑为绿。

| 拆掉的门 | 结果 |
| --- | --- |
| `retire_original` 第③步隔离复验（改成永不 Changed） | `a_writer_holding_the_fd_open_does_not_lose_its_bytes` FAILED、`a_cross_format_conversion_keeps_an_original_that_changed_in_the_delete_window` FAILED；`an_external_file_reoccupying_the_original_path_is_never_touched` 仍 ok —— 那条现场靠的是第②步隔离改名，不是复验。两条门各管各的现场，测试没有互相冒充 |
| `temp_is_protected`（改成恒 false） | `temp_cleanup_only_deletes_bytes_it_can_prove_ours` FAILED、`a_protected_marker_reports_whether_it_was_actually_written` FAILED |
| `install_at_free_path` 的硬链接占名 + 查空门（模拟 RC.3 的裸 `fs::rename`） | `installing_a_candidate_never_overwrites_a_foreign_file_at_the_target` FAILED、`quarantine_publish_refuses_to_install_over_a_file_dropped_into_the_free_slot` FAILED |
| Swift `TempOwnership.isOurs`（改成恒 true） | `scripts/test_swift_history.sh` EXIT=1，报 4 项 FAIL：无凭证的零字节残骸一律保留 / 交换失败现场里的外部字节不许被当残骸清掉 / 零字节不等于归属 / 两条凭证都证明不了归属时恢复入口拒绝删除 |
| 前端"本轮用过→搬到最近使用端"（改成恒假，即退回 FIFO） | `node tests/queue-virtualization.cjs` ERR_ASSERTION：第二次回到 A 窗口重建 2 行（期望 1） |

还原核对：`output_transaction.rs` md5 `c722cb99…` 与改前一致（diffstat 仍是 +790/-48）、
`OutputTransactionStore.swift` md5 `bd3952d2…` 一致（+236/-21）、`frontend/app.js`
md5 `bb6f122a…` 一致，三者复跑均为绿。

### 已知不对称与遗留（不冒充完成）

- **Windows 运行时**：~~本轮只做到"CI 里有作业真的跑 `cargo test --lib`"，其结果待推送
  后以 Actions 为准~~ —— 已兑现：run 41235873134 的 `windows-tests` **success**，
  `cargo test --lib` 249 passed / 0 failed / 2 ignored，32 条契约向量在 Windows 上一致
  （见下方第四轮）。目录 fsync 与隔离改名不再是"未验证"。
- **非 unix 平台的 inode 凭证**：~~`file_identity` 在非 unix 返回 `None`~~ —— Windows
  已在 0.3.6-rc.5 补上卷号 + 文件号（见下方第四轮）；只剩"其他平台"这一支仍返回
  `None`，此时归属判定退化为内容对账，只有 `output_sha256` 也没记上的老日志才会走
  "一律保留"。宁留孤儿残骸（可能残留 `.tmp`），不删别人的字节。
- **不让硬链接的卷**（部分网络盘 / FAT）：`install_at_free_path` 退化为"查空 + 改名"，
  该窗口非原子；最坏结果是拒绝提交，不是删他人内容。
- **Swift 无进程级崩溃注入**：以可直接驱动的原语（`TempOwnership` /
  `OriginalRetirement` / `restoreQuarantined`）+ 对生产源码的门断言替代；视频
  `CreateNew` 的端到端崩溃注入仍未单独补（语义与图片共用 `output_transaction`）。
- **发布门禁**：签名应用 GUI 点击验收、App Store 沙盒运行时、FFmpeg GPL 对应源码
  义务与 §11 fixture 矩阵其余条目**未完成** → `v0.3.6-rc.4` 依旧只是预发布标签。

## 第四轮：Windows 运行时补齐与 CI 缺口（2026-11-01，0.3.6-rc.5）

上一轮把 `windows-tests` 从"只做 `cargo check`"改成"真的 `cargo test --lib`"，这是它
第一次带回运行时结论：**241 passed / 7 failed**（run 41229460016）。本轮逐条对着源码
查这 7 项的根因并修，不做"改断言迁就平台"的动作；同时修掉 CI 暴露出的另外三处
门禁本身的问题。

### 7 项失败的根因与处置

| CI 报出的位置 | 根因 | 处置 |
| --- | --- | --- |
| `contract_vectors.rs:475` `output_retirement.json :: empty_placeholder_deleted_with_credential`（实际 kept，期望 deleted） | `file_identity()` 在 Windows 返回 `None` → 空占位的 `(dev,ino)` 凭证门永远开不了 | 新增 `src-tauri/src/win32_fs.rs`：`GetFileInformationByHandle` 取 `(dwVolumeSerialNumber, nFileIndex)`，与 unix 的 `(dev,ino)` 同语义 |
| `output_transaction.rs:2016` 空占位凭身份凭证清账 | 同上 | `file_identity` 改 unix / windows / 其他 三分支 |
| `output_transaction.rs:2137` 临时残骸要清掉 | 同上（临时名认领凭证也取自身份） | `SourceIdentity::read` 与 `fd_identity` 同样补 Windows 分支 |
| `commands.rs:4543`「测试前提没立住：改动后的 size/mtime/inode 必须和改动前一致」 | `set_times` 是 `#[cfg(not(unix))] → Ok(())` 的桩：Windows 上时间戳根本没钉回去，那条测试测的不是"内容指纹"那一支 | `win32_fs::set_times` 用 `SetFileTime`（创建时间传 `NULL` 不动），`source_identity` 与 `contract_vectors::freeze_mtime` 都换成真实现 |
| `commands.rs:4042` 左侧 `\` 右侧 `/` | 测试自己写了 `join("nested/photo.png")`，Windows 的 `join` 不折叠这个组件里的分隔符；生产代码是按组件拼的 | 改成 `join("nested").join("photo.png")`，断言的内容（产物落进已选根的子层级）一字未减 |
| `history.rs:2611` `assert!(output.exists())` | "另一个句柄开着"的现场用的是 `fs::File::open`，而 std 默认共享位含 `FILE_SHARE_DELETE` → 删除照样成功（只是延后到关闭），"删不掉"被测成"删得掉" | 显式 `share_mode(FILE_SHARE_READ \| FILE_SHARE_WRITE)`。`share_mode(0)` 也不行：那会连恢复自己读产物指纹这一步一起打死，走的是"证明不了归属所以不删"，不是"删的时候失败" |
| `image_capabilities.rs:280` `Direct 缺少 webp 输出` | `resources/bin` 里全是 macOS 的 Mach-O 二进制，Windows 找的是 `cwebp.exe` —— 这台机器上**真的**没有。测试断言的是"随包带了编码器"这条只在 macOS/Linux 成立的事实 | 每个平台断言各自成立的那一半（jpg/png 有进程内兜底，任何平台都必须可用；webp 的 `available` 必须等于 `tool_available("cwebp")`，不可用时必须点名缺哪个编码器且不许进输出目标），另加一条跨平台的"不可用格式必须自证理由"门 |

Windows 的 Direct 轨因此如实报告"这台机器写不出 webp/gif/avif/jxl"：不是回归，是随包
现状。要翻成"可用"得先补 `*.exe` 的打包，不是改断言。

### 顺带修掉的两个 Windows 真实功能缺口

`video::transaction::available_space` 此前在非 unix 直接 `Err("Disk-space verification
is unavailable on this platform")`，而它在编码前的第一个检查点就被 `?` 使用 ——
Windows 的视频轨因此在**还没开始编码时**就整条失败。现在走
`GetDiskFreeSpaceExW`。同盘的备份空间合并估算（AGENTS 规则 5）原来只有 unix 的
`dev()` 能判，现在 Windows 比卷号；判不出来时按"同一块盘"处理，多要空间是安全方向。

新模块刻意**不引入 `windows-sys` 依赖**：符号用 `#[link(name = "kernel32")]` 裸声明
（std 本来就链着它），离线 vendoring 与镜像风险为零。unix 分支一行未动。

### 契约向量门禁不再"只报第一条"

`assert_eq!` 写在循环里，于是一个平台上第一条不一致就中止，后面每一条的该平台表现
**从未被看过** —— CI 那次只报了 1 条向量不一致，不代表其余 31 条在 Windows 上是对的。
改成逐条收集、末尾一次性报出，并把成功时的"32 条一致"打进日志（CI 步骤加
`-- --nocapture`，与崩溃注入同一套自证要求）。

### 发布与 CI 的三处缺口（Release run 41229469102）

- `build-windows` 失败在 `optional pre-release identifier in app version must be
  numeric-only … for msi target`。工作流里**本来就有**去后缀那一步，但它的正则是
  `-(pre.*)$`，`0.3.6-rc.4` 的 `rc.4` 不匹配 → 版本串原样进了 MSI 打包。改成匹配
  任意 `-`，与 Linux 分支（早就按 `*-*` 处理）对齐。
- `release` 作业写死 `prerelease: false` + `make_latest: true`，且无条件生成
  `latest.json`。也就是说：**只要 Windows 打包一绿，一个 rc 标签就会成为仓库的
  "latest"，并把应用内更新源指向门禁未齐的构建**。现在按标签判定 —— 带 `-` 的
  （`vX.Y.Z-rc.N`）标成 pre-release、不抢 latest、不产 `latest.json`。
- `real media matrix` 在 Ubuntu 上从没真的跑过：`process::command("ffmpeg")` 直接
  panic（`ffmpeg_backend.rs:132`），报的是 0 passed / 2 failed。先装系统 ffmpeg 并断言
  `libx264`/`libx265` 都在场，再跑 `--ignored` —— 让门禁真跑，而不是把测试挪走。
  本轮把这一步接上之后它仍然红，红出来的东西见下一节：装 ffmpeg 只是必要条件，
  不是那一轮我以为的根因。

### CI 回证（run 41235873134）：`windows-tests` 首次转绿，Ubuntu 真实矩阵揭出更根本的一扇门

| 作业 | 结论 | 关键读数 |
| --- | --- | --- |
| `windows-tests` | **success** | `cargo test --lib` → **249 passed / 0 failed / 2 ignored**（上一轮 241 / 7 failed）；契约向量 **32 条与 Swift 期望一致**（上一轮的门只让人看到第 1 条） |
| `rust-tests` (Ubuntu) | failure，且只红在一步 | `cargo test (default)` 251 / 0；契约向量 32 条一致；崩溃注入绿；**只有 `real media matrix` 红** |
| `rust-tests-macos` / `appstore-tests` / `frontend-tests` / `swift-checks` | success | — |

上一轮 P0-3 要的"运行时结论"因此第一次拿到：Windows 的目录 `FlushFileBuffers`、隔离
改名、句柄身份、`SetFileTime` 都在真机上跑过了 249 条测试。7 项失败的处置逐条兑现，
没有一条是靠删测试或放宽断言变绿的。

要如实分开说：那 249 条里**没有** `GetDiskFreeSpaceExW` 与卷号比较 —— 这两个新原语
当时只有 `win32_fs.rs` 单文件的目标类型检查背书，一次都没被调用过（编码前的空间检查
在 `ffmpeg_backend.rs` 里，而 Windows 上那条路径根本跑不到）。按 P0-3 自己的标准，
"编译过 = 运行时成立"不成立，所以补了三条真调用的测试（见下表"原语的运行时背书"）。

**Ubuntu 那一步红出来的不是"缺 ffmpeg"**，而是两条 panic 的原文：

- `ffmpeg_backend.rs:132` → `called Result::unwrap() on an Err value: "Exec format error (os error 8)"`
- `ffmpeg_backend.rs:227` → `内置 ffmpeg 必须有 libx264/libx265`

根因：`src-tauri/resources/video/bin/{ffmpeg,ffprobe}` 与 `src-tauri/resources/bin/` 里
7 个图片编码器（`file -b` 实测全是 `Mach-O 64-bit executable arm64`，`resources/lib`
全是 dylib）**就提交在仓库里**，而 `video::process::tool()` 与 `engine::find_tool()`
判"这台机器有没有这个后端"用的是 `is_file()` / `exists()`。于是 Linux/Windows 上：
能力矩阵先把它报告成"可用"，直到真正 `spawn` 才炸 `Exec format error` —— 正是
AGENTS §12 反着要求的行为（不可用的原生后端绝不 spawn）。上一轮我在 `tests.yml`
里写的"runner 既不随包 `resources/video`"这句话是错的，随包了，随的是 macOS 的包；
已就地改正。

处置（本轮新增，`07061e3` 之后的追加提交）：

| 位置 | 改动 |
| --- | --- |
| `src-tauri/src/binfmt.rs`（新） | `format_of()` 读 4 字节魔数（Mach-O thin/fat 双向 6 个、ELF、PE），`runnable_here()` 只认本机平台那一种；认不出的头部（脚本、空文件、目录）一律"不是本机后端" |
| `video/process.rs::tool()` | 内置 bundle 候选、`resources/video/bin` 候选、PATH 候选都要过格式门；顺带修第二个真实缺口 —— 这里**从不加 `.exe`**，Windows 上即便随包放了 `ffmpeg.exe` 也永远命中不了（图片线的 `find_tool` 早就处理了这件事） |
| `engine.rs::find_tool()` | 三处随包候选同样加门，回退语义不变 |
| `history.rs` | 补 `#[cfg(windows)]` 的 `an_unreadable_output_is_a_conflict_not_a_pass`：这条"读不出来 = 证明不了归属，必须拦"的门此前只有 unix 覆盖。Windows 上权限位不管用，用 `share_mode(0)` 造 `ERROR_SHARING_VIOLATION`，断言与 unix 那条一字不差 |
| `swift/Tests/HistoryStoreCheck/ContractVectors.swift:368` | 通过时日志打成 `ok 契约向量不一致: …（实际 X，期望 X）` —— 自相矛盾的日志。改成中性标签，判定由 `ok`/`FAIL` 前缀给 |

新增 6 条测试（`binfmt` 2 + 图片线解析 2 + 视频线解析 2），其中两条正向对照只在
macOS 编译：格式闸门不能把自家内置产物一起拒掉，那会让 Direct 主线静默退化成
"全不可用"而单元测试照样全绿。`cargo test --lib` 因此 259 → **265**。

这道门和打包脚本是对得上的，不是新加的限制：`release.yml` 的 `build-windows`
先 `Remove-Item resources\bin\*` 再下载 `*.exe`，`build-linux` 先 `rm -f
resources/bin/*` + `rm -rf resources/lib` 再拷系统 ELF —— 也就是说**发布产物里
根本不会有这些 Mach-O**，被它们冒充的一直是"没跑打包准备步骤"的那条路（本机
`cargo test`、CI 的测试作业）。闸门在 Linux/Windows 上放行的是打包后真正的
ELF/PE 候选，方向与打包一致。

顺带一条如实的结论：Windows 与 Linux 的 Direct 视频轨**至今没有随包引擎**
（`video/bin` 只在 `build-macos` 里投递）。本轮之前它们不是"没有引擎"，而是
"拿着 macOS 的引擎冒充可用、编码时炸 `Exec format error`"；现在是明确的
`ffmpeg unavailable; build the bundled video runtime`。要让非 mac 平台真有视频轨，
缺的是打包投递（以及对应的编码器能力取证），不是把错误改成静默。

那条 `#[cfg(windows)]` 测试在 macOS 上**根本不参与编译**，本机无法证明它能编。用
std-only 表达式桩过了一遍目标类型检查：
`rustc --edition 2021 --crate-type lib --target x86_64-pc-windows-msvc`（`OpenOptions` +
`share_mode(0)` + `fs::read` + `Vec<u8>` 比较 + `remove_file`），EXIT=0。运行时结论
仍以下一次 Actions 为准。

### 追加一轮的门禁读数（macOS 本机，同一日内）

| 命令 | 结果 |
| --- | --- |
| `cargo test --lib` | 265 passed / 0 failed / 2 ignored（23.92s），EXIT=0 |
| `cargo test --lib --features inproc-backends` | 265 / 0 / 0，EXIT=0 |
| `cargo test --no-default-features --features inproc-backends --lib` | 265 / 0（10:19:26→10:20:16 实测重编），EXIT=0 |
| `cargo test`（含集成） | lib 265 / 0；崩溃注入 6 / 0；压缩契约 5 / 0，EXIT=0 |
| `cargo test --lib real -- --ignored` | 2 passed —— 捆绑的 macOS 引擎在闸门之后**仍**被正确解析 |
| `npm run test:frontend` | 全绿；N=1000 scroll×100=48.5ms、N=5000=89.6ms、N=10000=121.4ms（windowRows=20），10k 全量参考 first=246.2ms / clean=10.48ms |
| `bash scripts/test_swift_history.sh` | 全部 `ok` / 0 FAIL（含 32 条契约向量标签修正后复跑） |
| `rustc --crate-type lib --target x86_64-pc-windows-msvc`（Windows 新测试的表达式桩） | EXIT=0 |

追加轮的负向对照（亲手拆门，证这两条新门不是空话）：

| 拆掉的门 | 期望的失败 | 实测 |
| --- | --- | --- |
| `binfmt.rs`：`Some(Format::Elf) => cfg!(target_os = "linux")` 改成 `=> true` | 平台归属门必须响 | `assertion left == right failed / left: true / right: false`，1 passed / 1 failed |
| `binfmt.rs`：`runnable_here` 对任何存在的文件返回 `false` | 两条**正向**对照必须响 | `cwebp: macOS 上内置工具必须仍是解析结果`、`macOS 上内置 ffmpeg 必须仍是解析结果`，0 passed / 2 failed |

两处改完都原地还原并核对 md5（`2fd2425a…`，控制前后同值），还原后复跑 265 全绿。


### 本轮门禁命令与结果（macOS 本机，2026-11-01）

| 命令 | 结果 |
| --- | --- |
| `cargo test --lib` | 259 passed / 0 failed / 2 ignored（23.58s），EXIT=0 |
| `cargo test --lib --features inproc-backends` | 261 / 0 / 0（43.01s），EXIT=0 |
| `cargo test --no-default-features --features inproc-backends` | lib 261 / 0；集成 6 / 0 与 5 / 0；EXIT=0 |
| `cargo test --test crash_injection -- --nocapture` | 6 阶段逐条打印 + 每阶段二次恢复幂等，EXIT=0 |
| `cargo test --lib rust_executes_every_contract -- --nocapture` | 「契约向量 32 条与 Swift 期望一致」，EXIT=0 |
| `cargo test --lib real -- --ignored` | 2 passed（含 `s11_real_media_matrix`，捆绑 FFmpeg 实编实探），EXIT=0 |
| `npm run test:frontend` | 全绿 EXIT=0；基准 N=1000 scroll×100=48.4ms、N=5000=95.7ms、N=10000=121.3ms（windowRows=20），10k 全量参考 first=245.8ms / clean=10.51ms |
| `bash scripts/test_swift_history.sh` | 546 项 `ok`、0 项 FAIL，EXIT=0 |
| `rustc --crate-type lib --target x86_64-pc-windows-msvc src/win32_fs.rs` | EXIT=0（新模块单独通过 Windows 目标类型检查） |

测试数从 258 → 259（default）、260 → 261（inproc / appstore）：新增
`every_unavailable_format_explains_itself_and_stays_out_of_the_targets` 一条，无删除。

### 本轮负向对照

| 亲手拆的门 | 期望的失败 | 实测 |
| --- | --- | --- |
| 契约向量注入 3 条假结果（`same_size_*`、`empty_placeholder_*`） | 一次报全，而不是只报第一条 | `契约向量不一致 3 条（本轮跑了 32 条）` + 逐条列出，EXIT=101 |
| 把 `tiff` 的不可用理由改成 `None` | 新的"不可用必须自证理由"门必须响 | 该测试 FAILED，其余 5 条仍绿，EXIT≠0 |

两处改完都从 `/tmp` 逐字节还原并核对 md5（`89a15ecb…`、`c98f884f…`），还原后复跑为绿。

### CI 回证（run 41265224402）：格式闸门生效，红挪到下一层

| 作业 | 结论 | 关键读数 |
| --- | --- | --- |
| `windows-tests` | **success**（连续第二次） | `cargo check --lib` + `cargo test --lib` + 契约向量，运行时结论与上一轮同向 |
| `rust-tests` (Ubuntu) | failure，仍只红在 `real media matrix` | 上一轮的 `Exec format error (os error 8)` **消失**；这一轮两条 panic 原文：`FFmpeg failed: Unrecognized option 'fps_mode'. / Error splitting the argument list: Option not found`（`ffmpeg_backend.rs:144`、`:201`；`0 passed; 2 failed … 255 filtered out`） |
| `rust-tests-macos` / `appstore-tests` / `frontend-tests` / `swift-checks` | success | — |

这条读数恰好是新闸门**生效**的证据：Linux 不再拿 macOS 的 Mach-O 冒充可用，而是真的
解析并执行了 apt 装的 ELF `ffmpeg` —— 引擎跑起来了，才谈得上"参数不认识"。红点因此
从"这台机器根本没有引擎"挪到了更深一层：**参数写死了一台机器的拼法**。

根因：`-vsync` 在 FFmpeg 5.0 改名成 `-fps_mode`，而 FFmpeg 9 把 `-vsync` **删掉**了
（实测本机捆绑的 9.0.2：`-fps_mode` EXIT=0、`-vsync` EXIT=8）。Ubuntu 22.04 的 apt
ffmpeg 是 4.4，只有旧名。`video/plan.rs` 的 §5.3 passthrough 硬编码 `"-fps_mode"`，
真实矩阵的**夹具生成本身**也硬编码同一个串，于是 4.4 上必然中途炸 —— 而且炸在
`run()` 里，等于"已经开跑才发现在这台机器上根本说不通"。

处置（本轮，全部在编码前收口）：

| 位置 | 改动 |
| --- | --- |
| `video/capabilities.rs` | `CapabilityMatrix` 新增 `frame_timing_flag`（`#[serde(default)]`，不进前端契约的语义变更）；`probe_frame_timing_flag()` 用一次真试用（`lavfi color → -c:v rawvideo → <flag> passthrough → -f null -`）在新旧两名里选出这台引擎认得的那一个。选 `rawvideo` 是因为它在任何构建里都在，探测结果因此不会被编码器差异污染。两名都不认 → 空串 + `diagnostics` 写明 |
| `video/plan.rs` | `EncodePlan` 携带 `frame_timing_flag`（`#[serde(skip)]`）；argv 用它而不是写死的串；**编码前拒绝**：`This FFmpeg honors neither -fps_mode nor -vsync; preserving source timestamps cannot be guaranteed`。不退回 ffmpeg 默认的 `auto`（会丢帧/重复帧，那是拿另一件事冒充"保留时间戳"，规则 11） |
| `video/ffmpeg_backend.rs` | 真实矩阵的 VFR 夹具与断言改用探测到的名字；`cfr` 的反向断言同时盯新旧两名；两条真实测试各自加 `assert!(!caps.frame_timing_flag.is_empty(), …)`，让"这台引擎说不清时间轴"以这句话失败，而不是以 `Unrecognized option` 失败 |
| `src/win32_fs.rs` | 补 3 条 Windows 测试：目录给出卷号、文件与所在目录同卷、可写目录的剩余空间是正数。上一轮我写的"Windows 目录 fsync / 句柄身份 / `GetDiskFreeSpaceExW` 都在真机上跑过了 249 条测试"**是过度声明** —— `available_space()` 与 `volume_id_of_path()` 当时**一条测试都没有**（grep 为证），已就地更正文字并补上真的测试 |
| `video/transaction.rs` | `available_space_answers_for_a_real_directory`：能回答就必须给出正数；答不出来只允许发生在 unix/windows 之外的平台（不是删断言，是把"不许静默 0"这件事写成断言） |

新增 5 条本机可编译的测试（plan 2 + transaction 1 + real 矩阵内 2 处断言），win32_fs
另加 3 条只在 Windows 编译。`cargo test --lib` 因此 265 → **268**（三种特性组合都是
268 / 0）。

本轮负向对照（证时间轴这道门不是摆设）：把 `probe_frame_timing_flag()` 的候选拼法改成
两个不存在的名字（`-fps_modeXX` / `-vsyncXX`），等价于"这台引擎两名都不认"：

```
test video::ffmpeg_backend::tests::s11_real_media_matrix ... FAILED
panicked at src/video/ffmpeg_backend.rs:229:9:
这台 ffmpeg 既不认 -fps_mode 也不认 -vsync：时间轴语义无法保证
（metadata_policies_hold_on_real_encoded_fixtures 同样在 :166 以同一句失败）
```

改完原地还原并核对 md5（`1e7916da…`，控制前后同值），还原后 `real --ignored` 复跑 2 passed。

### 本轮（时间轴开关）的门禁读数（macOS 本机，2026-11-01）

| 命令 | 结果 |
| --- | --- |
| `cargo test --lib` | 268 passed / 0 failed / 2 ignored（24.06s），EXIT=0 |
| `cargo test --lib --features inproc-backends`（`cargo test --features inproc-backends` 全量） | lib 268 / 0；集成 6 / 0 与 5 / 0；崩溃注入 harness 逐阶段通过，EXIT=0 |
| `cargo test --no-default-features --features inproc-backends` | lib 268 / 0（43.51s）；集成同上，EXIT=0 |
| `cargo test --test crash_injection -- --nocapture` | 6 阶段（每阶段 SIGKILL + 两次恢复）逐条打印并全通过，EXIT=0 |
| `cargo test --lib real -- --ignored` | 2 passed —— 捆绑 FFmpeg 9 探测到 `-fps_mode`，真实编码与 §56 校验通过 |
| `npm run test:frontend` | 全绿；N=1000 scroll×100=47.9ms、N=5000=88.8ms、N=10000=119.3ms（windowRows=20），10k 全量参考 first=243.2ms / clean=10.34ms |
| `bash scripts/test_swift_history.sh` | 全部 `ok` / 0 FAIL，契约向量 32 条与 Swift 期望一致 |

`inproc-backends` 分支下 `probe_frame_timing_flag()` 同样带 `#[cfg(not(feature =
"inproc-backends"))]`：它引用只在 Direct 线存在的 `video::process`，不加闸门会让
App Store 组合编译失败 —— 三条特性组合实测 EXIT=0。

### CI 回证（run 41266916166）：六个作业全绿，两处"待取证"同时兑现

`b7d95ee` 触发的 run <https://github.com/misswell/Sizqo/actions/runs/41266916166>
**conclusion=success**，六个作业逐个的原文读数：

| 作业 | 结论 | 关键读数（日志原文） |
| --- | --- | --- |
| `rust-tests` (Ubuntu 22.04) | success | `cargo test (default)` → **258 passed / 0 failed / 2 ignored**；崩溃注入 6 阶段逐条打印绿；`契约向量 32 条与 Swift 期望一致`；**`real media matrix` → `test result: ok. 2 passed; 0 failed … 258 filtered out`** —— 这条门禁历史上第一次在 Linux 用真引擎转绿 |
| `windows-tests` | success（连续第三次） | `cargo check --lib` 通过；`cargo test --lib` → running 262 / **260 passed / 0 failed / 2 ignored**；`win32_fs::tests::a_directory_yields_its_volume_id`、`a_file_and_its_directory_share_a_volume`、`available_space_is_a_positive_number_for_a_writable_directory` 三条**逐个 `... ok`**；`video::transaction::tests::available_space_answers_for_a_real_directory ... ok`；契约向量 32 条一致 |
| `rust-tests-macos` | success | `cargo test --lib` → 268 / 0 / 2；`--features inproc-backends` → 268 / 0；崩溃注入绿 |
| `appstore-tests` | success | `cargo test --no-default-features --features inproc-backends` → lib 260 / 0；集成 6 / 0 与 5 / 0 |
| `frontend-tests` | success | 29 套件全绿（10k windowRows=20 基准照旧） |
| `swift-checks` | success | 历史/备份/调度/契约自检全 `ok` |

两条上一轮写着"只能等 Actions"的待证项因此同时关掉：

1. **`-vsync` 这一支第一次被真老引擎兑现。** Ubuntu 装的是
   `7:4.4.2-0ubuntu0.22.04.1`（日志 `Setting up ffmpeg …` 原行），它没有 `-fps_mode`：
   真实矩阵现在能过，只能是探测选出了 `-vsync`，夹具与 `plan` 的 argv 都用了旧名，
   产物还过了 §56 全量校验（时长/平均帧率在容差内）。这也反过来说明上一轮的红
   **不是"CI 环境缺东西"**，而是我们确实把一台只认旧名的引擎的参数写死了新名。
2. **Windows 的卷号与剩余空间原语拿到运行时结论。** `FILE_FLAG_BACKUP_SEMANTICS`
   开目录句柄、`GetFileInformationByHandle` 的卷号一致性、`GetDiskFreeSpaceExW` 给出
   正数，全部在 NTFS 上真跑过；P0-3 那条"编译检查通过不等于运行时成功"到此关闭，
   上一轮我写下的过度声明也被真测试替代。

仍要如实留着的一条：Windows / Linux 的 Direct 视频轨**依旧没有随包引擎**
（`resources/video/bin` 只由 `build-macos` 投递）。现在的行为是能力矩阵回答
`ffmpeg unavailable; build the bundled video runtime`，而不是拿 macOS 的 Mach-O 冒充
可用再在 `spawn` 处炸 —— 这一条的缺口在打包投递与对应平台的编码器取证，不在源码里
把错误改成静默。

### 同一类问题的另一半：平台对上，架构还没对上（Mach-O 切片门）

格式门转绿之后回头审它自己，发现 `runnable_here` 只回答"这是不是 mac 的产物"，
没有回答"这台 mac 接不接得住"。`lipo -info` 逐个实测（本轮取证）：

```
video/bin/ffmpeg   arm64      video/bin/ffprobe  arm64
bin/cwebp          arm64      bin/pngquant       arm64
bin/oxipng         arm64      bin/cjxl           arm64
video/lib/*.dylib  全部 arm64（libavcodec.63 / libavformat.63 / libSvtAv1Enc.4 / …）
```

而 `release.yml` 的 mac 线同时构建 `aarch64-apple-darwin`、`x86_64-apple-darwin`
与二者的 universal 合并包（`build-macos` 矩阵 + `build-macos-universal`）。也就是说：
**仓库里提交的这套引擎是 arm64 单切片，Intel 上拿到它 `posix_spawn` 只会得到
`EBADARCH`「Bad CPU type in executable」** —— Rosetta 只反着帮（Intel 机器跑不了
arm64），所以这不是"可能有问题"，是与上一轮 `Exec format error` 同一条链的下一环。

**这条结论的范围，本轮由 run 41268093305 的日志收窄了，属于我自己的过度声明，如实更正**：
`build-macos` 的 `Install CLI tools` 会在**各自 runner 上**跑
`scripts/collect_macos_tools.sh`，该脚本重写 `resources/video/bin` 并在末尾用
`expected_arch="$(uname -m)"` 对每个产物做 `file … | grep -q "${expected_arch}"`，
不匹配直接 `exit 1`；`build-macos-universal` 再对所有 Mach-O 成对 `lipo -create`。
实测日志：

```
build-macos (x86_64 …)  Install CLI tools    Collected 2 tools -> src-tauri/resources/video/bin (19 dylibs)
build-macos (x86_64 …)  Install CLI tools    Collected macOS tools for x86_64: src-tauri/resources
build-macos-universal   Merge Universal app  …/Contents/Resources/video/bin/ffmpeg: replacing existing signature
build-macos-universal   Merge Universal app  …/Contents/Resources/video/bin/ffprobe: replacing existing signature
```

所以**已分发的 Intel dmg 与 universal 包里的视频引擎是架构对得上的**（x86_64 切片 /
双切片），上一轮那句"那条轨道上根本没有可用引擎"对发布产物不成立，撤回。
架构门真正吃得住的是这四类：

| 场景 | 门生效与否 |
| --- | --- |
| Intel mac 上 `git clone` 后直接 `cargo tauri build` / 跑测试（用仓库里 arm64 的提交产物） | 生效 —— 这正是 `EBADARCH` 的现场 |
| Linux / Windows 上任何构建（提交的是 Mach-O） | 生效（上一轮的格式门，本轮仍在） |
| 任何跳过 `collect_macos_tools.sh` 的打包步骤 | 生效 —— 投递错切片不再静默 |
| CI 的 mac 矩阵（每次现场重采） | 不参与结论：那里的引擎本来就是本机架构 |

发布说明里"macOS Intel：已签名公证，但尚无人在 Intel 真机上完成验收"这条不变：
缺的仍是**真机验收**，不是引擎。

处置（`src/binfmt.rs`）：

| 点 | 内容 |
| --- | --- |
| 切片判定 | thin Mach-O 读 `magic + cputype`（`0x01000007` = x86_64、`0x0100000C` = arm64），字节序由魔数决定（`CFFAEDFE`/`FEEDFACE` 小端，`FEEDFACF`/`CEFAEDFE` 大端）；fat（`CAFEBABE`/`BEBAFECA`）逐条扫描 `fat_arch.cputype`，最多 16 条 |
| 判定失败的方向 | 结构读不出、本机架构不认识、切片里没有本机架构 → 一律"不是后端"。半截头（只有 4 字节魔数）也归此类：证明不了就不 exec |
| universal | 同时列出两个切片 → 两种 mac 都放行，否则闸门会把自家产物一起拒掉（这条有独立断言） |
| 刻意保守的一条 | 只有 x86_64 切片的引擎在 Apple Silicon 上返回不可用，尽管装了 Rosetta 2 能跑。不假设 Rosetta 存在，因为这条门要拦的正是"以为能跑"；真要支持，靠打包投 universal 引擎，不靠猜 |
| 结论要能区分 | `video::process::tool()` 现在分两句报错：`… cannot run on this machine (platform or CPU slice mismatch)` / `… unavailable: the bundled engine does not match this platform or CPU; ship the matching slice` 对比 `build the bundled video runtime`。"跑不动"与"没随包"的排查方向完全不同，含糊成一句会把人带去重跑打包 |

新增 3 条测试（`binfmt` 架构门 1 条 + 图片线"解析结果必须本机跑得动"1 条 + 视频线
"报错要说清是哪半条门失败"1 条），并把两条 macOS 正向对照的条件从"文件存在"改成
"文件存在**且本机跑得动**"，其反面由既有的拒绝断言负责 —— 两条合起来在 arm64、
Intel、Linux、Windows 上都不空判。`cargo test --lib` 271 passed / 0 failed（三种特性
组合 271 / 270 / 270，`real --ignored` 2 passed，崩溃注入 6 阶段全过）。

两条控制（都用 `/tmp` 里的逐字节副本还原并核对 md5 `eb907fe7…`）：

| 亲手拆的门 | 期望的失败 | 实测 |
| --- | --- | --- |
| `runnable_here` 里去掉 `&& macho_has_host_slice(path)` | 架构门必须响 | `a_macho_slice_the_host_cpu_cannot_run_is_never_a_backend ... FAILED`，2 passed / 1 failed |
| `macho_has_host_slice` 一律返回 `false` | 正向对照必须响 | `a_native_format_is_runnable_only_on_its_own_platform ... FAILED` 与同一条架构测试一起 FAILED（universal 不许被误杀那句），1 passed / 2 failed |

本轮另有一条自己的操作失误要记下：做控制 A 的还原时用了 `git checkout -- src/binfmt.rs`，
而该文件里还压着**本轮未提交的新代码**，一条命令把它打回上一版的 96 行。重写之后
先把干净版本复制成 `/tmp/binfmt_pristine.rs`，两条控制都改用副本还原并核对 md5。
**在带未提交工作的文件上做注入实验，还原手段必须是副本，不能是 `git checkout`。**

### 同一类问题的第三处：PATH 上的候选走的是同一条门，但不是同一句判断

`runnable_here` 补完架构之后，回头查它的**消费方**，发现还剩一段没问：随包候选过了门，
系统候选没过。图片线 `engine::find_tool` 的 PATH 扫描是 `p.exists()` 就返回，
`where` 只取第一行；视频线 `video::process::tool()` 更糟 —— 它把随包那条
**正向**门（`runnable_here`）直接套在 PATH 上。两边各错一半：

| 位置 | 原来 | 错在哪 |
| --- | --- | --- |
| `engine.rs` PATH / `where` | 存在即返回 | 用户从别处拷来的另一平台二进制（`~/.local/bin/ffmpeg` 是 Mach-O 而机器是 Linux）照样被端出去，炸的还是 `Exec format error (os error 8)`；`where` 命中多个时只看第一个，后面明明有一个能跑的 |
| `video/process.rs` PATH | 必须过 `runnable_here` | 把**认不出格式**也当成不可用。flatpak / snap 的 shim、shell 包装是真能 exec 的，这条会把跑得动的机器报成"引擎不可用" —— 是上一轮"闸门误杀自家产物"同一种错，只是这次杀的是用户的引擎 |

所以分家成两句（`src/binfmt.rs`，仍然只有一份实现）：

- `runnable_here`：正向门。格式认不出 → 不是后端。用在**随包**候选上，因为那是我们自己投的文件，
  必须是本机原生二进制，投错了就该拒。
- `plausible_system_backend`：`format_of(path).is_none() \|\| runnable_here(path)`。
  只拒"认得出格式且不是本机那一种"这一类。用在系统候选上；引擎到底行不行，交给真跑一次的
  能力探测（`discover()` / `probe_frame_timing_flag`）定论，而不是靠猜文件长相。
- `system_tool_in(dirs, file_name)`：PATH 扫描本身也收成一份，图片线（Windows 的 `;` 分隔 +
  `where` 全部命中逐条问）和视频线（`split_paths`）共用，避免"一处补、一处漏"。

新增 2 条测试（`binfmt`）：`the_path_gate_rejects_foreign_formats_but_not_wrappers`
（本机格式 / ELF / Mach-O / PE 四种输入的结论逐一对上本机平台，且同一条脚本文件在两句判断下
结论**必须相反** —— 这条是两句分家本身的断言）与
`the_path_scan_steps_over_an_unexecuable_candidate`（前一个目录躺着跑不动的候选时必须继续找，
而不是把第一个"存在"的交出去；找不到才是 `None`）。

本轮门禁命令与结果（macOS 本机，2026-11-01，全部 EXIT=0）：

| 命令 | 结果 |
| --- | --- |
| `cargo test --lib` | **273 passed / 0 failed / 2 ignored**（`binfmt` 单跑 5 条全绿） |
| `cargo test --features inproc-backends` | 272 passed / 0 failed，另 6 / 5 条全绿 |
| `cargo test --no-default-features --features inproc-backends` | 272 passed / 0 failed，另 6 / 5 条全绿 |
| `cargo test --test crash_injection` | 崩溃注入 6 个阶段（每阶段 SIGKILL + 两次恢复）全部通过 |
| `cargo test --lib rust_executes_every_contract` | 契约向量 32 条与 Swift 期望一致 |
| `cargo test --lib real -- --ignored` | 2 passed / 0 failed（本机 ffmpeg 真编码） |
| `npm run test:frontend` | 27 条 PASS，`N=10000` first=10.8ms / clean=0.75ms |
| `bash scripts/test_swift_history.sh` | 546 行 `ok`，脚本自检全通过；两次运行的差异仅为临时目录 UUID 与时间戳 |

负向对照（`/tmp/binfmt_pristine_b.rs`，md5 `a7a57cf3…`，两轮都按副本还原并核对）：

| 亲手拆的门 | 期望的失败 | 实测 |
| --- | --- | --- |
| `system_tool_in` 里去掉 `&& plausible_system_backend(path)` | 扫描门必须响 | `the_path_scan_steps_over_an_unexecuable_candidate ... FAILED`，`left: Some("…/bad/ffmpeg") right: None`，4 passed / 1 failed |
| `plausible_system_backend` 退化成 `runnable_here`（去掉"认不出就放行"那半） | 分家本身必须响 | `the_path_gate_rejects_foreign_formats_but_not_wrappers`（「脚本包装不该被格式门误杀」）与扫描那条一起 FAILED，3 passed / 2 failed |

### CI 回证（run 41268093305 = `v0.3.6-rc.5` 三轨道构建，run 41269122796 = `e175a7c` 测试）

`tests.yml` run **41269122796**（提交 `e175a7c`，含架构门）7 个作业全 success，逐作业读数：

| 作业 | 关键读数 |
| --- | --- |
| `rust-tests`（Ubuntu） | `cargo test (default)` **261 passed / 0 failed / 2 ignored**；契约向量 32 条一致；`real media matrix` **2 passed**（apt 的 4.4 走 `-vsync` 那一支） |
| `windows-tests` | **263 passed / 0 failed / 2 ignored**；契约向量 32 条一致 |
| `rust-tests-macos` | default **271 passed / 0**；`--features inproc-backends` **270 / 0**；APFS 崩溃注入 6 阶段全过 |
| `appstore-tests` | **262 passed / 0 failed** + 6 / 5 条 + 崩溃注入全过 |
| `frontend-tests` / `swift-checks` | success |

发布 run **41268093305**（标签 `v0.3.6-rc.5`）8 个作业全 success：
`verify-appstore`、`build-windows`、`build-macos (arm64)`、`build-macos (x86_64, macos-15-intel)`、
`build-macos-universal`、`build-linux`、`compression-parity`、`release`。
门禁项逐条对上：Windows 版本串去后缀生效（`Sizqo_0.3.6_x64_en-US.msi` / `_amd64.deb` 无 `rc`）；
`release` 作业按标签判定预发布 —— 日志原文「预发布 v0.3.6-rc.5：不产出 latest.json，
应用内更新源保持指向上一个正式版」，Release 的 `prerelease: true`，仓库 `latest` 仍是 `v0.3.5`。
`v0.3.6-rc.5` 资产 9 个（3 个 macOS dmg + universal 更新包及其 `.sig` + Windows msi/setup.exe +
Linux AppImage/deb）。

### 同一 run 的 rust-tests 里第一次同时拿到两条真实引擎读数

时间轴开关的取证矩阵现在补齐了三格，第四格仍然是空的：

| 引擎 | 开关 | 结论 | 来源 |
| --- | --- | --- | --- |
| macOS 本机（homebrew） | 已删 `-vsync` → 只有 `-fps_mode` | 探测选 `-fps_mode`，argv 里无裸 `fps_mode` | 本机 `cargo test --lib real -- --ignored` |
| Intel runner（apt/brew `ffmpeg 9.0.1`） | 同上 | 已部署的 universal 包（`lipo -archs` 两切片齐全）用的正是这支，`-vsync` 在其中不存在 | run 41268093305 的 `build-macos (x86_64)` 日志 `Pouring ffmpeg--9.0.1.sonoma.bottle.tar.gz` |
| Ubuntu 22.04（apt `7:4.4.2-0ubuntu0.22.04.1`） | 只有 `-vsync` | 探测回退到 `-vsync passthrough` 并真编码通过 | run 41269122796：`Setting up ffmpeg (7:4.4.2-0ubuntu0.22.04.1)` + `real media matrix` **2 passed / 0 failed** |
| Windows | **无任何真实编码取证**（Direct 轨缺 PE 引擎） | 仍未取证 | — |

这里要撤回我自己上一轮写的一句话。我写过"`file` 校验与 `libx264` 断言在同一 step、
同一份日志里"，并把 `ffmpeg -hide_banner -encoders | grep -q libx264` 当作某个
`Install system dependencies` 步骤里的**日志读数**。实际情况：这两条断言在
`.github/workflows/tests.yml:37` 与 `:38`，属于 `rust-tests` 作业的
**`real media matrix` 那一步**，而且 `windows-tests` 作业根本没有这一步（它只有
`cargo check --lib` / `cargo test --lib` / 契约向量三步）。把命令文本当成日志读数，
和把日志文本当成断言结果，是同一种错。**Windows 那一格仍然是空的。**

### CI 回证（run 41270303470 = `6217d6f`，PATH 门那一轮）：六个作业全绿

上一轮记在"仍未取证"里的那条（架构门 + PATH 门没有 CI 读数）由这一 run 兑现。
逐作业读数，条数都对得上本轮新增的 2 条 `binfmt` 测试：

| 作业 | 关键读数 |
| --- | --- |
| `rust-tests`（Ubuntu 22.04） | `cargo test (default)` **263 passed / 0 failed / 2 ignored**（上一 run 同一条是 261）；契约向量 32 条一致；`Setting up ffmpeg (7:4.4.2-0ubuntu0.22.04.1)` → `real media matrix` **2 passed / 0 failed** |
| `windows-tests` | **265 passed / 0 failed / 2 ignored**（上一 run 263）；契约向量 32 条一致 |
| `rust-tests-macos` | default **273 / 0 / 2 ignored**（与本机 `cargo test --lib` 完全一致）；`--features inproc-backends` **272 / 0**；APFS 崩溃注入 6 阶段全过 |
| `appstore-tests` | **264 passed / 0 failed**（上一 run 262）+ 6 / 5 条 + 崩溃注入全过 |
| `frontend-tests` / `swift-checks` | success |

Windows 那一格仍然**没有**真实编码读数：`windows-tests` 只有 `cargo check --lib`、
`cargo test --lib`、契约向量三步，没有 `--ignored` 真实矩阵，也没有 PE 引擎。
所以本轮补的是"解析门不再骗人"，不是"Windows 视频轨可用"——后者连引擎都还没有。

### CI 与发布回证（run 41271479123 / 41271481909 = `v0.3.6-rc.6`）

Tests run **41271479123**（版本提交 `c181614`）6 作业全 success，逐作业读数与上一 run
逐条相同（本轮只动版本串，测试集合没变）：`rust-tests` default **263 / 0 / 2 ignored**
+ 契约向量 32 条 + `Setting up ffmpeg (7:4.4.2-0ubuntu0.22.04.1)` + `real media matrix`
**2 / 0**；`windows-tests` **265 / 0 / 2 ignored**；`rust-tests-macos` **273 / 0** 与
`--features inproc-backends` **272 / 0** + APFS 崩溃注入 6 阶段；`appstore-tests`
**264 / 0** + 6 / 5 条；`frontend-tests` / `swift-checks` success。

Release run **41271481909**（标签 `v0.3.6-rc.6`）8 作业全 success：
`compression-parity`（含标签/版本守卫，日志 `RELEASE_TAG: v0.3.6-rc.6`、
`SOURCE_REF: v0.3.6-rc.6`）、`verify-appstore`、`build-windows`、
`build-macos (arm64)`、`build-macos (x86_64, macos-15-intel)`、`build-macos-universal`、
`build-linux`、`release`。发出去的门禁结论：

| 门禁项 | 读数 |
| --- | --- |
| 预发布不抢 latest | `gh release view v0.3.6-rc.6` → `prerelease: true`；`gh release view`（latest）→ `latest=v0.3.5 prerelease=false` |
| 不产 `latest.json` | `release` 作业日志原文「预发布 v0.3.6-rc.6：不产出 latest.json，应用内更新源保持指向上一个正式版」 |
| Windows/RPM 版本串去后缀 | 资产名 `Sizqo_0.3.6_x64_en-US.msi` / `Sizqo_0.3.6_x64-setup.exe` / `Sizqo_0.3.6_amd64.deb` / `_amd64.AppImage`（无 `rc`）；macOS 三件仍带完整 `v0.3.6-rc.6` |
| 更新包签名 | `Sizqo_v0.3.6-rc.6_macos_universal.app.tar.gz` + `.sig` 均在位（`TAURI_SIGNING_PRIVATE_KEY` 已配置，缺失时该作业按设计中止发版） |
| 资产总数 | 9 个 |

### 同一条门搬到投递侧：包里错切片现在会让发布作业红

运行时那道门（`binfmt.rs`）只会把错切片报成「引擎不可用」—— 用户看到的是图片/视频
功能凭空缺失，日志里一个错都没有。采集侧的校验（`collect_macos_tools.sh` 按 `uname -m`）
护的是**源目录**，护不到"跳过采集、换 runner 镜像、加缓存、手改资源路径"之后的包。
所以补上第三个时点：**装进 .app 之后**，按这个 job 的目标架构在包内实测。

- 新增 `scripts/verify_bundle_arch.sh <目录> <arm64|x86_64>`：对目录里每个 Mach-O
  跑 `lipo -archs`，缺目标切片就 `exit 1`；非 Mach-O 跳过；一个 Mach-O 都没数到也
  `exit 1`（空目录和通过长得一模一样，这是最危险的假绿）。
- `release.yml` 的 `build-macos` 矩阵在 `Bundle the video engine` 之后接一步
  `Refuse an engine bundle without the target slice`，检查
  `Contents/Resources/video/bin`（这一步的路径是本步自己写的，确定）与
  图片工具目录 —— 后者的落点由 Tauri 决定，所以 `resources/bin` 与 `bin` 两种已知布局
  都试，都没命中就先把 `Contents/Resources` 列出来再红：一次运行要能说清
  "缺的是切片，还是我的路径猜错"。
  **不**加到 `build-macos-universal`：合并包里合法地存在单架构 dylib
  （本轮日志就有 `Keeping arm64-only resource: Contents/Resources/video/lib/libcrypto.4.dylib`
  与 `Keeping x86_64-only resource: …/libssl.3.dylib`），那条 job 的判据是
  `merge_macos_universal.sh` 自己的配对与 `verify_macho_architectures`，不是这一条。
- 本轮只验到 YAML 可解析、脚本 `bash -n` 干净、循环在 `set -euo pipefail` 下不会被
  未命中的候选目录提前退出（实测过），以及 `build-macos` 步骤顺序正确。
  **这一步的真实执行要等下一个标签的 release run**，在那之前它属于"已接线、未跑过"，
  不算通过。

本机实测（提交树，五种读数，含两条对照）：

```
bash scripts/verify_bundle_arch.sh src-tauri/resources/bin arm64        → ok ×7，EXIT=0
bash scripts/verify_bundle_arch.sh src-tauri/resources/video/bin arm64  → ok ffmpeg/ffprobe，EXIT=0
bash scripts/verify_bundle_arch.sh src-tauri/resources/video/bin x86_64 → ✗ ffmpeg 的切片 [arm64] 里没有目标架构 x86_64，EXIT=1
bash scripts/verify_bundle_arch.sh <空目录> arm64                        → ✗ 没有任何 Mach-O，EXIT=1
bash scripts/verify_bundle_arch.sh src-tauri/resources/bin ppc64         → ✗ 目标架构只能是 arm64 / x86_64，EXIT=2
```

顺带把上一轮"待实测"那条结清：提交树里的引擎实测 `lipo -archs ffmpeg` = `arm64`，
图片工具 7 个同样只有 arm64 —— 也就是本轮文档里那句"仓库提交的产物是 arm64 单切片"
是量出来的，不是从 `file` 输出推的。**已分发 universal 包**内的双切片仍未直接实测
（190 MB 资产下载不动）；这一步上了 CI 之后，Intel 那条 job 会自己打出
`ok ffmpeg → x86_64` 这样的读数，届时按日志取证。

### 标签：`v0.3.6-rc.5` 与 `v0.3.6-rc.6` 都已发布，都不算正式版

- `v0.3.6-rc.5` 指向 `56589af`：`https://github.com/misswell/Sizqo/releases/tag/v0.3.6-rc.5`。
  它**不含**架构门也不含 PATH 门，因此那棵树上"Intel mac 本地构建 + 仓库提交的 arm64 引擎"
  仍是平台门放行、编码时才炸 `EBADARCH`；已分发的 Intel/universal 安装包不受影响，
  依据是上面表格里 runner 现场重采 + `lipo` 合并的日志。
- 这条标签说明正文里有一句笔误：写成了"FFmpeg 9 已删新…"，本意是"FFmpeg 9 已删 `-vsync`"。
  标签已分发，改它等于 force-push 覆盖已发布标签，因此**不改写**，正确结论以本节与代码
  注释为准。
- `v0.3.6-rc.6` 指向 `c181614`（架构门 `e175a7c` + PATH 门 `6217d6f` + 版本提交）：
  `https://github.com/misswell/Sizqo/releases/tag/v0.3.6-rc.6`。版本串这轮落到**四处**并对齐：
  `package.json` / `tauri.conf.json` / `tauri.conf.appstore.json` / `Cargo.toml`（+ `Cargo.lock`）。
  App Store 那份配置原先还停在 `rc.3` —— `scripts/build_appstore.sh` 只从它自己读版本号，
  没有任何地方把它和 Direct 对齐，而 `release.yml` 的守卫只查三处，所以那条轨道的 bundle
  元数据落后了三轮。守卫的覆盖面缺口记在这里，本轮先对齐数值，不改守卫逻辑（改法要么是
  第四处进守卫，要么是单一版本源，两者都超出本轮范围）。
- `rc.1…5` 一律不动、不改写；`v0.3.6` 正式版仍等发布门禁齐了才打。


### 仍未取证的部分（如实）

- 本机交叉编译依旧不可行（`ring` 的 cc-rs 报 `'assert.h' file not found`），所以
  Windows 分支里除 `win32_fs.rs` 单独过了目标类型检查外，其余（`source_identity`、
  `output_transaction`、`history` 的 `share_mode` 现场、`commands` 的路径、
  `image_capabilities` 的 Windows 半条、`video` 的两个新原语）**只能由 CI 的
  `windows-tests` 作业取证** —— 这一条已经在 run 41235873134 上兑现（249 / 0）。
- `binfmt` 格式门、`tool()` 的 `.exe`、Windows 版"读不出来"门已被 run 41265224402
  看过：`windows-tests` success，Linux 侧 `Exec format error` 消失、改为解析到 apt 的
  ELF 引擎。上一轮记在这里的两条待证项（`-vsync` 分支、win32_fs 的 3 条新测试）
  已由 run 41266916166 逐条兑现，见上表。
  - 附带一条方法上的教训：目标类型检查的探针**必须先自证它在编译**。第一版把测试
    原样留在 `#[cfg(test)]` 里，`--cfg test` 并**没有**让这段参与编译（rustc 反而提示
    "consider adding `#[cfg(test)]`"、`use super::*` 报 unused）—— 那次 EXIT=0 是假的。
    改成剥掉 `#[cfg(test)]`/`#[test]` 让函数体作为普通代码参与，并先注入一条故意的
    类型错误确认会报错，再撤掉错误取 EXIT=0。**一个不知道自己是否在跑的探针，
    和没有探针等价。**
- Windows / Linux 产物仍未签名（无对应签名链），正式版的门禁照旧不通。
- **仓库里提交的 macOS 产物仍只有 arm64 单切片**（7 个图片工具 + `video/bin/ffmpeg`、
  `ffprobe` + 全部 dylib）。CI 的 mac 矩阵不受影响：`collect_macos_tools.sh` 在每台
  runner 上现场重采并按 `uname -m` 硬门校验，universal 再由 `lipo -create` 成对合并 ——
  这一条由 run 41268093305 的日志取证：merge 步骤对 `Contents/Resources/video/bin/ffmpeg`
  与 `ffprobe` 都执行了"重签名"那一步，而该步骤只有在 `verify_macho_architectures`
  通过之后才跑到；脚本对"arm64 有、Intel 没有对应文件"的非 dylib 资源是直接 `exit 1`
  的。注意这是**按脚本契约推出来的结论，不是对已分发包的实测**：本轮试着把 universal
  包从 Release 资产拉下来直接 `lipo -archs`，两条路（`gh release download`、带
  `Accept: application/octet-stream` 的 `curl -C -`）都在 190 MB 的量级上跑到
  KB/分钟级就停滞，取不到读数，因此这条**仍记为待实测**，不当已通过。
  受影响的是**不经重采的那条路**：
  Intel mac 上 `git clone` 后直接构建 / 跑测试。架构门现在会正确拒绝 arm64 单切片，
  于是那条轨道的图片工具退到系统 PATH、视频轨报告不可用 —— 这是如实的拒绝，不是修复。
  要把这一条也补成"真有引擎"，缺的是把 universal / x86_64 切片一起提交进仓库
  （以及对应的 GPL 对应源码义务），不是把闸门放宽回"平台对上就 exec"。
- 本轮新记一条：**引擎版本随 runner 而变** —— Intel runner 实测装到
  `ffmpeg 9.0.1`（`-vsync` 已删的那一支），Ubuntu 22.04 的 apt 是 `7:4.4.2`（只有
  `-vsync`）。两条拼写由同一次真跑探测定论，因此这条差异不需要在文档里"钉一个版本"；
  但它意味着**已分发安装包里的引擎并不相同**，跨机器复现时间轴类问题时得先问
  `caps.frame_timing_flag`，而不是假设两处一致。

## Release gate (per [AGENTS.md](../AGENTS.md))

Before any tag/Release: full three-track build, signed Direct runtime check,
signed sandbox check, real fixture video matrix (AGENTS.md §11 items 1–12),
FFmpeg licensing review. A local commit is not a GitHub push; every published
tag must link its artifacts here with the exact commands used.
