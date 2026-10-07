//! Parsing and validating a server's `u2s.manifest` resource — the shape all
//! three real servers already serve (see `crates/u2s-xfa-mcp/src/specs.rs`
//! and its siblings), not a contract authored fresh here.
//!
//! ```json
//! { "contract": "1.0.0",
//!   "server": { "name": "u2s-xfa-mcp", "version": "0.1.0" },
//!   "tools": [ { "tool": "xfa_outline", "role": "query", "ingest": "outline",
//!                "scope": { "output_formats": [] } } ],
//!   "test_vectors": [ { "tool": "xfa_outline", "args": { "doc_path": "$FIXTURES/x.pdf" },
//!                       "expect": { "structured": { "count": 1 } } } ],
//!   "format": null }
//! ```
//!
//! `side_effecting` and `verify` are new here too, and follow the same
//! shape as `ingest`: a tool that installs a package on a spun-up instance
//! and drives a browser against it declares
//! `{ "role": "query", "side_effecting": true, "verify": "run" }` rather
//! than being trusted by name. Both default to `false`/absent so every
//! manifest predating them still parses unchanged.
//!
//! `format` is new here: an `encode` server's manifest carries its output
//! format's key, version, `description_md` and `json_schema` alongside the
//! tool list, so registering the server registers the format (PLAN.md: "An
//! output format IS an MCP server").
//!
//! `scope` names only *output* formats: a dataset fixes only its output
//! format, and which tool ingests a given document is resolved on demand
//! (see [`IngestCapability`]), never narrowed by a format a manifest
//! declared in advance. A manifest still sending `scope.input_formats`
//! parses without error -- the key is a compatible relaxation, not a
//! breaking one -- but the value is discarded and surfaces nowhere.

use serde_json::Value;

use u2s_core::{FormatIdError, FormatScope};

pub use u2s_core::FormatId;

/// The contract major this build understands. A manifest whose `contract`
/// has a different major is refused at registration — see
/// [`ServerManifest::check_compatible`] — because the convention itself
/// (roles, claims, the manifest shape, blob-handle discipline) is a protocol
/// third parties write against, and a major bump is where it is allowed to
/// break them.
pub const SUPPORTED_CONTRACT_MAJOR: u64 = 1;

/// The four roles PLAN.md defines for a pluggable tool.
///
/// `Decode` is `Encode`'s exact inverse, and the contract below is pinned
/// here — where the manifest shape already lives — so a third party writing
/// a decoder has one place to read it, the same way `format` is documented
/// on [`FormatModule`] rather than left to be inferred from `encode`'s own
/// tool description:
///
/// ```text
/// tool "decode", role Decode, scope { output_formats: ["<key>"] }
/// args   exactly one of `artifact_blob` (a blob handle) or `artifact_path`
///        (a filesystem path, for conformance test vectors only — see
///        crate::conformance's `$FIXTURES` substitution, which can only
///        target a string argument, never pre-load a blob), plus optional
///        `media_type`/`filename` hints
/// result on success: `structured_content.output_json` is a blob handle
///        `{ handle, byte_len, digest }`, never the document inline — the
///        same reasoning `encode` already returns its package by handle
///        (large payloads are always blobs, PLAN.md's own discipline)
///        on failure: a tool error naming what could not be represented
/// ```
///
/// Two properties every decoder must hold, because nothing downstream can
/// check them independently:
///
/// - **Losslessness is all-or-nothing.** There is deliberately no
///   `truncated`/`partial` flag in the result. A decoder that cannot fully
///   represent its input returns a tool error, never a best-effort document
///   — a caller's only independent check is schema validation, which proves
///   the *shape* is valid, not that nothing was silently dropped.
/// - **`artifact_path` grants no more than registering the server already
///   did.** Registering a stdio server is arbitrary command execution by
///   design (PLAN.md); a decoder reading an operator-supplied path is not a
///   new trust boundary, so there is no sandboxing to add here — only the
///   normal discipline of a registered server behaving as its manifest
///   claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolRole {
    Normalize,
    /// See this enum's own doc for the full `decode` contract.
    Decode,
    Encode,
    Query,
}

impl ToolRole {
    /// `pub` because `mcp_tools.role` is stored as text and the adapters
    /// resolution table reads it back — the alternative is a second copy of
    /// this mapping in `u2s-server`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "normalize" => Some(Self::Normalize),
            "decode" => Some(Self::Decode),
            "encode" => Some(Self::Encode),
            "query" => Some(Self::Query),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normalize => "normalize",
            Self::Decode => "decode",
            Self::Encode => "encode",
            Self::Query => "query",
        }
    }
}

