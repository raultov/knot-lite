//! Declaration-name location heuristic and UTF-16 column conversions.
//!
//! LSP positions are 0-based lines with **UTF-16 code-unit** columns (the
//! client never negotiates UTF-8 offsets, P3-D5). knot's entity table only
//! stores line ranges, so the column of a declaration name has to be found in
//! the source. Phase 0 measured this heuristic at 957/958 = 99.9 % against the
//! servers' own `selectionRange` (G10).

use crate::capability::languages::LspLang;

/// Location of a declaration name in LSP coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamePos {
    /// 0-based line.
    pub line0: u32,
    /// 0-based column in UTF-16 code units.
    pub col_utf16: u32,
    /// 0-based column in bytes.
    pub byte_col: usize,
}

/// How far below `start_line` a declaration name may sit (generics,
/// annotations, multi-line signatures).
const MAX_NAME_LOOKAHEAD_LINES: usize = 15;

/// Locate the declaration name of an entity in `source`.
///
/// `start_line`/`end_line` are 1-based, as stored by knot. Returns `None` for
/// the synthetic `<module>` entity (not a failure) and when no occurrence
/// scores `>= 0`. Callers that locate many entities of one file should split
/// the source once and use [`locate_name_in_lines`].
pub fn locate_name(
    source: &str,
    name: &str,
    start_line: usize,
    end_line: usize,
    lang: LspLang,
) -> Option<NamePos> {
    let lines: Vec<&str> = source
        .lines()
        .take(last_line(start_line, end_line))
        .collect();
    locate_name_in_lines(&lines, name, start_line, end_line, lang)
}

/// [`locate_name`] over a source that is already split into lines.
pub fn locate_name_in_lines(
    lines: &[&str],
    name: &str,
    start_line: usize,
    end_line: usize,
    lang: LspLang,
) -> Option<NamePos> {
    if name == "<module>" || name.is_empty() || start_line == 0 {
        return None;
    }
    let mut best: Option<(i64, usize, usize)> = None;

    for (offset, line) in lines
        .iter()
        .enumerate()
        .take(last_line(start_line, end_line))
        .skip(start_line - 1)
    {
        if is_annotation_or_comment(line) && !line_has_ident(line, name) {
            continue;
        }
        for byte_col in ident_occurrences(line, name) {
            let score = score_occurrence(line, byte_col, name, lang);
            if score < 0 {
                continue;
            }
            // Best score wins; ties keep the earliest line, then column
            // (iteration order already provides that, so strict `>` suffices).
            if best.is_none_or(|(best_score, _, _)| score > best_score) {
                best = Some((score, offset, byte_col));
            }
        }
    }

    let (_, line_index, byte_col) = best?;
    let line = lines.get(line_index)?;
    Some(NamePos {
        line0: u32::try_from(line_index).ok()?,
        col_utf16: byte_col_to_utf16(line, byte_col),
        byte_col,
    })
}

/// Last 1-based line considered for an entity spanning `start_line..=end_line`.
fn last_line(start_line: usize, end_line: usize) -> usize {
    end_line.min(start_line.saturating_add(MAX_NAME_LOOKAHEAD_LINES))
}

/// UTF-16 code units a character occupies.
fn utf16_units(ch: char) -> u32 {
    if ch.len_utf16() == 2 { 2 } else { 1 }
}

/// UTF-16 column of `byte_col` within `line`.
pub fn byte_col_to_utf16(line: &str, byte_col: usize) -> u32 {
    line.char_indices()
        .take_while(|(idx, _)| *idx < byte_col)
        .map(|(_, ch)| utf16_units(ch))
        .sum()
}

/// Byte column of `col_utf16` within `line`; `None` when the column falls
/// inside a surrogate pair or past the end of the line.
pub fn utf16_to_byte_col(line: &str, col_utf16: u32) -> Option<usize> {
    let mut units = 0u32;
    for (idx, ch) in line.char_indices() {
        if units == col_utf16 {
            return Some(idx);
        }
        units += utf16_units(ch);
        if units > col_utf16 {
            return None;
        }
    }
    (units == col_utf16).then_some(line.len())
}

/// Comment / attribute / annotation lines never introduce a declaration name.
fn is_annotation_or_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["//", "/*", "*", "#[", "#![", "@"]
        .iter()
        .any(|marker| trimmed.starts_with(marker))
}

/// Whether `name` occurs on `line` with identifier boundaries.
fn line_has_ident(line: &str, name: &str) -> bool {
    ident_occurrences(line, name).next().is_some()
}

