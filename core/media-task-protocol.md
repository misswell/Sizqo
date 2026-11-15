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
| completed | done | Final `outcome` decides the wording; see result contract |
| failed | failed | Probe/plan/encode/verify/commit failed with reason |
| cancelled | removed | User explicitly cancelled this video |
| deferred | pending | Stopped session did not start this item |

The legacy `skippedLarger` stage is retired: "valid but not smaller" is reported
through the unified outcome contract (`skippedNoBenefit` with its reason code),
never as a stage flag that doubles as a state.

Video results use the same outcome contract as images: `committed`,
`targetUnmet` (product committed, restorable, retry offered — never shown as a
plain success or a failure), `skippedNoBenefit`, `skippedAlreadyUnderTarget`,
`cancelled`, `failed`. A target-size miss is recorded as `targetUnmet` both in
the result and in the history entry, from the same decision point at commit.

## Cancel versus commit (one ledger)

Image and video tasks compete for the same `TaskLedger` (one lock, two sets:
`cancelled` and `committing`). Rules:

- `RUNNING` may be cancelled: `request_cancel` wins and the later commit attempt
  is refused (`cancelled-before-commit-gate`).
- Once a task wins `try_begin_commit` it enters `COMMITTING`: cancellation is
  refused and the UI must say "committing", never "cancelled".
- `COMMITTING` errors are `FAILED` (with their rollback report), never
  re-classified by reading the cancel flag.
- Cancellation points cover queueing, the parallelism permit, ffprobe, every
  encode pass, full-decode verification, output re-probe and per-attachment
  extraction/SHA verification.

## Video transaction stages

The replace/new-file transaction journal records a `stage`
(`preparing → backupVerified → encoded → outputValidated → committing →
outputPublished → historyPersisted`), and — once the atomic swap has happened —
the retained path of the swapped-out original (`retiredOriginalPath`, also
written to the history entry). Failure handling is decided by that recorded
stage plus content fingerprints, never by a caller-side boolean alone:

- After `outputPublished`: replace mode rolls the original back (verified
  against the backup voucher); a new-file transaction removes its own product
  through the fingerprint gate. Rollback failure keeps the journal, the backup
  and every retained file for the next recovery pass.
- Before it: the journal is closed after a non-destructive failure.
- History is "written" only when the entry reads back
  (`contains_committed`), and the journal is closed only after that.
- Every staged publish (history JSON, journal, settings, backup write-back)
  fsyncs the target directory through the single shared `sync_dir` primitive.
- On the next encode start, job directories (`.sizqo-video-*`) left behind by
  SIGKILL are swept only when their contents prove ownership
  (`output.partial.*` / `pass-stats*` only); anything else is kept for manual
  review. Journal-recorded output placeholders are recovered by their
  `(dev,ino)` voucher.

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
are estimates. Frame rate defaults to source timing: when no frame rate is
selected the source PTS are preserved (`fps_mode passthrough`) — variable-rate
sources keep their original timestamp distribution and are never silently
normalized to the average frame rate; an explicit frame rate wins and converts
to CFR. Input bit depth, alpha, float formats and HDR are decided by a strict
whitelist / transfer-function classifier, which rejects with an explanation
instead of silently down-converting. Encoder fallback happens only for `auto`,
with an explicit warning; a specifically
selected unavailable encoder is rejected. A claimed hardware encoder disables
FFmpeg software fallback and must pass a runtime trial.

Common persistent history includes `mediaKind` (default image) and optional
`videoMetadata`. The video snapshot records the request options actually used,
the target-size reconciliation (`targetBytes`/`targetMet`/`targetDeviation`),
the final outcome and reason code, the output re-probe summary and the
diagnostics log path, so the history detail survives an app restart without
re-probing. Source/output/backup existence is computed on read. For replace,
original playback/frame comparison must use backupPath, never sourcePath pointing
to the compressed replacement. Suffix restore removes a result only after the
shared history restore transaction succeeds and user confirmation where required.
