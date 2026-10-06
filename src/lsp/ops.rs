//! Which language-server operations can answer a target of a given entity
//! kind (Appendix G of the Phase 3 plan).
//!
//! The classification is by knot wire label: TypeScript, JavaScript and Java
//! entities carry the generic labels (`function`, `method`, `class`,
//! `interface`, `enum`, `constant`), Rust entities the `rust_*` ones.

/// What a language server can be asked about an entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetShape {
    /// A free function: incoming calls and references.
    Callable,
    /// A method: incoming calls, references and overriding implementations.
    Method,
    /// An interface or trait: references and implementations.
    InterfaceLike,
    /// A class: references and subclasses.
    ClassLike,
    /// An enum, struct, union or type alias: references.
    TypeLike,
    /// A constant, static or macro definition: references.
    ValueLike,
    /// Not something a language server knows as a symbol (impl blocks, modules,
    /// macro invocations, markup, configuration, documentation).
    None,
}

/// Classify a knot wire label (`ShardLine::kind`).
pub fn shape_of(kind: &str) -> TargetShape {
    match kind {
        "function" | "rust_function" => TargetShape::Callable,
        "method" | "rust_method" => TargetShape::Method,
        "interface" | "rust_trait" => TargetShape::InterfaceLike,
        "class" => TargetShape::ClassLike,
        "enum" | "rust_struct" | "rust_enum" | "rust_union" | "rust_type_alias" => {
            TargetShape::TypeLike
        }
        "constant" | "rust_constant" | "rust_static" | "rust_macro_def" => TargetShape::ValueLike,
        _ => TargetShape::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_table_matches_appendix_g() {
        let rows: &[(&[&str], TargetShape)] = &[
            (&["function", "rust_function"], TargetShape::Callable),
            (&["method", "rust_method"], TargetShape::Method),
            (&["interface", "rust_trait"], TargetShape::InterfaceLike),
            (&["class"], TargetShape::ClassLike),
            (
                &[
                    "enum",
                    "rust_struct",
                    "rust_enum",
                    "rust_union",
                    "rust_type_alias",
                ],
                TargetShape::TypeLike,
            ),
            (
                &["constant", "rust_constant", "rust_static", "rust_macro_def"],
                TargetShape::ValueLike,
            ),
            (
                &[
                    "rust_impl",
                    "rust_module",
                    "rust_macro_invoke",
                    "html_class",
                    "html_id",
                    "config_property",
                    "chunk",
                    "file",
                ],
                TargetShape::None,
            ),
        ];
        for (kinds, shape) in rows {
            for kind in *kinds {
                assert_eq!(shape_of(kind), *shape, "{kind}");
            }
        }
    }
}