/// Byte offsets of `name` in `line` where the surrounding characters are not
/// identifier characters (`[A-Za-z0-9_$]`).
fn ident_occurrences<'a>(line: &'a str, name: &'a str) -> impl Iterator<Item = usize> + 'a {
    let mut search_from = 0;
    std::iter::from_fn(move || {
        let found = line[search_from..].find(name)? + search_from;
        let after = found + name.len();
        let boundary_before = line[..found]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_ident_char(ch));
        let boundary_after = line[after..]
            .chars()
            .next()
            .is_none_or(|ch| !is_ident_char(ch));
        search_from = after;
        if boundary_before && boundary_after {
            Some(found)
        } else {
            None
        }
    })
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}

/// Score one identifier occurrence.
fn score_occurrence(line: &str, byte_col: usize, name: &str, lang: LspLang) -> i64 {
    let before = &line[..byte_col];
    let after = &line[byte_col + name.len()..];
    let mut score = 0i64;

    if let Some(token) = before.trim_end().rsplit(char::is_whitespace).next()
        && declaration_keywords(lang).contains(&token)
    {
        score += 3;
    }

    let next = after.chars().find(|ch| !ch.is_whitespace());
    let preceded_by_dot = before.trim_end().ends_with('.');
    if matches!(next, Some('(' | '<')) && !preceded_by_dot {
        score += 2;
    }
    if matches!(next, Some(':' | '=')) {
        score += 1;
    }
    if in_string_or_trailing_comment(line, byte_col) {
        score -= 5;
    }
    score
}

fn declaration_keywords(lang: LspLang) -> &'static [&'static str] {
    match lang {
        LspLang::Rust => &[
            "fn",
            "struct",
            "enum",
            "trait",
            "type",
            "const",
            "static",
            "mod",
            "union",
            "macro_rules!",
        ],
        LspLang::Ts => &[
            "function",
            "class",
            "interface",
            "type",
            "enum",
            "const",
            "let",
            "var",
            "namespace",
            "async",
            "get",
            "set",
        ],
        LspLang::Java => &["class", "interface", "enum", "record", "@interface"],
    }
}

