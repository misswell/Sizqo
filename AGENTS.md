# Sizqo engineering rules

Sizqo is an independent derivative of OctoShrink. The inherited commit history
and MIT LICENSE remain intact. The original engineering document is preserved at
[docs/UPSTREAM_ENGINEERING_RULES.md](docs/UPSTREAM_ENGINEERING_RULES.md).
Its image safety, queue/state, sandbox, history, restore and regression rules
continue to apply. Its original app IDs, App Store resource IDs, updater keys,
feed, release endpoints, version numbers and deployment instructions are
historical references only and MUST NOT be used for Sizqo publishing.

## Identity and tracks

Version starts at 0.1.0. Main development is in this repository's `main` branch.
Never write to `/Users/guofeng/Code/solo/octo-shrink` or push to misswell/octo-shrink.

| Track | Bundle ID | Images | Videos | Storage |
| --- | --- | --- | --- | --- |
| Direct | com.misswell.sizqo | Original CLI image engine | Bundled FFmpeg, runtime-tested capabilities | Tauri app-data/history |
| App Store | com.misswell.sizqo.appstore | Original in-process image engine | Explicitly disabled until native bridge and sandbox tests pass | Separate sandbox app-data/history |
| Swift | com.misswell.sizqo.swift | Original image functionality | Native capabilities must reflect the actual verified implementation | Independent Application Support root |

The exact Swift video status and observed build/test results belong in
`docs/validation.md`. Never label an unvalidated native path as production-ready.
The user specification explicitly allows accurate runtime feature disabling;
this exception applies only to unsupported new video capabilities. Existing
image operations cannot be removed or weakened to accommodate video.

- Direct is still `default = cli-backends`; App Store still uses
  `--no-default-features --features appstore` / `inproc-backends`.
- App Store cannot bundle or launch third-party CLI/dylib tools. Direct-only
  modules compile under `not(feature = "inproc-backends")`, even when default
  and inproc features are both selected for regression testing.
- Independent updater keys/feed must be deliberately configured. The build
  must keep updater operations disabled while `SIZQO_UPDATER_CONFIGURED` is absent.
  A historical OctoShrink public key or feed is never an acceptable default.
- Sizqo never imports, sweeps or shares OctoShrink histories/backups/bookmarks.
- Signing identities may be reused from Keychain; old app provisioning profiles
  cannot. Verify the current notary profile with notarytool before reuse.

## File safety (all media)

1. Video payloads are files, never `EngineResult.compressed: Vec<u8>`.
2. Canonicalize inputs, deduplicate, recursively freeze file snapshots at import.
   A running session may not pick up later files from a directory.
3. Image files alone enter image detection/engine. New video code branches on
   media kind; it cannot change image quality/algorithm or success semantics.
4. FFmpeg options become a typed `EncodePlan` with independently supplied argv.
   No shell, raw frontend commands, arbitrary codec/filter names or URL input.
5. Temporary video output and pass statistics live in a unique owned directory
   on the target filesystem; bounded logs live under Sizqo's app-data root.
   Check output space and persistent backup space separately before encoding.
6. Exit zero and progress=end do not prove success. Re-probe the output, validate
   dimensions/codec/duration/track counts, then decode it fully before commit.
7. Default result selection requires a valid, playable, smaller output.
   Larger output is `skippedLarger`, not success or encoding failure. Only an
   explicit professional choice may retain larger valid files; report it.
8. Suffix/directory output cannot clobber an existing file. Atomically claim a
   free name. Same-container replace requires persistent backup -> durable
   precommit journal -> fsync output -> atomic rename -> history -> settle journal.
9. Never unlink the original before replacement, overwrite a valid original
   backup, or sweep backups during startup. Failed rollback keeps the journal
   and backup. Startup recovery uses the inherited transaction recovery service.
10. History fields for new media are optional with serde defaults. File
    availability is refreshed at read time. Restore uses the shared service;
    replacing video cannot have a separate, weaker restore implementation.
11. Unsupported HDR/DV/HDR10+, high bit depth, Alpha, interlaced or complicated
    stream preservation must be rejected or explained before encoding. No
    silent tone mapping, lost tracks, format-incompatible copy or fake controls.
12. All security-scoped access guards cover probe, encode, temporary writes,
    commit, history and rollback. An unavailable native backend never spawns.

## Session and CPU contract

[core/state-machine.md](core/state-machine.md) still governs image behavior.
[core/media-task-protocol.md](core/media-task-protocol.md) extends it for video.

- `taskId/sessionId/queueRevision` and queue object identity reject stale events.
- Pending snapshots are immutable. Imports during processing run next session.
- Pause/stop gate future work; in-flight image/video finishes. Stop leaves
  unstarted files pending. Cancel-current-video is a separate explicit action.
- Cancellation wakes the gate without resuming it. Gate order is cancel -> stop
  -> pause/parallelism. No SIGSTOP pretending to work uniformly across tracks.
- Mixed media invokes one session; image and video groups currently run in
  separate phases. Video concurrency is one; decoder/filter/encoder threads are
  clamped to the current CPU budget. GPU use is not a promised percentage.
- UI state derives from backend state/events, never button labels or CSS classes.

## Regression and release gate

Build/compile/archive/sign/notarize/distribute operations are delegated to
`luna_worker` as required by the user's global rules. All workers share the
workspace and must not revert other contributors' changes.

Required checks include cargo test, cargo test --features inproc-backends,
cargo test --no-default-features --features inproc-backends, npm run test:frontend,
bash scripts/test_swift_history.sh, a real three-track build, signed sandbox
runtime checks, real fixture video tests and the user's full acceptance matrix.
Record exact commands and outcomes; pending/blocked is not passed.

Do not publish a formal Release or tag while the acceptance matrix, distribution
licenses/source obligations, signing/notarization/stapling or runtime verification
is incomplete. Preserve useful local artifacts and report their absolute paths.
A local commit is not a GitHub push. Every push/Release must be linked in the reply.
Never reuse old tags, OctoShrink resources/secrets/endpoints or unsigned assets.

Owner directive (2026-10-31): each completed work round is pushed to `main`
immediately in the same session — no "local commit only" endings. Tagging
follows the gate above: while the release gate is incomplete, mark the
work-complete state with a pre-release tag (`vX.Y.Z-rc.N`, never reusing
numbers); the final `vX.Y.Z` tag and its formal Release are created only once
the gate passes, together with the version-bump commit and artifacts.

The MarukoToolbox-Rewrite repository is a fan tribute rewrite, not original
Maruko source. Only its product/parameter ideas are referenced, without copying
its code. FFmpeg with GPL encoders requires documented corresponding source and
configuration plus release-owner/legal review. MIT licensing of Sizqo does not
remove obligations of separately distributed components or codec patents.

## Work environment

Read file line counts first, then <=200-line chunks and <=5000-token output.
Prefer AgentSpace for visible macOS UI. If unavailable, report the limitation
before any fallback to the user's desktop. Do not launch GUI silently.
Close only task-owned temporary processes/simulators; preserve pre-existing ones.
Inspect exact build caches before moving generated caches to Trash, preserving
release artifacts and all uncommitted work.