impl std::fmt::Display for ToolRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What ingest step a `query` tool serves, declared once in the manifest
/// rather than inferred from its name. This is how the normalizer stays
/// format-agnostic: it resolves "whichever registered tool serves
/// `outline`", never a tool named `xfa_outline` specifically, so a future
/// DOCX server becomes reachable by declaring the same four capabilities
/// rather than by adopting PDF-flavoured tool names.
///
/// Each capability has one fixed argument and result contract, format-
/// independent, all keyed on `doc_path` and (for `text`/`render`) a `page`
/// -- defined as a page or the format's closest presentation-unit
/// equivalent; a format with no intrinsic pagination reports exactly one.
///
/// ```text
/// info    args   { doc_path }
///         result { page_count: u32, applicable?: bool = true, title?, warning? }
///
/// outline args   { doc_path, max_depth?, limit? }
///         result { entries: [{ path, kind }], truncated: bool }
///
/// text    args   { doc_path, page, offset?, limit? }
///         result { text, truncated, total_chars? }
///
/// render  args   { doc_path, from, limit, dpi?, format? }
///         result { images: [{ page, width_px, height_px, inline, blob? }],
///                  next_from: u32|null, budget_hit? },
///                plus exactly one content block per `images` entry, in order
/// ```
///
/// `applicable: false` on an `info` result means "I can answer but I am the
/// wrong handler for this document" (pdfium's XFA shim page, for example) --
/// distinct from a tool *error*, which means "I cannot read this document at
/// all". Both are legitimate outcomes of a normal ingest probe, never a
/// fault; see `service::normalize`'s election.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IngestCapability {
    Info,
    Outline,
    Text,
    Render,
}

impl IngestCapability {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "info" => Some(Self::Info),
            "outline" => Some(Self::Outline),
            "text" => Some(Self::Text),
            "render" => Some(Self::Render),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Outline => "outline",
            Self::Text => "text",
            Self::Render => "render",
        }
    }
}

impl std::fmt::Display for IngestCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What verification step a `side_effecting` `query` tool serves, declared
/// once in the manifest exactly as [`IngestCapability`] is — a fixed,
/// format-independent contract the platform can call deterministically,
/// rather than leaving "which tool verifies this output" to the LLM to
/// discover from tool descriptions.
///
/// One variant today: a format that ships a verifier declares `Run` on
/// exactly the tool that performs the whole install-render-submit flow.
///
/// ```text
/// run  args   { package: <blob handle> | package_path: <path, vectors only>,
///                dry_run?: bool, fill?: { <field>: value }, submit?: bool }
///      result { dry_run: bool,
///                steps: [{ name, screenshot: <blob>, console_errors: [string],
///                          failed_requests: [{ url, status }] }],
///                artefacts: [{ kind: "download"|"package", label, blob: <blob> }],
///                findings: [{ severity: "error"|"warning", kind, message }],
///                duration_ms }
///              plus one text content block summarising the run. Every
///              image travels as a blob -- never inline -- both because the
///              agent funnel cannot show an image in a tool result at all
///              (see `u2s-agent`'s `classify`) and because a run yields more
///              images than a single response should carry.
/// ```
///
/// `dry_run: true` validates inputs and the tool's own readiness (a
/// malformed package, an unreachable backing service) without touching
/// anything external, so a conformance vector can exercise the contract
/// without whatever side effect `run` performs for real -- see
/// [`ToolManifest::verify`]'s validation: a tool declaring `verify` must
/// also declare `side_effecting: true`, but that does not mean every call
/// to it has a side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VerifyCapability {
    Run,
}

impl VerifyCapability {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "run" => Some(Self::Run),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
        }
    }
}

