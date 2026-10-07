//! `include_dir!` does not tell cargo which files it read, so a rule edited
//! under `rules/aem/` would otherwise leave a stale build running the old rule.

fn main() {
    println!("cargo:rerun-if-changed=../rules/aem");
}
