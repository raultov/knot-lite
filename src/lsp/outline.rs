//! `textDocument/documentSymbol` answers, flattened into one list (P5.11):
//! the outline `explore_file` shows for a file the index does not describe.

use async_lsp::lsp_types::{DocumentSymbol, DocumentSymbolResponse, SymbolKind};

/// One symbol of a document, positions as the server sent them (0-based lines,
/// UTF-16 columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineSymbol {
    /// The declared name ([`declared_name`] of what the server sent).
    pub name: String,
    /// The server's symbol kind.
    pub kind: SymbolKind,
    /// First line of the symbol's full range.
    pub first_line0: u32,
    /// Last line of the symbol's full range.
    pub last_line0: u32,
    /// Line of the symbol's name.
    pub selection_line0: u32,
    /// UTF-16 column of the symbol's name.
    pub selection_col: u32,
    /// Declared name of the enclosing symbol, when there is one.
    pub container: Option<String>,
}

/// The declared name inside a server's symbol name: jdtls names methods
/// `name(Type)`, tsserver and rust-analyzer may add generics or a type, so the
/// name is what comes before the first `(`, `<`, space or `:`.
pub fn declared_name(raw: &str) -> &str {
    raw.split(['(', '<', ' ', ':']).next().unwrap_or_default()
}

/// Every symbol of `response`, parents before their children, in the
/// server's order. The flat form (`SymbolInformation`) has no selection range:
/// the start of the symbol's range stands in for it.
pub fn flatten_outline(response: DocumentSymbolResponse) -> Vec<OutlineSymbol> {
    let mut out = Vec::new();
    match response {
        DocumentSymbolResponse::Nested(symbols) => flatten_nested(&symbols, None, &mut out),
        DocumentSymbolResponse::Flat(symbols) => {
            out.extend(symbols.into_iter().map(|symbol| {
                let range = symbol.location.range;
                OutlineSymbol {
                    name: declared_name(&symbol.name).to_owned(),
                    kind: symbol.kind,
                    first_line0: range.start.line,
                    last_line0: range.end.line,
                    selection_line0: range.start.line,
                    selection_col: range.start.character,
                    container: symbol
                        .container_name
                        .map(|name| declared_name(&name).to_owned()),
                }
            }));
        }
    }
    out
}

fn flatten_nested(
    symbols: &[DocumentSymbol],
    container: Option<&str>,
    out: &mut Vec<OutlineSymbol>,
) {
    for symbol in symbols {
        let name = declared_name(&symbol.name).to_owned();
        out.push(OutlineSymbol {
            name: name.clone(),
            kind: symbol.kind,
            first_line0: symbol.range.start.line,
            last_line0: symbol.range.end.line,
            selection_line0: symbol.selection_range.start.line,
            selection_col: symbol.selection_range.start.character,
            container: container.map(str::to_owned),
        });
        flatten_nested(
            symbol.children.as_deref().unwrap_or_default(),
            Some(&name),
            out,
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use async_lsp::lsp_types::{DocumentSymbol, SymbolKind};
    use serde_json::json;

    /// A nested symbol named `name` spanning lines `l0..=l1` (0-based), its
    /// name at columns 4–7 of `l0`. Built from JSON: the struct's deprecated
    /// `deprecated` field cannot be named without the `deprecated` lint.
    pub(crate) fn symbol(
        name: &str,
        kind: SymbolKind,
        (l0, l1): (u32, u32),
        children: Vec<DocumentSymbol>,
    ) -> DocumentSymbol {
        serde_json::from_value(json!({
            "name": name,
            "kind": kind,
            "range": {"start": {"line": l0, "character": 0}, "end": {"line": l1, "character": 1}},
            "selectionRange": {"start": {"line": l0, "character": 4}, "end": {"line": l0, "character": 7}},
            "children": (!children.is_empty()).then_some(children),
        }))
        .expect("document symbol")
    }
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::SymbolInformation;

    use super::test_support::symbol;
    use super::*;

    #[test]
    fn symbol_names_strip_params_and_generics() {
        assert_eq!(declared_name("total(List<Item>)"), "total");
        assert_eq!(declared_name("Repository<T>"), "Repository");
        assert_eq!(declared_name("count: number"), "count");
        assert_eq!(declared_name("impl Display for Widget"), "impl");
        assert_eq!(declared_name("plain"), "plain");
    }

    #[test]
    fn hierarchical_symbols_flatten_with_containers() {
        let nested = DocumentSymbolResponse::Nested(vec![symbol(
            "Shop",
            SymbolKind::CLASS,
            (0, 9),
            vec![
                symbol("total(List<Item>)", SymbolKind::METHOD, (2, 4), vec![]),
                symbol("name", SymbolKind::FIELD, (6, 6), vec![]),
            ],
        )]);
        let flat = flatten_outline(nested);
        let names: Vec<(&str, Option<&str>)> = flat
            .iter()
            .map(|s| (s.name.as_str(), s.container.as_deref()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Shop", None),
                ("total", Some("Shop")),
                ("name", Some("Shop"))
            ]
        );
        assert_eq!((flat[1].first_line0, flat[1].last_line0), (2, 4));
        assert_eq!((flat[1].selection_line0, flat[1].selection_col), (2, 4));
        assert_eq!(flat[1].kind, SymbolKind::METHOD);
    }

    #[test]
    fn flat_symbol_information_uses_the_range_start() {
        let information: SymbolInformation = serde_json::from_value(serde_json::json!({
            "name": "format(name)",
            "kind": SymbolKind::FUNCTION,
            "location": {
                "uri": "file:///work/a.ts",
                "range": {"start": {"line": 3, "character": 2}, "end": {"line": 5, "character": 1}},
            },
            "containerName": "util",
        }))
        .expect("symbol information");
        let flat = DocumentSymbolResponse::Flat(vec![information]);
        let symbols = flatten_outline(flat);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "format");
        assert_eq!(
            (symbols[0].selection_line0, symbols[0].selection_col),
            (3, 2)
        );
        assert_eq!(symbols[0].container.as_deref(), Some("util"));
    }
}