impl std::fmt::Display for VerifyCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolManifest {
    pub tool: String,
    pub role: ToolRole,
    /// `Some` only for a `query`-role tool that opts into serving one of the
    /// normalizer's ingest steps -- see [`IngestCapability`]. Any other role
    /// declaring one is a malformed manifest: a capability is a promise
    /// about a query tool's contract specifically.
    pub ingest: Option<IngestCapability>,
    /// Whether calling this tool for real has an effect outside u2s --
    /// installing a package on a spun-up instance, submitting a form. The
    /// gate that gives such a tool to the Output Review Agent alone and
    /// refuses it to every other role lives in `u2s-server` and reads this
    /// field, not the tool's name or description: a tool must say so of
    /// itself, not be trusted by convention. Defaults to `false` so every
    /// existing manifest still parses.
    pub side_effecting: bool,
    /// `Some` only for a `side_effecting`, `query`-role tool that opts into
    /// serving [`VerifyCapability::Run`] -- the platform's deterministic
    /// verification pass. Any manifest declaring `verify` without
    /// `side_effecting: true`, or on a non-`query` role, is malformed: a
    /// capability that is not side-effecting could not be `dry_run: false`
    /// verification in the first place.
    pub verify: Option<VerifyCapability>,
    /// Whether the tool is offered to the Conversion Agent on a dataset
    /// whose admin has not configured it. Tools of a server that reads
    /// source documents are on by default anyway; this is for a read that
    /// every conversion should have although it reads no document, such as
    /// the corpus search. Only a non-side-effecting `query` tool may
    /// declare it. Defaults to `false`.
    pub default_on: bool,
    pub scope: FormatScope,
}

/// A declared conformance vector: a tool to call, its arguments (any string
/// value may contain a `$FIXTURES` placeholder — not just `doc_path`, which
/// is only what every fixture happens to be named today), and a partial
/// expectation on the result.
#[derive(Debug, Clone, PartialEq)]
pub struct TestVector {
    pub tool: String,
    pub args: Value,
    /// Two shapes, mutually exclusive:
    ///
    /// - `expect.structured`, checked as a **subset match** against the
    ///   real result (see [`crate::conformance::subset_matches`]): every
    ///   key this carries must be present and equal in the actual
    ///   response, and the response may carry more.
    /// - `expect.error: true`, for a vector whose whole point is that a
    ///   call is refused (e.g. an unknown session handle, offline) rather
    ///   than answer anything -- requires `is_error == true` on the
    ///   result and skips the structured-content check entirely. An
    ///   optional sibling `expect.error_contains` (a string) additionally
    ///   requires some content block's text to contain it, so a vector can
    ///   pin *why* the call was refused, not only that it was.
    pub expect: Value,
}

/// The output-format module an `encode` server's manifest carries. Present
/// iff the server defines a format; `u2s-render-*`/`u2s-xfa-mcp` — all
/// `query` role — have none.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatModule {
    pub key: FormatId,
    pub version: semver::Version,
    pub description_md: String,
    pub json_schema: Value,
}

/// A server's session contract, present iff it lets a caller open a document
/// once and address it by handle afterwards instead of repeating `doc_path`
/// on every call. This is additive to [`IngestCapability`]'s `doc_path`-keyed
/// contracts, never a replacement for them: the normalizer drives those as
/// stateless one-shot probes with no lifecycle to hang a session on, and a
/// conformance vector is one call with scalar args, so it can only ever
/// address a document by `doc_path` too.
///
/// Naming the tools here, rather than trusting a `<prefix>_open` naming
/// convention, is what lets a generic session battery in
/// [`crate::conformance`] exercise any server that declares this without
/// server-specific knowledge.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSupport {
    /// `{ doc_path }` -> `{ session, revision: 0, ... }`.
    pub open: String,
    /// `{ session }` -> `{ closed: true }`.
    pub close: String,
    /// Any read tool that accepts either `doc_path` or `session` -- the
    /// battery's strongest assertion is that the two answer identically, and
    /// it needs one tool it can call both ways to check that.
    pub probe: String,
    /// Every tool that mutates the session and so requires
    /// `(session, expected_revision)`. Empty means a read-only session: a
    /// parse cache with nothing to guard against staleness.
    pub mutators: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerManifest {
    pub contract: semver::Version,
    pub server: ServerIdentity,
    pub tools: Vec<ToolManifest>,
    pub test_vectors: Vec<TestVector>,
    pub format: Option<FormatModule>,
    pub sessions: Option<SessionSupport>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ManifestError {
    #[error("{field}: missing")]
    Missing { field: String },
    #[error("{field}: {reason}")]
    Invalid { field: String, reason: String },
    #[error(
        "contract {found}: major version not supported (this build understands major {supported})"
    )]
    UnsupportedContract {
        found: semver::Version,
        supported: u64,
    },
    #[error("tool {tool:?} declared more than once")]
    DuplicateTool { tool: String },
}

/// A field name known at the call site, lifted to `String` once so every
/// `Missing`/`Invalid` construction reads the same way regardless of whether
/// the path was composed at runtime (`tools[2].scope.output_formats`) or is a
/// literal (`"contract"`).
fn field(name: impl Into<String>) -> String {
    name.into()
}

