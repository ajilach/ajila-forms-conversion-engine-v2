# Fixtures

Three real, machine-generated dumps copied from
`~/Documents/ajila-redacto-platform/conversion/output-sql/` (branch
`feature/ubs`), the forms conversion engine's own Redacto-target output for
real UBS forms:

- `redacto-AAEV_019.sql` -- the smallest (91 lines, one language, 19 assets,
  a two-column body section and a footnote panel).
- `redacto-AAAR_019.sql` -- medium (591 lines, three languages: en/de/es).
- `redacto-BAGC_019.sql` -- the largest (1173 lines, three languages, 192
  assets), exercising `layout-split` and `footnote` panels at scale.

These are real UBS legal/compliance document text (QI rules, account terms),
not synthetic fixtures -- present here only as immutable, machine-generated
SQL inserts for decode round-trip testing, the same sensitivity note
`crates/u2s-mapper-aem/tests/fixtures/README.md` carries for `AF_AABF.zip`.
