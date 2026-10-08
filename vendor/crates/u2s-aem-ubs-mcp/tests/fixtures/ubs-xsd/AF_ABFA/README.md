# AF_ABFA — AEM → XSD reference fixture

Copied verbatim from the UBS `af-xsd-automation` project (`src/tests/AF_ABFA/`),
which derives a UBSAF XSD from a finished AEM adaptive form. We reproduce the
same output from our own `AemNode` tree.

| File | Role |
|---|---|
| `source.content.xml` | Input: the AEM adaptive form as authored, without `bindRef` attributes. |
| `reference.schema.xsd` | Target: the schema UBS expects for this form. |
| `reference.content.xml` | `source.content.xml` plus the 16 `bindRef` lines UBS injects. Used to check that our bindings agree with theirs. |

## What this fixture pins

`reference.schema.xsd` is UBS's file, **not** ours — never regenerate it from
our output. If our schema disagrees with it, the generator or the config is
wrong, not the reference.

Our own output uses our formatting (2-space indent, no XMLSpy header comment),
so the comparison is **structural**: element tree, names, types, `ref` versus
`name`/`type`, occurrence attributes, and the ordered include list. Formatting
differences are deliberate and are not part of the contract.

## Known non-derivable names

Three names in the reference cannot be derived from the AEM tree and are
supplied by `[[aemElements]]` rules in `profiles/ubs/xsd/config.toml`:

- panel `jcr:title="Email address instructions"` → element `EmailAddressInstruction`
- panel `jcr:title="Domain name of the Client"` → element `DomainInstruction`
- two panels sharing `fragRef=…/affrg_SignatureGeneric1`, distinguished only by
  `jcr:title` ("Client" → `AccountHolderSignature`, "Authorized representative"
  → `AuthRepSignature`)

`abfa_shape_is_derivable_without_config_names` regenerates with those rules
stripped and asserts the result is a subsequence of the full schema: config may
name what the tree cannot name on its own, but may never reorder or re-nest what
it already determines, so a generator regression cannot be papered over by
adding a config entry.

Two nodes exist *only* because config names them — the partner-class radio,
whose `jcr:title` is empty, and the ContractualPartnerGeneric fragment, whose
identity is its element name in the type library.

## Two bindRefs point somewhere else

`reference.content.xml` carries 16 `bindRef`s; 14 are rooted at `/UBSAF_ABFA/`.
The other two — `/UBSAF/AccountHolderDetails/AccountHolder` and
`/UBSAF/AuthRepSignature/Signature` — sit on panels nested *inside* a bound
fragment and address the global fragment library's own schema. They are
deliberately not element paths in `reference.schema.xsd`.