impl ServerManifest {
    /// Parses the JSON a server's `u2s://manifest` resource returns.
    ///
    /// Accumulates nothing — the first problem wins — because a malformed
    /// manifest means the server does not speak the convention at all, and
    /// there is no partial manifest worth reporting piecemeal the way a
    /// config file's problems are (contrast `u2s_core::ConfigError`, which
    /// exists precisely because *that* case benefits from batching).
    pub fn parse(raw: &Value) -> Result<Self, ManifestError> {
        let contract = parse_version(raw, "contract")?;
        let server = parse_server(raw)?;
        let tools = parse_tools(raw)?;
        let test_vectors = parse_test_vectors(raw)?;
        let format = parse_format(raw)?;

        let mut seen = std::collections::HashSet::new();
        for t in &tools {
            if !seen.insert(t.tool.as_str()) {
                return Err(ManifestError::DuplicateTool {
                    tool: t.tool.clone(),
                });
            }
        }

        let sessions = parse_sessions(raw, &tools)?;

        Ok(Self {
            contract,
            server,
            tools,
            test_vectors,
            format,
            sessions,
        })
    }

    /// Refuses a manifest whose contract major this build does not
    /// understand. Called at registration, separately from [`parse`] — a
    /// manifest can be well-formed and still speak a contract version this
    /// build predates or has moved past.
    ///
    /// [`parse`]: Self::parse
    pub fn check_compatible(&self) -> Result<(), ManifestError> {
        if self.contract.major == SUPPORTED_CONTRACT_MAJOR {
            Ok(())
        } else {
            Err(ManifestError::UnsupportedContract {
                found: self.contract.clone(),
                supported: SUPPORTED_CONTRACT_MAJOR,
            })
        }
    }
}

fn parse_version(raw: &Value, field_name: &'static str) -> Result<semver::Version, ManifestError> {
    let text = raw
        .get(field_name)
        .and_then(Value::as_str)
        .ok_or(ManifestError::Missing {
            field: field(field_name),
        })?;
    semver::Version::parse(text).map_err(|e| ManifestError::Invalid {
        field: field_name.to_owned(),
        reason: e.to_string(),
    })
}

fn parse_server(raw: &Value) -> Result<ServerIdentity, ManifestError> {
    let server = raw.get("server").ok_or(ManifestError::Missing {
        field: field("server"),
    })?;
    let name = server
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ManifestError::Missing {
            field: field("server.name"),
        })?
        .to_owned();
    let version = server
        .get("version")
        .and_then(Value::as_str)
        .ok_or(ManifestError::Missing {
            field: field("server.version"),
        })?
        .to_owned();
    Ok(ServerIdentity { name, version })
}

fn parse_tools(raw: &Value) -> Result<Vec<ToolManifest>, ManifestError> {
    let array = raw
        .get("tools")
        .and_then(Value::as_array)
        .ok_or(ManifestError::Missing {
            field: field("tools"),
        })?;

    array
        .iter()
        .enumerate()
        .map(|(i, entry)| parse_tool(entry, i))
        .collect()
}

