use super::{plan::EncodePlan, probe::VideoProbe, MetadataPolicy};

/// §56 输出必须重新探测后逐项核对，不能把 ffmpeg 退出码当成成功。
pub fn validate(source: &VideoProbe, output: &VideoProbe, plan: &EncodePlan) -> Result<(), String> {
    if output.size_bytes == 0 {
        return Err("Empty encoded output".into());
    }
    // 视频流数量也是契约：plan 只要一条视频轨，输出里多出来的那条必须算失败，
    // 不能让 video() 悄悄挑第一条、把多余的留在产物里。
    let output_video_count = output
        .streams
        .iter()
        .filter(|s| s.codec_type == "video" && s.disposition["attached_pic"].as_u64() != Some(1))
        .count();
    if output_video_count != 1 {
        return Err(format!(
            "Output must have exactly one video stream, found {output_video_count}"
        ));
    }
    let v = output.video().ok_or("Output has no video stream")?;
    if (v.width, v.height) != (plan.width, plan.height) {
        return Err("Output dimensions do not match encode plan".into());
    }
    if v.pixel_format != "yuv420p" {
        return Err(format!("Unexpected pixel format {}", v.pixel_format));
    }
    let expected_codec = if plan.encoder == "libsvtav1" {
        "av1"
    } else if plan.encoder == "libvpx-vp9" {
        "vp9"
    } else if plan.encoder == "libx265" || plan.encoder == "hevc_videotoolbox" {
        "hevc"
    } else {
        "h264"
    };
    if v.codec_name != expected_codec {
        return Err("Output encoder differs from requested encoder".into());
    }
    let mux = match plan.container.as_str() {
        "mp4" | "mov" => output.format.contains("mov") || output.format.contains("mp4"),
        "mkv" => output.format.contains("matroska"),
        "webm" => output.format.contains("webm"),
        "ts" => output.format.contains("mpegts"),
        _ => false,
    };
    if !mux {
        return Err("Output container differs from plan".into());
    }
    // §5.4 时长容差按帧时间间隔计算，不再用百分比：一条 2 小时的视频丢了 60 秒，
    // 百分比阈值会放它过去；绝对容差（两帧，下限 250ms 的容器取整余量）必须拦下。
    if let Some(original) = source.duration_us {
        let actual = output
            .duration_us
            .ok_or("Output duration is unavailable")?;
        let fps = source.frame_rate.unwrap_or(30.0);
        let frame_interval_us = if fps > 0.0 { (1e6 / fps) as u64 } else { 33_333 };
        let tolerance = (frame_interval_us * 2).max(250_000);
        if (actual as i128 - original as i128).abs() as u64 > tolerance {
            return Err(format!(
                "Output duration differs from source by more than the frame-interval tolerance ({tolerance} µs)"
            ));
        }
    }
    // 帧率：用户指定了就按指定值核；否则按源平均帧率核（VFR 允许 5% / 0.5fps 偏差）。
    if let Some(expected) = plan.options.frame_rate.or(source.frame_rate) {
        let actual = output
            .frame_rate
            .ok_or("Output frame rate is unavailable")?;
        let tolerance = (expected * 0.05).max(0.5);
        if (actual - expected).abs() > tolerance {
            return Err(format!(
                "Output frame rate {actual:.2} differs from expected {expected:.2}"
            ));
        }
    }
    // §5.4 色彩元数据：源里明确写了的传递函数 / 原色 / 色彩矩阵，输出必须
    // **保持同一个值** —— 只检查"存在"拦得住"丢了"，拦不住"换了"：
    // bt709 的源被贴上 smpte2084 的标签就是静默重上色，比丢掉更隐蔽。
    // 管线没有任何显式色彩转换路径，因此值不一致一律判失败。
    fn color_specified(value: &str) -> bool {
        !value.is_empty()
            && !value.eq_ignore_ascii_case("unknown")
            && !value.eq_ignore_ascii_case("unspecified")
            && !value.eq_ignore_ascii_case("reserved")
    }
    let source_video = source.video();
    if let Some(source_video) = source_video {
        for (name, recorded, carried) in [
            ("transfer", source_video.color_transfer.as_str(), v.color_transfer.as_str()),
            ("primaries", source_video.color_primaries.as_str(), v.color_primaries.as_str()),
            ("space", source_video.color_space.as_str(), v.color_space.as_str()),
        ] {
            if !color_specified(recorded) {
                continue;
            }
            if !color_specified(carried) {
                return Err(format!(
                    "Encoded output dropped the source color {name} ({recorded})"
                ));
            }
            if !recorded.eq_ignore_ascii_case(carried) {
                return Err(format!(
                    "Encoded output changed the source color {name} from {recorded} to {carried}"
                ));
            }
        }
    }
    let expected_audio = if plan.options.audio_policy == "none" {
        0
    } else {
        source.audio().count()
    };
    if output.audio().count() != expected_audio {
        return Err("Encoded output lost audio tracks".into());
    }
    if plan.options.audio_policy != "none" {
        let expected_audio = match plan.options.audio_policy.as_str() {
            "aac" => Some("aac"),
            "opus" => Some("opus"),
            "mp3" => Some("mp3"),
            "flac" => Some("flac"),
            _ => None,
        };
        for (a, b) in source.audio().zip(output.audio()) {
            if a.channels != b.channels {
                return Err("Encoded output changed audio channel count".into());
            }
            // 采样率不是"编码器想给多少就给多少"：源轨 48 kHz，输出 44.1 kHz 就是变了。
            if a.sample_rate != 0 && b.sample_rate != 0 && a.sample_rate != b.sample_rate {
                return Err(format!(
                    "Encoded output changed audio sample rate from {} to {}",
                    a.sample_rate, b.sample_rate
                ));
            }
            if a.tags["language"] != b.tags["language"]
                && a.tags["language"].as_str().is_some_and(|s| s != "und")
            {
                return Err("Encoded output changed audio language tags".into());
            }
            if plan.options.audio_policy == "copy" && a.codec_name != b.codec_name {
                return Err("Copied audio codec changed".into());
            }
            if let Some(expected) = expected_audio {
                if plan.options.audio_policy != "copy" && b.codec_name != expected {
                    return Err(format!(
                        "Encoded audio codec is {} but {} was requested",
                        b.codec_name, expected
                    ));
                }
            }
        }
    }
    let expected_subs = if plan.options.subtitle_policy == "copy" {
        source.subtitles().count()
    } else {
        0
    };
    if output.subtitles().count() != expected_subs {
        return Err("Encoded output lost subtitle tracks".into());
    }
    validate_metadata(source, output, plan)?;
    if plan.container == "mkv"
        && source
            .streams
            .iter()
            .filter(|s| s.codec_type == "attachment")
            .count()
            != output
                .streams
                .iter()
                .filter(|s| s.codec_type == "attachment")
                .count()
    {
        return Err("Encoded output lost attachments".into());
    }
    if output.rotation.rem_euclid(360) != 0 {
        return Err("Unexpected output rotation".into());
    }
    Ok(())
}

