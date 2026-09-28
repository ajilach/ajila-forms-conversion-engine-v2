//! Resolving a local `$ref` against a schema's own root.
//!
//! Extracted out of `restore.rs`, where it originated, once
//! [`super::skeleton`] needed the identical resolution: one local-`$ref`
//! walker, not two, is the whole point — a second implementation is exactly
//! how the two could quietly diverge on which reference forms they accept.

use serde_json::Value;

/// Resolves a local `$ref` (`#/$defs/X`, `#/definitions/X`) against the
/// schema root. A foreign or malformed `$ref` yields `None`, and the caller
/// passes the value through untouched. A schema with no `$ref` at all
/// resolves to itself.
pub(super) fn resolve<'a>(root: &'a Value, schema: &'a Value) -> Option<&'a Value> {
    let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
        return Some(schema);
    };
    let path = reference.strip_prefix("#/")?;
    let mut node = root;
    for segment in path.split('/') {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        node = node.get(&segment)?;
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_schema_with_no_ref_resolves_to_itself() {
        let root = json!({});
        let schema = json!({ "type": "string" });
        assert_eq!(resolve(&root, &schema), Some(&schema));
    }

    #[test]
    fn a_defs_ref_resolves_against_the_root() {
        let root = json!({ "$defs": { "Node": { "type": "object" } } });
        let schema = json!({ "$ref": "#/$defs/Node" });
        assert_eq!(resolve(&root, &schema), root.pointer("/$defs/Node"));
    }

    #[test]
    fn a_definitions_ref_resolves_too() {
        let root = json!({ "definitions": { "Node": { "type": "object" } } });
        let schema = json!({ "$ref": "#/definitions/Node" });
        assert_eq!(resolve(&root, &schema), root.pointer("/definitions/Node"));
    }

    #[test]
    fn an_escaped_segment_is_unescaped() {
        let root = json!({ "$defs": { "a/b": { "type": "string" }, "c~d": { "type": "number" } } });
        assert_eq!(
            resolve(&root, &json!({ "$ref": "#/$defs/a~1b" })),
            root.pointer("/$defs/a~1b")
                .map(|_| root.get("$defs").unwrap().get("a/b").unwrap())
        );
        assert_eq!(
            resolve(&root, &json!({ "$ref": "#/$defs/c~0d" })),
            Some(root.get("$defs").unwrap().get("c~d").unwrap())
        );
    }

    #[test]
    fn a_foreign_ref_is_none() {
        let root = json!({});
        assert_eq!(
            resolve(&root, &json!({ "$ref": "https://example.com/schema" })),
            None
        );
    }

    #[test]
    fn an_unresolvable_path_is_none() {
        let root = json!({ "$defs": {} });
        assert_eq!(resolve(&root, &json!({ "$ref": "#/$defs/Missing" })), None);
    }
}
