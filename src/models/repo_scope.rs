//! Single-repository scope checks for `repo_name` arguments.
//!
//! knot-lite serves one repository (P1-D7). The checks accept knot's
//! `repo_name` forms (absent/`null`, string with commas, `"all"`/`"*"`, array
//! of strings) and verify they only name this repository.

use serde_json::Value;

/// `repo_name` forms that mean "the served repository" whatever its name.
fn is_sentinel(item: &str) -> bool {
    let lowered = item.to_ascii_lowercase();
    lowered == "all" || item == "*"
}

fn check_items<'a>(items: impl Iterator<Item = &'a str>, served: &str) -> Result<(), String> {
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if item == served || is_sentinel(item) {
            continue;
        }
        return Err(format!(
            "repo_name '{item}' is not served here: this knot-lite server serves only '{served}'. Omit repo_name, or pass '{served}', 'all' or '*'."
        ));
    }
    Ok(())
}

/// Validate a `repo_name` JSON argument (string, array of strings, or absent).
pub fn check_repo_scope(value: Option<&Value>, served: &str) -> Result<(), String> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(name)) => check_items(name.split(','), served),
        Some(Value::Array(items)) => {
            let mut names = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(name) => names.push(name),
                    None => {
                        return Err("repo_name must be a string or an array of strings".to_string());
                    }
                }
            }
            check_items(names.into_iter(), served)
        }
        Some(_) => Err("repo_name must be a string or an array of strings".to_string()),
    }
}

/// Validate a `repo_name` string argument (the CLI `--repo` form).
pub fn check_repo_scope_str(value: Option<&str>, served: &str) -> Result<(), String> {
    match value {
        None => Ok(()),
        Some(name) => check_items(name.split(','), served),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_absent_null_and_empty() {
        assert_eq!(check_repo_scope(None, "served"), Ok(()));
        assert_eq!(check_repo_scope(Some(&Value::Null), "served"), Ok(()));
        assert_eq!(check_repo_scope(Some(&json!("")), "served"), Ok(()));
    }

    #[test]
    fn accepts_sentinels_case_insensitively() {
        assert_eq!(check_repo_scope(Some(&json!("all")), "served"), Ok(()));
        assert_eq!(check_repo_scope(Some(&json!("ALL")), "served"), Ok(()));
        assert_eq!(check_repo_scope(Some(&json!("*")), "served"), Ok(()));
        assert_eq!(check_repo_scope(Some(&json!(" all ")), "served"), Ok(()));
    }

    #[test]
    fn accepts_self_and_lists_of_self() {
        assert_eq!(check_repo_scope(Some(&json!("served")), "served"), Ok(()));
        assert_eq!(
            check_repo_scope(Some(&json!("served,served,all")), "served"),
            Ok(())
        );
        assert_eq!(
            check_repo_scope(Some(&json!(["served", "all"])), "served"),
            Ok(())
        );
    }

    #[test]
    fn rejects_foreign_name_naming_the_item() {
        let err = check_repo_scope(Some(&json!("other")), "served").unwrap_err();
        assert!(err.contains("'other'"), "{err}");
        assert!(err.contains("not served here"), "{err}");
        assert!(err.contains("'served'"), "{err}");
    }

    #[test]
    fn rejects_mixed_list_naming_the_foreign_item() {
        let err = check_repo_scope(Some(&json!("x,served")), "served").unwrap_err();
        assert!(err.contains("'x'"), "{err}");
    }

    #[test]
    fn rejects_non_string_types() {
        let err = check_repo_scope(Some(&json!(42)), "served").unwrap_err();
        assert_eq!(err, "repo_name must be a string or an array of strings");
        let err = check_repo_scope(Some(&json!([1, 2])), "served").unwrap_err();
        assert_eq!(err, "repo_name must be a string or an array of strings");
    }

    #[test]
    fn string_form_matches_json_form() {
        assert_eq!(check_repo_scope_str(None, "served"), Ok(()));
        assert_eq!(check_repo_scope_str(Some("served,all"), "served"), Ok(()));
        let err = check_repo_scope_str(Some("other"), "served").unwrap_err();
        assert!(err.contains("'other'"), "{err}");
    }
}
