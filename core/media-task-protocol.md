# Unified media task protocol

The original image lifecycle in state-machine.md remains normative.
Media kinds are `image` / `video`. Imported media is detected from a bounded
header and a background probe, never inferred solely from a filename extension.
Files/folders expand to canonical file snapshots before a session starts.

Each video request contains `{taskId,path}`; progress contains the same task ID,
a session ID and queue revision. A consumer additionally checks the exact queue
object captured by its frozen snapshot. Remove/clear/reimport cannot revive a
stale worker callback. Encoding fraction is diagnostic and remains below 1 until
output verification, safe commit and history recording succeed.

| Stage | Queue effect | Meaning |
| --- | --- | --- |
| queued | pending | Captured for this session |
| starting | running | Scheduler permitted execution |
| encoding | running | Progress/time/speed estimate only |
| completed | done | Valid output selected and transaction committed |
| skippedLarger | done | Valid larger output deleted; source unchanged |
| failed | failed | Probe/plan/encode/verify/commit failed with reason |
| cancelled | removed | User explicitly cancelled this video |
| deferred | pending | Stopped session did not start this item |

The session has idle/running/paused/stopping states, independent of queue state.
Pause waits for unstarted work; current encoding/verification/commit completes.
Stop closes this session and preserves pending items for a new session.
Cancel-current-video alone may terminate its owned FFmpeg child, reap it and
remove its owned temporary directory. Batch stop cannot kill current video.

`start_media_session` owns a single scheduler lifecycle. The first implementation
runs the captured image group followed by one video at a time. It avoids shared
CPU oversubscription by separating image and video phases. FFmpeg decoder,
filter and software encoder threads use the actual budget, with encoder-specific
x265 pools/SVT logical processor mappings. This is a parallelism bound, not a
promise of a measured CPU/GPU percentage.

Unknown duration means indeterminate video fraction/ETA. Target size and ETA
are estimates. Frame rate defaults to source timing: constant-rate sources keep
their timestamps, variable-rate sources are normalized to the source average
frame rate so the output duration is preserved; an explicit frame rate wins.
Encoder fallback happens only for `auto`, with an explicit warning; a specifically
selected unavailable encoder is rejected. A claimed hardware encoder disables
FFmpeg software fallback and must pass a runtime trial.

Common persistent history includes `mediaKind` (default image) and optional
`videoMetadata`. Source/output/backup existence is computed on read. For replace,
original playback/frame comparison must use backupPath, never sourcePath pointing
to the compressed replacement. Suffix restore removes a result only after the
shared history restore transaction succeeds and user confirmation where required.
