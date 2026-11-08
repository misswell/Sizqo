//! T11（0.3.6-rc.15）：EXIF Orientation 1～8 的真实夹具测试。
//!
//! 夹具 = image crate 编码的 JPEG + 手工拼接的 APP1 EXIF 段（Little-Endian TIFF，
//! 单条目 0x0112 Orientation）。映射采用 image 0.25 的 `Orientation::to_exif`：
//! 1=NoTransforms 2=FlipHorizontal 3=Rotate180 4=FlipVertical 5=Rotate90FlipH
//! 6=Rotate90 7=Rotate270FlipH 8=Rotate270。
//!
//! 现场用"红蓝左右分栏 + 左上角白色标记像素"的 32×20 图：每个方向变换后的
//! 尺寸（5～8 交换为 20×32）、分栏位置、标记像素位置三者互相独立地钉住
//! `decode_with_orientation` 的烘焙结果 —— 只比尺寸拦不住"方向对了内容反了"。
//!
//! 端到端：带 Orientation=6（iPhone 竖拍）的源经过 `compress_image` 输出 JPEG
//! 必须 (a) 像素已转正、(b) 不再携带 EXIF 方向标签（编码器不写 EXIF），
//! 否则支持 EXIF 的看图软件会二次旋转。

use sizqo_lib::engine::{self, CompressOptions};

/// 手工构造 APP1 EXIF 段：Little-Endian TIFF 头 + IFD0 单条目（Orientation SHORT）。
fn exif_app1_segment(orientation: u16) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"Exif\0\0");
    // TIFF 头：II（小端）+ 42 + IFD0 偏移 8
    payload.extend_from_slice(&[0x49, 0x49, 0x2a, 0x00]);
    payload.extend_from_slice(&8u32.to_le_bytes());
    // IFD0：1 条目
    payload.extend_from_slice(&1u16.to_le_bytes());
    payload.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    payload.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    payload.extend_from_slice(&1u32.to_le_bytes()); // count
    payload.extend_from_slice(&(orientation as u16).to_le_bytes()); // value
    payload.extend_from_slice(&0u16.to_le_bytes()); // value 高位填充
    payload.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
    // JPEG 分段长度字段是**大端**：写成小端会被解成 0x2200=8704（本轮实测），
    // 解码器会以为后面还有 8696 字节扫描数据。
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(&payload);
    segment
}

/// 生成 32×20 JPEG（左右分栏 + 左上角白色标记）并拼入指定方向的 EXIF 段。
/// 返回 tempdir 守卫：调用方持有它，测试结束时自动清理。
fn fixture_with_orientation(
    orientation: u16,
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let source_path = tmp.path().join("orientation.png");
    let mut img = image::RgbImage::from_fn(32, 20, |x, _y| {
        if x < 16 {
            image::Rgb([220, 30, 30])
        } else {
            image::Rgb([30, 60, 220])
        }
    });
    img.put_pixel(0, 0, image::Rgb([250, 250, 250]));
    let dynamic = image::DynamicImage::ImageRgb8(img);
    dynamic.save(&source_path).unwrap();

    let jpeg_path = tmp.path().join("orientation.jpg");
    dynamic.save(&jpeg_path).unwrap();

    let original = std::fs::read(&jpeg_path).unwrap();
    assert_eq!(&original[..2], &[0xFF, 0xD8], "夹具必须是真实 JPEG");
    let mut spliced = Vec::with_capacity(original.len() + 40);
    spliced.extend_from_slice(&original[..2]);
    spliced.extend_from_slice(&exif_app1_segment(orientation));
    spliced.extend_from_slice(&original[2..]);
    let spliced_path = tmp.path().join(format!("orientation-{orientation}.jpg"));
    std::fs::write(&spliced_path, &spliced).unwrap();
    if let Err(error) = image::open(&spliced_path) {
        panic!("拼接后的 JPEG 无法被 image crate 解码（orientation {orientation}）: {error}");
    }
    (tmp, source_path, spliced_path)
}

/// 各方向的期望现场：(宽, 高, 标记像素位置, 分栏判定)。
/// `bar_is_red_on_top_or_left` = 红色分栏在旋转后位于上/左（true）还是下/右（false）。
fn expectations(orientation: u16) -> (u32, u32, (u32, u32), bool) {
    match orientation {
        1 => (32, 20, (0, 0), true),
        2 => (32, 20, (31, 0), false),  // FlipHorizontal：红→右半
        3 => (32, 20, (31, 19), false), // Rotate180：红→右半
        4 => (32, 20, (0, 19), true),   // FlipVertical：红→左半
        5 => (20, 32, (0, 0), true),    // Rotate90+FlipH：红→上半
        6 => (20, 32, (19, 0), true),   // Rotate90：红→上半
        7 => (20, 32, (19, 31), false), // Rotate270+FlipH：红→下半
        8 => (20, 32, (0, 31), false),  // Rotate270：红→下半
        other => unreachable!("EXIF orientation 只有 1～8：{other}"),
    }
}