fn parse_tool(entry: &Value, index: usize) -> Result<ToolManifest, ManifestError> {
    let path = |name: &str| format!("tools[{index}].{name}");

    let tool = entry
        .get("tool")
        .and_then(Value::as_str)
        .ok_or(ManifestError::Missing {
            field: path("tool"),
        })?
        .to_owned();

    let role_raw = entry
        .get("role")
        .and_then(Value::as_str)
        .ok_or(ManifestError::Missing {
            field: path("role"),
        })?;
    let role = ToolRole::parse(role_raw).ok_or_else(|| ManifestError::Invalid {
        field: path("role"),
        reason: format!("{role_raw:?} is not one of normalize|decode|encode|query"),
    })?;

    let ingest = match entry.get("ingest") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            let raw = raw.as_str().ok_or_else(|| ManifestError::Invalid {
                field: path("ingest"),
                reason: "must be a string".to_owned(),
            })?;
            let capability = IngestCapability::parse(raw).ok_or_else(|| ManifestError::Invalid {
                field: path("ingest"),
                reason: format!("{raw:?} is not one of info|outline|text|render"),
            })?;
            if role != ToolRole::Query {
                return Err(ManifestError::Invalid {
                    field: path("ingest"),
                    reason: format!(
                        "an ingest capability is a query-tool contract, not a {role} one"
                    ),
                });
            }
            Some(capability)
        }
    };

    let side_effecting = match entry.get("side_effecting") {
        None | Some(Value::Null) => false,
        Some(raw) => raw.as_bool().ok_or_else(|| ManifestError::Invalid {
            field: path("side_effecting"),
            reason: "must be a boolean".to_owned(),
        })?,
    };

    let verify = match entry.get("verify") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            let raw = raw.as_str().ok_or_else(|| ManifestError::Invalid {
                field: path("verify"),
                reason: "must be a string".to_owned(),
            })?;
            let capability = VerifyCapability::parse(raw).ok_or_else(|| ManifestError::Invalid {
                field: path("verify"),
                reason: format!("{raw:?} is not one of: run"),
            })?;
            if role != ToolRole::Query {
                return Err(ManifestError::Invalid {
                    field: path("verify"),
                    reason: format!(
                        "a verify capability is a query-tool contract, not a {role} one"
                    ),
                });
            }
            if !side_effecting {
                return Err(ManifestError::Invalid {
                    field: path("verify"),
                    reason: "a verify capability must also declare side_effecting: true"
                        .to_owned(),
                });
            }
            Some(capability)
        }
    };

    let default_on = match entry.get("default_on") {
        None | Some(Value::Null) => false,
        Some(raw) => raw.as_bool().ok_or_else(|| ManifestError::Invalid {
            field: path("default_on"),
            reason: "must be a boolean".to_owned(),
        })?,
    };
    if default_on && (role != ToolRole::Query || side_effecting) {
        return Err(ManifestError::Invalid {
            field: path("default_on"),
            reason: "only a query tool without side effects may be on by default".to_owned(),
        });
    }

    let scope_raw = entry.get("scope").ok_or_else(|| ManifestError::Missing {
        field: path("scope"),
    })?;
    let scope = parse_scope(scope_raw, &path("scope"))?;

    Ok(ToolManifest {
        tool,
        role,
        ingest,
        side_effecting,
        verify,
        default_on,
        scope,
    })
}

/// Parses a `FormatScope` and, unlike a plain `serde_json::from_value`,
/// validates every key as a real [`FormatId`] here rather than leaving that
/// to whatever reads the scope later — a manifest declaring
/// `"output_formats": ["AEM"]` (wrong case) is a malformed manifest, not a
/// scope that silently matches nothing.
///
/// `input_formats`, if present, is parsed for validity (so a malformed
/// value is still a malformed manifest) and then discarded: it is a
/// tolerated relic of the old two-sided scope, not part of this build's
/// contract, and nothing reads it.
fn parse_scope(raw: &Value, path: &str) -> Result<FormatScope, ManifestError> {
    let side = |key: &str| -> Result<Vec<FormatId>, ManifestError> {
        let array =
            raw.get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| ManifestError::Missing {
                    field: format!("{path}.{key}"),
                })?;
        array
            .iter()
            .map(|v| {
                let s = v.as_str().ok_or_else(|| ManifestError::Invalid {
                    field: format!("{path}.{key}"),
                    reason: "must be a string".to_owned(),
                })?;
                FormatId::parse(s).map_err(|FormatIdError { value }| ManifestError::Invalid {
                    field: format!("{path}.{key}"),
                    reason: format!("{value:?} is not a valid format id"),
                })
            })
            .collect()
    };

    if let Some(array) = raw.get("input_formats").and_then(Value::as_array) {
        for v in array {
            let s = v.as_str().ok_or_else(|| ManifestError::Invalid {
                field: format!("{path}.input_formats"),
                reason: "must be a string".to_owned(),
            })?;
            FormatId::parse(s).map_err(|FormatIdError { value }| ManifestError::Invalid {
                field: format!("{path}.input_formats"),
                reason: format!("{value:?} is not a valid format id"),
            })?;
        }
    }

    Ok(FormatScope {
        output_formats: side("output_formats")?,
    })
}

fn parse_test_vectors(raw: &Value) -> Result<Vec<TestVector>, ManifestError> {
    let Some(array) = raw.get("test_vectors").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    array
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let tool = entry
                .get("tool")
                .and_then(Value::as_str)
                .ok_or(ManifestError::Missing {
                    field: field("test_vectors[].tool"),
                })?
                .to_owned();
            let args = entry.get("args").cloned().unwrap_or(Value::Null);
            let expect = entry.get("expect").cloned().unwrap_or(Value::Null);
            let _ = i;
            Ok(TestVector { tool, args, expect })
        })
        .collect()
}

