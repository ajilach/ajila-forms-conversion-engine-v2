# UBS Frutiger fonts

`frutiger-light.ttf`, `frutiger-bold.ttf`, `frutiger-italic.ttf`, copied
verbatim from `ajila-forms-conversion-engine/profiles/ubs/parser/fonts/`
(upstream commit `7a7a18ded28122039f921c7645dcf1a07688a1ae`).

Frutiger is a commercial Linotype family. This project previously deliberately
excluded it (see the history of `crates/u2s-xfa/src/fonts.rs`'s module doc)
for lack of a licence covering redistribution in this workspace. That
constraint has since been confirmed cleared for this project, which is what
allows this directory to exist; it does not extend to any other use.

`crates/u2s-xfa/src/fonts.rs`'s `test_support::font_dir()` prefers this
directory (when present) over the plain-DejaVu fallback in `vendor/fonts/`,
so the vendored [UBS corpus](../../../corpus/ubs/README.md) renders with its
real metrics rather than a substitute font's.
