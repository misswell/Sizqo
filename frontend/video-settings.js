(function attachSizqoVideo(root) {
  'use strict';

  const DEFAULT_OPTIONS = Object.freeze({
    encoder: 'auto',
    mode: 'crf',
    crf: 28,
    bitrateKbps: 4000,
    targetMiB: 100,
    // Encoder speed preset (for example, medium/slow), not a named user profile.
    preset: 'medium',
    resolution: 0,
    frameRate: null,
    audioPolicy: 'copy',
    subtitlePolicy: 'copy',
    metadataPolicy: 'preserve',
    outputMode: 'suffix',
    outputDir: null,
    outputSuffix: '_compressed',
    allowLargerOutput: false,
    container: 'mp4',
    threads: 0,
  });

  const PRESET_LABELS = Object.freeze({
    fast: '快速',
    balanced: '均衡',
    highQuality: '高质量压缩',
    extreme: '极致体积',
    compatibility: '兼容优先',
    targetSize: '自定义目标大小',
  });

  function normalizeOptions(value) {
    return Object.assign({}, DEFAULT_OPTIONS, value || {});
  }

  function presetId(preset) {
    return String(preset && (preset.id || preset.key || preset.value) || '');
  }

  function presetLabel(preset) {
    if (preset && (preset.label || preset.name)) return String(preset.label || preset.name);
    return PRESET_LABELS[presetId(preset)] || presetId(preset);
  }

  function findEncoder(capabilities, id) {
    const encoders = capabilities && Array.isArray(capabilities.encoders) ? capabilities.encoders : [];
    return encoders.find(encoder => encoder && encoder.id === id) || null;
  }

  function encoderModes(capabilities, id) {
    const encoders = capabilities && Array.isArray(capabilities.encoders) ? capabilities.encoders : [];
    if (id !== 'auto') {
      const encoder = findEncoder(capabilities, id);
      return encoder && Array.isArray(encoder.modes) ? encoder.modes : [];
    }
    return Array.from(new Set(encoders.flatMap(encoder => Array.isArray(encoder.modes) ? encoder.modes : [])));
  }

  function knownEncoderFamily(id) {
    if (/^(?:libx264|h264_videotoolbox)$/.test(id)) return 'h264';
    if (/^(?:libx265|hevc_videotoolbox)$/.test(id)) return 'hevc';
    if (/^(?:libsvtav1|av1_)/.test(id)) return 'av1';
    if (/^libvpx-vp9$/.test(id)) return 'vp9';
    return '';
  }

  function validateOptions(value, probe, capabilities) {
    const options = normalizeOptions(value);
    const errors = [];
    const warnings = [];
    if (!capabilities || capabilities.available !== true) {
      errors.push((capabilities && capabilities.reason) || '视频编码当前不可用');
      return { valid: false, errors, warnings, options };
    }

    const modes = encoderModes(capabilities, options.encoder);
    if (options.encoder !== 'auto' && !findEncoder(capabilities, options.encoder)) {
      errors.push('所选编码器当前不可用');
    }
    if (!modes.includes(options.mode)) errors.push('所选编码器不支持此控制模式');
    if (!['mp4', 'mov', 'mkv', 'webm', 'ts'].includes(options.container)) errors.push('输出容器不受支持');
    if (!['copy', 'aac', 'opus', 'mp3', 'flac', 'none'].includes(options.audioPolicy)) errors.push('音频策略不受支持');
    if (!['copy', 'drop'].includes(options.subtitlePolicy)) errors.push('字幕策略不受支持');
    if (!['preserve', 'stripPrivate', 'strip'].includes(options.metadataPolicy)) errors.push('元数据策略不受支持');
    if (!['suffix', 'directory', 'replace'].includes(options.outputMode)) errors.push('输出方式不受支持');
    if (options.outputMode === 'directory' && !options.outputDir) errors.push('请先选择输出目录');
    if (options.mode === 'targetSize' && !(Number(options.targetMiB) > 0)) errors.push('目标体积必须大于 0');
    if ((options.mode === 'abr' || options.mode === 'twoPass') && !(Number(options.bitrateKbps) > 0)) errors.push('目标码率必须大于 0');
    if (options.resolution !== 0 && !(Number(options.resolution) >= 144 && Number(options.resolution) <= 16384)) {
      errors.push('自定义分辨率必须在 144 到 16384 像素之间');
    }

    const family = knownEncoderFamily(options.encoder);
    if (options.container === 'webm' && family && family !== 'av1' && family !== 'vp9') {
      errors.push('WebM 不能使用 H.264 或 HEVC 编码');
    }
    if (options.container === 'mp4' && options.audioPolicy === 'opus') {
      errors.push('MP4 不支持 Opus 音轨；请选择 AAC 或 MKV');
    }
    if (options.container === 'ts' && options.audioPolicy === 'opus') {
      errors.push('TS 不支持 Opus 音轨；请选择 AAC/MP3 或 MKV');
    }
    if (options.audioPolicy === 'flac' && options.container !== 'mkv') {
      errors.push('FLAC 音轨需要 MKV 容器');
    }

    const streams = probe && Array.isArray(probe.streams) ? probe.streams : [];
    const videoStream = streams.find(stream => stream && (stream.codecType === 'video' || stream.codec_type === 'video'));
    const hdrKind = String((probe && probe.hdrKind) || '').toLowerCase();
    if (hdrKind && hdrKind !== 'sdr' && hdrKind !== 'none') {
      if (!capabilities.preservesHdr) errors.push('检测到 HDR / Dolby Vision；当前能力未确认可保留色彩与动态元数据，已阻止转码');
      else warnings.push('将按当前编码器的 HDR 保留能力处理色彩与动态元数据');
    }
    if (videoStream && (videoStream.pixFmt || videoStream.pix_fmt || '').toLowerCase().includes('10')) {
      if (options.encoder === 'libx264' || options.encoder === 'h264_videotoolbox') {
        errors.push('所选 H.264 编码器不能保留此 10-bit 视频格式');
      }
    }
    if (options.audioPolicy === 'copy' && options.container === 'webm') {
      const audioStreams = streams.filter(stream => stream && (stream.codecType === 'audio' || stream.codec_type === 'audio'));
      if (audioStreams.some(stream => !['opus', 'vorbis'].includes(String(stream.codecName || stream.codec_name || '').toLowerCase()))) {
        warnings.push('源音频与 WebM 不兼容；后端需转换或拒绝此组合');
      }
    }
    if (options.allowLargerOutput) warnings.push('已允许保留大于原件的输出文件');
    return { valid: errors.length === 0, errors, warnings, options };
  }

  function parseRational(value) {
    if (typeof value === 'number') return value;
    if (typeof value !== 'string') return null;
    const parts = value.split('/').map(Number);
    if (parts.length === 2 && parts[1]) return parts[0] / parts[1];
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  }

  function firstVideoStream(probe) {
    const streams = probe && Array.isArray(probe.streams) ? probe.streams : [];
    return streams.find(stream => stream && (stream.codecType === 'video' || stream.codec_type === 'video')) || null;
  }

  function formatDurationUs(durationUs) {
    const totalSeconds = Math.max(0, Math.floor(Number(durationUs || 0) / 1000000));
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;
    return hours > 0
      ? hours + ':' + String(minutes).padStart(2, '0') + ':' + String(seconds).padStart(2, '0')
      : minutes + ':' + String(seconds).padStart(2, '0');
  }

  function formatFrameRate(value) {
    const rate = parseRational(value);
    return rate && Number.isFinite(rate) ? rate.toFixed(rate % 1 ? 2 : 0) + ' fps' : '—';
  }

  function videoSummary(probe) {
    const stream = firstVideoStream(probe);
    if (!stream) return { duration: formatDurationUs(probe && probe.durationUs), dimensions: '未知分辨率', frameRate: '—', codec: '未知编码' };
    const width = stream.width || stream.codedWidth || stream.coded_width;
    const height = stream.height || stream.codedHeight || stream.coded_height;
    const rotation = Number(stream.rotation || (stream.tags && stream.tags.rotate) || (probe && probe.rotation) || 0);
    const portrait = Math.abs(rotation) % 180 === 90;
    const dimensions = width && height
      ? (portrait ? height + ' × ' + width : width + ' × ' + height)
      : '未知分辨率';
    return {
      duration: formatDurationUs(probe && probe.durationUs),
      dimensions,
      frameRate: formatFrameRate(stream.avgFrameRate || stream.avg_frame_rate || stream.rFrameRate || stream.r_frame_rate),
      codec: stream.codecName || stream.codec_name || '未知编码',
    };
  }

  function estimateOutputBytes(probe, options) {
    const normalized = normalizeOptions(options);
    if (normalized.mode === 'targetSize' && Number(normalized.targetMiB) > 0) {
      return { bytes: Number(normalized.targetMiB) * 1024 * 1024, basis: 'target' };
    }
    const durationUs = Number(probe && probe.durationUs);
    if ((normalized.mode === 'abr' || normalized.mode === 'twoPass') && durationUs > 0 && Number(normalized.bitrateKbps) > 0) {
      const audioKbps = normalized.audioPolicy === 'aac' || normalized.audioPolicy === 'opus' || normalized.audioPolicy === 'mp3' ? 160 : 0;
      return { bytes: (Number(normalized.bitrateKbps) + audioKbps) * 1000 * durationUs / 8000000, basis: 'bitrate' };
    }
    return null;
  }

  function normalizeResult(result, path, probe, options) {
    const value = result || {};
    const originalBytes = Number(value.originalBytes != null ? value.originalBytes : (probe && probe.sizeBytes) || 0);
    const outputBytes = value.outputBytes == null ? null : Number(value.outputBytes);
    // 结论（outcome/reasonCode）才是"这次到底成没成、有没有产物"的出处；这里只把后端
    // 字段整理成同一条形状，不再自己造旗标。从前这里用 skippedLarger 同时表达状态和
    // 原因，于是同一件事在行、账、按钮上各有一种读法。
    const success = value.success === true;
    const savings = outputBytes != null && originalBytes > 0
      ? ((originalBytes - outputBytes) / originalBytes) * 100
      : null;
    return Object.assign({}, value, {
      mediaKind: 'video',
      file: value.sourcePath || value.file || path,
      sourcePath: value.sourcePath || path,
      outputPath: value.outputPath || null,
      originalSize: originalBytes,
      compressedSize: outputBytes,
      originalBytes,
      outputBytes,
      success,
      savings,
      outputMode: value.outputMode || normalizeOptions(options).outputMode,
      reason: value.reason || null,
      outcome: value.outcome || null,
      reasonCode: value.reasonCode || null,
    });
  }

  /// 一组编码参数的一句话摘要（意图条用它说明"现在真正在跑什么"）。
  /// 注意与 videoSummary(probe) 区分：那个描述的是**源文件**，这个描述的是**请求**。
  function controlSummary(options) {
    const value = normalizeOptions(options);
    const parts = [];
    if (value.mode === 'crf') parts.push('CRF ' + value.crf);
    else if (value.mode === 'targetSize') parts.push('目标 ' + value.targetMiB + ' MiB（估算）');
    else parts.push(value.bitrateKbps + ' kbps' + (value.mode === 'twoPass' ? ' · 两遍' : ''));
    parts.push(String(value.container || 'mp4').toUpperCase());
    if (value.encoder && value.encoder !== 'auto') parts.push(value.encoder);
    if (value.resolution) parts.push('最长边 ' + value.resolution);
    if (value.frameRate) parts.push(value.frameRate + ' fps');
    parts.push(value.audioPolicy === 'copy' ? '音轨复制' : value.audioPolicy.toUpperCase());
    if (value.subtitlePolicy === 'drop') parts.push('去字幕');
    if (value.metadataPolicy === 'strip') parts.push('去元数据');
    return parts.join(' · ');
  }

  const api = Object.freeze({
    DEFAULT_OPTIONS,
    PRESET_LABELS,
    normalizeOptions,
    presetId,
    presetLabel,
    controlSummary,
    findEncoder,
    encoderModes,
    knownEncoderFamily,
    validateOptions,
    parseRational,
    firstVideoStream,
    formatDurationUs,
    formatFrameRate,
    videoSummary,
    estimateOutputBytes,
    normalizeResult,
  });
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
  if (root) root.SizqoVideo = api;
})(typeof window === 'undefined' ? globalThis : window);
