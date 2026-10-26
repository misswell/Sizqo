use super::{plan::EncodePlan, probe::VideoProbe, MetadataPolicy};

/// §56 输出必须重新探测后逐项核对，不能把 ffmpeg 退出码当成成功。
pub fn validate(source: &VideoProbe, output: &VideoProbe, plan: &EncodePlan) -> Result<(), String> {
    if output.size_bytes == 0 {
        return Err("Empty encoded output".into());
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
    if let Some(original) = source.duration_us {
        let actual = output
            .duration_us
            .ok_or("Output duration is unavailable")?;
        if (actual as i128 - original as i128).abs() as u64 > (original / 100).max(250_000) {
            return Err("Output duration differs from source".into());
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
                for (key, _) in tags(&output.format_tags) {
                    if crate::video::is_private_metadata_key(key) {
                        return Err(format!(
                            "Privacy mode left private metadata tag {key} in the output"
                        ));
                    }
                }
                for stream in &output.streams {
                    for (key, _) in tags(&stream.tags) {
                        if crate::video::is_private_metadata_key(key) {
                            return Err(format!(
                                "Privacy mode left private metadata tag {key} on stream {}",
                                stream.index
                            ));
                        }
                    }
                }
            }
        }
        MetadataPolicy::StripAll => {
            if !output.chapters.is_empty() {
                return Err("Strip-all request retained chapters".into());
            }
            if let Some(remaining) = output.format_tags.as_object() {
                for key in source.format_tags.as_object().into_iter().flat_map(|o| o.keys()) {
                    if is_muxer_generated(key) {
                        continue;
                    }
                    if remaining.contains_key(key) {
                        return Err(format!(
                            "Strip-all request retained source metadata tag {key}"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn tags(value: &serde_json::Value) -> impl Iterator<Item = (&str, &serde_json::Value)> {
    value
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter().map(|(key, value)| (key.as_str(), value)))
}

/// 容器自己生成的结构字段：`-map_metadata -1` 也去不掉，不算"保留了源 metadata"。
fn is_muxer_generated(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "major_brand" | "minor_version" | "compatible_brands" | "encoder"
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
            vfr_rate: None,
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
}
