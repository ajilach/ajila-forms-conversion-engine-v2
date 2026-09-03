# UBS AEM Output — Agent Ruleset

## 0. The Prime Directive

- **Match the hand-built UBS reference form exactly.** When in doubt, do not
  invent structure — diff against the closest reference form (AAJC, AAHO,
  AAKI, AAAI, …) and reproduce its node names, ordering, and exclusion flags.
  Multiple commits exist *solely* to drop spurious nodes or re-order children to
  match the reference (e.g. `c81beb0`, `66fc9ce`).

---

## 1. Naming

- **R1.1** Name every component `PREFIX_<CamelCaseName>_<shortUuid>`; names must
  be unique within the form because rules reference them by name. (`9cde57b`)
- **R1.2** Use the exact prefixes from
  [AEM Naming Conventions.md](AEM%20Naming%20Conventions.md). Do not invent or
  abbreviate prefixes. (`9cde57b`)
- **R1.3** Conditional and grid-layout wrapper panels use `PN_` (not the old
  `COND_` / `GRID_`). (`2bc6d9d`, `a74a45f`)
- **R1.4** The repeatable *item container* uses `RP_` (not the old `RPT_`).
  (`2bc6d9d`)
- **R1.5** The inner repeat-container panel referenced by button scripts uses
  `RCP_<name>` — **not** `PN_`. Button scripts target this exact name. (`9cde57b`)
- **R1.6** When a custom element replaces a panel, the emitted tag reuses the
  **original panel's name**, not a generated `custom_<uuid>`. (`3869ac2`)

---

## 2. Form Structure & Ordering

- **R2.1** Root wizard order is fixed: generated section pages (`{{ children }}`)
  → optional `summaryPanel` → `previewpanel` → `toolbar`. Do not reorder.
  (`88b7eed`, `34123e3`; `root.xml`)
- **R2.2** **Preamble** (content before the first H2) is *prepended to the first
  H2 section's children* — it does **not** get its own page panel. Only when the
  document has no H2 at all may preamble become a standalone untitled page.
  (`b6a2294`)
- **R2.3** Each H2 heading starts a new page panel (`is_page=true`); the heading
  text becomes the step title. (`converter.rs`)
- **R2.4** Remove empty non-page panels after all transformations (a final
  retain-if-children-non-empty pass). (`3ac2e61`)
- **R2.5** Inside a repeatable panel the **remove button** (`BT_Remove`) comes
  **before the content panel** in the items list, never after — matching AF
  Design Guidelines §6.2.3 (the "Buttons" panel is added to the Content section
  ahead of the form components). (`repeatable.xml`)
- **R2.6** The H2/section-title detector regex must stay **tolerant**
  (case-insensitive, flexible whitespace, optional parenthetical) so minor
  textual variation still matches a section. (`c0b233d`; `config.toml`)

---

## 3. Step Titles

- **R3.1** A **page panel must NOT carry `jcr:title`**. `jcr:title` is emitted
  only for non-page panels (`{% if not is_page %}`). (`0d82c14`; `panel.xml`)
- **R3.2** A page panel must NOT carry `css="stepTitle"` on the panel element.
  The `stepTitle` class belongs on the generated title `titledraw` child only.
  (`0d82c14`)
- **R3.3** For each titled page panel, generate a `{{name}}Title` sub-panel as
  the **first child**, containing a `titledraw` named `TTL_{{name}}` with
  `css="stepTitle"`, `headingLevel="2"`, `dorExclusion="true"`,
  `summaryExclusion="true"`. (`0d82c14`)
- **R3.4** `PN_FormConfigurator` uses `css="ubs-margin-10"` only — never the
  `stepTitle` token. (`beef051`)
- **R3.5** Conditional panel titles use the human-readable **field label**, not
  the raw ID/UUID: `"Condition: {label} = {value}"`, falling back to the field
  name only if no label exists. (`3cf32e9`)

---

## 4. Toolbar

- **R4.1** Toolbar item order is fixed: `nextitemnav`, `submit`, `previtemnav`,
  `preview`. (`root.xml`)
- **R4.2** There is **no "Save Progress" button** (`fwbSaveProgress` was
  removed). Save-as-draft is expressed in the DAM asset via
  `menuOptions="[ajila-forms-ubs-menu-option-save]"`. (`048ad49`; `dam.xml`)
