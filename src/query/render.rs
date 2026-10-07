//! One renderer for every query result (the dupes ratchet's lesson from the
//! `status` slice): each tool builds a format-agnostic [`RenderDoc`] from its
//! JSON document and [`render`] turns it into a table, markdown or JSON.

use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table};

use crate::config::OutputFormat;

/// Text shown for a section without rows (table output).
const NO_RESULTS_TABLE: &str = "(no results)";
/// Text shown for a section without rows (markdown output).
const NO_RESULTS_MARKDOWN: &str = "_No results._";

/// One table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Column {
    /// Header text.
    pub name: &'static str,
    /// Right-aligned (numbers).
    pub right: bool,
}

impl Column {
    /// A left-aligned column.
    pub const fn left(name: &'static str) -> Self {
        Self { name, right: false }
    }

    /// A right-aligned column.
    pub const fn right(name: &'static str) -> Self {
        Self { name, right: true }
    }
}

/// A titled table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderSection {
    /// Optional heading above the table.
    pub heading: Option<String>,
    /// Optional line between the heading and the table.
    pub caption: Option<String>,
    /// Column definitions.
    pub columns: Vec<Column>,
    /// Rows; every row has one cell per column.
    pub rows: Vec<Vec<String>>,
}

/// A whole result, independent of the output format.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderDoc {
    /// Title line (`knot-lite search — repository 'x'`).
    pub title: String,
    /// Plain lines under the title.
    pub summary: Vec<String>,
    /// Tables.
    pub sections: Vec<RenderSection>,
    /// Footnotes: `note: …` lines (table) or `- ` bullets (markdown).
    pub notes: Vec<String>,
}

/// Render `doc` in `format`. `json` is the source document, printed as is for
/// [`OutputFormat::Json`].
pub fn render(doc: &RenderDoc, json: &serde_json::Value, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(json).unwrap_or_default(),
        OutputFormat::Table => render_table(doc),
        OutputFormat::Markdown => render_markdown(doc),
    }
}

/// The string at the nested `keys` path of a JSON document.
pub(crate) fn str_at<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .try_fold(value, |acc, key| acc.get(key))
        .and_then(|v| v.as_str())
}

/// The unsigned integer at the nested `keys` path of a JSON document.
pub(crate) fn u64_at(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .try_fold(value, |acc, key| acc.get(key))
        .and_then(|v| v.as_u64())
}

/// Make `text` safe inside a markdown table cell: `|` is escaped and line
/// breaks become spaces.
pub fn md_cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn render_table(doc: &RenderDoc) -> String {
    let mut out = String::new();
    out.push_str(&doc.title);
    out.push('\n');
    for line in &doc.summary {
        out.push_str(line);
        out.push('\n');
    }
    for section in &doc.sections {
        out.push('\n');
        if let Some(heading) = &section.heading {
            out.push_str(heading);
            out.push('\n');
        }
        if let Some(caption) = &section.caption {
            out.push_str(caption);
            out.push('\n');
        }
        if section.rows.is_empty() {
            out.push_str(NO_RESULTS_TABLE);
        } else {
            out.push_str(&comfy_table(section));
        }
        out.push('\n');
    }
    for note in &doc.notes {
        out.push_str("note: ");
        out.push_str(note);
        out.push('\n');
    }
    out
}

fn comfy_table(section: &RenderSection) -> String {
    let mut table = Table::new();
    table.set_content_arrangement(ContentArrangement::DynamicFullWidth);
    table.set_header(
        section
            .columns
            .iter()
            .map(|column| {
                Cell::new(column.name)
                    .set_alignment(alignment(column))
                    .fg(Color::Cyan)
                    .add_attribute(Attribute::Bold)
            })
            .collect::<Vec<_>>(),
    );
    for row in &section.rows {
        table.add_row(
            row.iter()
                .zip(&section.columns)
                .map(|(cell, column)| Cell::new(cell).set_alignment(alignment(column)))
                .collect::<Vec<_>>(),
        );
    }
    table.to_string()
}

fn alignment(column: &Column) -> CellAlignment {
    if column.right {
        CellAlignment::Right
    } else {
        CellAlignment::Left
    }
}

