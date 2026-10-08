# Golden UBS AEM outputs

Produced by the deterministic engine of `ajilach/ajila-forms-conversion-engine` at commit
`81697bc`, before that engine was retired in favour of this crate's UBS layer.

| Form | Sources | Files |
|---|---|---|
| `AAOS_033_IT` | `AAOS_033_IT.pdf` | `document.json`, `package.zip`, `schema.xsd`, `feedback.txt` |
| `AAEV_019_EN` | `AAEV_019_EN.pdf` | same |
| `AABF_019` | `AABF_019_{DE,EN,SP}.pdf` (one multilingual form) | same, plus `known-dictionary-gaps.txt` |

For each form, one run of that engine's pipeline wrote all three:

- `package.zip` and `schema.xsd`, exactly as `blueprint <source pdfs> --aem --xsd --profile ubs`
  builds them;
- `document.json`, the `UbsAemDocument` of the same AEM tree: the form's XFA variables, header
  and languages, and the tree lifted with its dictionary (without the old engine's `field_id`,
  which only its own converter read). An Italian-only form's texts are keyed
  `it`, not under the profile's fixed master `en`.

Writing them in one run keeps the old converter's random node uuids consistent between the
document and the package, so `tests/golden_parity.rs` compares them exactly.

Since then two things in the documents changed, and nothing else:

- the retired engine's `Custom` nodes (the configurator choice, the account-holder cluster and the
  signature block, which were whole profile templates) are authored as ordinary nodes: a
  `RadioButton` whose conditions drive conditional panels, `Repeatable`s wrapping the UBS partner
  generics (with `init_hide`), and signature `Repeatable`s `RCP_SGN_CPGRP` / `RCP_Sign_AHGRP`
  wrapping `affrg_SignatureGeneric1` fragments of the same names. Those subtrees render differently
  from the templates, so `golden_parity.rs` leaves them (`REAUTHORED`) out of the package
  comparison on both sides, and compares the schema for the document without them;
- the `Appendix` nodes are gone: the profile's appendix template was empty.

The golden packages and schemas are unchanged. The re-authored clusters pass the feedback guard
except for `PROBLEM-signature-name-fill` (the signer-name fill is left to the AEM author) and, on
`AAOS_033_IT`, `PROBLEM-repeatable-add-label` (the guard wants the known wording `Cliente` as an
English master). These packages are
structurally identical (uuids renumbered, timestamps masked) to the ones the `blueprint` CLI
wrote at the same commit, and `feedback.txt` is the verdict of the
`ajila-forms-conversion-feedback` CI guard on those: every enrolled rule clean.

`known-dictionary-gaps.txt` lists the golden dictionary entries `encode` does not reproduce, and
why none of them is read by the form. The Redacto dumps of the same forms are in
`u2s-redacto-ubs-mcp/tests/fixtures/golden/`.

Never regenerate these from this crate's own output.

`../ubs-xsd/` holds four reference XSDs from UBS's own `af-xsd-automation` project, each with
the content XML it was derived from; see each directory's README.
