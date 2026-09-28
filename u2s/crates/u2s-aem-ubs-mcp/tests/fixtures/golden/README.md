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
document and the package, so `tests/golden_parity.rs` compares them exactly. These packages are
structurally identical (uuids renumbered, timestamps masked) to the ones the `blueprint` CLI
wrote at the same commit, and `feedback.txt` is the verdict of the
`ajila-forms-conversion-feedback` CI guard on those: every enrolled rule clean.

`known-dictionary-gaps.txt` lists the golden dictionary entries `encode` does not reproduce, and
why none of them is read by the form. The Redacto dumps of the same forms are in
`u2s-redacto-ubs-mcp/tests/fixtures/golden/`.

Never regenerate these from this crate's own output.

`../ubs-xsd/` holds four reference XSDs from UBS's own `af-xsd-automation` project, each with
the content XML it was derived from; see each directory's README.
