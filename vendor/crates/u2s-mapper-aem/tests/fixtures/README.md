# Fixtures

## `AF_AABF.zip`

A real, human-authored UBS AEM Adaptive Forms FileVault content package
("AABF"), copied verbatim from
`ajila-forms-conversion-engine/AABF_019_merged.zip` (upstream commit
`aee5aef09e47818fe99558aeb96d110be842b570`), confirmed clear to commit.

This is the round-trip decoder's primary fixture -- "a golden-package test
needs a real `AemForm` document" was this crate's own "still needs" item,
and this is that document, in its native format. It exercises, in ways no
synthetic fixture would:

- the UBS overlay component catalogue (`ajila-forms-customers/ajila-forms-ubs/…`)
- a nested JCR folder path (`afforms_germany_all/af_aa/AF_AABF`)
- two ~920 KB XDP Document-of-Record renditions (`de`, `en`) — the only
  realistic test that a decoder does not inline large binaries into
  `output_json`
- 7 translation dictionaries whose keys disagree with the DAM asset's own
  `<dictionary>` node list (a real-world inconsistency the decoder must
  warn on, not refuse — see the module doc on `Note`)
- 11 fragment references, 53 nested panels, 8 wizard steps, an authored
  summary step (`summarypanel`) with `summary`/`dorOptionsUBS`/`metadata`
  components

**Never regenerate this file from our own encoder.** Its entire value is
that it was *not* produced by `u2s-mapper-aem::encode` — a decoder that
only has to read its own writer's output proves nothing about real AEM
content.

**Content note.** The package embeds real (if unremarkable) business
configuration: German-language form copy, an internal mailbox alias
(`SEC-SH-Dauerauftrag-DE`), and a document-management reference
(`formrange_cdokinfo="63138"`). Confirmed clear to commit as a test fixture;
do not extract or repurpose this content outside the round-trip test it
exists for.

## `script-corpus.zip`

Every distinct rule attribute (`fd:click`, `fd:init`, `fd:visible`, ... on
`fd:scripts`/`fd:rules`) of the 420 reviewed UBS packages in
`ajila-forms-conversion-feedback/forms/issued/` at commit `c0fdc08a`
(2026-09-30): 6'443 values, each with the form, file, element and owning
component it was first seen on, and the value exactly as the file spells it
(still XML-escaped). One `attributes.jsonl` inside the zip.

It is the reference of `tests/script_encoding.rs`: the codec of
`u2s_mapper_aem::script` and this crate's XML writer must carry every one of
these through their three encodings (XML attribute, JCR multi-value, JSON
SCRIPTMODEL) unchanged.

Regenerate with `scripts/extract-script-corpus.py <feedback-checkout>` when
the reviewed set changes, and say in the commit which commit of the feedback
repository it was taken from. Like `AF_AABF.zip`, it was not produced by this
crate's encoder, and must never be.

**Content note.** The values are the forms' own JavaScript rules: UBS client
library calls, component names and validation messages, no client data. Do
not extract or repurpose them outside the encoding tests.