fn parse_format(raw: &Value) -> Result<Option<FormatModule>, ManifestError> {
    match raw.get("format") {
        None | Some(Value::Null) => Ok(None),
        Some(format) => {
            let key_raw =
                format
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or(ManifestError::Missing {
                        field: field("format.key"),
                    })?;
            let key = FormatId::parse(key_raw).map_err(|e| ManifestError::Invalid {
                field: "format.key".to_owned(),
                reason: e.to_string(),
            })?;
            let version = parse_version(format, "version")?;
            let description_md = format
                .get("description_md")
                .and_then(Value::as_str)
                .ok_or(ManifestError::Missing {
                    field: field("format.description_md"),
                })?
                .to_owned();
            let json_schema = format
                .get("json_schema")
                .cloned()
                .ok_or(ManifestError::Missing {
                    field: field("format.json_schema"),
                })?;
            Ok(Some(FormatModule {
                key,
                version,
                description_md,
                json_schema,
            }))
        }
    }
}

/// Parses the optional `sessions` block, validating every named tool exists,
/// is a `query` tool, and is not `side_effecting` -- a session lives inside
/// u2s by construction, so it never has the outside-u2s effect that flag
/// means, and a manifest claiming otherwise is contradicting itself.
fn parse_sessions(
    raw: &Value,
    tools: &[ToolManifest],
) -> Result<Option<SessionSupport>, ManifestError> {
    let Some(sessions) = raw.get("sessions") else {
        return Ok(None);
    };
    if sessions.is_null() {
        return Ok(None);
    }

    let find = |name: &str| tools.iter().find(|t| t.tool == name);

    let session_tool = |key: &'static str| -> Result<String, ManifestError> {
        let name = sessions
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| ManifestError::Missing {
                field: format!("sessions.{key}"),
            })?
            .to_owned();
        check_session_tool(&name, &format!("sessions.{key}"), find)?;
        Ok(name)
    };

    let open = session_tool("open")?;
    let close = session_tool("close")?;
    let probe = session_tool("probe")?;

    if open == close || open == probe || close == probe {
        return Err(ManifestError::Invalid {
            field: "sessions".to_owned(),
            reason: format!(
                "open ({open:?}), close ({close:?}) and probe ({probe:?}) must be three \
                 distinct tools"
            ),
        });
    }

    let mutators = match sessions.get("mutators") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => {
            let array = value.as_array().ok_or_else(|| ManifestError::Invalid {
                field: "sessions.mutators".to_owned(),
                reason: "must be an array".to_owned(),
            })?;
            let mut names = Vec::with_capacity(array.len());
            let mut seen = std::collections::HashSet::new();
            for (i, entry) in array.iter().enumerate() {
                let name = entry.as_str().ok_or_else(|| ManifestError::Invalid {
                    field: format!("sessions.mutators[{i}]"),
                    reason: "must be a string".to_owned(),
                })?;
                check_session_tool(name, &format!("sessions.mutators[{i}]"), find)?;
                if !seen.insert(name) {
                    return Err(ManifestError::Invalid {
                        field: "sessions.mutators".to_owned(),
                        reason: format!("{name:?} declared more than once"),
                    });
                }
                names.push(name.to_owned());
            }
            names
        }
    };

    Ok(Some(SessionSupport {
        open,
        close,
        probe,
        mutators,
    }))
}

