# Third-Party Notices

Sizqo is MIT licensed (see [LICENSE](LICENSE)), but the compression engines it
bundles are separate projects with their own licenses. This file records what is
bundled, under which license, and the obligations that apply before any public
distribution.

## Bundled command-line tools (Direct / Swift Native)

These binaries are copied into the app bundle (`Contents/Resources/bin`), with
their non-system dynamic libraries in `Contents/Resources/lib` and the video
engine's binaries in `Contents/Resources/video/bin`.

| Binary | Project | License | Source |
|--------|---------|---------|--------|
| `pngquant` | pngquant | **GPL-3.0-or-later** (CLI) | https://github.com/kornelski/pngquant |
| `oxipng` | Oxipng | MIT | https://github.com/oxipng/oxipng |
| `cjpeg` | Mozjpeg | BSD-3-Clause | https://github.com/mozilla/mozjpeg |
| `gifsicle` | gifsicle | **GPL-2.0-or-later** | https://github.com/kohler/gifsicle |
| `cwebp` | libwebp | BSD-3-Clause | https://developers.google.com/speed/webp |
| `cjxl` | libjxl | BSD-3-Clause (with Apache-2.0 portions) | https://github.com/libjxl/libjxl |
| `avifenc` | libavif (+ libaom) | BSD-3-Clause (libaom: BSD-2-Clause + Alliance codecs license) | https://github.com/AOMediaCodec/libavif |

`pngquant` and `gifsicle` are copyleft-licensed. Distributing the Sizqo
installer that contains their binaries is allowed, but each distribution of the
installer must make the corresponding source for those binaries available (a
link to the upstream repository of the exact bundled version satisfies this
when unmodified). The app stays MIT: bundling a GPL binary does not relicense
Sizqo's own code.

## Bundled FFmpeg (Direct edition, video engine)

FFmpeg is bundled for the Direct video pipeline (`Contents/Resources/video/bin/ffmpeg`,
`ffprobe`). Its license depends on the build configuration:

- A plain FFmpeg build is **LGPL-2.1-or-later**.
- Builds that include GPL components (`libx264`, `libx265`, `libxvid`, …) make
  the whole FFmpeg binary **GPL-2.0-or-later**, which requires offering the
  corresponding source and build configuration.

Before every public release, the release owner must:

1. Record `ffmpeg -version` (the `configuration:` line) of the exact bundled
   binary in this file.
2. If GPL encoders are enabled, either provide the corresponding source
   (and configuration) for download alongside the release, or rebuild FFmpeg
   without them (capabilities are detected at runtime; the UI only offers what
   the bundled binary actually supports).
3. Re-check codec/patent considerations for H.264/HEVC/AV1 distribution in the
   target market — open-source licensing does not settle patent licensing.
4. The App Store track never bundles or launches FFmpeg; this notice does not
   apply to that track.

**Status for v0.1.0 (recorded 2026-10-11):** the bundled binary is
`ffmpeg version 9.0.2` (Homebrew build, `Lavf63/Lavc63`), configuration:
`--enable-gpl --enable-libsvtav1 --enable-libopus --enable-libx264
--enable-libmp3lame --enable-libdav1d --enable-libvmaf --enable-libvpx
--enable-libx265 --enable-openssl --enable-videotoolbox --enable-audiotoolbox
--enable-neon` (full line ships in the release notes). Because GPL encoders
(libx264, libx265) are enabled, the whole FFmpeg binary is GPL-2.0-or-later;
the corresponding-source offer is provided through the release page links:
FFmpeg n9.0.2 (https://ffmpeg.org/download.html), x264
(https://www.videolan.org/developers/x264.html), x265
(https://bitbucket.org/multicoreware/x265_git), libsvtav1
(https://gitlab.com/AOMediaCodec/SVT-AV1), libvpx
(https://chromium.googlesource.com/webm/libvpx), and the image tool sources
listed above. pngquant 3.0.3 and gifsicle are GPL binaries bundled in the same
installer and are covered by the same source-offer approach.

**Status for v0.3.6 (re-checked 2026-10-31):** the bundled binary is unchanged
(`ffmpeg version 9.0.2`, Homebrew build, same configuration line captured live
from `resources/video/bin/ffmpeg`):
`--enable-shared --enable-pthreads --enable-version3 --enable-gpl --enable-libsvtav1 --enable-libopus --enable-libx264 --enable-libmp3lame --enable-libdav1d --enable-libvmaf --enable-libvpx --enable-libx265 --enable-openssl --enable-videotoolbox --enable-audiotoolbox --enable-neon`.
GPL encoders (libx264, libx265) remain enabled → the bundled FFmpeg stays
**GPL-2.0-or-later**; the source-offer list above still covers it and must be
attached to the 0.3.6 release page before any public distribution. The 0.3.6
code changes touch how Sizqo *invokes* FFmpeg (strict input decoding, metadata
scope flags, passthrough) — they do not change what is bundled.

FFmpeg homepage: https://www.ffmpeg.org — licenses: https://www.ffmpeg.org/legal.html

## Rust dependencies

The Rust backend's runtime dependencies are listed in
[src-tauri/Cargo.toml](src-tauri/Cargo.toml). Notable components:

- `tauri` and friends — MIT / Apache-2.0
- `image`, `imagequant`, `ravif`, `rgb` — MIT / Apache-2.0 (imagequant CLI
  separation does not affect the Rust crates)
- `serde`, `tokio`, `libc` — MIT / Apache-2.0

A machine-readable inventory (`cargo license` / `cargo deny`) should be attached
to release engineering before the first public installer.

## Frontend

The frontend in `frontend/` is plain HTML/CSS/JavaScript with no third-party
runtime dependencies and no build step. The i18n catalog, icons, and styles are
original Sizqo code.

## Swift edition

The Swift app uses Apple system frameworks only (AppKit, AVFoundation,
ImageIO); no third-party components are bundled.

## Attribution

Sizqo is an independent derivative of OctoShrink (MIT, © the OctoShrink
authors). The inherited engineering document is preserved at
[docs/UPSTREAM_ENGINEERING_RULES.md](docs/UPSTREAM_ENGINEERING_RULES.md) and the
original MIT LICENSE and copyright notices remain intact.
