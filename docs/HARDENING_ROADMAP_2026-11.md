# 可靠性加固路线图（2026-11）

来源：外部静态审查（19 项发现）与《全仓库可靠性重构方案》。本文不是照抄：
全部发现已按 `main` @ `8ef5a73`（0.3.6-rc.9）逐条回证，行号为回证时的实测行号。
审查承认未经运行复现；本文只把**回证成立**的项排入轮次，并裁定与继承规则的冲突。

回证方式：8 个只读代理分别核对 output_transaction.rs、history.rs/恢复、Swift 取消、
图片完整性、视频校验、输出路径、队列状态机、CI/轨道。未做运行时复现。

---

## 一、验证结论总表

| # | 发现 | 回证 | 关键证据（file:line） | 轮次 |
|---|---|---|---|---|
| F1 | 三级台阶第三级占位身份竞态 | 确认 | output_transaction.rs:949-1000；凭证来自关闭句柄后的路径 stat（:934，`file_identity`=:148 fs::metadata），:986 裸 `fs::rename` 无提交前复核，失败分支 :990 才比对 | R1 |
| F2 | 隔离名占位凭证未留存，后续裸 rename | 确认 | :810-818 占位后 `keep()` 丢弃凭证；:1074、:1216 覆盖式 rename | R1 |
| F3 | conflict_sibling 的 exists()+rename 残留 | 确认 | :1389-1393 exists() 选名；消费点 :772、:1167 | R1 |
| F4 | publish_claimed 凭证缺失退化裸 publish | 确认 | :1736-1738 `let Some(expected)=identity else { return self.publish(target); }`；publish :1718 裸 rename；全仓唯一调用点 commands.rs:1447 | R1 |
| F5 | Rust restore 主链无原子身份校验 | 确认 | history.rs:1511 冲突检查 → :1545 写回 → :1412-1420 → StagedOutput::publish :1717 裸 rename；对照 rollback :467-540 有完整复核而 restore 未复用 | R1 |
| F6 | 后缀撤销：SHA 检查后裸 remove_file | 确认 | history.rs:1569 检查 → :1583 `fs::remove_file`；带指纹门的 retire_generated_output :603-631 未复用 | R1 |
| F7 | 前端 Missing 收走恢复入口，与注释矛盾 | 确认 | history.rs:543-545 装饰为 Missing；app.js:4483-4485 要求 `status==='compressed'`；注释 :4481-482 明言不应收；后端 restore 本体无 status 门槛（:1472-1564） | R2 |
| F8 | Swift restore 缺 already_restored / 指纹门 | 确认 | HistoryStore.swift:1248-1298 仅 `status != .restored` 门槛；Rust 幂等在 history.rs:1487-1490；Swift 删产物 :1229-1239 无指纹门（Rust :1433-1458 有） | R2 |
| F9 | Swift 清空队列不拦提交（P1） | 确认 | AppState.swift:1030-1033 compressOneStatic 零取消检查点；cancelBox 仅闸门前消费（:863/:876）；writeOutputStatic :1288-1345 直接 rename+history；Rust 参照系：commit_output 首行取消闸 commands.rs:1211-1213 → publish_cancelled :1162-1165 | R2 |
| F10 | Swift 调度无内存预算 | 确认 | CompressionScheduler.swift:235-252 仅 CPU 判据；Rust 参照：image_memory.rs:40-88 + commands.rs:370-399 三档内存门 | R2 |
| F11 | 预检期暂停/停止被 try_begin_batch 清零 | 确认（低危） | commands.rs:241-246 清位；但预检期按钮本就隐藏（app.js:2999/3023、index.html:608/614），phaseState 只写不读 | R2（低优） |
| F12 | EXIF Orientation 两侧高级路径均未应用 | 确认 | Swift 四个裸解码点 CompressionEngine.swift:320-323/359-363/374-378/460-464；Rust image::open :838/:870/:920/:1290，image 0.25.10（Cargo.lock:1842）API 在而未用；系统路径两侧已正确 | R3 |
| F13 | Swift 高级模式动画静默丢帧 | 确认 | compressWebPImageIO :460-473 单帧；换格式分支 :250-270 无帧数检查；Rust animation_rejection engine.rs:624-660 六处拦截 + 4 条测试；Swift 系统路径已拒绝（SystemImageConverter.swift:40-49） | R3 |
| F14 | 帧率容差过宽，无帧数/时间轴校验 | 确认 | validation.rs:69 `tolerance=(expected*0.05).max(0.5)`；无 nb_frames 比较；缓解：时长帧间隔容差已存在（:56-58） | R3 |
| F15 | KeepAll 只比数量 | 确认 | validation.rs:190-192 仅章节数；字幕/音轨逐流 disposition、章节起止、附件内容、元数据键值全缺 | R3 |
| F16 | ffprobe 阶段不可取消 | 确认 | ffmpeg_backend.rs:18（源 45s）、:56（复探 300s）走 process::capture（probe.rs:98→process.rs:56-75），无令牌；编码/解码走 run_command 50ms 轮询可取消 | R3 |
| F17 | FLAC 预算失真 + 无目标体积达成字段 | 确认 | plan.rs:92 统一 `audio_bitrate_kbps*1000*count`；flac 不写 -b:a（:125）；VideoResult（mod.rs:101-112）无 target* 字段 | R3 |
| F18 | 来源根：批级并集 + 最深根胜出，冻结字段成死字段 | 确认（**图片侧同病**） | app.js:1229-1230 冻结字段无消费者；:3316-3320 批级并集；source_root.rs:11-25 最深根胜出；图片侧 commands.rs:766-771 同样重算 | R4 |
| F19 | 视频输出名编码期间不占位，冲突即整任务失败 | 确认 | video/transaction.rs:37 exists() 探测；:81 hard_link 一次性，无换号重试；应复用 OutputNameAllocator::claim（output_transaction.rs:1571-1606） | R4 |
| F20 | 导入静默遗漏 | 确认（两侧） | video_commands.rs:25-26/31/58 canonicalize/read_dir 失败静默丢；图片侧 commands.rs:626-628 read_dir 同病，:607 canonicalize 失败退回原路径（差异） | R4 |
| F21 | 混合媒体图片先于视频 | 确认（成文决定） | video_commands.rs:289-290；协议 core/media-task-protocol.md:31-33 明文为避免 CPU 超订；UI 未呈现阶段 | R4（仅呈现） |
| F22 | CI 空缺：AppState 层取消、macOS 真实矩阵、双实例抢占 | 部分确认 | test_swift_history.sh:44-61 不编译 AppState.swift（但调度器层取消已有行为测试 main.swift §25）；macOS 无 --ignored 真实矩阵、无 VideoToolbox 实测；抢占测试为故障注入式（占位被换/恢复被占/删除前改写已有，rc.9 五条），缺双实例与真实并发 | R5 |
| F23 | 轨道差异 | 属实且合规 | inproc 下 probe.rs:102-104 / capabilities.rs:19-20 / video_commands.rs:271-272 硬禁并返回 NATIVE_UNAVAILABLE；Swift 轨道视频 not implemented（validation.md:322-323） | 无需改码 |