- **R4.3** All toolbar buttons carry `dorExclusion="true"`. (`root.xml`)
- **R4.4** When `use_summary == "true"`, the `nextitemnav` button's `fd:click`
  prepends `window.ajila.forms.ubs.control.summary.setSummaryData(guideRootPanel);`
  before the standard `nextStep(this)` script. (`88b7eed`)

---

## 5. Preview & Summary

- **R5.1** The **preview panel is ALWAYS emitted** for UBS, regardless of
  `use_summary`. Do not re-wrap it in a conditional. (`34123e3`)
- **R5.2** `previewpanel` carries `summaryExclusion="true"`. (`root.xml`)
- **R5.3** When `use_summary == "true"`: emit a `summaryPanel` titled
  "Summary of form information" containing `messagebox_ElsigCheck`, a
  `summaryComponent` (`replaceEmptyValues="true"`, `showStaticText="true"`), and
  `submitErrorMessage`; and set `redactoSummary="true"` on the DAM asset.
  (`88b7eed`)
- **R5.4** The hidden `doroptionsubs` and `metadata` control nodes live inside
  the `summarypanel` (not `previewpanel`), and each carries `dorExclusion="true"`
  and `visible="false"`. (`e6f2f80`)

---

## 6. Document of Record (DoR)

- **R6.1** Do **NOT** set `excludeFromDoRIfHidden="true"` on the root container —
  hidden fields must remain in the DoR. (`00a4cd7`)
- **R6.2** Every page panel carries `dorExcludeTitle="true"` and
  `dorExcludeDescription="true"`; non-page panels carry
  `dorExcludeDescription="true"`. (`panel.xml`)
- **R6.3** For `PN_FormConfigurator`, `dorExclusion="true"` sits on the generated
  `{{name}}Title` sub-panel, **not** on the parent panel (excluding the whole
  subtree was a bug). The parent keeps only `dorExcludeTitle` /
  `dorExcludeDescription`. Configurator panels also carry
  `summaryExclusion="true"`. (`23a438d`, `b949801`)
- **R6.4** Generated title `titledraw` (`TTL_*`) and the remove button
  (`BT_Remove`) carry `dorExclusion="true"`. (`0d82c14`, `66fc9ce`)
- **R6.5** Fragment panels are excluded from the DoR (`dorExclusion="true"`,
  `dorExcludeTitle`, `dorExcludeDescription`). (`08968b2`)
- **R6.6** DoR field styling flows from `variables.dor_field_styling`; apply it
  where the templates do (e.g. preface, fragments). (`78846f8`)
- **R6.7** Select the meta DoR template by entity:
  `019` → `UBS_General_Germany_DOR.xdp`, `033` → `UBS_General_Italy_DOR.xdp`,
  else `UBS_Blank_DoR.xdp`. (`7208d8a`)
- **R6.8** The DoR address block's `senderAddressTitle` value is
  `"Banking Relationship"`. (`0caa7d2`)

---

## 7. Fragments

- **R7.1** Run fragment replacement only when `use_fragments = true` **and** at
  least one fragment parsed; it requires `fragment_xsd_ref`,
  `fragment_ref_prefix`, and an XSD config with `registered_types`. (`08968b2`)
- **R7.2** Match fragments **by XSD type**, not by name: map each panel child's
  `bindRef` leaf to its registered XSD type, then pick fragments whose
  `fragmentModelRoot` type is among them. (`08968b2`)
- **R7.3** Require meaningful overlap: a fragment matches only if the panel
  leaves cover at least half (ceiling) of its `bound_elements`; in strict mode,
  all panel leaves must be ⊆ the fragment's `bound_elements`. This stops a
  generic name like `Name` from matching a 3-element fragment. (`0e69f02`)
- **R7.4** Tie-break deterministically: (1) highest overlap, (2) most specific
  XSD type, (3) most `bound_elements`. (`0e69f02`)
- **R7.5** **Insert fragments at the position of the replaced fields — never
  append at the end.** Only fall back to appending when no position was
  recorded. (`074b603`)
- **R7.6** Match parent panels **outer-first, then recurse**, so a parent sees
  all descendant fields before an inner panel can consume them. (`0e69f02`)
- **R7.7** Emit **N fragment instances** for repeated bound elements (N = min
  per-element occurrence across leaves). N=1 replaces the whole panel; N>1
  replaces the panel's children with N Fragment nodes. (`0e69f02`)
