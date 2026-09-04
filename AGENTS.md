# Agent Instructions

## General

- When a bug was identified, always add a test to confirm the issue. Then fix the bug, and finally run the same test again to confirm that the issue is gone. Keep the test to ensure that the bug will not happen anymore.
- After implementing a feature, always run all tests to ensure that the feature din't introduce a bug.
- Before assuming that a test failure was preexisting, confirm it by stashing the changes and running the tests again in the original code.
- In case you introduced a bug, try to identify the issue by comparing with the previous working state. Based on that analysis, fix the issue on the new code.
- Instead of assuming something when the task is unclear, ask the user.
- When making an architectural decision, always ask the user first with all possible options.
- When asked to find the issue of a failing test, do not simplify the test itself without the users consent.
- As soon as you have implemented a feature and all tests pass, commit it. Use a simple one-sentence commit message.
- After finishing the implementation, review the code from a separate subagent that only knows the plan but not the actual implementation. The subagent should review the code (cleanliness, code duplication, potential bugs or missing implementation details, run clippy lints, removed or changed tests ...) and provide feedback to the main agent.
- Do not use emojis.

## Project-specific

- When running tests, use `cargo test --release` such that they are faster. Also make sure to save the entire output to a log file to avoid needing to re-run the tests if output information is required. Do NOT run tests if you only modified the app/cli wrappers.
- Before implementing XFA functionality, always consult the [XFA specs](./specs/XFA-3_3.txt) (text extraction of `specs/XFA-3_3.pdf`).
- When checking bounds in a module, consider adding a helper method to Bounds directly or check if a helper function already exists.
- Tests should never work with the ouput files. Instead they should analyze the intermediate structures (StrucuredNode, FlattenedNode, Document, ...) directly.
- AEM output should be moved as much as possible to the templating engine.
- The AEM output is judged by the feedback repo's CI guard, because a converted form joins the
  corpus that guard polices. Run it on a real conversion with
  `python3 scripts/check_feedback_rules.py core/input/AAOS_033_IT.pdf` (it needs
  `../ajila-forms-conversion-feedback`); every enrolled rule must be clean. The rules themselves are
  in `specs/feedback/consistent-problems.md`, the shapes a person still applies by hand in
  `specs/feedback/manual-changes-italy-033.md`, and the subset the engine can check on its own
  output in `review.rs`'s `feedback_violations`.
- Three of those rules are about a node's position among its siblings, so no template can satisfy
  them: they live in `core/src/aem/normalize.rs` and run over a copy of the tree on the way into
  the writer, which is what makes them hold for an agent-authored or loaded tree as well.
- Where a node shows up is `AemAttrs` on the node, not a template guess: `summary_exclude` is what
  keeps content out of the UBS DoR (Redacto renders it from the summary), `dor_exclude` is Adobe's
  own switch, and `always_in_pdf` is how a hidden node still reaches the printed document.
- Partner and signature blocks follow the UBS general-fragments directive (specs/"AF Fragments and Common Fields with XSD List.md", 2026-08-20): the custom templates and the role prompts reference only the four `afforms_ubs_fragmentlib` partner generics plus `affrg_SignatureGeneric1`, hide unneeded sub-panels via one Initialize `hideAFHideDor` rule, pair each data panel with its signature panel by name (`PN_CPGRP`/`PN_SGN_CPGRP`, `PN_AHGRP`/`PN_Sign_AHGRP`), and let the host author the signer-name fill (`TXT_Donotdelete` calc). `test_custom_templates_reference_only_ubs_generic_fragments` pins it; do not reintroduce germany/italy person or signature fragments.
- The XSD is generated from the **AemNode** tree (`core/src/xsd/from_aem.rs`), and each node's `bindRef` is assigned during that same walk, so a form's bindRefs are by construction exact element paths in its schema. Do not add a second XSD source. Customer-specific element names, ignore rules and occurrence overrides belong in `profiles/<name>/xsd/config.toml` under `[[aemElements]]`, never in Rust.

## Layout

| Crate | What belongs there |
|---|---|
| `core` (`blueprint`) | The engine: parsing, analysis, and every renderer. No UI, no network, no LLM. |
| `agent` | The headless conversion agent: the tool catalog and executor, the edit-history store, the reference store, the AEM HTTP client, and the browser client (`browser.rs`: a Playwright MCP server spawned per run and driven over stdio). No UI and no LLM. |
| `pipeline` | The conversion controller: Analyst → Author → Reviewer sequencing, retry recovery, the stuck watchdog. The per-stage loop is rig's sans-IO `AgentRun`, stepped by `run_stage` so every piece of I/O stays here. Reaches the outside world only through `TurnProvider` (the model) and `RunObserver` (progress), so it needs neither a UI framework nor a network to test. |
| `runner` | The host side of a run, shared by `app` and `cli`: the rig client factory (`client.rs`, the only place a provider is named), the streamed model call (`stream.rs`), the context budget (`context.rs`) and model limits (`models.rs`), the operator settings, and the entry points that build the agent, open a history session and record the result. |
| `app` | The Dioxus desktop app: the observer implementation and all UI state. |
| `mcp` | A stdio MCP server exposing `agent`'s tools to an external LLM client. |
| `cli` | Thin arg-parse and dispatch over `core`, plus `convert`/`sessions` — the AI conversion run headless, over `runner`. |
| `judge` | Offline eval harness scoring translation quality to CSV. |

- A run produces one **output target** (`OutputTarget::Aem` or `Redacto`). Targets are configured per profile under `profiles/<name>/<target>/`.
- Tools are scoped **once**, in `SCOPING` in `agent/src/conversion/catalog.rs`: each tool declares which targets may execute it and which pipeline stages are offered it. Do not add a second allow-list anywhere — add a row there. The browser tool family (names only known at runtime) is scoped once too, as the whole family, in `BROWSER_SCOPES` next to it; a stage's actual tool list is `ConversionAgent::tools_for_stage`.
- The Playwright MCP server is pinned to `PLAYWRIGHT_MCP_VERSION` in `agent/src/browser.rs` and its tool surface to `agent/tests/playwright_mcp_tools.json`; never `latest`. Bumping the version means regenerating that snapshot (ignored test `playwright_mcp_tool_surface_matches_snapshot`, needs Node and Chrome) and re-reading the prompts that name browser tools. The browser preflight fails loudly and refuses the run; do not add a silent fallback.
- The tool catalog is pinned by `agent/tests/catalog.json`. After an intended change, regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p agent` and review the diff: tool descriptions are prompt surface, so a wording change is a behaviour change.
- Tool descriptions and role prompts may only name tools that exist; `prose_only_names_tools_that_exist` enforces it. Prompt text lives in `agent/src/conversion/prompts.rs`.
- Controller behaviour (stage order, retry, abort) belongs in `pipeline` and gets a test there — a scripted `TurnProvider` plus a recording `RunObserver` drive the real `run` with no network. Do not add sequencing logic to the app.