fn render_markdown(doc: &RenderDoc) -> String {
    let mut out = format!("## {}\n\n", doc.title);
    for line in &doc.summary {
        out.push_str(line);
        out.push('\n');
    }
    if !doc.summary.is_empty() {
        out.push('\n');
    }
    for section in &doc.sections {
        if let Some(heading) = &section.heading {
            out.push_str(&format!("### {heading}\n\n"));
        }
        if let Some(caption) = &section.caption {
            out.push_str(&format!("{caption}\n\n"));
        }
        if section.rows.is_empty() {
            out.push_str(NO_RESULTS_MARKDOWN);
            out.push_str("\n\n");
        } else {
            out.push_str(&markdown_table(section));
            out.push('\n');
        }
    }
    for note in &doc.notes {
        out.push_str("- ");
        out.push_str(note);
        out.push('\n');
    }
    out
}

fn markdown_table(section: &RenderSection) -> String {
    let header: Vec<&str> = section.columns.iter().map(|c| c.name).collect();
    let rule: Vec<&str> = section
        .columns
        .iter()
        .map(|c| if c.right { "---:" } else { "---" })
        .collect();
    let mut out = format!("| {} |\n| {} |\n", header.join(" | "), rule.join(" | "));
    for row in &section.rows {
        let cells: Vec<String> = row.iter().map(|cell| md_cell(cell)).collect();
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(rows: Vec<Vec<String>>) -> RenderDoc {
        RenderDoc {
            title: "knot-lite demo — repository 'x'".to_owned(),
            summary: vec!["query: hello".to_owned()],
            sections: vec![RenderSection {
                caption: None,
                heading: Some("Results".to_owned()),
                columns: vec![Column::right("#"), Column::left("NAME")],
                rows,
            }],
            notes: vec!["be careful".to_owned()],
        }
    }

    fn rows() -> Vec<Vec<String>> {
        vec![
            vec!["1".to_owned(), "alpha".to_owned()],
            vec!["2".to_owned(), "be|ta\nx".to_owned()],
        ]
    }

    #[test]
    fn json_is_the_source_document_pretty_printed() {
        let json = serde_json::json!({"a": 1});
        assert_eq!(
            render(&doc(rows()), &json, OutputFormat::Json),
            "{\n  \"a\": 1\n}"
        );
    }

    #[test]
    fn table_has_title_summary_headers_rows_and_notes() {
        let text = render(&doc(rows()), &serde_json::Value::Null, OutputFormat::Table);
        assert!(
            text.starts_with("knot-lite demo — repository 'x'\nquery: hello\n"),
            "{text}"
        );
        for needle in ["Results", "NAME", "alpha", "be|ta"] {
            assert!(text.contains(needle), "{needle} missing in {text}");
        }
        assert!(text.ends_with("note: be careful\n"), "{text}");
    }

    #[test]
    fn markdown_has_heading_alignment_rule_and_bullets() {
        let text = render(
            &doc(rows()),
            &serde_json::Value::Null,
            OutputFormat::Markdown,
        );
        assert!(
            text.starts_with(
                "## knot-lite demo — repository 'x'\n\nquery: hello\n\n### Results\n\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("| # | NAME |\n| ---: | --- |\n| 1 | alpha |\n"),
            "{text}"
        );
        assert!(text.ends_with("- be careful\n"), "{text}");
    }

    #[test]
    fn render_escapes_pipes_in_markdown() {
        let text = render(
            &doc(rows()),
            &serde_json::Value::Null,
            OutputFormat::Markdown,
        );
        assert!(text.contains("| 2 | be\\|ta x |"), "{text}");
        assert_eq!(md_cell("a|b\r\nc"), "a\\|b  c");
    }

    #[test]
    fn empty_sections_say_so() {
        let empty = doc(Vec::new());
        let table = render(&empty, &serde_json::Value::Null, OutputFormat::Table);
        assert!(
            table.contains("(no results)") && !table.contains("NAME"),
            "{table}"
        );
        let markdown = render(&empty, &serde_json::Value::Null, OutputFormat::Markdown);
        assert!(
            markdown.contains("_No results._") && !markdown.contains("| NAME |"),
            "{markdown}"
        );
    }

    #[test]
    fn sections_without_heading_and_docs_without_summary_render_cleanly() {
        let bare = RenderDoc {
            title: "t".to_owned(),
            summary: Vec::new(),
            sections: vec![RenderSection {
                caption: None,
                heading: None,
                columns: vec![Column::left("A")],
                rows: vec![vec!["x".to_owned()]],
            }],
            notes: Vec::new(),
        };
        let markdown = render(&bare, &serde_json::Value::Null, OutputFormat::Markdown);
        assert_eq!(markdown, "## t\n\n| A |\n| --- |\n| x |\n\n");
        let table = render(&bare, &serde_json::Value::Null, OutputFormat::Table);
        assert!(table.starts_with("t\n\n"), "{table}");
    }
}