rc.9 已完成（勿重复做）：三级无覆盖改名台阶（renamex_np(RENAME_EXCL)/renameat2(NOREPLACE)/MoveFileExW 无 REPLACE）、atomic_swap、publish_replace_if_identity_matches（换出内容复核、不符换回）、Swift OutputTransactionStore 降级台阶 + ⑧b 八项降级测试、故障注入窗口测试五条。

---

## 二、与方案的冲突裁定

1. **统一 FIFO 调度（方案 8.1）→ 不采纳**。core/media-task-protocol.md:31-33 成文：图片组先行、视频逐个，避免 CPU 超订。维持分阶段；R4 仅把阶段呈现到 UI。
2. **FileOperationService 更名/新建 + 13 态状态机（方案 3.2/3.4）→ 不采纳为新建**。现有 journal + 继承恢复服务 + StagedOutput 状态已覆盖同等职责；只做增量扩展（新增失败语义时补状态/字段，serde default 兼容旧记录）。方案 16.10 自己禁止无作用抽象层。
3. **清空队列语义 → 以 Rust 为参照系统一**（commands.rs 四道闸：闸门、Encode 前、编码中 rung 间、commit 前）。Swift 对齐即可，不改 Rust 语义。
4. **错误分类（方案 14）→ 映射到现有 reason_code/stage 字段**，缺码时补枚举，不建平行错误体系。
5. **Alpha/ICC 策略 UI、HDR 扩展、万张级性能矩阵 → 延后**。HDR/高位深已有拒绝策略（继承规则 11）；前者需产品决策；性能先做内存回归断言。

## 三、统一禁令（每轮守门）

