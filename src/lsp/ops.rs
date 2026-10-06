//! Which language-server operations can answer a target of a given entity
//! kind (Appendix G of the Phase 3 plan).
//!
//! The classification is by knot wire label: TypeScript, JavaScript and Java
//! entities carry the generic labels (`function`, `method`, `class`,
//! `interface`, `enum`, `constant`), Rust entities the `rust_*` ones.
//!
//! The answer for a target is the union of up to three requests (Phase 0 §5
//! measured it: calls alone recover only 11–38 % of the callers, because type
//! references dominate): `callHierarchy/incomingCalls`, `textDocument/references`
//! and `textDocument/implementation`.

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

/// One language-server operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Op {
    /// `callHierarchy/incomingCalls` (after `prepareCallHierarchy`).
    Incoming,
    /// `textDocument/references`, declaration excluded.
    References,
    /// `textDocument/implementation`.
    Implementation,
}

/// The operations to run for one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpSet {
    /// Run `incomingCalls`.
    pub incoming: bool,
    /// Run `references`.
    pub references: bool,
    /// Run `implementation`.
    pub implementation: bool,
}

impl OpSet {
    /// Whether `op` is in the set.
    pub fn contains(self, op: Op) -> bool {
        match op {
            Op::Incoming => self.incoming,
            Op::References => self.references,
            Op::Implementation => self.implementation,
        }
    }

    /// Whether no operation applies.
    pub fn is_empty(self) -> bool {
        !(self.incoming || self.references || self.implementation)
    }
}

/// The operations that answer a target of `shape` (Appendix G).
pub fn ops_for(shape: TargetShape) -> OpSet {
    let (incoming, references, implementation) = match shape {
        TargetShape::Callable => (true, true, false),
        TargetShape::Method => (true, true, true),
        TargetShape::InterfaceLike | TargetShape::ClassLike => (false, true, true),
        TargetShape::TypeLike | TargetShape::ValueLike => (false, true, false),
        TargetShape::None => (false, false, false),
    };
    OpSet {
        incoming,
        references,
        implementation,
    }
}

/// Relationship label of the `implementation` results of a target of `shape`:
/// an overriding method, an implementing class, a subclass.
pub fn implementation_relation(shape: TargetShape) -> Option<&'static str> {
    match shape {
        TargetShape::Method => Some("OVERRIDES"),
        TargetShape::InterfaceLike => Some("IMPLEMENTS"),
        TargetShape::ClassLike => Some("EXTENDS"),
        _ => None,
    }
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

    const ALL_SHAPES: [TargetShape; 7] = [
        TargetShape::Callable,
        TargetShape::Method,
        TargetShape::InterfaceLike,
        TargetShape::ClassLike,
        TargetShape::TypeLike,
        TargetShape::ValueLike,
        TargetShape::None,
    ];

    #[test]
    fn ops_for_each_shape() {
        let ops = |incoming, references, implementation| OpSet {
            incoming,
            references,
            implementation,
        };
        let expected = [
            (TargetShape::Callable, ops(true, true, false)),
            (TargetShape::Method, ops(true, true, true)),
            (TargetShape::InterfaceLike, ops(false, true, true)),
            (TargetShape::ClassLike, ops(false, true, true)),
            (TargetShape::TypeLike, ops(false, true, false)),
            (TargetShape::ValueLike, ops(false, true, false)),
            (TargetShape::None, ops(false, false, false)),
        ];
        assert_eq!(expected.len(), ALL_SHAPES.len(), "every shape has a row");
        for (shape, set) in expected {
            assert_eq!(ops_for(shape), set, "{shape:?}");
            assert_eq!(set.is_empty(), shape == TargetShape::None, "{shape:?}");
        }
    }

    #[test]
    fn op_set_membership_follows_its_flags() {
        let set = ops_for(TargetShape::Method);
        for op in [Op::Incoming, Op::References, Op::Implementation] {
            assert!(set.contains(op), "{op:?}");
        }
        let set = ops_for(TargetShape::TypeLike);
        assert!(!set.contains(Op::Incoming));
        assert!(set.contains(Op::References));
        assert!(!set.contains(Op::Implementation));
    }

    #[test]
    fn implementation_relation_by_shape() {
        for shape in ALL_SHAPES {
            let expected = match shape {
                TargetShape::Method => Some("OVERRIDES"),
                TargetShape::InterfaceLike => Some("IMPLEMENTS"),
                TargetShape::ClassLike => Some("EXTENDS"),
                _ => None,
            };
            assert_eq!(implementation_relation(shape), expected, "{shape:?}");
            assert_eq!(
                ops_for(shape).implementation,
                expected.is_some(),
                "{shape:?}: a relation exists exactly when implementation runs"
            );
        }
    }

    /// Every label a language-server language emits, and whether a server can
    /// be asked about it: knot's labels for Rust and the generic ones shared by
    /// TypeScript, JavaScript and Java (plan §4, "Kind labels").
    const LSP_LANGUAGE_KINDS: &[(&str, bool)] = &[
        ("function", true),
        ("method", true),
        ("class", true),
        ("interface", true),
        ("enum", true),
        ("constant", true),
        ("rust_function", true),
        ("rust_method", true),
        ("rust_struct", true),
        ("rust_enum", true),
        ("rust_union", true),
        ("rust_trait", true),
        ("rust_type_alias", true),
        ("rust_constant", true),
        ("rust_static", true),
        ("rust_macro_def", true),
        ("rust_impl", false),
        ("rust_module", false),
        ("rust_macro_invoke", false),
    ];

    #[test]
    fn every_lsp_language_kind_is_classified() {
        let knot_kinds: Vec<&str> = knot::models::EntityKind::all_wire_kinds().collect();
        for (label, addressable) in LSP_LANGUAGE_KINDS {
            assert!(
                knot_kinds.contains(label),
                "knot no longer has the kind `{label}`: update Appendix G"
            );
            assert_eq!(
                shape_of(label) != TargetShape::None,
                *addressable,
                "classification of `{label}`"
            );
        }
        // A new `rust_*` kind in knot must be classified on purpose.
        for label in knot_kinds.iter().filter(|label| label.starts_with("rust_")) {
            assert!(
                LSP_LANGUAGE_KINDS.iter().any(|(known, _)| known == label),
                "new knot kind `{label}`: classify it in LSP_LANGUAGE_KINDS and Appendix G"
            );
        }
    }
}