- **R7.8** **Never replace conditional panels, or panels containing
  conditionals**, and never remove conditional children when replacing siblings
  — conditional names are referenced by visibility scripts. Do not "hoist"
  conditionals out (that approach was reverted). (`c7bdd7f`, `848eaac`)
- **R7.9** Place fragments **inside** Repeatable/Conditional wrappers, not over
  them: put Fragment node(s) into the wrapper's children, preserving the
  add/remove wrapper. (`fb3e884`)
- **R7.10** Rewrite fragment `bindRef` from `/<form_root>/...` to the generic
  `/<fragmentBindRefPrefix>/...` (e.g. `UBSAF`) so fragments are reusable across
  forms. (`07cd62c`)
- **R7.11** The repeatable **inner panel owns the bindRef**, not the wrapping
  section panel; emit `bindRef` only when set; `strip_bind_refs` clears it on
  Repeatables when `bind_to_xsd` is off but **always keeps it on Fragment
  nodes**. (`07cd62c`)
- **R7.16** A candidate type may declare **element aliases** (`[typeAliases.<Type>]`
  in the XSD profile), applied to the resolved leaf while that type alone is
  tested. Label resolution is context-free, so `Ort` / `Località` always resolve
  to `Place` — correct in a signature row (`SignatureType.Place`), wrong for the
  city of an address — and `N.` resolves to a bare `N` that no type declares.
  Two of an Italian address's five leaves therefore sat outside `AddressType`
  and it could match nothing. `AddressType` aliases `Place` → `City` and
  `N` → `StreetNumber`; every other type, and the default resolution, is
  untouched. The profile is `rename_all = "camelCase"`, so the table is
  `typeAliases` — a snake_case key is silently ignored.

- **R7.15** A fragment also matches a **run of consecutive sibling children**,
  not just a whole panel. Whole-panel matching fires only when a section holds
  the fragment's own fields and nothing else, and a layout row holds too few of
  them to clear R7.3's floor — so an address sitting among other fields was
  matched by neither. A run stops at a conditional or a repeatable (R7.8/R7.9),
  matching stays strict, and the fragment lands where the replaced fields were
  (R7.5) because the run is what it replaces. Corpus effect: `affrg_AddressGeneric1`
  9 → 71 instances across 25 of 110 forms, every other fragment count unchanged.

- **R7.14** The internal-bank-use block uses
  `afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature`
  in **both** markets. The corpus migrated: of the 78 Italian packages issued
  2026-09-01, 57 use the global fragment and 4 the Italian one (three
  un-migrated, one legacy); the German reference AABF_019 uses the global one
  too. `specs/feedback/consistent-problems.md:663` records the pre-migration
  counts and `:721` an owner decision not to re-route this family — both are
  dated 2026-08-27 and predate the migration, so do not restore them from that
  snapshot. Pinned by
  `custom_templates_use_the_global_internal_bank_use_fragment`.

- **R7.13** A fragment the converter derives from a source must survive into
  the shipped tree. `review_output`'s `dropped_fragments` re-converts the same
  input and reports every `fragRef` the reviewed tree references fewer times
  than the engine's own conversion does, so a hand-authored rebuild of a
  fragment-backed section cannot pass silently. Needs no per-form list: the
  deterministic conversion is the authority.

- **R7.12** Point all custom-element fragRefs at the shared
  `afforms_ubs_fragmentlib` (not per-country libs), and use the single generic
  `affrg_SignatureGeneric1` (not per-role signature fragments). (`3bbdd5f`)

---

## 8. Banking Relationship (Preface)

- **R8.1** Inject the entity-conditional banking-relationship fragment as the
  preface (first item of the first page panel). Its `fragRef` is selected by
  `xfa.formrange_entity`: `019` → Germany lib, `001` → UBS/CH lib, else → Italy
  lib. (`e2674f1`)
- **R8.2** Wrap it in a `PN_BR` panel: the exclusions (`dorExclusion="true"` +
  `summaryExclusion="true"`) go on the surrounding `PN_BR`, with
  `PN_BankingRelationship` (carrying the `fragRef`) nested inside — mirroring the
  AAJC reference, not putting exclusions on `PN_BankingRelationship` itself.
  (`621da8d`)

---

## 9. Custom Elements

- **R9.1** Match custom elements by field label (`TextField`/`Dropdown`) or
  panel title (`Panel`) via regex; first-match-wins per node. (`3e81862`,
  `3ac2e61`)
