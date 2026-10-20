# v3 requirement coverage

The v3 engine (`ajila-forms-conversion-engine-v3`, `specs/form-model/` and `specs/aem/`) states
what a converted UBS form must contain as numbered requirements over its generic form model. This
engine has no generic document: the agent authors the UBS AEM document (`UbsAemDocument`, the
multilingual AEM tree of `vendor/crates/u2s-aem-ubs-mcp`) directly. So each requirement is carried
here by the rules in `rules/aem/`, which say it in terms of that document, and every rule cites the
requirements it carries by their v3 IDs. Keep this file in step with `rules/aem/` and the v3 specs:
a new, renumbered or reworded requirement changes a row here and the rules citing it.

How the generic model maps onto the AEM document, for reading the rules:

| Generic model | UBS AEM document |
|---|---|
| top-level `panel` | a page: a `Panel` directly under `form` with `is_page: true` |
| `panel` below the top level, `title` | a `Panel`; its title does not render, a sub-heading is a `TitleDraw` |
| `text` `h2`/`h3`... | `TitleDraw` with `heading_level` |
| `text` `p` | `TextDraw` |
| `notice` | `MessageBox` |
| `table` | an `HtmlDisplayer` `TBL_` (text only) or a `TBL_` Panel of row Panels (with inputs) |
| `text-input` (`multiline`, `email`, `phone`) | `TextField` (`kind` `Multiline`, `Email`, `Telephone`) |
| `number-input`, `date-input` | `NumberField` (`lead_digits`, `frac_digits`), `DatePicker` (or `TextField`s for a partial date) |
| `single-choice` radio / dropdown, `multi-choice`, `checkbox` | `RadioButton` / `Dropdown`, `Checkbox` list, `Checkbox` |
| `visible` expression on a choice | a `ConditionRule` on the choice and `is_conditional` on the target Panel |
| `visible` on `form.language` | `only_languages` |
| `value` expression, `setValue` on change, shared `bind` | `calculate` with `read_only`, `copy_from`, a ring of `copy_from` |
| literal `value`, `readOnly` | `default_value`, `read_only` |
| `repeat` (`min`, `max`) | `Repeatable` (`min_occur`, `max_occur`; no initial count) |
| root `footnotes`, `<fn ref>` | inline references with their hidden description, one `FootnotePlaceholder` per page |
| `ubs:page-header`, `ubs:banking-relationship`, `ubs:issuer` | the first page's `Preface` and the hidden `TextDraw`s in the DoR header slots |
| `ubs:party`, `ubs:signature`, `ubs:address`, `ubs:account-number`, `ubs:iban`, `ubs:internal-bank-use` | the UBS fragments (`Fragment`), in Repeatables where they repeat |
| `ubs:inputs`, a block's parts and fields | the fragment's `init_hide`, `init_show`, `init_required`, `init_optional` by path inside it |
| `ubs:sharedPlaceDate` | `shared_place_date` on the signature generic |
| `ubs:configurator` | a choice in the `PN_FormConfigurator...` band |
| `ubs:subtitle` | the first page's `title`, further subtitles `TextDraw`s on the first page |
| `metadata.ubs` | `variables` (the source's XFA template variables) |

The fields the rules name beyond the AEM model's own (`default_value`, `read_only`, `calculate`,
`copy_from`, `only_languages`, `hide_title`, `lead_digits`, `frac_digits`, a Checkbox's `mandatory`,
the fragment's `init_*` paths and `shared_place_date`) and the signer-name fill are the UBS layer's
since v3 `d2dc4a0`.

The rules read the source with the tools the agent has: rendered pages and regions, page text,
`xfa_controls` and `xfa_field`, sessions (`xfa_open`, `xfa_set`, `xfa_click`, `xfa_reset`, whose
results list what appeared, disappeared and changed), and the raw template (`xfa_search`,
`xfa_node`) for what the page text does not say: a draw's font, colour and master page, a
subform's `occur`, a field's `bind`, screen-only content (`relevant="-print"`).

## Conversion guidelines (`CNV-*`)

| Requirement | In short | Rules |
|---|---|---|
| CNV-SRC-1 | The document carries exactly the languages of its sources: one per source file, or every language a multilingual source offers. | `ubs-aem-text-languages-complete` |
| CNV-SRC-2 | Text is taken as the MCP reports it, in the state being read. | `ubs-aem-structure-matches-source` |
| CNV-SRC-3 | A control without `caption` is labelled from the text shown beside it, paired by the control's `positions` and the boxes of the text runs. | `ubs-aem-input-labels` |
| CNV-SRC-4 | Control names and paths are hints only: the same name stands for different things across forms, and different names for the same thing. | `ubs-aem-input-labels` |
| CNV-SRC-5 | The document holds what a person filling the form sees and fills. | `ubs-aem-empty-panels`, `ubs-aem-field-fidelity`, `ubs-aem-source-coverage` |
| CNV-SRC-6 | A button whose `click` adds or removes instances is the add or remove control of a repeatable section: `repeat` and the section's events express it. | `ubs-aem-field-fidelity`, `ubs-aem-source-coverage` |
| CNV-SRC-7 | Behaviour comes from branches, and from sessions for combinations of choices: `appeared` and `disappeared` give `visible` expressions; ... | `ubs-aem-condition-targets` |
| CNV-SRC-8 | A section repeats when the MCP lists it as repeatable and the form has an add control for it; `min`, `max` and `initial` come from that listing. | `ubs-aem-repeatable-bounds` |
| CNV-SRC-9 | A radio group with a single radio button is a `single-choice` with that one option. | `ubs-aem-single-option-choice` |
| CNV-SRC-10 | An empty first option of a dropdown only makes it start empty and is not an option. | `ubs-aem-options-wellformed` |
| CNV-SRC-11 | Pagination is the output's concern: a page count, blank pages for duplex printing and print dialogs (`messages`) have no representation, and a ... | `ubs-aem-structure-matches-source` |
| CNV-SRC-12 | A list is often a column of markers (numbers, letters or dashes) set as separate texts left of the item texts. | `ubs-aem-paragraphs` |
| CNV-SRC-13 | A footnote marker is a digit or symbol set smaller and raised after a word (a run with a smaller font size, above the line). | `ubs-aem-footnotes` |
| CNV-TXT-1 | All text is taken verbatim from the sources, in every language. | `ubs-aem-no-untranslated-stubs`, `ubs-aem-verbatim-source-text` |
| CNV-TXT-2 | Every fillable input of the source has a counterpart in the document, and every visible text of the source appears in it, unless a rule says ... | `ubs-aem-addresses`, `ubs-aem-source-coverage` |
| CNV-TXT-3 | All strings are Unicode NFC (`ü` is one code point). | `ubs-aem-text-conventions` |
| CNV-TXT-4 | Content that exists only in some language sources (a paragraph, a field, an option, a block) is kept once and shown only in those languages: ... | `ubs-aem-language-specific-content`, `ubs-aem-text-languages-complete` |
| CNV-TXT-5 | Paragraphs that correspond one to one across the languages are one `text` each. | `ubs-aem-empty-panels`, `ubs-aem-paragraphs` |
| CNV-TXT-6 | A paragraph break that falls inside a sentence (the paragraph has no sentence end and the next one continues it) is a layout artefact: the two ... | `ubs-aem-paragraphs` |
| CNV-TXT-7 | Text that the rendered pages show twice is kept twice. | `ubs-aem-paragraphs` |
| CNV-TXT-8 | A static caption identical to the label of the input next to it is not repeated as a `text`; headings are kept even when an input repeats them as ... | `ubs-aem-headings-rendered-once`, `ubs-aem-input-labels` |
| CNV-STR-1 | Every panel has content. | `ubs-aem-empty-panels` |
| CNV-STR-2 | A collapsible section (accordion) is an ordinary `panel`. | `ubs-aem-structure-matches-source`, `ubs-aem-sub-headings-are-title-draws` |
| CNV-STR-3 | A grid of rows and columns in the source is a `table`, with or without inputs: header rows as `header`, the other rows as `rows`, cell texts as ... | `ubs-aem-tables-are-html-tables` |
| CNV-STR-4 | Table rows the user can add or remove are one row with `repeat` and an `id`. | `ubs-aem-repeatable-title`, `ubs-aem-tables-are-html-tables` |
| CNV-INP-1 | Every input has its own label (generic §5): the question it asks, without markup. | `ubs-aem-input-labels` |
| CNV-INP-2 | An input that belongs to an option and has no label of its own gets a label that says which option it belongs to: the option text, or, when that ... | `ubs-aem-input-labels`, `ubs-aem-option-fields-after-group` |
| CNV-INP-3 | A date is a `date-input`, also when the source asks day, month and year in separate inputs aligned with separators or a "dd.mm.yyyy" caption. | `ubs-aem-dates` |
| CNV-INP-4 | An input that asks for an e-mail address is a `text-input` with `variant: "email"`; one that asks for a telephone, mobile or fax number has ... | `ubs-aem-email-phone-kind` |
| CNV-INP-5 | Checkboxes that the source keeps mutually exclusive (checking one clears the other in a session) are one `single-choice` with `presentation: ... | `ubs-aem-option-fields-after-group` |
| CNV-INP-6 | Inputs inside running text (an input placed within a sentence) are `inline` inputs of the `text` (generic §6.4). | `ubs-aem-field-fidelity` |
| CNV-INP-7 | `required` follows the source (`mandatory`). | `ubs-aem-account-numbers`, `ubs-aem-copied-values`, `ubs-aem-field-fidelity` |
| CNV-INP-8 | Every literal an expression compares with a `single-choice` or `multi-choice` (in `visible`, `value`, `options`, `when` or an action) is one of ... | `ubs-aem-options-wellformed` |
| CNV-INP-9 | A dependent option list is an `options` expression that is a lookup with one entry per parent value (a map literal or a `constants` map); a third ... | `ubs-aem-cascading-dropdowns-static-variants` |
| CNV-VAL-1 | A value the source copies from one input (it appears in `side_effects` when that input is set) into a target the user cannot change (its `access` ... | `ubs-aem-copied-values` |
| CNV-VAL-2 | A value the source copies into an input that stays editable is a `setValue` action on `change` of the source input. | `ubs-aem-copied-values` |
| CNV-VAL-3 | Inputs that share one value in the source (the same `bind`) and are all editable carry the same `bind` and stay editable. | `ubs-aem-copied-values` |
| CNV-FN-1 | Each footnote of the source is declared once in the root `footnotes`, with its text without the marker (several footnotes fused in one paragraph ... | `ubs-aem-footnotes` |

## UBS profile requirements (`UBS-*`)

| Requirement | In short | Rules |
|---|---|---|
| UBS-GEN-1 | The UBS language code `SP` is the language `es`. | `ubs-aem-source-languages`, `ubs-aem-variables` |
| UBS-GEN-2 | The master language is English for a multilingual form, and the only language for a single-language form. | `ubs-aem-source-languages` |
| UBS-GEN-3 | Option values of `single-choice` and `multi-choice` are numbers as strings, `"1"` to `"n"`, in source order. | `ubs-aem-cascading-dropdowns-static-variants`, `ubs-aem-options-wellformed` |
| UBS-PAGE-1 | The block printed in the top right corner of the first page (banking relationship, custody account, issuing entity) becomes one `ubs:page-header`, ... | `ubs-aem-banking-relationship`, `ubs-aem-page-header` |
| UBS-PAGE-2 | The issuing entity text in the top right corner of the first page (for example "UBS Europe SE", "UBS Europe SE (Succursale Italia)") becomes one ... | `ubs-aem-dor-header-slots`, `ubs-aem-issuer-information`, `ubs-aem-signatures` |
| UBS-PAGE-3 | Red page furniture on the first page becomes a `notice` right after `ubs:page-header`. | `ubs-aem-notices`, `ubs-aem-page-header` |
| UBS-PAGE-4 | Other page furniture of the header that is particular to the form becomes `ubs:issuer` lines when it describes the issuer, otherwise a `notice` ... | `ubs-aem-dor-header-slots`, `ubs-aem-page-header` |
| UBS-PAGE-5 | The order of the root `children` is: `ubs:page-header`, page-furniture notices, the subtitles (UBS-TTL-2), then top-level panels only. | `ubs-aem-page-header` |
| UBS-PAGE-6 | The legal registration text of the issuing entity (6.5 pt page furniture: seat, commercial register, supervisory authority, deposit protection) ... | `ubs-aem-issuer-information` |
| UBS-TTL-1 | The form `title` is the 18 pt text at the start of the body of the first page. | `ubs-aem-form-title` |
| UBS-TTL-2 | A subtitle is a short paragraph between the title and the first section heading, set smaller than the title (8 pt) and often bold: capitalised, no ... | `ubs-aem-form-title` |
| UBS-TTL-3 | Paragraphs in the title area that are not subtitles (sentences, longer explanations) are introductory text: `text` paragraphs at the start of the ... | `ubs-aem-form-title` |
| UBS-TTL-4 | A radio row in the title area, between the title and the first section heading (for example "Neuanlage / Änderung / Löschung", "SWIFT - Format / ... | `ubs-aem-form-title`, `ubs-aem-label-visibility` |
| UBS-TTL-5 | Texts made only of dashes or list markers at title size are list markers, not titles. | `ubs-aem-form-title` |
| UBS-SEC-1 | A section heading is an 8 pt bold paragraph with a thin rule above it across the body. | `ubs-aem-headings-rendered-once`, `ubs-aem-structure-matches-source` |
| UBS-SEC-2 | A section heading appears once: as the panel `title`, never also as a `text` inside the panel. | `ubs-aem-headings-rendered-once`, `ubs-aem-structure-matches-source` |
| UBS-SEC-3 | A heading without the rule (a short standalone 8 pt bold paragraph followed by inputs or paragraphs) is a subsection, never a section: a `text` ... | `ubs-aem-headings-rendered-once`, `ubs-aem-structure-matches-source`, `ubs-aem-sub-headings-are-title-draws` |
| UBS-SEC-4 | Each top-level panel is a step of its own. | `ubs-aem-configurator-panels`, `ubs-aem-step-titles`, `ubs-aem-structure-matches-source` |
| UBS-SEC-5 | Body text is never a direct child of the root, except the subtitles. | `ubs-aem-empty-panels`, `ubs-aem-structure-matches-source` |
| UBS-SEC-6 | An annex (its pages print another CDOK in the footer line) is part of the same form document. | `ubs-aem-page-header`, `ubs-aem-structure-matches-source`, `ubs-aem-variables` |
| UBS-REP-1 | Every repeatable (panel, table row or block) has a `title` naming one instance. | `ubs-aem-repeatable-title` |
| UBS-REP-2 | No heading inside a repeatable names or numbers the instance ("Client 1"); the numbered titles of the source become the `title` (UBS-REP-1). | `ubs-aem-repeatable-title` |
| UBS-REP-3 | Copies of the same block numbered by hand in the source ("Signature 1", "Signature 2", ... | `ubs-aem-repeatable-bounds`, `ubs-aem-signature-family` |
| UBS-REP-4 | A grid is a `table` (CNV-STR-3) also when only some rules are drawn, when it has a single column, when one column is empty on every row, or when ... | `ubs-aem-tables-are-html-tables` |
| UBS-TXT-1 | A web address or e-mail address in static text is a link: `<a href>` with `https://` added to `www.` addresses and `mailto:` to e-mail addresses. | `ubs-aem-text-conventions` |
| UBS-TXT-2 | Quotation marks follow the language and the country of the form's issuing entity (`metadata.ubs.entity`: 019 Germany, 033 Italy, 001 Switzerland), ... | `ubs-aem-quotes` |
| UBS-TXT-3 | "UBS Switzerland AG" is the same in every language. | `ubs-aem-text-conventions` |
| UBS-TXT-4 | Bold lead-in sentences, table labels and bold connector words between blocks (a preamble "Between ... | `ubs-aem-paragraphs` |
| UBS-TXT-5 | Superscript digits written as characters (`¹²³`) become `<sup>`, unless they are footnote markers (CNV-FN-1). | `ubs-aem-footnotes` |
| UBS-TXT-6 | Text set in several columns in the source is read column by column (first column top to bottom, then the next) and each `text` takes the full width. | `ubs-aem-single-column-text` |
| UBS-TXT-7 | A list of plain items is one `text` with one `<p>` per item, each starting with its marker (CNV-SRC-12). | `ubs-aem-paragraphs` |
| UBS-NOT-1 | A text is a `notice` when one of these holds, in order of strength: it is set in red; it is screen-only static text; it is a standalone ... | `ubs-aem-notices` |
| UBS-NOT-2 | The label prefix of a notice ("Hinweis:", "Nota:") stays part of its content. | `ubs-aem-notices` |
| UBS-NOT-3 | Instruction and information blocks with a section heading ("Information", "Instructions", "Hinweis für Markt", "Nota importante", "Informativa") ... | `ubs-aem-notices` |
| UBS-NOT-4 | A notice that the source repeats on every page appears once. | `ubs-aem-notices` |
| UBS-FN-1 | Footnote texts are 6.5 pt page furniture, usually on the last page, starting with their marker (CNV-SRC-13, CNV-FN-1). | `ubs-aem-footnotes`, `ubs-aem-issuer-information` |
| UBS-CFG-1 | A configurator is an input of the source's configurator band: the screen-only block at the top of the first page, on a tinted band, headed "Form ... | `ubs-aem-configurator-panels` |
| UBS-CFG-2 | The configurator inputs form their own top-level panel, the first one, titled with its heading ("Form configurator"), or, when content of the ... | `ubs-aem-configurator-panels`, `ubs-aem-structure-matches-source` |
| UBS-CFG-3 | What each configurator option shows and hides comes from the configurator's branches (`appeared`, `disappeared`, `instances_changed`, ... | `ubs-aem-configurator-panels`, `ubs-aem-repeatable-bounds`, `ubs-aem-signatures` |
| UBS-CFG-4 | Changing a configurator value clears everything the configurator shows or hides, and everything logically tied to it, and brings their repeatable ... | `ubs-aem-configurator-panels` |
| UBS-INP-1 | Every input shows its label, also when it repeats the heading right above it ("Between", "Other arrangements"). | `ubs-aem-configurator-panels`, `ubs-aem-label-visibility` |
| UBS-INP-2 | When the caption of an input is too long for a label (a full sentence, or more than one line at the input's width on the rendered page), the input ... | `ubs-aem-label-visibility` |
| UBS-INP-3 | Inputs grouped under a common heading text (a column or block of a grid, for example "Issued by" over "OU ref." and "Date") form a `panel` titled ... | `ubs-aem-global-internal-bank-use`, `ubs-aem-input-labels`, `ubs-aem-sub-headings-are-title-draws` |
| UBS-INP-4 | A bold text directly above a group of radio buttons or checkboxes, with nothing between them, is the label of that group, not a subheading. | `ubs-aem-input-labels`, `ubs-aem-label-visibility`, `ubs-aem-sub-headings-are-title-draws` |
| UBS-INP-5 | A hint in parentheses next to a caption stays a separate `text` or goes to `description`; when the only caption is a bracketed hint, the label is ... | `ubs-aem-input-labels` |
| UBS-INP-6 | Checkboxes that answer one question (aligned in a row, column or grid) are one `multi-choice`. | `ubs-aem-option-fields-after-group` |
| UBS-INP-7 | A list of radio buttons or checkboxes is not split, even when the source places inputs or text between its options. | `ubs-aem-option-fields-after-group` |
| UBS-INP-8 | Not e-mail or phone inputs (CNV-INP-4), despite the words in their label: mailing address confirmation, telephone order, channel numbers, an input ... | `ubs-aem-email-phone-kind` |
| UBS-VAL-1 | An account number the source copies from the banking relationship (it shares the banking relationship's `bind`, or it changes in `side_effects` ... | `ubs-aem-copied-values` |
| UBS-PTY-1 | A section that captures the identity of a person or company in a role (account holder, client, authorized representative, ...), as its heading or ... | `ubs-aem-account-holder-cluster` |
| UBS-PTY-2 | The role is decided per party from, in order: the section's own heading, the enclosing heading, the signature that belongs to it, the configurator ... | `ubs-aem-account-holder-cluster` |
| UBS-PTY-3 | `ubs:parts` lists the groups of inputs the source shows, in source order. | `ubs-aem-account-holder-cluster`, `ubs-aem-account-holder-wiring` |
| UBS-PTY-4 | The structure of the source is replicated. | `ubs-aem-account-holder-cluster`, `ubs-aem-addresses` |
| UBS-PTY-5 | Inputs the source adds to a party (client ID, tax identification, place of birth, phone, e-mail) are generic inputs placed where the source places ... | `ubs-aem-account-holder-cluster`, `ubs-aem-account-holder-wiring` |
| UBS-PTY-6 | A minor taking part in the form is a party with role `minor`. | `ubs-aem-account-holder-cluster` |
| UBS-PTY-7 | Client advisor, branch and bank officer blocks are not parties. | `ubs-aem-account-holder-cluster` |
| UBS-PTY-8 | A name pair whose context gives no role (for example a questionnaire's "Last name / First name(s)" with no heading naming who it is), and a single ... | `ubs-aem-account-holder-cluster`, `ubs-aem-addresses`, `ubs-aem-standard-sections-are-fragments` |
| UBS-ADR-1 | An address in the source is a group of street, number, postal code, city and country inputs, sometimes with an additional address line, a canton ... | `ubs-aem-addresses` |
| UBS-ADR-2 | The country is the standard country choice of the profile. | `ubs-aem-addresses` |
| UBS-ADR-3 | A multi-line "name and address" window box under the title is a `text-input` with `variant: "multiline"`, not a `ubs:address`. | `ubs-aem-addresses` |
| UBS-ADR-4 | A sender or branch contact block (branch address, branch phone, client advisor phone), filled from a branch choice, is built from generic ... | `ubs-aem-addresses` |
| UBS-ACC-1 | A custody account in the top right corner of the first page ("Depot-Nr.") is a `ubs:account-number` with `ubs:kind: "custody"` inside ... | `ubs-aem-page-header` |
| UBS-ACC-2 | An account number is a `ubs:account-number` when the source shows its parts: a clearing number (an input, or fixed text such as "0319 00" printed ... | `ubs-aem-account-numbers` |
| UBS-ACC-3 | An IBAN input (caption "IBAN") is a `ubs:iban`. | `ubs-aem-account-numbers` |
| UBS-ACC-4 | A banking relationship repeated in the body for another party (clearing and account number inputs in that party's section) is a ... | `ubs-aem-account-numbers` |
| UBS-ACC-5 | The same custody account number ("Depot-Nr.", "Custody account no.") is captured once, as one `ubs:account-number` with `ubs:kind: "custody"`, ... | `ubs-aem-account-numbers`, `ubs-aem-copied-values` |
| UBS-ACC-6 | A fixed number printed before an input that is not part of an account number is a `text`, and the input sits next to it on the same row, with a ... | `ubs-aem-account-numbers`, `ubs-aem-single-column-text` |
| UBS-SIG-1 | A signature block in the source is a group of place, date and full name inputs with an empty signing area, usually repeatable. | `ubs-aem-account-holder-cluster`, `ubs-aem-signatures` |
| UBS-SIG-2 | The signer role comes from the signature captions as the form shows them for each configurator option: client for private persons, authorized ... | `ubs-aem-account-holder-cluster` |
| UBS-SIG-3 | A signature section stays aligned with a party when pressing the party's add or remove control in a session also changes the signature section ... | `ubs-aem-account-holder-cluster`, `ubs-aem-account-holder-wiring`, `ubs-aem-repeatable-bounds` |
| UBS-SIG-4 | The signature `name` is filled from the matching party's first and last name: `setValue` on `change` of either name input, and on `initialize` of ... | `ubs-aem-account-holder-cluster`, `ubs-aem-signatures` |
| UBS-SIG-5 | When a minor fills the form, the legal guardians sign: a `ubs:signature` with role `legal-guardian`, visible only for the minor option, with the ... | `ubs-aem-account-holder-cluster` |
| UBS-SIG-6 | Signatures of the bank are `ubs:signature` with role `bank` and have no name fill. | `ubs-aem-account-holder-cluster`, `ubs-aem-issuer-information`, `ubs-aem-signatures`, `ubs-aem-sub-headings-are-title-draws` |
| UBS-SIG-7 | Place and date are shared by all instances of one signature block (one place and one date for the whole block, or inputs that share one `bind`): ... | `ubs-aem-account-holder-cluster`, `ubs-aem-signatures` |
| UBS-IBU-1 | An internal bank use block is headed "Nur für bankinterne Zwecke", "For internal bank use only" or equivalent. | `ubs-aem-global-internal-bank-use` |
| UBS-IBU-2 | The block's heading in the source is its `title`; it is not repeated as a `text` next to or inside the block. | `ubs-aem-global-internal-bank-use` |
| UBS-IBU-3 | When the source shows an intermediary part only when the financial intermediary checkbox of the configurator is ticked (the part is in `appeared` ... | `ubs-aem-global-internal-bank-use` |
| UBS-IBU-4 | When the block's content is a combination of standard fields (`ubs:fields`, see the predefined combinations in the types document), it uses ... | `ubs-aem-global-internal-bank-use`, `ubs-aem-standard-sections-are-fragments` |
| UBS-IBU-5 | Working instructions for the bank printed as page furniture (for example "Arbeitsanweisung für Client Onboarding") are a `notice` where the source ... | `ubs-aem-notices` |
| UBS-LTR-1 | A letter-type form (addressee window, place and date line, subject, salutation, body, closing, signature) loses no information. | `ubs-aem-letter-detection` |
| UBS-MET-1 | `metadata.ubs` is read from the footer line: `formCode` is its form code, `manCode` its mandator code and `jVersion` its J version. | `ubs-aem-variables` |
| UBS-MET-2 | When language variants of a form disagree on metadata (for example different release dates), the values of the master-language source apply. | `ubs-aem-variables` |
| UBS-WRD-1 | Standard labels of profile blocks come from the profile's label catalogue in each language. | `ubs-aem-standard-sections-are-fragments` |
| UBS-WRD-2 | When the source gives a signature block no heading of its own, the title of the `ubs:signature` names the signer, but the source does not have it ... | `ubs-aem-signatures` |
| UBS-WRD-3 | The signer of a signature is named as in the signature captions of the source, even when the matching party section uses another word for the same ... | `ubs-aem-account-holder-cluster`, `ubs-aem-signatures` |

## Generic model rules

The validation rules of the generic model (`generic-form-model.md` §14) that concern content:

| Rule | Carried by |
|---|---|
| V1 unique ids | `ubs-aem-unique-node-names` |
| V2, V4 references resolve, slots | `ubs-aem-condition-targets`, `ubs-aem-copied-values`, the UBS layer's build checks |
| V5 every text in every language | `ubs-aem-text-languages-complete` |
| V6 rich text subset, footnotes F1 to F3, tables TB1 to TB4 | `ubs-aem-headings-rendered-once`, `ubs-aem-footnotes`, `ubs-aem-tables-are-html-tables`, `ubs-aem-html-displayer-markup` |
| R1 to R4 repeatables | `ubs-aem-repeatable-bounds`, `ubs-aem-repeatable-title`, `ubs-aem-account-holder-wiring` (aligned panels) |
| L5 separator lines are no components | nothing to author: the document has no line node |

## AEM UBS converter backlog

`specs/form-model/aem-ubs-converter-backlog.md` lists the AEM-only requirements. Each is held by a
rule, or by the UBS layer's writer, templates and normalize passes (see
`specs/feedback/rule-coverage.md` for the guard problems among them):

| Topic | Carried by |
|---|---|
| Wizard, steps, step titles, top-level panel without title, heading levels | `ubs-aem-step-titles`, `ubs-aem-structure-matches-source`, `ubs-aem-headings-rendered-once`, `ubs-aem-sub-headings-are-title-draws`; writer (`step_title`) |
| Toolbar, drafts, FormMetadata, summary, no preview step | writer and templates |
| Jump to field | `ubs-aem-jump-to-field-placement`; writer |
| Panels are the UBS panel | templates; `ubs-aem-passthrough-attributes` |
| Naming | `ubs-aem-naming-prefix`, `ubs-aem-unique-node-names` |
| Banking relationship, page header, header slots, issuer | `ubs-aem-banking-relationship`, `ubs-aem-page-header`, `ubs-aem-dor-header-slots`, `ubs-aem-issuer-information` |
| Subtitle | `ubs-aem-form-title`; writer (`subtitle-after-form-title`) |
| DoR: template, hidden fields, exclusion implies summary, two-column display | writer; `ubs-aem-passthrough-attributes`, `ubs-aem-placement-flags`, `ubs-aem-grid-widths` |
| Static text in a content panel, full width | normalize pass; `ubs-aem-single-column-text`, `ubs-aem-empty-panels` |
| Notices, Italy infobox | `ubs-aem-notices`, `ubs-aem-issuer-information`; writer |
| Internal bank use: global fragment, PDF only, several blocks, content panel | `ubs-aem-global-internal-bank-use`, `ubs-aem-placement-flags`; normalize pass |
| Fragments: generics, Initialize rules, no added content, retired libraries, block titles | `ubs-aem-standard-sections-are-fragments`, `ubs-aem-retired-market-fragments`, `ubs-aem-account-numbers`, `ubs-aem-addresses`; writer (`fragment_init`) |
| Parties and signatures (V2), split blocks, signature name fill, shared place and date, bank signature | `ubs-aem-account-holder-wiring`, `ubs-aem-account-holder-cluster`, `ubs-aem-signatures`, `ubs-aem-signature-family`; writer (twins, name fill) |
| Repeatables: archetype, Add labels, unbounded is 20, titles | `ubs-aem-repeatable-no-handwritten-rules`, `ubs-aem-repeatable-bounds`, `ubs-aem-repeatable-title`; writer |
| Rules: code editor, visibility on Initialize, multi-value encoding, renamed panels | `ubs-aem-visual-editor-rules`, `ubs-aem-condition-targets`, `ubs-aem-passthrough-attributes`; writer |
| Configurator: band, horizontal radio row, reset, DoR exclusion of its heading | `ubs-aem-configurator-panels`; writer |
| Cascading dropdowns | `ubs-aem-cascading-dropdowns-static-variants` |
| Inputs: email and telephone, leaf panel, datepicker default, numeric box, read-only and disabled, hidden labels, minimum widths | `ubs-aem-email-phone-kind`, `ubs-aem-dates`, `ubs-aem-field-fidelity`, `ubs-aem-copied-values`, `ubs-aem-label-visibility`, `ubs-aem-grid-widths` |
| Single-option choice | `ubs-aem-single-option-choice` |
| Footnotes | `ubs-aem-footnotes` |
| Metadata, page title, languages, synonyms | `ubs-aem-variables`, `ubs-aem-source-languages`; writer |
| Language gating | `ubs-aem-language-specific-content`; writer (`only_languages`) |
| HTML component, tables, tables with inputs | `ubs-aem-html-displayer-markup`, `ubs-aem-tables-are-html-tables` |
| Rich text in titles, footnote in a step title | `ubs-aem-headings-rendered-once`, `ubs-aem-footnotes` |
| Inline inputs and interpolations | `ubs-aem-field-fidelity`, `ubs-aem-floating-fields` |
| Account numbers, IBAN, fragment gaps | `ubs-aem-account-numbers`, `ubs-aem-standard-sections-are-fragments` |
| Free text country | `ubs-aem-addresses` (the fragment's dropdown; free entry is lost) |
| Annex panels | `ubs-aem-structure-matches-source` (the annex title is its first page's title, its first section heading a level-2 TitleDraw) |

## AEM XML specification

`specs/aem/aem-xml-spec.md` of v3 is `specs/AEM.md` here. It describes the package format the
writer produces and concerns no rule.
