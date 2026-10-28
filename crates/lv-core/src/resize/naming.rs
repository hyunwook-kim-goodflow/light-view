//! The names a resized photo is saved under: a new file beside the original
//! (`<stem>_small.<ext>`, then `<stem>_small (2).<ext>`, `(3)`…), or the original's own
//! name with the output format's extension when it replaces the original.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::OutputFormat;

/// The extension a new file of `format` gets, without the dot.
fn extension(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Jpeg => "jpg",
        OutputFormat::Png => "png",
        OutputFormat::WebP => "webp",
    }
}

/// The extensions (lowercase, without the dot) that already name `format`.
fn same_format_extensions(format: OutputFormat) -> &'static [&'static str] {
    match format {
        OutputFormat::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
        OutputFormat::Png => &["png"],
        OutputFormat::WebP => &["webp"],
    }
}

/// The first free name for a new file made from `src` in its folder:
/// `<stem>_small.<ext>`, else `<stem>_small (2).<ext>`, `(3)`… `exists` tells whether a
/// path is taken.
pub fn new_file_name(src: &Path, format: OutputFormat, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let stem = src.file_stem().unwrap_or_default();
    let ext = extension(format);
    let name = |n: u32| {
        let mut name = OsString::from(stem);
        name.push("_small");
        if n > 1 {
            name.push(format!(" ({n})"));
        }
        name.push(".");
        name.push(ext);
        src.with_file_name(name)
    };
    (1..)
        .map(name)
        .find(|p| !exists(p))
        .expect("some numbered name is free")
}

/// Where the result goes when it replaces `src`: `src` itself when its extension already
/// names `format` (any case), else `src` with the format's extension (`IMG.HEIC` →
/// `IMG.jpg`).
pub fn replace_target(src: &Path, format: OutputFormat) -> PathBuf {
    let same = src.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        same_format_extensions(format)
            .iter()
            .any(|s| s.eq_ignore_ascii_case(e))
    });
    if same {
        src.to_path_buf()
    } else {
        src.with_extension(extension(format))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn taken<'a>(names: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |p: &Path| {
            names
                .iter()
                .any(|n| p.file_name().is_some_and(|f| f.eq_ignore_ascii_case(n)))
        }
    }

    #[test]
    fn new_file_name_counts_up() {
        let src = Path::new(r"C:\photos\a.jpg");
        let none = taken(&[]);
        assert_eq!(
            new_file_name(src, OutputFormat::Jpeg, &none),
            Path::new(r"C:\photos\a_small.jpg")
        );
        let one = taken(&["a.jpg", "a_small.jpg"]);
        assert_eq!(
            new_file_name(src, OutputFormat::Jpeg, &one),
            Path::new(r"C:\photos\a_small (2).jpg")
        );
        let two = taken(&["a.jpg", "a_small.jpg", "a_small (2).jpg"]);
        assert_eq!(
            new_file_name(src, OutputFormat::Jpeg, &two),
            Path::new(r"C:\photos\a_small (3).jpg")
        );
    }

    #[test]
    fn new_file_name_uses_the_format_extension() {
        let src = Path::new(r"C:\photos\IMG_0001.HEIC");
        let none = taken(&[]);
        assert_eq!(
            new_file_name(src, OutputFormat::Png, &none),
            Path::new(r"C:\photos\IMG_0001_small.png")
        );
        assert_eq!(
            new_file_name(src, OutputFormat::WebP, &none),
            Path::new(r"C:\photos\IMG_0001_small.webp")
        );
        // The stem keeps its inner dots.
        assert_eq!(
            new_file_name(Path::new("x.y.jpeg"), OutputFormat::Jpeg, &none),
            Path::new("x.y_small.jpg")
        );
    }

    #[test]
    fn new_file_name_asks_in_order() {
        let asked = RefCell::new(Vec::new());
        let exists = |p: &Path| {
            asked.borrow_mut().push(p.to_path_buf());
            asked.borrow().len() < 3
        };
        let got = new_file_name(Path::new("a.png"), OutputFormat::Png, &exists);
        assert_eq!(got, Path::new("a_small (3).png"));
        assert_eq!(
            *asked.borrow(),
            ["a_small.png", "a_small (2).png", "a_small (3).png"].map(PathBuf::from)
        );
    }

    #[test]
    fn replace_target_changes_only_the_extension() {
        let heic = Path::new(r"C:\photos\IMG.HEIC");
        assert_eq!(
            replace_target(heic, OutputFormat::Jpeg),
            Path::new(r"C:\photos\IMG.jpg")
        );
        assert_eq!(
            replace_target(heic, OutputFormat::WebP),
            Path::new(r"C:\photos\IMG.webp")
        );
        assert_eq!(
            replace_target(Path::new("a.jpg"), OutputFormat::Png),
            Path::new("a.png")
        );
    }

    #[test]
    fn replace_target_keeps_a_same_format_name() {
        for name in ["a.jpg", "a.JPG", "a.jpeg", "a.JPE", "a.jfif"] {
            assert_eq!(
                replace_target(Path::new(name), OutputFormat::Jpeg),
                Path::new(name)
            );
        }
        assert_eq!(
            replace_target(Path::new("b.PNG"), OutputFormat::Png),
            Path::new("b.PNG")
        );
        assert_eq!(
            replace_target(Path::new("c.WebP"), OutputFormat::WebP),
            Path::new("c.WebP")
        );
    }
}
