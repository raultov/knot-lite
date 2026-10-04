//! Language label table (Appendix B of the Phase 1 plan) and LSP language mapping.
//!
//! [`label_for`] classifies a path by filename first, then by lower-cased
//! extension. The drift-guard test ties the table to
//! `knot::pipeline::input::SUPPORTED_EXTENSIONS`, so adding a language to knot
//! fails a test here and forces an update of Appendix B.

use std::path::Path;

/// Language servers knot-lite can detect (Phase 0 evidence: tier-3 candidates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LspLang {
    /// `rust-analyzer`.
    Rust,
    /// `typescript-language-server` (TypeScript and JavaScript).
    Ts,
    /// Eclipse JDT Language Server.
    Java,
}

impl LspLang {
    /// Default server program name on `PATH`.
    pub fn server_program(self) -> &'static str {
        match self {
            Self::Rust => "rust-analyzer",
            Self::Ts => "typescript-language-server",
            Self::Java => "jdtls",
        }
    }

    /// Environment variable that overrides the server program.
    pub fn env_override_key(self) -> &'static str {
        match self {
            Self::Rust => "KNOT_LITE_LSP_RUST",
            Self::Ts => "KNOT_LITE_LSP_TS",
            Self::Java => "KNOT_LITE_LSP_JAVA",
        }
    }
}

/// Filename rules (checked first, exact match) — Appendix B.
const FILENAME_LABELS: &[(&str, &str)] = &[
    ("Jenkinsfile", "jenkins"),
    ("pom.xml", "maven"),
    ("Cargo.toml", "cargo"),
    ("package.json", "npm"),
    ("tsconfig.json", "tsconfig"),
    ("Directory.Packages.props", "msbuild"),
];

/// Extension rules (lower-cased) — Appendix B.
const EXTENSION_LABELS: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("cts", "typescript"),
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("java", "java"),
    ("kt", "kotlin"),
    ("kts", "kotlin"),
    ("py", "python"),
    ("pyi", "python"),
    ("pyw", "python"),
    ("html", "html"),
    ("htm", "html"),
    ("css", "css"),
    ("scss", "scss"),
    ("sass", "scss"),
    ("groovy", "groovy"),
    ("gradle", "groovy"),
    ("c", "c_cpp"),
    ("h", "c_cpp"),
    ("cpp", "c_cpp"),
    ("hpp", "c_cpp"),
    ("cc", "c_cpp"),
    ("cxx", "c_cpp"),
    ("hh", "c_cpp"),
    ("hxx", "c_cpp"),
    ("cs", "csharp"),
    ("csproj", "msbuild"),
    ("md", "markdown"),
    ("vcl", "varnish"),
    ("vtc", "varnish"),
    ("vcc", "varnish"),
    ("yml", "yaml"),
    ("yaml", "yaml"),
    ("json", "json"),
    ("properties", "properties"),
    ("tpl", "helm"),
];

/// Fallback label for paths outside the table.
pub const OTHER_LABEL: &str = "other";

/// Language label for a path: filename table first, then lower-cased extension.
pub fn label_for(path: &Path) -> &'static str {
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && let Some((_, label)) = FILENAME_LABELS.iter().find(|(file, _)| *file == name)
    {
        return label;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    ext.as_deref().map_or(OTHER_LABEL, |ext| {
        EXTENSION_LABELS
            .iter()
            .find(|(candidate, _)| *candidate == ext)
            .map_or(OTHER_LABEL, |(_, label)| label)
    })
}

/// The LSP language serving a label, if any (Appendix B, LSP column).
pub fn lsp_lang_for_label(label: &str) -> Option<LspLang> {
    match label {
        "rust" => Some(LspLang::Rust),
        "typescript" | "javascript" => Some(LspLang::Ts),
        "java" => Some(LspLang::Java),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_knot_supported_extension_has_a_label() {
        for ext in knot::pipeline::input::SUPPORTED_EXTENSIONS {
            let label = label_for(Path::new(&format!("x.{ext}")));
            assert_ne!(
                label, OTHER_LABEL,
                "extension '{ext}' has no label: update Appendix B (docs/PLAN_PHASE1_SKELETON.md)"
            );
        }
    }

    #[test]
    fn tsx_maps_to_typescript() {
        assert_eq!(label_for(Path::new("web/app.tsx")), "typescript");
    }

    #[test]
    fn mjs_maps_to_javascript() {
        assert_eq!(label_for(Path::new("web/app.mjs")), "javascript");
    }

    #[test]
    fn cargo_toml_maps_to_cargo() {
        assert_eq!(label_for(Path::new("sub/Cargo.toml")), "cargo");
    }

    #[test]
    fn pom_xml_maps_to_maven() {
        assert_eq!(label_for(Path::new("pom.xml")), "maven");
    }

    #[test]
    fn jenkinsfile_maps_to_jenkins() {
        assert_eq!(label_for(Path::new("Jenkinsfile")), "jenkins");
    }

    #[test]
    fn upper_case_extension_is_lowered() {
        assert_eq!(label_for(Path::new("src/lib.H")), "c_cpp");
    }

    #[test]
    fn makefile_is_other() {
        assert_eq!(label_for(Path::new("Makefile")), OTHER_LABEL);
    }

    #[test]
    fn lsp_lang_covers_the_three_tier3_languages() {
        assert_eq!(lsp_lang_for_label("rust"), Some(LspLang::Rust));
        assert_eq!(lsp_lang_for_label("typescript"), Some(LspLang::Ts));
        assert_eq!(lsp_lang_for_label("javascript"), Some(LspLang::Ts));
        assert_eq!(lsp_lang_for_label("java"), Some(LspLang::Java));
        assert_eq!(lsp_lang_for_label("markdown"), None);
        assert_eq!(lsp_lang_for_label(OTHER_LABEL), None);
    }

    #[test]
    fn lsp_lang_server_programs_and_env_keys() {
        assert_eq!(LspLang::Rust.server_program(), "rust-analyzer");
        assert_eq!(LspLang::Ts.server_program(), "typescript-language-server");
        assert_eq!(LspLang::Java.server_program(), "jdtls");
        assert_eq!(LspLang::Rust.env_override_key(), "KNOT_LITE_LSP_RUST");
        assert_eq!(LspLang::Ts.env_override_key(), "KNOT_LITE_LSP_TS");
        assert_eq!(LspLang::Java.env_override_key(), "KNOT_LITE_LSP_JAVA");
    }
}
