//! Keeps `catalog::FOUNDATION` and `specs/AEM.md` from drifting apart
//! silently: every resource type the catalogue claims is stock must appear
//! literally in the spec, and every `fd/af/...` resource-type literal the
//! spec itself documents must be in the catalogue (or in this test's own
//! short, explicit exclusion list, for spec strings that are not
//! components at all). A future edit to either side that forgets the
//! other fails this test instead of silently going stale.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// `specs/AEM.md` strings that start with `fd/af/` but are not themselves
/// catalogue-worthy components: prose/example text, or a documented
/// alternative this crate deliberately does not treat as its own separate
/// entry.
const SPEC_EXCLUSIONS: &[&str] = &[
    // The §6.12 fragment-reference example's own `fragRef` *value*, not a
    // resource type -- it happens to start with `fd/af/` too
    // (`/content/forms/af/fragments/...`) but names a JCR path, not a
    // component.
    "fd/af/fragments/my-fragment",
    // §14's own "Resource type pattern" headers and prose glob
    // (`fd/af/components/**`, `fd/af/components/*`,
    // `fd/af/components/controls/*`) -- describing a family, not naming
    // one component.
    "fd/af/components/**",
    "fd/af/components/*",
    "fd/af/components/controls/*",
    // The Appendix's own "§16 submit actions" bullet describes the
    // fixture's toolbar actions living under `fd/af/components/actions/*`
    // as a family (glob), plus one concrete example
    // (`fd/af/components/actions/nextitemnav`) -- both left as prose in
    // the corresponding stock entry's own `description` (see
    // `catalog::FOUNDATION`'s `nextitemnav`/`previtemnav`/`submit`
    // entries) rather than three additional catalogue entries, since the
    // stock spelling is what this table teaches an agent to write and the
    // deviation is exactly that -- a deviation, not a second stock option.
    "fd/af/components/actions/*",
    "fd/af/components/actions/nextitemnav",
    // A markdown line-wrap artifact: the Appendix's own toolbar-layout
    // bullet quotes `sling:resourceType="fd/af/layouts/toolbar/` and
    // wraps mid-value onto the next line before `defaultToolbarLayout"`.
    // This tokenizer splits on whitespace, so the wrap produces this
    // fragment as a spurious extra token; the real, complete value
    // (`fd/af/layouts/toolbar/defaultToolbarLayout`) is catalogued and
    // matched separately.
    "fd/af/layouts/toolbar/",
];

fn spec_text() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs/aem/aem-xml-spec.md");
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} must be readable: {err}", path.display()))
}

/// Every `fd/af/...` token in `text`, extracted the same crude way for
/// both directions of this test: split on characters that never appear
/// inside a resource type, then keep tokens that look like one. Good
/// enough for a drift guard over a spec file that always quotes resource
/// types as bare, unquoted path-like strings.
fn fd_af_tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| c.is_whitespace() || "\"'`<>=,()[]{}".contains(c))
        .map(|tok| tok.trim_matches(|c: char| c == '.' || c == ':'))
        .filter(|tok| tok.starts_with("fd/af/"))
        .map(|tok| tok.to_owned())
        .collect()
}

#[test]
fn every_catalogue_entry_appears_literally_in_the_spec() {
    let spec = spec_text();
    let mut missing = Vec::new();
    for entry in u2s_mapper_aem::catalog::FOUNDATION {
        if !spec.contains(entry.resource_type) {
            missing.push(entry.resource_type);
        }
    }
    assert!(
        missing.is_empty(),
        "catalog::FOUNDATION claims these as stock resource types, but specs/AEM.md \
         no longer mentions them: {missing:?} -- did the spec change under this table?"
    );
}

#[test]
fn every_fd_af_resource_type_the_spec_documents_is_either_catalogued_or_excluded() {
    let spec = fd_af_tokens(&spec_text());
    let catalogued: BTreeSet<&str> = u2s_mapper_aem::catalog::FOUNDATION
        .iter()
        .map(|e| e.resource_type)
        .collect();
    let excluded: BTreeSet<&str> = SPEC_EXCLUSIONS.iter().copied().collect();

    let uncovered: Vec<&String> = spec
        .iter()
        .filter(|tok| !catalogued.contains(tok.as_str()) && !excluded.contains(tok.as_str()))
        .collect();

    assert!(
        uncovered.is_empty(),
        "specs/AEM.md names these fd/af/ resource types, but catalog::FOUNDATION does not \
         list them and this test's own SPEC_EXCLUSIONS does not name them either: \
         {uncovered:?} -- add a catalog entry, or add an explicit exclusion with a reason"
    );
}
