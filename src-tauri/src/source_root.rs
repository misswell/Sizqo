//! 源根目录 → 相对路径只有这一份实现。
//!
//! 图片线和视频线都要回答「这个文件在导入的哪个根目录下、相对位置是什么」。
//! 各写一份就等于允许同一次导入在两条线上落到不同的子目录（§39 / §40）。
use std::path::{Path, PathBuf};

/// 最深的、真正包住 `file` 的那个来源根目录。
///
/// `roots` 是导入时冻结下来的来源根，不是每批任务重新攒出来的候选集：
/// 传进来的根不存在或不是目录时它不参与匹配，绝不按文件名去猜。
pub(crate) fn relative_to_roots(file: &Path, roots: &[String]) -> Option<PathBuf> {
    let canonical_file = file.canonicalize().ok()?;
    roots
        .iter()
        .filter_map(|root| {
            let canonical_root = Path::new(root).canonicalize().ok()?;
            if !canonical_root.is_dir() {
                return None;
            }
            let relative = canonical_file.strip_prefix(&canonical_root).ok()?;
            Some((canonical_root.components().count(), relative.to_path_buf()))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, relative)| relative)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_deepest_frozen_root_wins() {
        let temp = tempfile::tempdir().unwrap();
        let outer = temp.path().join("photos");
        let inner = outer.join("2026");
        fs::create_dir_all(&inner).unwrap();
        let file = inner.join("a.jpg");
        fs::write(&file, b"x").unwrap();
        let roots = vec![
            outer.to_string_lossy().into_owned(),
            inner.to_string_lossy().into_owned(),
        ];
        assert_eq!(
            relative_to_roots(&file, &roots),
            Some(PathBuf::from("a.jpg")),
            "同一批里给了两个根时，离文件最近的那个才是它的来源根"
        );
        assert_eq!(
            relative_to_roots(&file, &[outer.to_string_lossy().into_owned()]),
            Some(PathBuf::from("2026/a.jpg"))
        );
    }

    #[test]
    fn a_file_outside_every_root_has_no_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let outside = temp.path().join("elsewhere.jpg");
        fs::write(&outside, b"x").unwrap();
        assert_eq!(relative_to_roots(&outside, &[root.to_string_lossy().into_owned()]), None);
        assert_eq!(relative_to_roots(&outside, &[]), None, "没有根就没有相对路径，不能退回文件名充数");
    }

    #[test]
    fn a_root_that_is_a_file_or_missing_never_matches() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("a.jpg");
        fs::write(&file, b"x").unwrap();
        let missing = temp.path().join("gone");
        assert_eq!(
            relative_to_roots(&file, &[file.to_string_lossy().into_owned(), missing.to_string_lossy().into_owned()]),
            None
        );
    }
}