/// §55 §56 元数据策略必须能在输出里被证明，而不是只在命令行里写过。
fn validate_metadata(
    source: &VideoProbe,
    output: &VideoProbe,
    plan: &EncodePlan,
) -> Result<(), String> {
    match plan.metadata_policy {
        MetadataPolicy::KeepAll | MetadataPolicy::StripPrivate => {
            if source.chapters.len() != output.chapters.len() {
                return Err("Encoded output lost chapters".into());
            }
            if plan.metadata_policy == MetadataPolicy::StripPrivate {
                // 隐私键要扫全部作用域：全局、每条流、每个章节、每个 program ——
                // 手机拍的文件把位置写在任何一层都不奇怪。
                let mut leaked = |key: &str, where_: String| -> Result<(), String> {
                    if crate::video::is_private_metadata_key(key) {
                        return Err(format!(
                            "Privacy mode left private metadata tag {key} {where_}"
                        ));
                    }
                    Ok(())
                };
                for (key, _) in tags(&output.format_tags) {
                    leaked(key, "in the output".into())?;
                }
                for stream in &output.streams {
                    for (key, _) in tags(&stream.tags) {
                        leaked(key, format!("on stream {}", stream.index))?;
                    }
                }
                for (chapter_index, chapter) in output.chapters.iter().enumerate() {
                    for (key, _) in tags(&chapter["tags"]) {
                        leaked(key, format!("on chapter {chapter_index}"))?;
                    }
                }
                for (program_index, program) in output.programs.iter().enumerate() {
                    for (key, _) in tags(&program["tags"]) {
                        leaked(key, format!("on program {program_index}"))?;
                    }
                }
            }
        }
        MetadataPolicy::StripAll => {
            if !output.chapters.is_empty() {
                return Err("Strip-all request retained chapters".into());
            }
            // "全部移除"要在每一个作用域上兑现：源里出现过的任何键（全局/流/
            // 章节/program）不许在输出的任何作用域里活着；隐私键更是无论
            // 源里有没有都直接判失败。
            let source_keys: std::collections::HashSet<String> = source_keys(source).collect();
            let check = |key: &str, where_: String| -> Result<(), String> {
                if crate::video::is_private_metadata_key(key) {
                    return Err(format!("Strip-all request left private metadata tag {key} {where_}"));
                }
                if !is_muxer_generated(key) && source_keys.contains(&key.to_ascii_lowercase()) {
                    return Err(format!(
                        "Strip-all request retained source metadata tag {key} {where_}"
                    ));
                }
                Ok(())
            };
            for (key, _) in tags(&output.format_tags) {
                check(key, "in the output".into())?;
            }
            for stream in &output.streams {
                for (key, _) in tags(&stream.tags) {
                    check(key, format!("on stream {}", stream.index))?;
                }
            }
            for (chapter_index, chapter) in output.chapters.iter().enumerate() {
                for (key, _) in tags(&chapter["tags"]) {
                    check(key, format!("on chapter {chapter_index}"))?;
                }
            }
            for (program_index, program) in output.programs.iter().enumerate() {
                for (key, _) in tags(&program["tags"]) {
                    check(key, format!("on program {program_index}"))?;
                }
            }
        }
    }
    Ok(())
}