- 禁止 `exists() + rename()` 作占名；禁止普通 rename 在目标身份不明时覆盖。
- 凭证（(dev,ino)）缺失时禁止退化到裸 publish——失败关闭，明确报错。
- 占位身份必须取自打开句柄（fstat）或提交前后双侧复核；复核不符按冲突处理，绝不静默覆盖。
- 取消后不得产生未告知的提交：commit 之前必须过取消闸；进入原子提交后不可中断，等收尾。
- 动画→静态、方向丢失：拒绝或明确确认，不得静默。
- 历史字段只增不改，serde default 兼容旧记录；旧备份、旧 SHA 字段、旧事务必须可读。
- 回归门照 AGENTS.md：cargo test 三变体 + npm run test:frontend + scripts/test_swift_history.sh + 三轨构建 + 真实 fixture 视频测试。构建/打包走 luna_worker；不可用时如实报告再降级本机执行。

---

## 四、轮次

### R1 文件安全（Rust）——最高优先
1. 台阶③占位身份：claim_empty_placeholder 保留句柄/fstat 凭证；rename 前后双侧复核目标身份；不符→按已占冲突处理（换 conflict_sibling 原子占名或拒绝），扩 TEST_FAULT_IN_INSTALL_WINDOW 注入点到 claim 之后。
2. 隔离名：claim_quarantine_name 留凭证；:1074/:1216 改走 rename_no_replace→hard_link→O_EXCL 台阶；被占→下一个隔离候选名。
3. conflict_sibling :772/:1167：选名与占用合一，改原子占名原语。
4. publish_claimed：identity None → 拒绝提交并报告，删除退化分支。
5. restore 主链：写回改走 publish_replace_if_identity_matches 模式——预期身份=冲突检查时捕获的产物身份；换出内容 SHA 对账 output_sha256，不符换回拒绝；复用 rollback :467-540 的复核范式。
6. 后缀撤销删除：改走带指纹/归属门的 retire 路径（:603-631 语义）。
测试：每个缺口一条故障注入测试 + 双实例同源用例（crash_injection 风格 spawn）。

### R2 Swift 任务生命周期
1. 取消链：compressOneStatic/writeOutputStatic 增加检查点（开工前、编码后、验证后、**提交前**——镜像 Rust commit 闸语义）；cancelBox 全程可消费；清空队列后不再产生落盘提交。
2. restore 对齐：already_restored（SHA=原图指纹前置）、写回身份复核（复用 OutputTransactionStore rc.9 台阶）、删产物指纹门。
3. canRestoreHistory：replace 模式 backup 存在即保留入口（missing 亦然），与后端无门槛语义一致。
4. 内存预算：image_memory.rs 估算 + 三档门移植到 CompressionScheduler。
5. （低优）try_begin_batch 清位语义：改到会话收尾清位，commands.rs:3553 测试同步迁移。

### R3 媒体完整性
1. EXIF：Rust 四处解码用 image 0.25.10 orientation API 烘焙像素、输出方向置 1/省略；Swift 高级路径按 CGImagePropertyOrientation 变换烘焙；Orientation 1-8 双侧测试。
2. 动画：Swift 高级路径移植 animation_rejection 判定（CGImageSourceGetCount 等），拒绝语义与 Rust 同文。
3. 视频：加解码帧数对账（-count_frames）；VFR 比时间轴+帧数+首尾时间戳；用户主动改帧率按目标校验；KeepAll → 逐流 MediaPreservationReport（音轨/字幕 codec、语言、标题、disposition、章节起止标题、附件名+SHA）。
4. probe 可取消：process.rs capture 加令牌轮询，:18/:56 接入现有取消链。
5. FLAC：预算改用源实测音频码率；VideoResult 增加 target_bytes/target_met/target_deviation；前端呈现达成情况。

### R4 调度与路径
1. 视频占名：编码前 OutputNameAllocator::claim；commit 落到已占位；冲突自动换下一个候选（有界），partial 保留重试，不再整任务作废。
2. 每文件来源根：VideoTask 与图片批 options 携带冻结的 sourceRoot/relativePath，废除批级最深根猜测；根缺失/不可信→明确报错。
3. 导入诊断：read_dir/canonicalize 失败计数上报（两侧），前端呈现，不再伪装完整导入。
4. UI 呈现图片阶段/视频阶段。

### R5 测试与 CI
1. Swift AppState 生命周期：抽取可测取消/提交闸核心并纳入 test_swift_history.sh 编译清单。
2. macOS 真实矩阵：brew ffmpeg + --ignored 真实媒体；VideoToolbox 实测（不可用则如实记录）。
3. 真实抢占补缺：隔离名被换、conflict 名被占、publish_claimed 凭证缺失、编码中输出名被抢（换号重试路径）、双实例同文件。
4. 契约向量扩展：恢复幂等 + 取消语义进 ContractVectors.swift（沿用现有框架）。

---

## 五、验收矩阵（每轮相关项必须绿）

