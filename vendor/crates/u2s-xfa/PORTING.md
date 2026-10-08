# Porting notes

This crate is a copy of the XFA render closure from
`ajila-forms-conversion-engine`, taken at upstream commit:

```
f161e656f01c0ea8c80cbf76861bdd4466f224f7
```

The rule is **verbatim copy wherever possible**, so a future re-sync has a small
diff to reason about. Every deliberate difference is listed below with the
reason. If you change something here that upstream also has, add a row.

## What was copied

| This crate | Upstream | Lines |
|---|---|---|
| `src/flattened.rs` | `core/src/flattened/mod.rs` | 10,874 |
| `src/xfa/mod.rs` | `core/src/xfa/mod.rs` | 2,052 |
| `src/xfa/font_manager.rs` | `core/src/xfa/font_manager.rs` | 1,154 |
| `src/xfa/text_metrics.rs` | `core/src/xfa/text_metrics.rs` | 1,014 |
| `src/xfa/hyphenation.rs` | `core/src/xfa/hyphenation.rs` | 365 |
| `src/xfa/scripting/som.rs` | `core/src/xfa/scripting/som.rs` | 636 |
| `src/xfa/scripting/{engine,events,state,registry,dependency,script_object,js_helpers,tests}.rs` | same paths upstream | 6,698 |
| `src/xfa/script_executor.rs` | `core/src/xfa/script_executor.rs` | 1,171 |
| `src/xfa/scripting/form.rs` | `core/src/xfa/scripting/form.rs` | 2,277 |
| `src/exhaustive.rs` | `core/src/exhaustive.rs` | 1,698 |
| `src/selection.rs` | 3 types from `structured/{mod,merger}.rs` | 224 |
| `src/util.rs` | 3 fns from `core/src/util.rs` | 88 |
| `src/extract.rs` | `extract_xfa_from_pdf_bytes` from `core/src/lib.rs` | rewritten |

Deliberately **not** copied: `document/`, `structured/`, `aem/`, `xsd/`,
`semantic/`, `pdf_parser/`, `review.rs`, `pipeline.rs`, `html/`, `graphviz/`,
`redacto/` — about 55,000 lines. Nothing in the render closure references them; this was
verified by auditing every `crate::` path in the copied files.

## Deviations

1. **Boa-free imports.** `flattened.rs` upstream does
   `use crate::xfa::scripting::{Presence, SomPath};`. `Presence` is really
   defined in `xfa/mod.rs` and only re-exported through `scripting/state.rs`,
   which imports `boa_engine`. Importing both directly keeps ~9k lines of
   scripting and the JS engine out of the build entirely.

2. **Nothing dropped from `scripting/mod.rs` any more.** Milestone 2 omitted
   `form.rs` (an audit confirmed the fidelity-3 path never reaches it), and
   milestone 4 restored it for state exploration.

3. **Deterministic family lookup.** `FontManager::loaded_fonts` is a `BTreeMap`
   rather than a `HashMap`, and `find_loaded_by_family` / `find_loaded_fuzzy`
   pick the most neutral variant (`variant_neutrality`) rather than the first
   entry. Upstream's version returns an arbitrary variant, and with the UBS
   directory registering three faces under one family (`frutiger`: normal,
   italic, bold) the winner depends on the process's hash seed. This is not
   cosmetic: different faces have different advances → different wrap points →
   different node heights → different `page_breaks` → a different rendered page
   *count* between runs of the same input. Requires `PartialOrd, Ord` on
   `FontVariant`, `FontWeight` and `FontPosture`.
   Covered by `an_ambiguous_family_lookup_resolves_to_the_neutral_face`.

4. **Explicit fallback font.** Upstream's `load_profile_fonts` sets the fallback
   to whichever file the directory yields first — alphabetically
   `frutiger-bold.ttf` — so every unresolvable typeface renders **bold**.
   `fonts::register_dir` chooses explicitly: a caller-named stem, else the first
   face that does not look styled, else the first file. The choice is returned
   so it can be asserted on.

5. **`profiles.rs` not ported; `src/fonts.rs` replaces it.** Upstream compiles a
   ~9 MB `profiles/` tree into the binary with `include_dir!`, of which ~300 KB
   is fonts and the rest is AEM XML rendering never reads. More importantly the
   only profile it ships is UBS, whose Frutiger faces are a commercial Linotype
   typeface with **no licence file anywhere in that repo** — they are not
   redistributed here. Fonts load at runtime from `U2S_FONT_DIR`.

