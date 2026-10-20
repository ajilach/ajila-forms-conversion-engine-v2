//! Where this engine follows Acrobat rather than the letter of XFA 3.3.
//!
//! Everything else in the crate is derived from the specification. Each
//! deviation below is admitted because real forms, authored for Acrobat,
//! depend on it: the evidence is named, and so is the passage the
//! behaviour departs from. A deviation is never added to make a rendering
//! look like a previous one.
//!
//! 1. **`this.X` is scope-matched.** XFA 3.3 §3 "Differences Between
//!    Qualified and Unqualified References" calls a reference starting with
//!    `this` (or `$`) qualified: `X` must be a child of the container.
//!    Acrobat resolves `this.X` the way it resolves an unqualified `X`, by
//!    the scope walk from the container, and throws when nothing in scope
//!    has the name. Forms depend on both: ACYI_001_EN's row-adding button
//!    reaches `this.GWM_Option_DYN` from inside the row, and AALP_019_EN
//!    tries `xfa.form.UBSForms.UBSForms_<id>` inside `try` to fall back to
//!    the root. Implemented by `_xfa_node_proto_` in
//!    `scripting::js_helpers` (`XFA_SOM_WALKER`). `xfa.resolveNode("$.X")`
//!    keeps the specified meaning: children only.
//!
//! 2. **`<bind match="global"/>` on an exclusion group.** XFA 3.3 §17 allows
//!    `match="global"` on a field only. Acrobat honours it on an exclGroup,
//!    which then shares one value with every group of its name, and
//!    ACYI_001_EN's cover page depends on that to keep its selection masks
//!    in step. Implemented by [`crate::xfa::XfaNode::is_globally_bound`].
//!
//! 3. **A leading `=` in a measurement.** XFA 3.3 "Measurements" gives no
//!    meaning to `h="=0mm"`; Designer writes it for a dimension it locked,
//!    and Acrobat reads it as the measurement itself (185 times in the UBS
//!    corpus, on table rules). Implemented by
//!    [`crate::xfa::XfaNode::parse_dimension`].
//!
//! 4. **`xfa.resolveNode("this")`.** `this` is JavaScript's name for the
//!    script's container, not a SOM shortcut (`$` is); Acrobat accepts it
//!    and the corpus writes it (six forms). Implemented by
//!    `_xfa_resolve_nodes_` in `scripting::js_helpers`.
//!
//! 5. **Assigning a value to a node reference sets the node's value.** XFA
//!    3.3 §3 "Obtaining the value of an expression" says a node is reached
//!    through its value properties (`rawValue`, `value`), and gives no
//!    meaning to `Subform.Field = "text"` in JavaScript. Acrobat sets the
//!    field's value: AAJB_033_IT's signature block does
//!    `this.ffDesSignature = my66420IT.Sign_Advisor` and Acrobat shows that
//!    caption ("Firma del client advisor", `pdf-screenshots/aajb`, page 3);
//!    ACYI_001_EN's "Back to selection" clears its option group the same
//!    way. Implemented by `_xfa_set_child_` in `scripting::js_helpers`.
//!
//! 6. **A positioned subform is not split.** XFA 3.3 §8 "Split Restrictions"
//!    gives a subform's `keep intact` the default `contentArea` only for a
//!    table row and "when the subform's container's layout strategy is
//!    positioned", and otherwise `none`: a `layout="position"` subform in a
//!    flowed container would split between its children by the consensus
//!    of §8 "Splitting a Container Having Child Containers" (Example 8.19).
//!    Acrobat moves it to the next content area whole: AAKF_019_DE's and
//!    AAJB_033_IT's question blocks (positioned subforms under a `tb`
//!    section) start a page later than a split would put them
//!    (`pdf-screenshots/aakf`, `aajb`; `tests/acrobat_reference.rs`), and
//!    Designer offers "allow page breaks within content" for flowed subforms
//!    only. Implemented by `Flattened::end_container_extent`.
//!
//! 7. **A relative (`em`) `letterSpacing` is not applied.** XFA 3.3 §2
//!    "Measurements" lets `letterSpacing` be given in `em`, relative to the
//!    font size, and Designer writes `-0.01em` on many corpus texts (AABK,
//!    AAKF, AAGZ, AAPS, AAOR, ...). Acrobat lays those texts out as if the
//!    spacing were zero: with it applied, our lines of AAKF_019_DE's
//!    "Entschädigung" paragraph hold a few characters more than Acrobat's
//!    (`pdf-screenshots/aakf`, page 3: Acrobat breaks after "Soll-" and
//!    "Kreditbetrages", an unspaced measure does the same) and the
//!    "Datenbankabfrage" block lands a page earlier than in Acrobat
//!    (`tests/acrobat_reference.rs`). An absolute `letterSpacing` (`pt`,
//!    `mm`) is applied as the spec says; the corpus has none to check it
//!    against. Implemented by `XfaNode::parse_font`.