/// 源文件里出现过的全部 metadata 键（小写归一），跨作用域收集。
fn source_keys(
    source: &VideoProbe,
) -> impl Iterator<Item = String> + '_ {
    let format_keys = tags(&source.format_tags).map(|(k, _)| k.to_ascii_lowercase());
    let stream_keys = source
        .streams
        .iter()
        .flat_map(|s| tags(&s.tags).map(move |(k, _)| k.to_ascii_lowercase()));
    let chapter_keys = source
        .chapters
        .iter()
        .flat_map(|c| tags(&c["tags"]).map(move |(k, _)| k.to_ascii_lowercase()));
    let program_keys = source
        .programs
        .iter()
        .flat_map(|p| tags(&p["tags"]).map(move |(k, _)| k.to_ascii_lowercase()));
    format_keys.chain(stream_keys).chain(chapter_keys).chain(program_keys)
}

fn tags(value: &serde_json::Value) -> impl Iterator<Item = (&str, &serde_json::Value)> {
    value
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter().map(|(key, value)| (key.as_str(), value)))
}

/// 容器自己生成的结构字段：`-map_metadata -1` 也去不掉，不算"保留了源 metadata"。
/// mp4 的 brand 组、编码器署名；mkv/mp4 muxer 会往**流标签**里写 DURATION /
/// NUMBER_OF_FRAMES / NUMBER_OF_BYTES 这类结构统计 —— 它们不是源 metadata。
fn is_muxer_generated(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "major_brand" | "minor_version" | "compatible_brands" | "encoder"
            | "duration" | "duration-eng" | "duration_chs" | "duration_cty"
            | "number_of_frames" | "number_of_bytes" | "streamorder"
            | "statistics_tags" | "statistics_layout"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::VideoOptions;

    fn plan(policy: MetadataPolicy) -> EncodePlan {
        EncodePlan {
            encoder: "libx264".into(),
            container: "mp4".into(),
            bitrate_kbps: None,
            crf: Some(23.0),
            width: 160,
            height: 120,
            passes: 1,
            threads: 1,
            warnings: vec![],
            metadata_policy: policy,
            options: VideoOptions {
                encoder: "libx264".into(),
                mode: "crf".into(),
                ..Default::default()
            },
        }
    }

    fn probe(tags: serde_json::Value, chapters: bool, fps: &str, sample_rate: &str) -> VideoProbe {
        let mut root = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 160, "height": 120,
                 "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": fps},
                {"index": 1, "codec_type": "audio", "codec_name": "aac",
                 "channels": 2, "sample_rate": sample_rate}
            ],
            "format": {"duration": "1", "format_name": "mp4", "tags": tags}
        });
        if chapters {
            root["chapters"] = serde_json::json!([{"id": 0}]);
        }
        VideoProbe::parse(root, 1000).unwrap()
    }

    fn ordinary() -> VideoProbe {
        probe(serde_json::json!({"title": "Keep me"}), true, "10/1", "48000")
    }

    #[test]
    fn privacy_mode_refuses_an_output_that_still_carries_location() {
        let source = probe(
            serde_json::json!({"title": "Keep me", "location": "+37.7-122.4/"}),
            true,
            "10/1",
            "48000",
        );
        let leaked = probe(
            serde_json::json!({"title": "Keep me", "location": "+37.7-122.4/"}),
            true,
            "10/1",
            "48000",
        );
        let error = validate(&source, &leaked, &plan(MetadataPolicy::StripPrivate))
            .expect_err("输出里还留着 location，隐私模式必须判失败");
        assert!(error.contains("location"));
        assert!(validate(&source, &ordinary(), &plan(MetadataPolicy::StripPrivate)).is_ok());
    }

    #[test]
    fn strip_all_refuses_retained_tags_or_chapters() {
        let source = probe(serde_json::json!({"title": "Keep me"}), true, "10/1", "48000");
        let retained = probe(serde_json::json!({"title": "Keep me"}), false, "10/1", "48000");
        assert!(validate(&source, &retained, &plan(MetadataPolicy::StripAll))
            .expect_err("title 还在，全部移除没有兑现")
            .contains("title"));
        let chapters_left = probe(serde_json::json!({}), true, "10/1", "48000");
        assert!(validate(&source, &chapters_left, &plan(MetadataPolicy::StripAll))
            .expect_err("章节还在，全部移除没有兑现")
            .contains("chapters"));
        let clean = probe(serde_json::json!({}), false, "10/1", "48000");
        assert!(validate(&source, &clean, &plan(MetadataPolicy::StripAll)).is_ok());
    }

    #[test]
    fn keep_all_refuses_a_lost_chapter() {
        let source = probe(serde_json::json!({"title": "Keep me"}), true, "10/1", "48000");
        let lost = probe(serde_json::json!({"title": "Keep me"}), false, "10/1", "48000");
        assert!(validate(&source, &lost, &plan(MetadataPolicy::KeepAll))
            .expect_err("保留策略丢了章节")
            .contains("chapters"));
    }

    #[test]
    fn a_changed_sample_rate_or_frame_rate_is_a_failure() {
        let source = ordinary();
        let wrong_rate = probe(serde_json::json!({"title": "Keep me"}), true, "10/1", "44100");
        assert!(validate(&source, &wrong_rate, &plan(MetadataPolicy::KeepAll))
            .expect_err("采样率被改了")
            .contains("sample rate"));
        let wrong_fps = probe(serde_json::json!({"title": "Keep me"}), true, "12/1", "48000");
        assert!(validate(&source, &wrong_fps, &plan(MetadataPolicy::KeepAll))
            .expect_err("帧率被改了")
            .contains("frame rate"));
    }

    /// §5.4 时长容差按帧间隔计算：2 小时的视频丢了 60 秒必须失败，
    /// 百分比阈值会放它过去。
    #[test]
    fn a_two_hour_video_that_lost_a_minute_fails_the_duration_check() {
        // 源 2 小时（fps 10/1 → 帧间隔 100ms → 容差 = 2 帧 = 200ms，下限 250ms）。
        let mut source_root = serde_json::json!({
            "streams": [{"index": 0, "codec_type": "video", "width": 160, "height": 120,
                         "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": "10/1"}],
            "format": {"duration": "7200", "format_name": "mp4", "tags": {}}
        });
        source_root["format"]["duration"] = serde_json::json!("7200");
        let source = VideoProbe::parse(source_root.clone(), 1000).unwrap();
        let mut output_root = source_root.clone();
        output_root["format"]["duration"] = serde_json::json!("7140"); // 丢 60 秒
        let output = VideoProbe::parse(output_root, 1000).unwrap();
        let error = validate(&source, &output, &plan(MetadataPolicy::KeepAll))
            .expect_err("丢了 60 秒的 2 小时视频不许通过");
        assert!(error.contains("frame-interval tolerance"), "{error}");
        // 1 秒以内的容器取整误差照常放行。
        let mut close_root = source_root.clone();
        close_root["format"]["duration"] = serde_json::json!("7200.2");
        let close = VideoProbe::parse(close_root, 1000).unwrap();
        assert!(validate(&source, &close, &plan(MetadataPolicy::KeepAll)).is_ok());
    }

    /// §5.4 输出多余的视频流必须失败，不许被"挑第一条"吞掉。
    #[test]
    fn an_extra_video_stream_in_the_output_is_a_failure() {
        let mut root = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 160, "height": 120,
                 "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": "10/1"},
                {"index": 1, "codec_type": "video", "width": 160, "height": 120,
                 "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": "10/1"},
                {"index": 2, "codec_type": "audio", "codec_name": "aac",
                 "channels": 2, "sample_rate": "48000"}
            ],
            "format": {"duration": "1", "format_name": "mp4", "tags": {}}
        });
        let source = VideoProbe::parse(root.clone(), 1000).unwrap();
        let output = VideoProbe::parse(root, 1000).unwrap();
        assert!(validate(&source, &output, &plan(MetadataPolicy::KeepAll))
            .expect_err("多余的视频流必须失败")
            .contains("exactly one video stream"));
    }

    /// §5.4 审查修复回归：源 bt709 的 transfer 被贴上 smpte2084 标签
    /// —— 只查"存在"会放行，静默重上色比丢标签更隐蔽。
    #[test]
    fn a_changed_color_value_is_a_failure_not_just_a_dropped_one() {
        let mut root = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 160, "height": 120,
                 "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": "10/1",
                 "color_transfer": "bt709", "color_primaries": "bt709", "color_space": "bt709"},
                {"index": 1, "codec_type": "audio", "codec_name": "aac",
                 "channels": 2, "sample_rate": "48000"}
            ],
            "format": {"duration": "1", "format_name": "mp4", "tags": {}}
        });
        let source = VideoProbe::parse(root.clone(), 1000).unwrap();
        root["streams"][0]["color_transfer"] = serde_json::json!("smpte2084");
        let relabeled = VideoProbe::parse(root, 1000).unwrap();
        let error = validate(&source, &relabeled, &plan(MetadataPolicy::KeepAll))
            .expect_err("换了色彩标签必须失败");
        assert!(error.contains("changed the source color transfer"), "{error}");
    }

    /// §5.4 源里明确写了的色彩信息，输出不许悄悄丢掉。
    #[test]
    fn dropped_color_metadata_is_a_failure() {
        let mut root = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 160, "height": 120,
                 "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": "10/1",
                 "color_transfer": "bt709", "color_primaries": "bt709"},
                {"index": 1, "codec_type": "audio", "codec_name": "aac",
                 "channels": 2, "sample_rate": "48000"}
            ],
            "format": {"duration": "1", "format_name": "mp4", "tags": {}}
        });
        let source = VideoProbe::parse(root.clone(), 1000).unwrap();
        root["streams"][0]["color_transfer"] = serde_json::json!("");
        let stripped = VideoProbe::parse(root, 1000).unwrap();
        assert!(validate(&source, &stripped, &plan(MetadataPolicy::KeepAll))
            .expect_err("色彩传递函数被丢了")
            .contains("color transfer"));
    }

    /// §6 隐私键与源键在任何作用域（流/章节/program）残留都算失败。
    #[test]
    fn metadata_scans_every_scope_for_private_and_source_keys() {
        let mut source_root = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 160, "height": 120,
                 "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": "10/1",
                 "tags": {"stream_title": "keep?"}},
                {"index": 1, "codec_type": "audio", "codec_name": "aac",
                 "channels": 2, "sample_rate": "48000"}
            ],
            "format": {"duration": "1", "format_name": "mp4", "tags": {}},
            "chapters": [{"id": 0, "tags": {"chapter_title": "Intro"}}],
            "programs": [{"program_id": 1, "tags": {"service_gps_note": "hidden"}}]
        });
        let source = VideoProbe::parse(source_root.clone(), 1000).unwrap();
        // 章节没清干净 → StripAll 失败。
        let mut chapter_leak = source_root.clone();
        chapter_leak["programs"] = serde_json::json!([]);
        let leak = VideoProbe::parse(chapter_leak, 1000).unwrap();
        assert!(validate(&source, &leak, &plan(MetadataPolicy::StripAll))
            .expect_err("章节没清干净必须失败")
            .contains("chapters"));
        // 流级残留源键 → StripAll 失败（全局键早就扫，这里证的是流作用域）。
        let mut stream_leak = source_root.clone();
        stream_leak["chapters"] = serde_json::json!([]);
        stream_leak["programs"] = serde_json::json!([]);
        stream_leak["format"]["tags"] = serde_json::json!({});
        let leak = VideoProbe::parse(stream_leak, 1000).unwrap();
        assert!(validate(&source, &leak, &plan(MetadataPolicy::StripAll))
            .expect_err("流级残留源键必须失败")
            .contains("stream_title"));
        // program 级隐私键 → StripPrivate 失败。
        source_root["programs"] = serde_json::json!([]);
        source_root["chapters"] = serde_json::json!([]);
        let source_clean = VideoProbe::parse(source_root.clone(), 1000).unwrap();
        let mut program_privacy = serde_json::json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 160, "height": 120,
                 "pix_fmt": "yuv420p", "codec_name": "h264", "avg_frame_rate": "10/1"},
                {"index": 1, "codec_type": "audio", "codec_name": "aac",
                 "channels": 2, "sample_rate": "48000"}
            ],
            "format": {"duration": "1", "format_name": "mp4", "tags": {}},
            "programs": [{"program_id": 1, "tags": {"location": "+37.7-122.4/"}}]
        });
        let private_leak = VideoProbe::parse(program_privacy.clone(), 1000).unwrap();
        assert!(validate(&source_clean, &private_leak, &plan(MetadataPolicy::StripPrivate))
            .expect_err("program 级隐私键必须失败")
            .contains("location"));
        // 干净输出放行（消掉 program_privacy 未用告警的写法）。
        program_privacy["programs"] = serde_json::json!([]);
        let clean = VideoProbe::parse(program_privacy, 1000).unwrap();
        assert!(validate(&source_clean, &clean, &plan(MetadataPolicy::StripPrivate)).is_ok());
    }
}