6. **No implicit font environment in tests.** Upstream's `get_font_manager()`
   seeds itself with the UBS profile under `#[cfg(test)]`, so its tests never
   had to bootstrap fonts. That hook is removed: tests register the committed
   fonts with `fonts::register_dir_once(u2s_test_assets::font_dir(), None)`
   and fail, rather than skip, when they are missing. Removing it immediately
   surfaced three tests
   (`test_line_height_scaling`, `test_space_above_scaling`,
   `test_tokenize_paragraph_runs_preserves_bold_across_space_run`) that depend
   on real font metrics without saying so.

7. **`/XFA` packet names preserved.** Upstream's array branch iterates
   `(1..len).step_by(2)`, taking the streams and discarding the packet *names*,
   then concatenates every payload into one buffer. `extract_xfa_packets`
   returns `(name, content)` pairs, which is the difference between handing a
   caller 40 MB and handing it the 200 KB it asked for.
   `extract_xfa_from_pdf_bytes` keeps upstream's concatenating behaviour for the
   parser, which expects the fragment sequence.

8. **`hyphenation` keeps `embed_all`.** The plan intended to trim this to German
   and US English, the only two dictionaries the code instantiates. That feature
   does not exist: `hyphenation` 0.8.4 offers `embed_all` or `embed_en-us` and
   nothing per-language for German. German drives layout on the UBS corpus, so
   dropping it would change rendering rather than just binary size.

9. **`loaded_variants()` added** to `FontManager` — a read-only view of the
   registered variants, so a test can assert *which* face an ambiguous lookup
   resolves to.

10. **The fidelity ladder is explicit** (`src/fidelity.rs`). Upstream's
    `XfaForm::new` runs scripts, applies presence changes, does the three Form
    DOM merges, flattens — and then builds a *persistent* `XfaScriptEngine`
    mirroring the whole SOM hierarchy as JS objects. That last part is only for
    interactive events, so `prepare_default` stops before it. A panicking script
    degrades to the merge-only level with a warning rather than failing the
    render, because a broken script in one field should not make the whole
    document invisible.