/// Cheap scan of `line[..byte_col]` for an unterminated string or a `//`
/// comment.
fn in_string_or_trailing_comment(line: &str, byte_col: usize) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = line[..byte_col].chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '/' if !in_string && chars.peek() == Some(&'/') => return true,
            _ => {}
        }
    }
    in_string
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos_of(source: &str, name: &str, start: usize, end: usize, lang: LspLang) -> NamePos {
        locate_name(source, name, start, end, lang).expect("name should be located")
    }

    #[test]
    fn rust_struct_after_derive() {
        let source = "#[derive(Debug)]\npub struct Foo {\n    x: u8,\n}\n";
        let pos = pos_of(source, "Foo", 2, 3, LspLang::Rust);
        assert_eq!(pos.line0, 1);
        assert_eq!(pos.byte_col, 11);
        assert_eq!(pos.col_utf16, 11);
    }

    #[test]
    fn rust_fn_with_doc_comments_above() {
        let source = "/// Docs mentioning bar in prose.\npub async fn bar<T>(x: T) {\n}\n";
        let pos = pos_of(source, "bar", 2, 3, LspLang::Rust);
        assert_eq!(pos.line0, 1);
        assert_eq!(pos.byte_col, 13);
    }

    #[test]
    fn rust_method_in_impl_block() {
        let source = "impl Trait for X {\n    fn baz(&self) {}\n}\n";
        let pos = pos_of(source, "baz", 2, 2, LspLang::Rust);
        assert_eq!(pos.line0, 1);
        assert_eq!(pos.byte_col, 7);
    }

    #[test]
    fn rust_body_call_site_loses_to_declaration() {
        let source = "impl Foo {\n    fn new() -> Self {\n        Self::new()\n    }\n}\n";
        let pos = pos_of(source, "new", 2, 4, LspLang::Rust);
        assert_eq!(pos.line0, 1);
        assert_eq!(pos.byte_col, 7);
    }

    #[test]
    fn java_annotations_on_previous_lines() {
        let source = "    @Override\n    public String call(String prompt) {\n    }\n";
        let pos = pos_of(source, "call", 2, 3, LspLang::Java);
        assert_eq!(pos.line0, 1);
        assert_eq!(pos.byte_col, 18);
    }

    #[test]
    fn java_generic_and_type_annotation_before_name() {
        let source = "    public static <T> @Nullable T mergeOption(T a, T b) {\n";
        let pos = pos_of(source, "mergeOption", 1, 1, LspLang::Java);
        assert_eq!(pos.line0, 0);
        assert_eq!(pos.byte_col, 34);
        assert_eq!(pos.col_utf16, 34);
    }

    #[test]
    fn java_self_call_in_default_method_loses() {
        let source =
            "    default String call(String prompt) {\n        return call(prompt);\n    }\n";
        let pos = pos_of(source, "call", 1, 3, LspLang::Java);
        assert_eq!(pos.line0, 0);
        assert_eq!(pos.byte_col, 19);
    }

    #[test]
    fn ts_exported_function() {
        let source = "export function jsonText(value: unknown): string {\n";
        let pos = pos_of(source, "jsonText", 1, 1, LspLang::Ts);
        assert_eq!(pos.line0, 0);
        assert_eq!(pos.byte_col, 16);
    }

    #[test]
    fn ts_exported_const_object() {
        let source = "export const router = {\n  page: '',\n};\n";
        let pos = pos_of(source, "router", 1, 1, LspLang::Ts);
        assert_eq!(pos.byte_col, 13);
    }

    #[test]
    fn ts_object_method() {
        let source = "const store = {\n  subscribe(listener) {\n    return () => {};\n  },\n};\n";
        let pos = pos_of(source, "subscribe", 2, 2, LspLang::Ts);
        assert_eq!(pos.line0, 1);
        assert_eq!(pos.byte_col, 2);
    }

    #[test]
    fn ts_exported_interface() {
        let source = "export interface ToolDefinition {\n  name: string;\n}\n";
        let pos = pos_of(source, "ToolDefinition", 1, 1, LspLang::Ts);
        assert_eq!(pos.byte_col, 17);
    }

    #[test]
    fn utf16_column_counts_code_units_not_bytes() {
        // "ñ" is 2 bytes / 1 UTF-16 unit; the emoji is 4 bytes / 2 UTF-16 units.
        let source = "const s = \"ñ\"; const t = \"😀\"; function name() {}\n";
        let pos = pos_of(source, "name", 1, 1, LspLang::Ts);
        let line = source.lines().next().expect("one line");
        assert_eq!(pos.byte_col, line.find("name").expect("name present"));
        assert_eq!(pos.col_utf16, byte_col_to_utf16(line, pos.byte_col));
        assert_ne!(usize::try_from(pos.col_utf16).expect("small"), pos.byte_col);
    }

    #[test]
    fn module_entity_is_skipped() {
        assert!(locate_name("mod a;\n", "<module>", 1, 1, LspLang::Rust).is_none());
    }

    #[test]
    fn occurrences_in_strings_and_comments_score_negative() {
        let source = "let x = \"parse\"; // parse\nfn parse() {}\n";
        let pos = pos_of(source, "parse", 1, 2, LspLang::Rust);
        assert_eq!(pos.line0, 1);
    }

    #[test]
    fn no_nonnegative_occurrence_returns_none() {
        // The only occurrence is inside a trailing comment.
        assert!(locate_name("let x = 1; // parse\n", "parse", 1, 1, LspLang::Rust).is_none());
    }

    #[test]
    fn utf16_conversions_round_trip() {
        let line = "a😀b";
        assert_eq!(byte_col_to_utf16(line, 0), 0);
        assert_eq!(byte_col_to_utf16(line, 1), 1);
        assert_eq!(byte_col_to_utf16(line, 5), 3);
        assert_eq!(utf16_to_byte_col(line, 0), Some(0));
        assert_eq!(utf16_to_byte_col(line, 1), Some(1));
        assert_eq!(utf16_to_byte_col(line, 2), None); // inside the surrogate pair
        assert_eq!(utf16_to_byte_col(line, 3), Some(5));
        assert_eq!(utf16_to_byte_col(line, 4), Some(line.len())); // end of line
        assert_eq!(utf16_to_byte_col(line, 5), None); // past the end
    }

    #[test]
    fn start_line_zero_is_not_located() {
        assert!(locate_name("fn a() {}\n", "a", 0, 1, LspLang::Rust).is_none());
    }

    #[test]
    fn name_beyond_the_lookahead_window_is_not_located() {
        let mut source = "\n".repeat(20);
        source.push_str("fn far() {}\n");
        assert!(locate_name(&source, "far", 1, 40, LspLang::Rust).is_none());
        assert!(locate_name(&source, "far", 6, 40, LspLang::Rust).is_some());
    }

    #[test]
    fn pre_split_lines_give_the_same_answer() {
        let source = "#[derive(Debug)]\npub struct Foo {\n    x: u8,\n}\n";
        let lines: Vec<&str> = source.lines().collect();
        assert_eq!(
            locate_name_in_lines(&lines, "Foo", 2, 3, LspLang::Rust),
            locate_name(source, "Foo", 2, 3, LspLang::Rust)
        );
    }
}
