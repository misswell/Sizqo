# Workspace and intent model

Normative for all three tracks (Direct Tauri, App Store Tauri, Swift native).
Extends [state-machine.md](state-machine.md) and
[media-task-protocol.md](media-task-protocol.md); those two documents keep full
authority over queue items, sessions, gates, progress arithmetic and event
envelopes. This document defines only page structure and how a user goal becomes
engine parameters.

## Why this exists

The previous layout exposed engine fields directly (quality, engine, effort,
smart mode, format, effort ladder) and asked the user to reason about
compression internals. Several of those fields were never read by the Rust
engine, which violates the no-fake-control rule in the engineering rules. The
product question a user actually asks is "how small, for what purpose", so the
UI now asks that and the intent compiler chooses parameters.

## Page structure

Three panes selected by a persistent leading rail. The rail is navigation inside
one window; switching panes never touches the queue, a running session or an
open detail selection's underlying item.

| Region | Contents |
| --- | --- |
| Rail | 本轮 / 历史 / 设置. 本轮 carries a running indicator while the session is not idle. |
| Workspace column | Intent bar, then the media list (thumbnails, size, per-item state and inline progress), then list filters. |
| Detail column | Selected item: preview, before/after bytes, savings, dimensions, algorithm, elapsed, target verdict, warnings, and only the actions this item can really perform now. With no selection: this run's totals plus the active intent summary. |
| Run bar | Full width, always visible while work exists: progress fill, `processed / total`, saved bytes and percent, CPU share, and start / pause / stop. Labels derive from backend session phase, never from button text or CSS. |

History uses the same column pair (history list + detail). Settings keeps
language, theme, backup retention, CPU ceiling, update and reset-defaults, each
mounted on its editor rather than a read-only report.

Removed, and not to be reintroduced: the 全部/图片/视频/历史 tab strip (media kind
becomes a list filter, history becomes a rail destination), the collapsed 压缩设置
and 视频设置 accordion panels (replaced by the intent bar plus one disclosure for
raw parameters), and the separate 压缩结果 panel (results are per-item state in
the list plus the detail column and run bar totals).

## Intent model

An intent is a named, validated parameter set. Exactly one image intent and one
video intent are active at a time; changing any raw parameter in the disclosure
sets that kind's intent to `custom`. Intent selection applies to items not yet
submitted to a session; a running session keeps the snapshot it captured.

### Image

| id | Label | Compiled engine request |
| --- | --- | --- |
| `losslessFirst` | 肉眼无差优先 | quality 90, requested format `original`, smart candidate search, no dimension cap |
| `balanced` | 均衡（默认） | quality 75, requested format `original`, smart candidate search |
| `asSmallAsPossible` | 尽量变小 | quality 60, smart candidate search (a PNG is also tried as WebP and the smaller wins; requested format stays `original`) |
| `webSize` | 网页尺寸 | longest edge capped to 2560 px, quality 72, plain encode (no candidate search, format unchanged) |
| `targetSize` | 指定体积 | byte budget from the user, bounded solver (see below) |
| `custom` | 自定义 | raw fields exactly as edited, plain encode |

Output mode (覆盖原文件 / 加后缀 / 输出到指定目录) and 自动开始 stay orthogonal to
the intent: they are consequences the user must be able to set and see, not
advanced trivia.

### Video

| id | Label | Compiled EncodePlan request |
| --- | --- | --- |
| `videoSmall` | 通用更小 | CRF 28, MP4, compatible audio copied |
| `videoHq` | 高画质 · 保留轨道 | CRF 20, audio and subtitles copied, metadata preserved |
| `videoTarget` | 指定体积 | `targetSize` mode, MiB from the user, labelled as an estimate |
| `videoWeb` | 1080p 网页 | longest edge 1080, CRF 26, MP4, AAC 160 kbps |
| `videoCustom` | 自定义 | full existing validated control set |

Video exists on Direct only. App Store keeps the runtime-detected
`NATIVE_UNAVAILABLE` explanation in place of these chips; Swift has no video
engine and must not render a video intent, video filter or video affordance at
all.

## Engine surface the intents require

`CompressOptions` gains two read fields, both optional and defaulting to no
constraint: `maxDimensionPx` (u32, downscale only, never upscale) and
`maxOutputBytes` (u64). The solver tries at most six encodes in this exact order
and returns the smallest output at or under the budget: quality 80, 65, 50 at the
original size, then longest edge 2560/1920/1280 with quality 65/60/55. A user-set
`maxDimensionPx` is merged into each rung with `min`, never overwritten. An
original that already fits the budget is returned unencoded. Encodes that come out
larger than the original are not candidates (rule 7).

The smart candidate set belongs to the engine, not to an intent: a PNG is always
tried as both PNG and WebP and the smaller wins; every other format is re-encoded
in place. No track may add a candidate for one intent only — the same intent must
produce the same bytes on Direct, App Store and Swift.

`CompressResult` gains optional, serde-defaulted fields: source and output
dimensions, `elapsedMs`, `targetBytes`, `targetMet`, `warnings`.

Fields removed because no engine path reads them: `backend`, `effort`,
`convertToWebp`, `lossless`, `smart_mode`. Smart routing stays a real engine
behavior (`use_smart` on the session command), not an inert option.

## Honesty rules

1. A missed byte budget is reported as the closest attempt plus its reason. It is
   never rendered as success.
2. An output larger than the input stays `skippedLarger`, per the inherited file
   safety rules.
3. Any intent that re-encodes pixels states that EXIF and GPS are not carried
   into the output; 系统转换 remains the mode that keeps metadata properties.
4. Unsupported source streams keep the existing pre-encode rejection; no intent
   may bypass it.
5. HDR, Dolby Vision, HDR10+, alpha, interlaced and multi-track preservation
   follow the existing plan validation verdicts shown verbatim in the detail
   column.
6. Every visible control changes engine behavior. If the engine cannot honor a
   request, the control does not exist.

## Parity and gates

`tests/compression-parity.cjs` compares the compiled request per intent between
tracks instead of comparing raw control defaults. `tests/app-slices.cjs` marker
comments in `frontend/app.js` are load-bearing and must survive the rewrite. The
Swift icon library is regenerated from the rewritten HTML sprite. Three-track
build plus the full acceptance matrix remain the release gate.

## Video parameter states (Intent / Preset / Options)

The image-side rules above apply to video with one difference: video presets
come from the backend (`video/presets.rs`), so presets and intents are two
different entry points into the same options snapshot. The enforced contract:

1. The UI summary, the submitted request and the saved settings are three views
   of one options snapshot. A non-custom intent's snapshot is its compiled
   request; `videoCustom`'s snapshot is the live control state.
2. First launch shows the default intent's compiled options and **no** preset
   selection (the preset dropdown offers an explicit "no preset" entry). It
   never claims a preset it is not sending.
3. Applying a preset updates the preset id **and** the intent state: the intent
   switches back only when the merged options exactly equal that intent's
   compiled request; otherwise it honestly switches to `videoCustom`.
4. Switching intents re-derives the preset selection from the resulting options
   (matching preset or none). No stale preset name survives a parameter change.
5. `null`/empty option values clear their controls; the stale value can no
   longer leak back through the control read-back into the next submission.
6. Restarts restore the saved options and preset id verbatim; the saved intent
   is restored too, and non-custom intents re-render their compiled request.
   A saved preset therefore survives restarts.
7. A running batch uses the options snapshot captured when it started; later UI
   edits affect only the next batch.
8. The frontend fallback preset label table uses the backend preset ids
   (`quick/balanced/quality/smallest/compatible/target`) as the canonical keys.