| 场景 | 结果要求 |
|---|---|
| 占位被外部 unlink+重建 | 不覆盖外部文件；身份不符走冲突路径 |
| 恢复期间产物被外部替换 | 拒绝恢复并换回，外部修改保留 |
| Swift 编码中清空队列 | 不再产生备份/覆盖/历史；原图不动 |
| EXIF Orientation 5-8 | 输出竖向正确，无二次旋转 |
| 动画 WebP/APNG → 静态格式 | 拒绝，原文同 Rust |
| VFR 长视频 | 帧数+时间轴符合保留策略 |
| KeepAll 多字幕多章节 | 逐流逐项对账通过 |
| FLAC + 目标体积 | 报告 target_deviation，不谎称达成 |
| 长编码中输出名被抢 | 换号重试或保产物明确失败，不覆盖 |
| 嵌套来源根 | 按每文件冻结快照输出 |
| ffprobe 期取消 | 进程被终止回收 |
| 恢复后历史写失败再点恢复 | already_restored 收敛，不误判外部修改 |

发布门不变：验收矩阵、许可/签名/公证/装订、运行时核验齐全前不打正式 Release；
每轮完成即推 main，工作态用 v0.3.6-rc.N 标记（编号不复用）。

---

## 六、实施记录

### R1+R2 —— 0.3.6-rc.10（10b99a5，2026-11-06）
按上文轮次完成，CI run 42221134520 六作业全绿。见 docs/validation.md rc.10 节。

### R3+R4+R5 —— 0.3.6-rc.11（2026-11-08）
外部审查（0.3.6-rc.10 报告）15 项 + 22 条强制回归测试以本轮为最终交付轮。
逐项回证与测试读数见 docs/validation.md 的 0.3.6-rc.11 节，要点：

- **P1-1/P1-2（本轮新增超出原 F1–F4 范围的部分）**：`StagedOutput`/`ClaimedOutput`
  （Rust）与 `StagedWrite`/`StagedTemp`/`ClaimedOutput`/`claimQuarantine`（Swift）全部
  句柄化——写入走句柄、truncate 显式归零、凭证从 fstat 取；发布双侧对账
  （换出身份 + 目标位内容 SHA256），对不上换回撤销。
- **P1-3（超出 F 系列的新决定）**：原图退休与同格式换出字节**保留不删**——
  事务管理的 `.sizqo-retire-*.keep` 保留文件 + 历史新字段 `retiredOriginalPath`
  + 清空历史时清理。原文档 F 系列没有这一条；外部审查的四项"绝不妥协"之一。
- **P1-4**：`remove_generated_output`（journal 版 + HistoryEntry 版）与 Swift
  `removeGeneratedOutput` 缺产物指纹一律不删，体积不再构成所有权证明。
- **R3 媒体完整性**：EXIF 方向两侧高级路径烘焙（Rust orientation API / Swift
  WithTransform）；Swift 高级路径统一多帧检测；`-count_frames` 帧数对账；KeepAll
  逐流逐章节逐附件对账；StripPrivate 作用域共享定义 + 章节/program 隐私键预检
  拒绝；FLAC 实测码率预算 + `VideoResult.target*` 三字段；`capture_cancellable`
  接入探测取消链。
- **R4 调度与路径**：视频编码前 `OutputNameAllocator::claim` 占名 + commit 换号
  重试（partial 复用）；`VideoTask.sourceRoot/relativePath` 与图片
  `perFileRelativePaths` 冻结值优先；`ImportReport` 两侧记账 + 前端呈现；混合
  会话阶段播报（`media-phase` 事件 + 横幅）。
- **R5 测试与 CI**：T01/T02/T03/T05/T16/T17/T18/T19/T20/T21/T22 新增落地
  （T21 = `tests/dual_instance.rs` 双真实进程 O_EXCL 竞争）；T08 Swift CancelBox
  三态账本并发断言；CI 增 dual-instance 作业步、macOS x86_64 交叉 check、
  Swift 全源 typecheck（含 AppState）。
- **P2-15**：`MAX_SINGLE_TASK_BYTES = 4 GiB` 两侧同值，超限任务按
  `resource_limit_exceeded` 明确拒绝；估算未知不拒（"不知道"不是"知道很大"）。

如实记录的未竟项（继续挡发布门禁）：T12–T15 的真实媒体端到端用例、附件内容
SHA（需解包）、FFmpeg GPL 对应源码义务、Direct Windows/Linux 随包引擎、
签名/公证/装订。T11 于 rc.15 补齐——Rust 侧在 rc.11 曾漏做（validation.md 已
更正失实记录），rc.15 以 APP1 EXIF 拼接夹具 1–8 八方向 + Orientation=6 端到端
闭合（tests/exif_orientation.rs）。