fn pixel(decoded: &image::DynamicImage, x: u32, y: u32) -> [u8; 3] {
    let rgb = decoded.to_rgb8();
    let p = rgb.get_pixel(x, y);
    [p[0], p[1], p[2]]
}

fn is_red(p: [u8; 3]) -> bool {
    p[0] > 180 && p[1] < 110 && p[2] < 110
}
fn is_white(p: [u8; 3]) -> bool {
    p[0] > 200 && p[1] > 200 && p[2] > 200
}

#[test]
fn exif_orientation_1_to_8_is_baked_into_pixels() {
    for orientation in 1u16..=8 {
        let (_dir, _source, path) = fixture_with_orientation(orientation);
        let decoded = engine::decode_with_orientation(&path)
            .unwrap_or_else(|| panic!("orientation {orientation}: 解码失败"));
        let (width, height, marker, red_top_or_left) = expectations(orientation);
        assert_eq!(
            (decoded.width(), decoded.height()),
            (width, height),
            "orientation {orientation}: 烘焙后尺寸必须匹配"
        );
        assert!(
            is_white(pixel(&decoded, marker.0, marker.1)),
            "orientation {orientation}: 标记像素应在 {marker:?}，实际 {}",
            pixel(&decoded, marker.0, marker.1).map(|b| b.to_string()).join(",")
        );
        // 分栏判定：分栏恰好在半线（红占 [0,16)），按"前半段是否全红"判方向。
        // 原尺寸类（1～4）沿第 10 行取前半段；交换类（5～8）沿第 10 列取前半段。
        let (red_first_half, half) = if width > height {
            let count = (0..width / 2).filter(|&x| is_red(pixel(&decoded, x, 10))).count();
            (count, width / 2 as u32)
        } else {
            let count = (0..height / 2).filter(|&y| is_red(pixel(&decoded, 10, y))).count();
            (count, height / 2 as u32)
        };
        // 前半段的期望：红在第一半 ⇒ 全红；红在第二半 ⇒ 全蓝（0 红）。
        let expected_red_first_half = if red_top_or_left { half } else { 0 };
        assert_eq!(
            red_first_half as u32, expected_red_first_half,
            "orientation {orientation}: 采样线前半段红样本 {red_first_half}/{half}，与期望不符"
        );
    }
}

/// 端到端：Orientation=6 的 iPhone 竖拍场景 → compress_image 输出 JPEG
/// 像素转正、不再携带 EXIF 方向标签。
#[tokio::test]
async fn an_orientation6_jpeg_compresses_upright_without_the_exif_tag() {
    let (_dir, _source, path) = fixture_with_orientation(6);
    let options = CompressOptions {
        quality: 80,
        output_format: "jpg".into(),
        ..Default::default()
    };
    let result = engine::compress_image(&path, &options).await;
    if !result.success {
        // 无收益跳过 = 交回原图字节：原图带 EXIF，这在跳过语义里是诚实的；
        // 但这条断言现场（q80 重编码 32×20 实心分栏）应当压得动。
        panic!("此现场应能产出更小的候选: {:?}", result.error);
    }
    assert_eq!(result.out_type, "jpg");
    // 产物不携带 EXIF 方向标签（APP1 "Exif" 段缺席）。
    let has_exif = result
        .compressed
        .windows(6)
        .any(|w| w == [0xFF, 0xE1, b'E', b'x', b'i', b'f']);
    assert!(!has_exif, "输出 JPEG 不许再携带 EXIF 方向标签");
    // 像素已转正：20×32、白标记（JPEG 色度抽样会晕开小标记，按亮度判）在右上、
    // 红栏在上半。
    let decoded = image::load_from_memory(&result.compressed).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (20, 32));
    let marker = pixel(&decoded, 19, 0);
    let luminance = (marker[0] as u32 + marker[1] as u32 + marker[2] as u32) / 3;
    assert!(
        luminance > 190,
        "标记像素应已烘到右上角（亮度 {luminance}）: {marker:?}"
    );
    let red_top = (0..10).filter(|&y| is_red(pixel(&decoded, 10, y))).count();
    let red_bottom = (22..32).filter(|&y| is_red(pixel(&decoded, 10, y))).count();
    assert!(
        red_top >= 8 && red_bottom == 0,
        "红栏应在上半（上 {red_top} / 下 {red_bottom}）"
    );
}