/// The two checks every tool named in `sessions` must pass: it exists, and
/// it is an ordinary, non-side-effecting query tool.
fn check_session_tool<'a>(
    name: &str,
    field_path: &str,
    find: impl Fn(&str) -> Option<&'a ToolManifest>,
) -> Result<(), ManifestError> {
    let tool = find(name).ok_or_else(|| ManifestError::Invalid {
        field: field_path.to_owned(),
        reason: format!("{name:?} is not one of this manifest's tools"),
    })?;
    if tool.role != ToolRole::Query {
        return Err(ManifestError::Invalid {
            field: field_path.to_owned(),
            reason: format!("{name:?} is a {} tool, not a query tool", tool.role),
        });
    }
    if tool.side_effecting {
        return Err(ManifestError::Invalid {
            field: field_path.to_owned(),
            reason: format!(
                "{name:?} declares side_effecting: true, but a session lives inside u2s and \
                 can never have that kind of effect"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_query_manifest() -> Value {
        json!({
            "contract": "1.0.0",
            "server": { "name": "u2s-xfa-mcp", "version": "0.1.0" },
            "tools": [
                { "tool": "xfa_outline", "role": "query", "ingest": "outline",
                  "scope": { "output_formats": [] } }
            ],
            "test_vectors": [
                { "tool": "xfa_outline", "args": { "doc_path": "$FIXTURES/x.pdf" },
                  "expect": { "structured": { "count": 1 } } }
            ]
        })
    }

    #[test]
    fn parses_a_real_server_manifest_shape() {
        let m = ServerManifest::parse(&valid_query_manifest()).expect("parses");
        assert_eq!(m.contract, semver::Version::new(1, 0, 0));
        assert_eq!(m.server.name, "u2s-xfa-mcp");
        assert_eq!(m.tools.len(), 1);
        assert_eq!(m.tools[0].role, ToolRole::Query);
        assert_eq!(m.tools[0].ingest, Some(IngestCapability::Outline));
        assert_eq!(m.tools[0].scope.output_formats, Vec::<FormatId>::new());
        assert!(m.format.is_none());
    }

    #[test]
    fn ingest_is_absent_when_not_declared() {
        let mut raw = valid_query_manifest();
        raw["tools"][0].as_object_mut().unwrap().remove("ingest");
        let m = ServerManifest::parse(&raw).expect("parses");
        assert_eq!(m.tools[0].ingest, None);
    }

    #[test]
    fn an_unknown_ingest_capability_is_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["ingest"] = json!("rasterize_everything");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "tools[0].ingest"
        ));
    }

    #[test]
    fn an_ingest_capability_on_a_non_query_role_is_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["role"] = json!("decode");
        raw["tools"][0]["ingest"] = json!("info");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "tools[0].ingest"
        ));
    }

    #[test]
    fn a_manifest_still_sending_input_formats_parses_and_the_value_is_discarded() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["scope"]["input_formats"] = json!(["pdf"]);
        let m = ServerManifest::parse(&raw).expect("a relic key must not break parsing");
        assert_eq!(
            m.tools[0].scope.output_formats,
            Vec::<FormatId>::new(),
            "input_formats must not leak into the surviving scope"
        );
    }

    #[test]
    fn a_malformed_input_formats_relic_is_still_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["scope"]["input_formats"] = json!(["PDF"]);
        let err = ServerManifest::parse(&raw).expect_err("wrong case must be refused even in the tolerated key");
        assert!(err.to_string().contains("PDF"));
    }

    #[test]
    fn parses_a_format_module_when_present() {
        let mut raw = valid_query_manifest();
        raw["format"] = json!({
            "key": "aem",
            "version": "1.0.0",
            "description_md": "AEM Adaptive Forms JSON",
            "json_schema": { "type": "object" }
        });
        let m = ServerManifest::parse(&raw).expect("parses");
        let format = m.format.expect("format present");
        assert_eq!(format.key, FormatId::parse("aem").unwrap());
        assert_eq!(format.description_md, "AEM Adaptive Forms JSON");
    }

    #[test]
    fn missing_contract_is_reported() {
        let mut raw = valid_query_manifest();
        raw.as_object_mut().unwrap().remove("contract");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Missing { field }) if field == "contract"
        ));
    }

    #[test]
    fn an_unparseable_contract_version_is_invalid_not_missing() {
        let mut raw = valid_query_manifest();
        raw["contract"] = json!("not-a-version");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "contract"
        ));
    }

    #[test]
    fn an_unknown_role_is_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["role"] = json!("delete_everything");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { .. })
        ));
    }

    #[test]
    fn a_badly_cased_format_id_in_scope_is_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["scope"]["output_formats"] = json!(["AEM"]);
        let err = ServerManifest::parse(&raw).expect_err("wrong case must be refused");
        assert!(err.to_string().contains("AEM"));
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let mut raw = valid_query_manifest();
        let tool = raw["tools"][0].clone();
        raw["tools"].as_array_mut().unwrap().push(tool);
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::DuplicateTool { .. })
        ));
    }

    #[test]
    fn compatibility_check_accepts_the_current_major_and_refuses_others() {
        let m = ServerManifest::parse(&valid_query_manifest()).expect("parses");
        assert!(m.check_compatible().is_ok());

        let mut future = m.clone();
        future.contract = semver::Version::new(2, 0, 0);
        assert!(matches!(
            future.check_compatible(),
            Err(ManifestError::UnsupportedContract { .. })
        ));
    }

    #[test]
    fn test_vectors_are_optional() {
        let mut raw = valid_query_manifest();
        raw.as_object_mut().unwrap().remove("test_vectors");
        let m = ServerManifest::parse(&raw).expect("parses");
        assert!(m.test_vectors.is_empty());
    }

    #[test]
    fn side_effecting_and_verify_are_absent_by_default() {
        let m = ServerManifest::parse(&valid_query_manifest()).expect("parses");
        assert!(!m.tools[0].side_effecting);
        assert_eq!(m.tools[0].verify, None);
    }

    #[test]
    fn a_side_effecting_verify_tool_parses() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["side_effecting"] = json!(true);
        raw["tools"][0]["verify"] = json!("run");
        let m = ServerManifest::parse(&raw).expect("parses");
        assert!(m.tools[0].side_effecting);
        assert_eq!(m.tools[0].verify, Some(VerifyCapability::Run));
    }

    #[test]
    fn an_unknown_verify_capability_is_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["side_effecting"] = json!(true);
        raw["tools"][0]["verify"] = json!("crawl_everything");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "tools[0].verify"
        ));
    }

    #[test]
    fn a_verify_capability_on_a_non_query_role_is_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0].as_object_mut().unwrap().remove("ingest");
        raw["tools"][0]["role"] = json!("decode");
        raw["tools"][0]["side_effecting"] = json!(true);
        raw["tools"][0]["verify"] = json!("run");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "tools[0].verify"
        ));
    }

    #[test]
    fn a_verify_capability_without_side_effecting_is_rejected() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["verify"] = json!("run");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "tools[0].verify"
        ));
    }

    #[test]
    fn side_effecting_must_be_a_boolean() {
        let mut raw = valid_query_manifest();
        raw["tools"][0]["side_effecting"] = json!("yes");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "tools[0].side_effecting"
        ));
    }

    fn manifest_with_session_tools() -> Value {
        let mut raw = valid_query_manifest();
        raw["tools"] = json!([
            { "tool": "xfa_outline", "role": "query", "ingest": "outline",
              "scope": { "output_formats": [] } },
            { "tool": "xfa_open", "role": "query", "scope": { "output_formats": [] } },
            { "tool": "xfa_close", "role": "query", "scope": { "output_formats": [] } },
            { "tool": "xfa_set", "role": "query", "scope": { "output_formats": [] } }
        ]);
        raw["sessions"] = json!({
            "open": "xfa_open",
            "close": "xfa_close",
            "probe": "xfa_outline",
            "mutators": ["xfa_set"]
        });
        raw
    }

    #[test]
    fn sessions_is_absent_when_not_declared() {
        let m = ServerManifest::parse(&valid_query_manifest()).expect("parses");
        assert!(m.sessions.is_none());
    }

    #[test]
    fn a_well_formed_session_block_parses() {
        let m = ServerManifest::parse(&manifest_with_session_tools()).expect("parses");
        let s = m.sessions.expect("sessions present");
        assert_eq!(s.open, "xfa_open");
        assert_eq!(s.close, "xfa_close");
        assert_eq!(s.probe, "xfa_outline");
        assert_eq!(s.mutators, vec!["xfa_set".to_string()]);
    }

    #[test]
    fn mutators_defaults_to_empty() {
        let mut raw = manifest_with_session_tools();
        raw["sessions"].as_object_mut().unwrap().remove("mutators");
        let m = ServerManifest::parse(&raw).expect("parses");
        assert!(m.sessions.unwrap().mutators.is_empty());
    }

    #[test]
    fn a_session_tool_that_does_not_exist_is_rejected() {
        let mut raw = manifest_with_session_tools();
        raw["sessions"]["open"] = json!("xfa_nonexistent");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "sessions.open"
        ));
    }

    #[test]
    fn a_session_tool_that_is_side_effecting_is_rejected() {
        let mut raw = manifest_with_session_tools();
        raw["tools"][1]["side_effecting"] = json!(true);
        let err = ServerManifest::parse(&raw).expect_err("must be refused");
        assert!(err.to_string().contains("xfa_open"), "{err}");
    }

    #[test]
    fn a_session_tool_that_is_not_a_query_tool_is_rejected() {
        let mut raw = manifest_with_session_tools();
        raw["tools"][1]["role"] = json!("decode");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "sessions.open"
        ));
    }

    #[test]
    fn open_close_and_probe_must_be_distinct() {
        let mut raw = manifest_with_session_tools();
        raw["sessions"]["probe"] = json!("xfa_open");
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "sessions"
        ));
    }

    #[test]
    fn duplicate_mutators_are_rejected() {
        let mut raw = manifest_with_session_tools();
        raw["sessions"]["mutators"] = json!(["xfa_set", "xfa_set"]);
        assert!(matches!(
            ServerManifest::parse(&raw),
            Err(ManifestError::Invalid { field, .. }) if field == "sessions.mutators"
        ));
    }
}
