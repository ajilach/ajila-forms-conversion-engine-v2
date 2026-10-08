# Porting notes

Parts of this crate's decode direction are ported from
`ajila-forms-conversion-engine`, taken at upstream commit:

```
aee5aef09e47818fe99558aeb96d110be842b570
```

Same rule as `crates/u2s-xfa/PORTING.md`: **verbatim copy wherever possible**,
so a future re-sync has a small diff to reason about. Every deliberate
difference is listed below with the reason. If you change something here
that upstream also has, add a row.

## Why a port at all

`u2s-mapper-aem::encode` is a from-scratch, deliberately mechanical
writer (see the crate's module doc) — there was nothing to port for the
encode direction. Decode is different: parsing real, human-authored AEM
FileVault packages into a typed tree is a large, already-solved problem
(`core/src/aem/parser.rs`, 1812 lines, exercised against years of real UBS
form corpus), and re-deriving it from the spec alone would both take longer
and be less trustworthy than a fresh implementation the corpus has never
seen. The mechanical, structure-only pieces are ported; the business logic
that decides *what a node means* is not (see "What was not ported" below) —
the same split this crate's own module doc (`src/lib.rs`) already made for
the encoder.

## What was copied

| This crate | Upstream | Lines | Fidelity |
|---|---|---|---|
| `src/jcr/tree.rs` — `JcrNode`, `parse_jcr_xml`, `serialize_jcr_node`, `find_node_by_resource_type` | `parser.rs:410-570` | ~160 | verbatim, plus a typed `JcrXmlError` (upstream returns `String`) and a `leaf`/`child`/`passthrough_children` convenience API this crate's own tests and later decode phases need |
| `src/jcr/value.rs` — `split_jcr_list`, `jcr_unescape`, `parse_jcr_array`, `parse_bool_attr`, `parse_visible`, `parse_options` | `parser.rs:1596-1677` | ~150 | verbatim, except `parse_options` returns plain `(String, String)` pairs rather than upstream's own `AemOption` -- this module names no `u2s_aem` type, and lowering a pair into a typed `OptionValue`/`PlainText` is `decode::form`'s job (not built yet) |
| `src/decode/zip.rs` — ZIP→map (`parser.rs:124-142`), `open_zip`/`locate_roots` | `parser.rs:89-253` | ~165 | the ZIP-to-map loop is verbatim; root discovery is **new**, not ported -- see the module's own doc for why `filter.xml` is the authority and upstream's `find_form_content_xml` heuristic (ported alongside it) is kept only as its fallback. `detect_aem_zip` itself was not ported: nothing here needs a yes/no "is this an AEM zip" test standalone, since a caller either successfully locates roots or gets a typed `RootError` either way |
| `u2s-aem/src/model/passthrough.rs` — `Passthrough` | `mod.rs:652-670` | ~20 | reshaped, see deviation 1 -- not yet landed (Phase 3) |
| `src/profile.rs` — `REGENERATED_CHILD_TAGS`, `REPEATING_PANEL_ARCHETYPE_ATTRS` | `parser.rs:433-451` | ~20 | not yet landed (Phase 2); `JcrNode::passthrough_children` (`src/jcr/tree.rs`) already carries the `REGENERATED_CHILD_TAGS` list forward as a `const` inside the method, to be replaced by the shared profile table once it exists |

Not yet ported (later phases, per the design doc's C12 phasing): `extract_translations`/`parse_sling_dictionary` (`parser.rs:1521-1595`, dictionaries) and `parse_visibility_rules`/`parse_fd_scripts_json`/`AemScript`/`VisibilityCondition` (`parser.rs:1374-1520`, `fd:rules`/`fd:scripts`) both land with the `JcrNode → AemForm` lowering (`decode::form`, `decode::i18n`, `decode::rules`), not before.

## Deviations

1. **`Passthrough::raw_children` is a typed recursive `RawJcrNode { name,
   attributes, children }`, not verbatim XML strings.** Upstream carries
   unmodeled children as opaque XML text because its own editing surface
   never addresses into a node's raw children. This workspace's editing
   surface is `u2s-jsondoc` (`outline`/`get`/`search`/`patch` over a JSON
   document, PLAN.md), which can address into a typed tree but not into an
   opaque XML blob — a passthrough child the Conversion Agent cannot even
   point at is not meaningfully editable. The cost is the same either way:
   nothing round-trips through this shape unless it is re-serialized
   byte-for-byte-equivalent (not byte-identical — see `canonical.rs`), which
   a typed tree does just as faithfully as a string.

2. **`AemAttrs` is not carried forward as its own struct.** Its four
   presence-adjacent fields (`dor_exclude`, `summary_exclude`, `hidden`,
   `dor_include_in_pdf`-shaped flags) already have a home in this crate's
   existing `Presence` (`u2s-aem/src/model/common.rs`); the remainder
   (`dorFieldStyling`, `jumpToFieldButtonVisible`, `dorHeaderSlot`, and the
   rest) is added directly to the relevant existing structs (`Presence`,
   `PanelLayout`) rather than kept as a second grouping struct, so a caller
   has one place to look for "what can suppress this node from the DoR",
   not two.

3. **Fragment inlining (`convert_fragment`'s reading of the referenced
   fragment's own content) is not ported.** Upstream inlines a fragment's
   children so its generic `StructuredNode` tree is self-contained. Inlining
   destroys the `fragRef` boundary — a fragment reference re-encoded from
   inlined children is a different, larger document than the source package
   had, which is lossy under this work's round-trip requirement. This
   crate's decoder keeps `Node::Fragment` as a reference (matching the
   existing model, `u2s-aem/src/model/mod.rs`) and does not open the
   referenced content at all.

4. **`to_structured.rs` (upstream `AemNode → StructuredNode`, 686 lines) is
   not ported.** It targets the reference's own generic intermediate
   representation, which this workspace has no equivalent of and no use
   for — the decoder's target is `u2s_aem::model::AemForm` directly. Its
   *shape* (one function per component kind, ending in a call that records
   whatever the function did not consume as passthrough) is the pattern this
   crate's `decode/form.rs` follows; its bodies are not reusable, because
   they build `AemNode` values (`uuid: Uuid`, `label: String`,
   `colspan: u32`) rather than this crate's `ComponentName`, `I18nText`,
   `ColSpan`, sealed `ValidForm`.

5. **`converter.rs`, `normalize.rs`, `xml_writer.rs` (upstream, the
   `StructuredNode → AemNode` encode direction) are not ported at all.**
   this crate's own module doc (`src/lib.rs`'s "Why this crate carries no
   business logic") already explains why the encode direction has no
   counterpart to port; nothing about adding decode changes that reasoning
   for the encode side.

## Provenance

`ajila-forms-conversion-engine` carries no LICENSE file; its `Cargo.toml`
names the same organisation this workspace's own history traces to
(`crates/u2s-xfa/PORTING.md` made an equivalent note when porting the XFA
render closure from the same repository). Nothing here is redistributed
beyond this workspace.

The one piece of that repository's *content*, rather than code, that this
work also brings in is the fixture package itself — see
`tests/fixtures/README.md` for its own provenance and confidentiality note.
