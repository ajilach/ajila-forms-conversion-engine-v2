# Golden UBS Redacto dumps

Produced by the deterministic engine of `ajilach/ajila-forms-conversion-engine` at commit
`81697bc`, with `blueprint <source pdfs> --redacto --profile ubs` over the same sources as
`u2s-aem-ubs-mcp/tests/fixtures/golden/` (see its README). `dump.sql` is the Redacto INSERT script.

`document.json` is the `UbsRedactoDocument` for the same form: the dump's body and assets
without the header and footer, and each language's source (its XFA variables and recovered page
header) exactly as that engine's pipeline read them from the PDFs, exported from the same commit.
`tests/golden_parity.rs` encodes it and requires a dump that decodes to the golden one: the same
metadata, furniture and body per language, with assets compared by content, since that engine
drew its asset ids at random.

Never regenerate these from this crate's own output.
