// §9.3 真实图片矩阵：拒绝/接受路径由真实文件背书（含 Alpha、损坏、空文件、
// Unicode 路径）。常规套件即可运行，无需外部工具。
use sizqo_lib::engine;
use std::fs;

#[tokio::test]
async fn an_alpha_png_keeps_its_transparency_through_png_output() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("overlay 图 带Alpha.png");
    let mut img = image::RgbaImage::from_fn(200, 150, |x, y| {
        image::Rgba([(x * 255 / 200) as u8, (y * 255 / 150) as u8, 64, 255])
    });
    // 左上角挖出一块真正的透明区：压缩产物必须原样保留，不许被拍成黑底。
    for y in 0..40 {
        for x in 0..40 {
            img.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
        }
    }
    image::DynamicImage::ImageRgba8(img).save(&path).unwrap();

    let options = engine::CompressOptions {
        quality: 80,
        output_format: "png".into(),
        ..Default::default()
    };
    let result = engine::compress_image(&path, &options).await;
    assert!(result.success, "带 Alpha 的 PNG 必须压得动: {:?}", result.error);
    assert_eq!(result.out_type, "png");
    let decoded = image::load_from_memory(&result.compressed).unwrap();
    assert!(decoded.as_rgba8().map(|b| b.get_pixel(0, 0)[3] == 0).unwrap_or_else(|| {
        decoded.color().has_alpha()
    }), "压缩产物必须仍是 RGBA 且透明区保留");
}

#[tokio::test]
async fn a_corrupt_jpeg_fails_cleanly_instead_of_pretending() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("broken.jpg");
    let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0];
    bytes.extend_from_slice(b"this payload is not a jpeg at all......");
    fs::write(&path, &bytes).unwrap();

    let options = engine::CompressOptions {
        quality: 75,
        output_format: "original".into(),
        ..Default::default()
    };
    let result = engine::compress_image(&path, &options).await;
    assert!(!result.success, "损坏文件不许伪装成功");
    assert!(result.error.is_some(), "失败必须带原因");
}

#[tokio::test]
async fn an_empty_file_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("空文件.png");
    fs::write(&path, b"").unwrap();

    let options = engine::CompressOptions {
        quality: 75,
        output_format: "original".into(),
        ..Default::default()
    };
    let result = engine::compress_image(&path, &options).await;
    assert!(!result.success, "空文件不许伪装成功");
}

#[tokio::test]
async fn a_unicode_and_space_path_compresses_normally() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("截图 2026-10-27 中文目录/微笑 的 猫.png");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let img = image::RgbImage::from_fn(320, 240, |x, y| {
        image::Rgb([(x * 255 / 320) as u8, (y * 255 / 240) as u8, 96])
    });
    image::DynamicImage::ImageRgb8(img).save(&path).unwrap();

    let options = engine::CompressOptions {
        quality: 80,
        output_format: "webp".into(),
        ..Default::default()
    };
    let result = engine::compress_image(&path, &options).await;
    assert!(result.success, "Unicode/空格路径必须正常压缩: {:?}", result.error);
    assert_eq!(result.out_type, "webp");
}

#[tokio::test]
async fn an_oversized_target_budget_is_rejected_not_faked() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("smart.png");
    let img = image::RgbImage::from_fn(300, 200, |x, y| {
        image::Rgb([(x * 255 / 300) as u8, (y * 255 / 200) as u8, 128])
    });
    image::DynamicImage::ImageRgb8(img).save(&path).unwrap();

    // 目标体积小到物理上不可能（64 字节）：指定体积路径必须给明确结论，
    // 绝不把"没达标"标成"命中目标"（§61 §62）。
    let options = engine::CompressOptions {
        quality: 75,
        output_format: "original".into(),
        max_output_bytes: Some(64),
        ..Default::default()
    };
    let result = engine::compress_image(&path, &options).await;
    if result.success {
        assert_ne!(
            result.target_met, Some(true),
            "64 字节的物理不可达预算不许被报告为命中"
        );
    } else {
        assert!(result.error.is_some(), "失败必须带原因");
    }
}
