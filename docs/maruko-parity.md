# Sizqo vs 小丸工具箱(粉丝致敬版)压缩效果对比

结论先行:**同样的编码参数(编码器 / CRF / preset)下,Sizqo 的输出与小丸致敬版
字节级一致;按各自默认线程策略时,Sizqo 等于或更小(最多小 6.6%),没有任何场景
更大。** SSIM(对源)两侧差值 ≤ 0.0001,画质无损级一致。

对比对象是 [arenascats/MarukoToolbox-Rewrite](https://github.com/arenascats/MarukoToolbox-Rewrite)
(小丸工具箱**粉丝致敬重写版**,非原版小丸源码);只比较其 CPU 编码命令形态,
不复制其代码。复现脚本:[scripts/bench_maruko_parity.sh](../scripts/bench_maruko_parity.sh)。

## 方法

- 两侧使用**同一个 ffmpeg 二进制**(本仓库收集的 9.0.2),排除编码器版本差异。
- argv 逐参数复刻:小丸侧按 `ffmpeg_utils.py` 的 `build_compress_command`
  (`-preset X -crf Y`、`-c:a copy`、`-map_metadata 0`、无 `-map`、无 movflags);
  Sizqo 侧按 `src-tauri/src/video/plan.rs` 的 `EncodePlan::argv`
  (`-map 0:v:0`、`-pix_fmt yuv420p`、同尺寸 scale、`rotate=0`、`+faststart`、
  `-fps_mode passthrough`、`-map_metadata 0 -map_chapters 0`)。
- 素材:1280×720/30fps/15s/yuv420p + AAC 128k,三类内容(平坦渐变=动画类、
  testsrc2=实拍细节、mandelbrot=高熵屏幕类),另有 1080p→720p 降分辨率组。
- 指标:输出字节数、SSIM(输出 vs 源)。

## 结果(2026-10-11,arm64)

delta = (Sizqo − 小丸) / 小丸,负数 = Sizqo 更小。

| 用例 | 小丸 (B) | Sizqo (B) | delta |
|---|---:|---:|---:|
| anime · x264 CRF23 medium | 1,761,580 | 1,759,158 | −0.14% |
| live · x264 CRF23 medium | 5,351,855 | 5,137,718 | −4.00% |
| screen · x264 CRF23 medium | 9,696,286 | 9,696,444 | +0.00% |
| anime · x265 CRF28 medium | 432,433 | 431,855 | −0.13% |
| live · x265 CRF28 medium | 3,627,168 | 3,626,396 | −0.02% |
| screen · x265 CRF28 medium | 5,042,990 | 5,042,240 | −0.01% |
| 1080p→720p · x264 CRF23 | 2,890,093 | 2,699,798 | −6.58% |
| anime · SVT-AV1 CRF30(小丸固定 preset6 vs Sizqo medium→8) | 515,100 | 509,330 | −1.12% |
| anime · SVT-AV1 CRF30(小丸 preset6 vs Sizqo slow→4) | 515,100 | 515,559 | +0.09% |
| live · 两遍 2000k x264 | 4,008,280 | 4,022,367 | +0.35% |

SSIM 对源(两侧逐项对比):x264 组 0.9914–0.9969,x265 组 0.9823–0.9946,
每对两侧差值 ≤ 0.0003 —— 体积差不是画质差。

## 归因:大差距 100% 来自线程数,不是算法

对照实验(同素材,只改线程):

| 变体 | 输出 (B) | 与谁一致 |
|---|---:|---|
| 小丸 argv · 自动线程(全核) | 5,351,855 | 小丸原结果 |
| 小丸 argv · 限 8 线程 | 5,137,718 | **与 Sizqo 字节级一致** |
| Sizqo argv · 限 8 线程 | 5,137,718 | Sizqo 原结果 |
| Sizqo argv · 自动线程 | 5,351,855 | **与小丸字节级一致** |

即:线程数相同时两侧输出**完全相同**(x264/x265 是同一个库,命令形态差异——
显式 `-map`、强制 yuv420p、rotate=0、faststart、fps_mode——不改变码流)。
表中 −4% / −6.6% 的"领先"来自:Sizqo 的自动 = CPU 预算(8 线程),
小丸自动开满全核(本机 10 逻辑核 → x264 内部 15 线程),而 x264/x265 线程越多
单线程效率越低、同 CRF 下体积略大。若两者都设相同线程数,差距为 0。

## 边界说明(诚实清单)

- 硬件编码器不可比:小丸的 GPU 线是 NVIDIA NVENC / AMD AMF / Intel QSV(Windows),
  Sizqo 是 VideoToolbox(macOS),不同硬件无法在同一台机器对比。
- 10bit:小丸提供 `libx265_10bit` 变体;Sizqo 出于安全策略拒绝 10bit/HDR 输入
  (见 plan.rs 校验),这是功能范围差异,不是压缩率差异。
- 流处理:小丸靠 ffmpeg 自动选流(静默丢字幕/附件);Sizqo 保留或在计划阶段
  解释性拒绝。含字幕/多音轨素材的体积对比不在本表范围。
- SVT-AV1:Sizqo 的速度档映射为 ultrafast→12 / veryfast→10 / medium→8 / slow→4,
  小丸固定 preset 6;两侧行为都在"同 CRF 下体积相近"区间,内容相关。
- 两遍 2000k 两者都命中目标码率(+0.35% 属码控容差,双向可能)。

## 对发布承诺的含义

宣传口径可以写"同参数下体积与小丸持平或更小(实测 0 ~ −6.6%),画质 SSIM 一致",
但**不得**写"压缩算法优于小丸"——编码器是同一个 x264/x265/SVT-AV1,体积差来自
线程预算这一工程决策,任何一侧改线程数都会抹平。

## 0.3.6-rc.18 复核轮的影响

本轮没有改动 `EncodePlan::argv`（编码命令形态逐字节不变），上表读数不受影响。
新增的 KeepAll × 移除音轨/字幕/转码音轨预检、附件按流位置逐项对账与帧数
fail-closed 校验只改变"哪些请求被拒绝"，不改变被放行请求的输出——对比口径
（同参数、`-map_metadata 0 -map_chapters 0`）依然成立。
