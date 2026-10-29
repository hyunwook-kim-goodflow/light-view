//! The source boundary of the details handler (test only): every path its product code
//! names must be on [`ALLOWED`]. Any new import or fully qualified path fails the test until
//! someone adds it here on purpose, so a way to a worker, a thread, a process, a file or C
//! cannot creep in by accident.

/// The paths the details code (`properties.rs`, `properties/read.rs`) and the stream reader
/// it uses (`stream.rs`) may name: in `use` declarations (expanded) and written out in code.
/// Only items, never whole modules (a module would let anything under it through).
pub(super) const ALLOWED: &[&str] = &[
    // std
    "std::ops::Range",
    "std::sync::Mutex",
    "std::sync::MutexGuard",
    // this crate: the panic guard, the object count, the capped stream read
    "crate::guarded",
    "crate::ObjectCount",
    "crate::stream::read_capped",
    "crate::stream::READ_CAP",
    // the pure parsing of lv-core
    "lv_core::exif_info::camera_facts",
    "lv_core::heif_edit::display_size",
    // the details module's own pure part
    "read::heif_properties",
    "read::DateTaken",
    "read::HeifProps",
    // COM, PROPVARIANT and time conversion
    "windows::core::implement",
    "windows::core::Result",
    "windows::core::PROPVARIANT",
    "windows::Win32::Foundation::ERROR_ALREADY_INITIALIZED",
    "windows::Win32::Foundation::E_FAIL",
    "windows::Win32::Foundation::E_INVALIDARG",
    "windows::Win32::Foundation::E_POINTER",
    "windows::Win32::Foundation::FILETIME",
    "windows::Win32::Foundation::STG_E_ACCESSDENIED",
    "windows::Win32::Foundation::S_FALSE",
    "windows::Win32::Foundation::SYSTEMTIME",
    "windows::Win32::Storage::EnhancedStorage::PKEY_Image_Dimensions",
    "windows::Win32::Storage::EnhancedStorage::PKEY_Image_HorizontalSize",
    "windows::Win32::Storage::EnhancedStorage::PKEY_Image_VerticalSize",
    "windows::Win32::Storage::EnhancedStorage::PKEY_Photo_CameraManufacturer",
    "windows::Win32::Storage::EnhancedStorage::PKEY_Photo_CameraModel",
    "windows::Win32::Storage::EnhancedStorage::PKEY_Photo_DateTaken",
    "windows::Win32::System::Com::IStream",
    "windows::Win32::System::Com::STATFLAG_NONAME",
    "windows::Win32::System::Com::STATSTG",
    "windows::Win32::System::Com::STGM_READWRITE",
    "windows::Win32::System::Com::STGM_WRITE",
    "windows::Win32::System::Com::STREAM_SEEK_SET",
    "windows::Win32::System::Com::StructuredStorage::InitPropVariantFromFileTime",
    "windows::Win32::System::Com::StructuredStorage::PropVariantChangeType",
    "windows::Win32::System::Com::StructuredStorage::PROPVAR_CHANGE_FLAGS",
    "windows::Win32::System::Time::FileTimeToSystemTime",
    "windows::Win32::System::Time::SystemTimeToFileTime",
    "windows::Win32::System::Time::TzSpecificLocalTimeToSystemTime",
    "windows::Win32::System::Variant::VT_LPWSTR",
    "windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream",
    "windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream_Impl",
    "windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore",
    "windows::Win32::UI::Shell::PropertiesSystem::IPropertyStoreCapabilities",
    "windows::Win32::UI::Shell::PropertiesSystem::IPropertyStoreCapabilities_Impl",
    "windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore_Impl",
    "windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY",
];

/// Words that are not paths but would reach outside: foreign code, inline assembly, other
/// source files, `#[path]` modules.
const FORBIDDEN_WORDS: &[&str] = &[
    "extern",
    "asm!",
    "#[link",
    "include",
    "#[path",
    "macro_rules!",
];

/// Primitive types, whose associated functions (`u32::from`) are not paths to anything.
const PRIMITIVES: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32",
    "f64", "bool", "char", "str",
];

/// What in `source` is outside the boundary: paths not on [`ALLOWED`] and forbidden words.
pub(super) fn violations(source: &str) -> Vec<String> {
    let code = product_code(source);
    let mut out: Vec<String> = FORBIDDEN_WORDS
        .iter()
        .filter(|w| code.contains(*w))
        .map(|w| format!("forbidden word {w}"))
        .collect();
    let (uses, rest) = take_uses(&code);
    for path in uses {
        if !ALLOWED.contains(&path.as_str()) {
            out.push(format!("use {path}"));
        }
    }
    for path in written_paths(&rest) {
        let first = path.split("::").next().unwrap_or_default();
        // A type's associated item (`Value::Count`, `PROPVARIANT::from`) is reached through
        // an import, which is checked; a primitive's is harmless. Anything starting with a
        // module or crate name must be listed (an allowed item, or below one).
        let starts_with_module = first.starts_with(|c: char| c.is_ascii_lowercase());
        if !starts_with_module || PRIMITIVES.contains(&first) {
            continue;
        }
        let listed = ALLOWED
            .iter()
            .any(|ok| path == *ok || path.starts_with(&format!("{ok}::")));
        if !listed {
            out.push(format!("path {path}"));
        }
    }
    out
}

