//! `explore_file`'s outline from a language server (P5-D15): the symbols of a
//! file that changed since the index (or is not indexed), matched back to the
//! indexed records so they keep their id, FQN and signature.

use async_lsp::lsp_types::SymbolKind;

use super::explore::{KindFamily, family_of};
use crate::index::load::LoadedIndex;
use crate::lsp::ops::TargetShape;
use crate::lsp::outline::OutlineSymbol;

/// One outline symbol and the indexed record it matched, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineRow {
    /// The symbol as the server reported it.
    pub symbol: OutlineSymbol,
    /// The record with the same name and family, nearest in line, if one exists.
    pub record: Option<usize>,
}

/// The explore family of a server symbol: types, callables, or other
/// (members, modules and the rest).
pub fn symbol_family(kind: SymbolKind) -> KindFamily {
    match kind {
        SymbolKind::CLASS
        | SymbolKind::STRUCT
        | SymbolKind::INTERFACE
        | SymbolKind::ENUM
        | SymbolKind::TYPE_PARAMETER => KindFamily::Types,
        SymbolKind::FUNCTION | SymbolKind::METHOD | SymbolKind::CONSTRUCTOR => {
            KindFamily::Callables
        }
        _ => KindFamily::Other,
    }
}

/// What a language server can be asked about a symbol (`None`: not asked).
pub fn symbol_shape(kind: SymbolKind) -> TargetShape {
    match kind {
        SymbolKind::FUNCTION => TargetShape::Callable,
        SymbolKind::METHOD | SymbolKind::CONSTRUCTOR => TargetShape::Method,
        SymbolKind::INTERFACE => TargetShape::InterfaceLike,
        SymbolKind::CLASS => TargetShape::ClassLike,
        SymbolKind::STRUCT | SymbolKind::ENUM | SymbolKind::TYPE_PARAMETER => TargetShape::TypeLike,
        SymbolKind::CONSTANT
        | SymbolKind::VARIABLE
        | SymbolKind::FIELD
        | SymbolKind::PROPERTY
        | SymbolKind::ENUM_MEMBER => TargetShape::ValueLike,
        _ => TargetShape::None,
    }
}

/// `lsp:<kind>` for a symbol that matched no indexed record (`lsp:method`).
pub fn symbol_kind_label(kind: SymbolKind) -> String {
    let name = match kind {
        SymbolKind::FILE => "file",
        SymbolKind::MODULE => "module",
        SymbolKind::NAMESPACE => "namespace",
        SymbolKind::PACKAGE => "package",
        SymbolKind::CLASS => "class",
        SymbolKind::METHOD => "method",
        SymbolKind::PROPERTY => "property",
        SymbolKind::FIELD => "field",
        SymbolKind::CONSTRUCTOR => "constructor",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::ENUM_MEMBER => "enum_member",
        SymbolKind::STRUCT => "struct",
        SymbolKind::TYPE_PARAMETER => "type_parameter",
        _ => "symbol",
    };
    format!("lsp:{name}")
}

/// Match every symbol to the unused record of `file` with the same name and
/// family whose first line is nearest to the symbol's (ties: the earlier
/// record). Symbols are matched in the server's order.
pub fn match_symbols(
    index: &LoadedIndex,
    file: Option<&str>,
    symbols: Vec<OutlineSymbol>,
) -> Vec<OutlineRow> {
    let mut free: Vec<usize> = file
        .and_then(|file| index.by_file.get(file))
        .map(|positions| {
            positions
                .iter()
                .copied()
                .filter(|&at| !index.records[at].line.is_tier1())
                .collect()
        })
        .unwrap_or_default();
    symbols
        .into_iter()
        .map(|symbol| {
            let family = symbol_family(symbol.kind);
            let line1 = symbol.first_line0 as usize + 1;
            let best = free
                .iter()
                .enumerate()
                .filter(|(_, at)| {
                    let line = &index.records[**at].line;
                    line.name == symbol.name && family_of(&line.kind) == family
                })
                .min_by_key(|(_, at)| (index.records[**at].line.l0.abs_diff(line1), **at))
                .map(|(slot, _)| slot);
            let record = best.map(|slot| free.remove(slot));
            OutlineRow { symbol, record }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::load::test_support::{entity, synthetic_index};

    fn symbol(name: &str, kind: SymbolKind, line0: u32) -> OutlineSymbol {
        OutlineSymbol {
            name: name.to_owned(),
            kind,
            first_line0: line0,
            last_line0: line0 + 2,
            selection_line0: line0,
            selection_col: 4,
            container: None,
        }
    }

    #[test]
    fn symbols_match_records_by_name_family_and_nearest_line() {
        let index = synthetic_index(
            vec![
                (
                    "src/a.rs",
                    entity("t", "rust_struct", "Widget", "a::Widget", (1, 5)),
                ),
                (
                    "src/a.rs",
                    entity("f1", "rust_function", "run", "a::run", (10, 12)),
                ),
                (
                    "src/a.rs",
                    entity("f2", "rust_function", "run", "b::run", (30, 32)),
                ),
            ],
            &[],
        );
        let rows = match_symbols(
            &index,
            Some("src/a.rs"),
            vec![
                symbol("Widget", SymbolKind::STRUCT, 2),
                symbol("run", SymbolKind::FUNCTION, 31),
                symbol("run", SymbolKind::FUNCTION, 14),
                symbol("Widget", SymbolKind::FUNCTION, 40),
                symbol("added", SymbolKind::FUNCTION, 50),
            ],
        );
        let ids: Vec<Option<&str>> = rows
            .iter()
            .map(|row| row.record.map(|at| index.records[at].line.id.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![Some("t"), Some("f2"), Some("f1"), None, None],
            "nearest line, each record used once, family must agree"
        );
    }

    #[test]
    fn unindexed_file_matches_nothing() {
        let index = synthetic_index(vec![], &[]);
        let rows = match_symbols(&index, None, vec![symbol("x", SymbolKind::FUNCTION, 0)]);
        assert_eq!(rows[0].record, None);
    }

    #[test]
    fn symbol_kinds_map_to_families_shapes_and_labels() {
        assert_eq!(symbol_family(SymbolKind::INTERFACE), KindFamily::Types);
        assert_eq!(
            symbol_family(SymbolKind::CONSTRUCTOR),
            KindFamily::Callables
        );
        assert_eq!(symbol_family(SymbolKind::FIELD), KindFamily::Other);
        assert_eq!(symbol_shape(SymbolKind::METHOD), TargetShape::Method);
        assert_eq!(symbol_shape(SymbolKind::MODULE), TargetShape::None);
        assert_eq!(symbol_kind_label(SymbolKind::METHOD), "lsp:method");
        assert_eq!(
            symbol_kind_label(SymbolKind::ENUM_MEMBER),
            "lsp:enum_member"
        );
    }
}
