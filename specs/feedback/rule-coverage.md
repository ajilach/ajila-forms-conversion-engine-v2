# Rule coverage

The feedback repo's CI guard (`scripts/check_feedback_rules.py`, which runs the detectors of
`../ajila-forms-conversion-feedback/.claude/scripts/find_*.py`) is the acceptance test for the AEM
output, because a converted form joins the corpus the guard polices. This map lists every problem
the guard enrols (the entries of `consistent-problems.md` whose `Runs automatically:` is `yes`,
which is what `check_regressions.swept_problems()` returns: 35 today) and what guarantees each one
here. A guarantee is one of: a check rule in `rules/aem/`, a normalize pass of the UBS layer
(`u2s/crates/u2s-aem-ubs-mcp/src/aem/normalize.rs`), a template under
`u2s/crates/u2s-aem-ubs-mcp/profiles/ubs/aem/`, a writer function (`xml_writer.rs`,
`package_writer.rs`), or an exception accepted by decision (AGENTS.md). Keep this file in step with
`rules/aem/`: a new or removed rule changes a row here.

Reading the "guaranteed by" column: a rule is checked on every edit of the document (`rule_check`),
so it covers what an author can get wrong; a template, writer function or normalize pass makes the
shape hold by construction, so no check is needed; the parity and writer tests of the UBS layer
hold those to the retired engine's output. Where both appear, the template covers the generated
shape and the rule covers what a loaded or authored node can still carry.

What templates and the writer guarantee is checked as well, on the package every build writes:
`agent/src/package_checks.rs` scans the rendered form XML for the problems marked "package check"
below (and for checkbox `richTextOptions` and the jump-to-field button, which the guard no longer
enrols). `build_aem_package` lists a finding under ENGINE DEFECTS and `rule_check` as
`package_findings`: it is a template or writer regression, which no edit of the document fixes.