/// The paths `source`'s product code names: its `use` paths, expanded, and the paths it
/// writes out (to check the list holds nothing unused).
pub(super) fn used_paths(source: &str) -> Vec<String> {
    let (mut uses, rest) = take_uses(&product_code(source));
    uses.extend(written_paths(&rest));
    uses
}

/// `source` without comments, string and char literal contents, and `#[cfg(test)]` items
/// (wherever they are: product code after a test module is still scanned).
fn product_code(source: &str) -> String {
    let c: Vec<char> = source.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        match (c[i], c.get(i + 1).copied()) {
            ('/', Some('/')) => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ('"', _) => {
                out.push_str("\"\"");
                i += 1;
                while i < c.len() && c[i] != '"' {
                    i += if c[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
            }
            // A char literal ('x' or '\x..'), not a lifetime ('a).
            ('\'', Some(next)) if next == '\\' || c.get(i + 2) == Some(&'\'') => {
                out.push_str("' '");
                i += 2;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
                i += 1;
            }
            (ch, _) => {
                out.push(ch);
                i += 1;
            }
        }
    }
    while let Some(at) = out.find("#[cfg(test)]") {
        let item = at + "#[cfg(test)]".len();
        let end = match out[item..].find([';', '{']).map(|o| item + o) {
            Some(semi) if out.as_bytes()[semi] == b';' => semi + 1,
            Some(open) => matching_brace(&out, open).map_or(out.len(), |close| close + 1),
            None => out.len(),
        };
        out.replace_range(at..end, "");
    }
    out
}

/// The index of the `}` closing the `{` at `open`.
fn matching_brace(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, b) in code.bytes().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The paths of every `use` declaration in `code` (trees expanded, aliases dropped), and
/// `code` without them.
fn take_uses(code: &str) -> (Vec<String>, String) {
    let mut paths = Vec::new();
    let mut rest = String::new();
    let mut at = 0;
    while let Some(found) = code[at..].find("use").map(|o| at + o) {
        let before = code[..found].chars().next_back();
        let after = code[found + 3..].chars().next();
        if before.is_some_and(is_ident) || !after.is_some_and(char::is_whitespace) {
            rest.push_str(&code[at..found + 3]);
            at = found + 3;
            continue;
        }
        let end = code[found..].find(';').map_or(code.len(), |o| found + o);
        // Whitespace only matters around `as`; none is left around `::`, `{`, `}` or `,`.
        let mut tree: String = code[found + 3..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for (spaced, tight) in [(" ::", "::"), (":: ", "::"), (" {", "{"), ("{ ", "{")] {
            tree = tree.replace(spaced, tight);
        }
        for (spaced, tight) in [(" }", "}"), (" ,", ","), (", ", ",")] {
            tree = tree.replace(spaced, tight);
        }
        expand("", &tree, &mut paths);
        rest.push_str(&code[at..found]);
        at = (end + 1).min(code.len());
    }
    rest.push_str(&code[at..]);
    (paths, rest)
}

/// Expands the use tree `tree` under `prefix` into `out` (`x as y` keeps `x`).
fn expand(prefix: &str, tree: &str, out: &mut Vec<String>) {
    let tree = tree.trim();
    let Some(open) = tree.find('{') else {
        let leaf = tree.split_whitespace().next().unwrap_or_default();
        let path = if leaf == "self" {
            prefix.trim_end_matches("::").to_string()
        } else {
            format!("{prefix}{leaf}")
        };
        out.push(path.trim_start_matches("::").to_string());
        return;
    };
    let head = &tree[..open];
    let close = tree.rfind('}').unwrap_or(tree.len());
    let inner = &tree[open + 1..close];
    let mut depth = 0;
    let mut start = 0;
    for (i, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                if !inner[start..i].trim().is_empty() {
                    expand(&format!("{prefix}{head}"), &inner[start..i], out);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if !inner[start..].trim().is_empty() {
        expand(&format!("{prefix}{head}"), &inner[start..], out);
    }
}

/// Every `a::b..` path written in `code` (a trailing `::<` turbofish is not part).
fn written_paths(code: &str) -> Vec<String> {
    let c: Vec<char> = code.chars().collect();
    let mut paths = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let starts = (is_ident(c[i]) || c[i] == ':') && (i == 0 || !is_ident(c[i - 1]));
        if !starts {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < c.len() && (is_ident(c[j]) || (c[j] == ':' && c.get(j + 1) == Some(&':'))) {
            j += if c[j] == ':' { 2 } else { 1 };
        }
        let path: String = c[i..j].iter().collect();
        let path = path.trim_start_matches("::").trim_end_matches("::");
        if path.contains("::") {
            paths.push(path.to_string());
        }
        i = j.max(i + 1);
    }
    paths
}
