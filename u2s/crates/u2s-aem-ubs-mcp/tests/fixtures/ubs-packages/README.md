# UBS AEM package fixtures

Real deployed UBS adaptive-form packages (FileVault ZIPs), used to test the
`parse_aem_zip` / `aem_to_translated` / `lower` / `generate_aem_xml_with_passthrough`
round trip (load → edit → save losslessness) without going through the
mechanical PDF/XFA conversion.

Copied verbatim (binary-identical) from
`ajilach/ajila-forms-conversion-engine`'s `core/input/` directory at commit
`f5f596a` (the last commit before `core/` was deleted from that repository),
via `git cat-file` against the repository's git-lfs objects:

| File | Notes |
|---|---|
| `AACX.zip` | Rich in unmodeled attributes, `fd:rules`/`fd:scripts` children, fragments. |
| `AAFM_019.zip` | Multilingual (≥2 languages carried per text). |
| `Germany_AAJC.zip` | Deployed German form. |
| `Germany_AACR.zip` | Deployed German form; also the regression fixture for the repeating-panel archetype attributes (`b15ff20`). |
| `AAGO.zip` | Deployed form. |
| `AAOW.zip` | Deployed form. |
| `AAOX.zip` | Deployed form. |

Used by `tests/aem_translated.rs`.