| Problem | The detector checks | Guaranteed by | Why |
|---|---|---|---|
| PROBLEM-banking-relationship-fragment | every node whose `fragRef` holds `BankingRelationship` (custody account excluded) has the canonical `/content/forms/af/afforms_ubs_fragmentlib/affrg_BankingRelationship1` and `dorExclusion="true"` | `preface.xml` (writes the canonical fragment in `PN_BR` with the exclusions); `fragment.xml` (adds `dorExclusion` to that fragment); rule `ubs-aem-banking-relationship` | The `Preface` node owns the banking block; a hand-authored Fragment can still carry a market path, which the rule reports. |
| PROBLEM-dor-exclude-hidden | no tag carries `excludeFromDoRIfHidden="true"` | `root.xml` (its `dorProperties` never writes the attribute); rule `ubs-aem-passthrough-attributes` | A typed document cannot say it; only a loaded node's passthrough can carry it into the package. |
| PROBLEM-dor-custom-template | `metaTemplateRef` is not the blank DoR template on a 019/033 form, and `AF_FORM_TITLE` takes `valueFrom="formTitle"` | `config.toml` `meta_template_ref` (entity General templates) and `root.xml` (`AF_FORM_TITLE` valueFrom); rule `ubs-aem-variables` | The template follows `formrange_entity`; the variables rule keeps the entity well formed so the choice is not silently skipped. |
| PROBLEM-step-title-panel | each wizard step's heading lives in a `{step}Title` panel, not in an unwrapped h2 or a panel `jcr:title` | `panel.xml` (builds the step-title panel from a page's `title`); rule `ubs-aem-step-titles` | The title is generated from the page, so only a non-page step, or an authored heading in an untitled page, breaks it. |
| PROBLEM-panel-type-ubs | no node has `sling:resourceType="fd/af/components/panel"` | every panel template (`panel.xml`, `conditional.xml`, `fragment.xml`, `repeatable.xml`, `preface.xml`) writes the UBS panel; rule `ubs-aem-passthrough-attributes`; package check | A raw attribute of that name is dropped by the template, so only a raw child element could carry the default panel in. |
| PROBLEM-nav-button-order | the toolbar buttons are Next, Submit, Back | `root.xml` (the toolbar is fixed) | Not authorable. |
| PROBLEM-footnote-modernize | a form has footnote references and a placeholder together; no legacy `ubsAccordionFootnote` that converts cleanly | `footnoteplaceholder.xml`; rule `ubs-aem-footnotes` (references without a placeholder, a placeholder without references) | A document is free HTML, so it can carry one without the other. The legacy accordion shape is not checked: the detector counts it only when every marker resolves, which needs the rendered markers. |
| PROBLEM-infobox-dor-copy | the on-screen infobox is out of the DoR and summary, and a hidden copy with `alwaysInPdf` and `summaryExclusion` sits on the last content step | `normalize.rs` `copy_infobox_into_the_dor`; rule `ubs-aem-infobox-dor-copy`; package check | The pass repairs a visible infobox and adds a missing copy; it leaves a hidden copy the document already carries, which the rule holds to the shape. |
| PROBLEM-metadata-name | the metadata control is named `metadataTextDraw`, not `metadata` | `root.xml` (`name="metadataTextDraw"`) | Not authorable. |
| PROBLEM-formcode-page-title | the page title equals `formrange_code` | `root.xml` (`jcr:title="{{ form_code }}"`); rule `ubs-aem-variables` | The title is the variable; the rule keeps the code in the detectors' `[A-Z0-9]{3,6}` shape, since a form without a match is skipped. |
| PROBLEM-default-translations | the form dictionaries carry the default UI translations | `package_writer.rs` (`assemble_package` merges `translations/{de,en,fr,it,sp}.toml`) | Written for every package, per language the form ships. |
| PROBLEM-formconfig-private-person-default | a configurator choice with an individual option opens on it | `xml_writer.rs` `collect_preselections` with `radiobutton.xml` (writes the option's `_value`) | Decided from the option labels, not authored. |
| PROBLEM-save-as-draft-menu | the DAM asset's `menuOptions` holds the save option | `dam.xml` (`menuOptions="[ajila-forms-ubs-menu-option-save]"`) | Not authorable. |
| PROBLEM-formmetadata-step | the FormMetadata fragment is the first step, hidden and DoR-excluded | `root.xml` (`fragment_formmetadata`) | Not authorable. |
| PROBLEM-step-title-single | exactly one `stepTitle` marker per step, on the first h2 | `panel.xml` (stamps the one marker); rule `ubs-aem-step-titles` (no authored node carries `stepTitle`) | A second marked node makes the step render two titles. |
| PROBLEM-signature-panel-ref | a signature name-calc's identifier resolves to one repeatable panel | `xml_writer.rs` `collect_signature_twins` (pairs a party and its signature by name, so the Add and Remove buttons name real panels); no name-retrieval calc is written | The only calc of that shape came with the retired market signature fragments, which `ubs-aem-retired-market-fragments` refuses. |
| PROBLEM-banking-relationship-margin | every `PN_BR` tag carries `ubs-margin-20` | `preface.xml` (writes `css="ubs-margin-20"`); rule `ubs-aem-banking-relationship` (a `PN_BR` node without the class) | An authored `PN_BR` panel has no template default for the class. |
| PROBLEM-toolbar-nav-handler | Submit and Back carry the UBS navigation scripts | `root.xml` (the toolbar and its scripts are fixed) | Not authorable. |
| PROBLEM-datepicker-current-date-default | no tag carries `defaultToCurrentDate="true"` | `datepicker.xml` (never writes it); rule `ubs-aem-passthrough-attributes` | Only a loaded node's passthrough can carry it. |
| PROBLEM-visible-on-init | every visibility rule has an Initialize rule with the same condition | `conditional.xml` (writes `fd:visible` and `fd:init` together for a conditional panel); rule `ubs-aem-passthrough-attributes` (a raw `fd:scripts` with `fd:visible` and no `fd:init`) | A condition is authored as `conditions`, which the template writes both ways; only a loaded raw script can lack the init half. |
| PROBLEM-address-city-country-mandatory | an address fragment's City and Country are not required | `fragment.xml` with `address_init` and `address_generic_init` in `config.toml` (the Initialize rule calling `setMandatory(..., false)`) | Written per address fragment. |
| PROBLEM-configurator-reset-on-change | every approved configurator choice carries the reset script over the panels it shows | `xml_writer.rs` `collect_configurator_resets` (writes the reset); rule `ubs-aem-configurator-panels` (the wiring the writer needs) | The writer resets only conditional, uniquely named, non-empty `Panel` nodes, and the detector compares against the same set. |
| PROBLEM-repeatable-add-label | a repeatable's Add button reads `Add <panel title>` in the form's language | `xml_writer.rs` (`config.add_label(base_language, subject)` from the repeatable's title) | Accepted exception (decision 2026-09-29): it may fail only on a form that ships no English; elsewhere the Author titles the repeatable in English with its translations, as the prompts explain, and a failure is a defect. |
| PROBLEM-banking-relationship-default-de | on a 019 form the banking field defaults to `0319`, disabled | `preface.xml` and `fragment.xml` (`banking_default_de_init`, gated on entity 019); rule `ubs-aem-variables` (keeps the entity well formed) | The gate is the entity. |
| PROBLEM-email-phone-component | a textbox or numeric box with an email or phone label is the email or telephone component, with its canonical attributes and message translations | `email.xml` and `telephone.xml` (canonical attributes), `translations/*.toml` (the two messages), rule `ubs-aem-email-phone-kind` | The writer never infers the kind from a label, so the document has to say it. |
| PROBLEM-signature-name-fill | a signature block whose party can be paired has the signer-name fill | none | Accepted exception (decision 2026-09-29): the fill is left to a person in AEM. |
| PROBLEM-fragment-library-consolidation | no `fragRef` into the germany or italy libraries where a shared one exists | rules `ubs-aem-retired-market-fragments` and `ubs-aem-global-internal-bank-use`; the kept families (infobox, footnote, banking relationship, configurator) are written by their templates; package check | Person and signature blocks are authored from the four partner generics and `affrg_SignatureGeneric1`. |
| PROBLEM-signature-hardcoded-family | no two signature blocks named alike with trailing numbers 1 to N | rule `ubs-aem-signature-family` | An authored structure, so a rule is the only guard. |
| PROBLEM-visual-editor-rules | no `fd:rules` node carries a rule property | the templates write empty `fd:rules`; rule `ubs-aem-visual-editor-rules`; package check | Only a loaded node's passthrough can carry one. |
| PROBLEM-nav-save-progress-required | the toolbar has a visible `fwbSaveProgress` button | `root.xml` (`fwbSaveProgress`); package check | Not authorable. |
| PROBLEM-fragment-title-duplicate | a referencing panel does not repeat the fragment's own title | `fragment.xml` (writes no `jcr:title` on the referencing panel) | Not authorable: a Fragment node's `title` is never written. |
| PROBLEM-static-text-orphan-step | no static text is a direct child of a wizard step | `normalize.rs` `wrap_static_text` | The pass wraps each run of such texts in a content panel on the way to the writer. |
| PROBLEM-dor-exclusion-implies-summary | every `dorExclusion="true"` tag also has `summaryExclusion="true"` | every template writes `summaryExclusion` when `dor_exclude` is set; rule `ubs-aem-passthrough-attributes` (a raw `dorExclusion` on the one node type whose template does not own it); package check | A typed flag writes both. |
| PROBLEM-summary-step-redacto | the DAM asset has `redactoSummary` and the form has the summary step and hook | `dam.xml` (`redactoSummary`), `root.xml` (summary component and hook), both gated on `use_summary` in `config.toml` | Not authorable. |
| PROBLEM-internal-bank-use-pdf-only | the internal-bank-use panels are summary-excluded, always in the PDF, and never toggled by a visibility rule | `normalize.rs` `internal_bank_use_is_pdf_only`; rule `ubs-aem-global-internal-bank-use`; package check | The pass fixes the flags for the fragment family; the writer only writes visibility scripts for conditional panels, never for a fragment. |
