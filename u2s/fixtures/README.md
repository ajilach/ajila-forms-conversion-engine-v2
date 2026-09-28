# Shared MCP conformance fixtures

`U2S_MCP_FIXTURE_DIR` points here. This is the one directory
`run_mcp_server_conformance` resolves every registered server's own
`$FIXTURES`-prefixed test vector against (`crates/u2s-server/src/service/mcp.rs`'s
`mcp_fixture_dir`) -- one shared location across every server, not one per
crate, since a real deployment runs conformance against whichever servers
happen to be registered without knowing in advance which crates they came
from.

Every file here is a symlink into the crate that actually owns and commits
it, so there is exactly one copy of each fixture, never a duplicate that can
drift:

| File | Source | Server(s) that need it |
|---|---|---|
| `ten-pages.pdf`, `unicode-text.pdf` | `crates/u2s-render-pdf/fixtures/generated/` | `u2s-render-pdf-mcp`, and other servers' vectors that reference a plain PDF |
| `minimal.xfa.pdf` | `crates/u2s-render-xfa/fixtures/generated/` | `u2s-render-xfa-mcp`, `u2s-xfa-mcp` |
| `AF_AABF.zip` | `crates/u2s-mapper-aem/tests/fixtures/` | `u2s-aem-ubs-mcp`, `u2s-aem-ubs-verify-mcp` (a real, human-authored UBS package -- see that directory's own README for its sensitivity note) |
| `generic_minimal.zip` | `crates/u2s-aem-mcp/tests/fixtures/` | `u2s-aem-mcp`, `u2s-aem-verify-mcp` (a small synthetic package, reproducible via `cargo run -p u2s-aem-mcp --example generate_fixture`) |
| `verify_fixture_package.json` | `crates/u2s-test-verify-mcp/tests/fixtures/` | `u2s-test-verify-mcp` (arbitrary bytes -- `verify_run`'s `dry_run: true` vector only needs the file to exist, never reads its content) |
| `redacto-AAEV_019.sql` | `crates/u2s-mapper-redacto/tests/fixtures/` | `u2s-redacto-ubs-mcp` (a real, machine-generated UBS Redacto dump -- see that directory's own README for its sensitivity note) |

`AF_AABF.zip`, `generic_minimal.zip` and `verify_fixture_package.json` are
committed directly and need nothing further. `ten-pages.pdf`, `unicode-text.pdf` and `minimal.xfa.pdf`
are also committed at their source paths above, despite living under a
directory named `generated` -- see the README's own "Getting started"
section (`cargo test --workspace` regenerates them deterministically if
ever deleted).

Add a symlink here, matching the exact filename a new server's own test
vectors declare under `$FIXTURES`, whenever a new format or capability
server needs one.