- **R9.2** Match against **all language variants** of a section title, so merged
  multi-language forms still match. (`3ac2e61`)
- **R9.3** For an `is_page` panel match, keep the page/wizard-step wrapper and
  replace only its **children** with one Custom node; for non-page nodes,
  replace the node entirely. (`3ac2e61`)
- **R9.4** Force full width on custom elements: `colspan = 12` and
  `dor_colspan = 12`, regardless of source span. (`2b90719`)
- **R9.5** Gate custom elements on `depends_on`: discover matches first, then
  iteratively drop any rule whose `depends_on` templates aren't all matched, to
  a fixed point (transitive drops propagate). This prevents scripts/visibility
  rules from referencing names a missing template would have produced.
  (`3e81862`)
- **R9.6** Treat dependency **cycles as all-or-nothing**: the fixed point keeps a
  cycle only when every member matches; otherwise the whole cycle drops.
  (`c9108e8`)
- **R9.7** Concrete UBS dependency graph (keep wired exactly): `account_holder`
  ↔ `signatures`, both depend on `formular_adressat_radio`; Italian
  `account_holder_it` ↔ `signatures_it` ↔ `tipo_radio` (mutually circular).
  (`3e81862`, `c9108e8`)
- **R9.8** Keep German/English vs Italian account-holder & signature variants
  **separate**: German+English use `^Kundendaten$`,
  `^(Signature\(s\)|Unterschrift\(en\))$` driven by `RB_FormularAdressat`;
  Italian uses `^Dati del/i cliente/i...$`, `^Firma/e$` driven by `RB_GroupTipo`.
  Addressee radio matches `^(Formular Adressat|Form addressee)$`. (`3e81862`,
  `2b90719`)
- **R9.9** Account-holder visibility rules compare `RB_FormularAdressat` against
  **textual** values (`Private Person`, `Minderjährige`, `Firma`/`GbR`), not
  numeric codes `1`/`2`/`3`/`4`. (`ac7396b`)
- **R9.13** A cluster member may declare `inject = "append"` / `"step"`: when
  its own pattern matches nothing but every `depends_on` template does, the
  block is placed rather than dropped — appended to the page named by `page`,
  or inserted there as a new wizard step. `account_holder` and `signatures`
  use it, so an addressee-driven form gets both blocks however it words (or
  omits) those section headings; the configurator stays match-only and remains
  the cluster's trigger. Without this, R9.6 dropped the whole cycle on any form
  that does not spell out `Kundendaten` / `Unterschrift(en)`, leaving a bare
  optional dropdown and hand-built person blocks. (AABF_019)

- **R9.15** A custom template's conditional container pairs `this.visible` with
  `window.forms.ubs.showAFShowDor(this)` / `hideAFHideDor(this)`, the same idiom
  `conditional.xml` already emits — without it a hidden addressee branch still
  prints into the DoR. The outer `PN_AccountHolder` is the exception and carries
  no toggle (AABF_019). Add-button instance caps read
  `<panel>.instanceManager.maxOccur`, never a hardcoded count, since R11.1 sets
  `maxOccur` from the node. Each add button targets the remove button its own
  fragment exposes: `affrg_ContractualPartnerGeneric1` has `BT_RemoveCPG`,
  `affrg_PartnertoPartnerGeneric1` has `BT_RemoveAHG`.

- **R9.14** An `inject = "append"` block goes on the page holding one of its
  `depends_on` templates, falling back to `page` only when none is on a page of
  its own. The account holder belongs on the configurator's step, and the
  configurator is page 0 in one form (AABF_019) and page 1 in the next
  (AAOV_033), so a fixed index puts it on the wrong step. Both Italian and
  German cluster members declare `inject`; keying it to one lineage left the
  other dropping the whole cycle.

- **R9.10** Cross-link add/remove buttons between account-holder and signature
  panels (`BT_Add`/`BT_AddLR`/`BT_RemoveLR` call `addInstance`/`removeInstance`
  on both the holder panel and its matching signature panel), cap at 4
  instances, remove button visible only on the last of >1 instances. The
  legal-representative panel is `PN_ARP` (renamed from `PN_LRP`). (`3e81862`)
- **R9.11** Add the legal-entity section's `fd:init` visibility scripts (hide
  `PN_EntityBasic`, `PN_Address`/`PN_AddressClient`, `DATE_Birth`,
  `DD_Nationality`; Italian variant also sets `dorExclusion=true`) targeting the
  fully-qualified `...PN_LRP`/`...RCP_LR` path. (`24167e0`)