11. **`FieldId`, `Selection`, `SelectionKind` (`src/selection.rs`) and the
    exhaustive explorer (`exhaustive::collect_states` and everything it built
    on — roughly 1,150 of `exhaustive.rs`'s original 1,700 lines) were
    removed, not ported forward.** They existed to walk the combinatorial
    product of every control and dedup the results by rendered appearance,
    which interaction made unnecessary: an agent addresses one control at a
    time through `XfaForm::interact` (`materialize`, and a live session in
    `u2s-render-xfa`), and never enumerates anything. `exhaustive.rs` now
    holds only the *discovery* half this walk was always built on:
    `SelectableField`, `SelectableFieldKind`, `search_selectable_fields`,
    `get_all_selectable_fields_ordered`, and the new `field_affects_layout`
    (deviation 14). The dedup machinery this removal takes with it —
    `Flattened::flattened_key`/`FlattenedKey`/`FlattenedKeyKind` and the
    `rayon` dependency — is gone for the same reason: appearance-based
    dedup has no consumer once nothing enumerates.

12. **`XfaError` is aliased as `Error`.** Originally because `exhaustive.rs`
    referred to `crate::Error::FormCreation` in 21 places and the alias kept
    that file verbatim; the explorer is gone now (deviation 11), but the
    alias remains, since `states.rs` uses the same name for the same reason
    on a smaller scale.

13. *(Removed.)* Upstream's rayon-ordered, label-sorted enumeration
    (`states::enumerate`) no longer exists — see deviation 11.

14. **`SelectableField`, `SelectableFieldKind` and
    `get_all_selectable_fields_ordered` made public**, and `FontManager::
    loaded_variants` added. These expose a form's *controls* without exploring
    its state space — the basis of on-demand addressing. `get_all_selectable_fields_ordered`'s
    filter has since narrowed to only the XFA 3.3 §17 access rule (a control
    a person cannot touch either); the script-reachability half of the old
    filter is preserved as its own predicate, `field_affects_layout`, reported
    as data on every listed control rather than applied as a filter — a
    control no script reads is still one a person can click.

15. **The tall-buffer raster cache is an addition, not a deviation** — it lives
    in `u2s-render-xfa` (the facade), not in the ported engine, so it is not
    tracked here in detail. It matters for anyone reading `u2s-xfa` alone: the
    ported `render_to_image_buffer_plain` rasterizes the whole column every
    time it is called, exactly as upstream does. Caching that result across a
    cursor walk is the facade's job, and its correctness depends on choosing
    the *same* reference dimensions (the tallest page, not the whole column)
    for every page in one walk — using the column's total height would clamp
    a short document's resolution based on its unrelated total length. See
    `u2s-render-xfa/src/bands.rs::max_page_height`.

16. **The layout is a stack of whole pages.** Upstream lays a document out as
    one continuous column of content areas and records where it would break;
    the page margins are never in the column, so its "pages" are contentArea-
    sized bands and the master page is drawn once, at the first page's
    coordinates. Here `from_xfa_paged_with` lays the body out, decides the page
    breaks, then shifts each page's content to `k * page.height +
    contentArea.y` and emits the master page once per page, choosing between
    masters by `pagePosition`. The column is exactly `Page::page_count` pages
    tall, so a page is the band `[k*H, (k+1)*H)` and a short last page is still
    a whole page. `Page::page_breaks` is kept, and is now simply the multiples
    of `page.height` inside the column.

17. **Page-dependent master-page scripts are evaluated once per page.**
    `xfa.layout.page()`, `pageCount()`, `pageSpan()` and a `pageArea`'s `index`
    cannot be answered during the first script pass, because the page count is
    a *result* of laying the body out — upstream's stubs threw, which silently
    lost the whole script (a footer that reads `if (!this.rawValue)
    this.rawValue = getPagination(...)` simply stayed blank, and a barcode that
    sets `this.presence = "hidden"` before reading `MP.index` stayed hidden).
    `ScriptExecutor::execute_with_layout` keeps the engine alive and
    `LayoutScripts::evaluate` re-runs the master page's initialize, ready and
    calculate events once per page against it. Body scripts still see page 1 of
    1.

18. **`XfaForm::interact` and `XfaForm::refresh_paged` are additions, not a
    port.** `set_value_as_user` alone fires only Change, which is right for
    replaying a recorded value and wrong for simulating a person: `interact`
    wraps it with `enter`/`exit` (both already existed but were never fired
    outside tests), and dispatches to `select_radio_button` rather than
    `set_value_as_user` for a radio, since only the former deselects the
    button's siblings and sets the exclGroup's own value — without it, a
    selected radio button did not render as selected at all.
    `refresh_paged` is `refresh` with the final `reflatten` swapped for
    `Flattened::from_xfa_paged`, driven by the same `LayoutScripts` deviation
    17 introduced for the default state: without it, a session's page
    footers stayed stamped with whatever the document-wide pass produced,
    since `refresh` has no master-page engine to re-evaluate them against.
    `XfaForm::new_with_layout` exists so a caller gets that `LayoutScripts`
    from the exact same document-wide pass that seeded the form, rather than
    running script execution a second time in a fresh engine.

19. **Dropdown options are read via `dropdown_options()`, not
    `extract_item_values()`, in `states::controls`.** The latter is upstream's
    and returns only the first two values of the first `<items>` element —
    fine for a checkbox's on/off pair, wrong for a dropdown, where it silently
    truncated every list past two entries. `dropdown_options()` already
    existed, pairing every entry's display and save value and handling
    `<items save="1">`; `controls` now uses it and reports both.

Not yet reached: `render_labelled` is not ported (it needs the excluded analysis
pipeline).

## Known characteristics (upstream behaviour, not deviations)

- **`FlattenedNode::Field.label` mirrors `name`; it is not the caption.**
  Verified against the real corpus: on `AAAA_019_DE.pdf`, zero of 90 fields have
  a label distinct from their name. Human-readable captions appear as separate
  `Text` nodes. Page-text extraction must therefore read `Text` content plus
  field *values*, and treat `label` as an identifier.
- **Rendering produces one tall buffer**, now `Page::page_count` pages tall,
  sliced by `slice_into_pages` into one band per page. Per-page is the crop, not
  the render.
- **`Page::height` means different things by source.** For XFA it is the first
  `pageArea`'s height; upstream's AcroForm path sets it to the *sum* of all page
  heights. Only the XFA path is ported, but do not assume the field means "one
  page" if the AcroForm path is ever added.
- **`from_xfa_simple` degrades silently without fonts** — a failed metrics
  lookup falls back to `approximate_text_bounds`, producing wrong heights and
  wrong page breaks rather than an error. `render_to_image_buffer_plain` does
  fail loudly. This asymmetry is why the server refuses to boot without fonts.
