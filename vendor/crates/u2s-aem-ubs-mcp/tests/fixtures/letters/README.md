# UBS letter packages

Two issued UBS letters (FileVault ZIPs), whose first page opens with the UBS
letterhead component (`controls/letterhead`). Used by `tests/letterhead.rs` to
read a deployed letterhead back into a `Letterhead` node.

Taken from the issued packages the conversion engine was compared against
(2026-10), with the DoR template renditions (`_jcr_content/renditions/`) left
out: nothing here reads them.

| File | Notes |
|---|---|
| `AAQR_033.zip` | The canonical letterhead the profile's `aem/letterhead.xml` is taken from. Sender and receiver entered, no subject value, no `redactoLayout`. Its letterhead sits in a panel of its own, before the banking relationship. |
| `BATA.zip` | A letter with the metadata flag `redactoLayout="letterDor"` (on a metadata component named `metadata`). Fixed bank sender (`UBS Europe SE (Succursale Italia)`, bold), entered receiver, subject and both reference lines prefilled. |
