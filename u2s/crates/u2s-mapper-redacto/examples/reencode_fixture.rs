//! Decodes a real Redacto dump and re-encodes it, printing the result to
//! stdout. Useful for manually checking a fixture actually imports into a
//! real Postgres:
//!
//! ```sh
//! cargo run -p u2s-mapper-redacto --example reencode_fixture -- \
//!     crates/u2s-mapper-redacto/tests/fixtures/redacto-AAEV_019.sql > /tmp/out.sql
//! psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f /tmp/out.sql
//! ```

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: reencode_fixture <path-to-dump.sql>");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("could not read {path}: {e}"));
    let doc = u2s_mapper_redacto::decode::decode(&bytes)
        .unwrap_or_else(|e| panic!("could not decode {path}: {e}"));
    let encoded = u2s_mapper_redacto::encode(&doc).expect("a decoded document always re-encodes");
    print!("{}", String::from_utf8(encoded.bytes).expect("the encoder only ever writes UTF-8"));
}
