# UBS XFA test corpus

42 real UBS AEM Adaptive Forms XFA templates. 41 are copied verbatim from
`ajila-forms-conversion-engine/core/input/` (upstream commit
`7a7a18ded28122039f921c7645dcf1a07688a1ae`), confirmed clear to commit; one
(`AAJB_033_IT.pdf`) is copied from this repo's own `pdf-screenshots/aajb/`,
already committed there with an Acrobat reference capture. These are blank
form templates — no filled-in customer or personal data.

Filenames follow the upstream naming scheme `<4-letter code>_<019|033|
001>_<DE|EN|SP|IT>.pdf` (form code, form group, language). Resolve them in
tests via `u2s_xfa::corpus::form("<name>.pdf")` / `u2s_xfa::corpus::dir()`
rather than hardcoding this path.

## Selection

Chosen from the 190 genuine XFA forms in the upstream `core/input/` directory
to cover, at once:

- all three form groups (`019`, `033`, `001`) and all four languages
  (DE/EN/SP/IT);
- a control-density spread, from forms with no interactive controls at all
  (`AACS_019_EN`, `AANE_019_SP`, `AADQ_033_IT`, `BBDO_019_DE`, `AACE_019_DE`)
  to the densest in the corpus (`AABK_019_DE`: 129 exclGroups / 297 radios /
  25 checkboxes / 569 fields);
  dropdown-heavy forms (`AACB_033_IT`, `AACR_019_DE`, `AAKO_019_DE`, …) and
  checkbox-only forms with no radios (`AALQ_019_DE`, `AALP_019_EN`,
  `BAGQ_019_SP`);
- every form the pre-existing (now-migrated) test suite already named by
  absolute path: `AAAA_019_DE/EN/SP`, `AAAB_019_DE`, `AAAI_019_DE`,
  `AAAL_019_DE`, `ACAV_001_DE`;
- every form a ported upstream test case names specifically (see
  `crates/u2s-render-xfa-mcp/tests/ported_ubs_cases.rs`): `AAOE_033_IT`
  (dropdown), `AAKS_019_DE` (radio group), `AAEI_019_DE` (field labels),
  `AACJ_019_DE` (dropdown), `BBDO_019_SP` (address-label alignment).

## Fonts

These forms are authored against the commercial "Frutiger" family. The
needed font files are vendored separately at
[`vendor/fonts/ubs-frutiger/`](../../vendor/fonts/ubs-frutiger/README.md) —
see that directory for the licensing note. Without them (`U2S_FONT_DIR`
unset and `vendor/fonts/ubs-frutiger/` absent), rendering falls back to the
DejaVu metrics in `vendor/fonts/`, which is fine for structural tests
(control discovery, positions, interaction) but not metric-faithful to the
original design.

## Adding a form

The set is fixed; add a row above and a one-line reason when adding a form,
rather than growing this directory unboundedly.

- `AAJB_033_IT.pdf`: exercises a `keep next="contentArea"` on a plain
  section-heading `<draw>` (not a wrapper subform) that must move with the
  unsplittable positioned subform following it — see the Acrobat capture
  under `pdf-screenshots/aajb/` and `facade.rs`
  `a_heading_with_keep_next_moves_with_its_question_block`.