- **R9.12** Match the reference exactly — drop spurious nodes (e.g. remove the
  extra `BT_RemoveLR` from the LE section and the `IS_NOT_EMPTY` rule from the
  IT template). (`c81beb0`)

---

## 10. Components

- **R10.1** Numeric inputs use `guideNodeClass="guideNumericBox"` — **never**
  `guideNumberBox` (the latter fails AEM validation). (`35e7ba5`)
- **R10.2** Map a `CheckboxGroup` to a checkbox with
  `guideNodeClass="guideCheckBox"`, vertical alignment, and a translatable group
  `label` emitted as `jcr:title` only when non-empty. (`de644ce`)
- **R10.3** Output a table as **one HTML component**
  (`controls/htmlDisplayer`), named `TBL_`, whose `localeContent` carries a real
  HTML `<table>` per locale — `<thead>`/`<th>` for the header row,
  `<tbody>`/`<tr>`/`<td>` for the body. Supersedes the earlier rule (a single
  panel with every cell emitted linearly as a child paragraph), which existed
  only because AEM had no table component.
- **R10.3a** A table whose cells hold input fields keeps the older panel shape:
  the HTML component is static markup, so a field routed through it would be
  lost. The converter checks this and falls back with a warning.
- **R10.3b** The HTML component does **not** use the Sling i18n dictionary. Its
  markup lives per locale on the node, one positional `item{N}` child per locale
  the form ships (synonyms included, for the same reason a synonym dictionary
  file is written). The language → locale spelling is `[html_locales]` in the
  profile.
- **R10.6** Source content marked `relevant="-print"` is a **notice**, not
  furniture: it becomes `controls/messagebox` (`TB_` prefix, `messageboxType`
  `{Long}2`, `hideTitle`), carrying `dorExclusion` and `summaryExclusion`
  because it addresses whoever fills the form and never the printed document.
  Only static text qualifies — a screen-only button or input is still dropped.
  A draw laid out repeatedly is one notice, and a draw whose text spans several
  leaves is joined back into one. Before this the engine dropped every such
  element, so the text had to be retyped into AEM by hand (the reference form's
  `TB_Info`). (AABF_019)

- **R10.5** Two sibling inputs may not carry the same `jcr:title`. Nothing
  tells them apart afterwards — on screen, in the summary or in the DoR — and
  it is what filling one stand-in title into a group of caption-less fields
  looks like (AABF_019 shipped seven date pickers in one panel, all titled
  "Date"). `review_output` reports them as `label_issues` of kind `duplicate`.

- **R10.4** Drive each field's `mandatory` attribute from the source field's
  `required` flag; do not hardcode. (`8c85784`, `e7a0034`)

---

## 11. Repeatables (button behavior)

- **R11.1** Set `minOccur` / `maxOccur` on the repeatable panel from the node's
  min/max. (`2bc6d9d`)
- **R11.2** Generate add/remove button scripts **inline in `repeatable.xml`**,
  not precomputed in Rust. (`7e43650`)
- **R11.3** Remove-button `fd:click`: remove the instance, then loop instances
  setting `BT_Remove.visible = (i === len-1 && len > 1)`, and when
  `len < maxOccur` restore the add button via `<name>.BT_Add.visible = true`.
  (`a0344e9`, `cd085b5`, `2fbe488`)
- **R11.4** Reference the add button as `<name>.BT_Add.visible` (the
  repeatable's own name) — **not** `RCP_<name>.BT_Add` and **not**
  `instances[len-1].BT_Add`. (`cd085b5`, `2fbe488`)
- **R11.5** Hide the add button (`this.visible = false`) once `len >= maxOccur`
  in the add-button `fd:click`/`fd:init`. (`7e43650`, `a0344e9`)

---

## 12. Translations & Script Escaping

- **R12.1** Escape every `jcr:title` value and every field `label` with
  `xml_escape` before insertion. (`072e157`, `de644ce`)
- **R12.2** Escape option `value` and `label` in the `options="[...]"` attribute
  with `xml_escape`. (`a1bfd61`)
- **R12.3** **Additionally** JCR-escape option values/labels for the
  comma-separated list: `\` → `\\` and `,` → `\,`, so the list splits on
  unescaped commas only (e.g. `options="[1=Yes\, definitely,2=No\, thanks]"`).
  Applies to radio buttons, dropdowns, checkboxes. (`9c350a1`)
- **R12.4** Register the H1 translation key HTML-wrapped as `<p>…</p>` (it
  becomes `guideformtitle` `_value`), not plain text. Register **both** keys for
  an H2 (plain-text for panel `jcr:title` + HTML-wrapped for the titledraw
  `_value`). H3+ headings register HTML-wrapped; plain field labels register as
  plain text. (`bed8998`, `006b750`)
- **R12.10** A static text that says exactly what the input beside it already
  says is dropped on the way into the writer (`normalize.rs`). R12.9 fixes this
  at the source, but only for a tree the engine converted itself; an authored
  or loaded tree arrives already shaped, and authoring the caption twice — once
  as a draw, once as the field's title — is what a model does when the source
  shows it twice. Immediate siblings only, exact match after stripping markup.
  (AABF_019 `ST_Amount`)

- **R12.9** A colon-terminated static text stranded beside a field that has no
  label becomes that field's label, and is dropped. Positional label
  attachment leaves such a caption unclaimed when it sits too far from its
  field, and the output then renders it twice: a static draw and an input
  title saying the same thing. Captions preceding their field are paired one
  by one; a group closing with K captions preceded by K unlabeled fields is
  paired positionally as a whole, since one at a time would attach each
  caption to the wrong field. In a grid the caption is emptied in place, never
  removed, because the columns are positional. (AABF_019)

- **R12.8** A colon-terminated static text between a choice control and its
  inline field is the **field's caption**, and becomes the field's label on its
  own; the control's own label stays with the control, which renders it. Only a
  text with no caption marker continues the label (`Nur für Benutzer Nr. [__]`).
  Prefixing gave one field a different name per option
  ("Neuer Auftrag Währung:", "Änderung Währung:") where the reference says
  "Währung:" once. (AABF_019)

- **R12.5** Use `Option<String>` for missing translations; merge orphan
  paragraphs (a non-master language entry the master lacks) into a single
  static-text element so **no missing-translation dictionary entry** is
  produced. (`f1d48bb`, `367f1c1`)
- **R12.6** Inside embedded `script` content (`fd:click`/`fd:init`/`fd:visible`),
  **double-escape** quotes and newlines: comparison literals use `\\&quot;` (not
  `\&quot;`) and newlines use `\\n`; the visual-editor `STRING_LITERAL` value
  stays plain text. (`29b9e93`, `7f84f6d`)
- **R12.7** Emit the address `fd:init` script (the `hideAFHideDor` calls + country
  lookup) only when the fragment's `frag_ref` ends with `affrg_AddressGeneric1`;
  otherwise emit an empty `<fd:scripts>`. (`b20adfc`)

---

## 13. XML Validity

- **R13.1** The form `.content.xml` must begin with
  `<?xml version="1.0" encoding="UTF-8"?>`. Without the UTF-8 declaration CRX
  falls back to ISO-8859-1 and corrupts umlauts/Sonderzeichen. (`ed8f128`)
- **R13.2** All generated forms must pass XML syntax validation — no unescaped
  attribute values. Run the validation tests before considering output changes
  done. (`a1bfd61`, `072e157`)

---

## 14. Packaging

- **R14.1** Deduplicate every ZIP entry by path through a shared
  `HashSet<String>` (skip already-written paths), including intermediate
  `.content.xml` folder nodes, to avoid duplicate-filename errors. (`074cd19`)
- **R14.2** Emit `bindRef="{{ bind_ref }}"` only when `bind_ref` is set; populate
  bindRefs by precomputing XSD paths when `bind_to_xsd` is true. (`36da73c`)
- **R14.3** When `bind_to_xsd` is true and `xsd_path` is non-empty: add the XSD
  path as a filter root in `filter.xml` (CRX ignores content outside filter
  roots) and write intermediate `.content.xml` folder nodes for each XSD
  directory segment under `content/dam/formsanddocuments/`. (`235ca97`, `09dd68b`)
- **R14.4** Drive the DAM asset's `formmodel` (`"xsd"` vs `"none"`) and `xsdRef`
  from whether `xsd_path` is non-empty — **not** from `bind_to_xsd`; emit
  `xsdRef` only when an XSD path exists. (`09dd68b`)
- **R14.5** Do not require `xsd_path` when `bind_to_xsd=true` — treat a missing
  path as empty rather than erroring. (`09dd68b`)