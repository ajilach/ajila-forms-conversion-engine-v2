//! The tool executor: one match over the catalog's tool names.
//!
//! Each arm is the tool's own work; the shared plumbing — the `AI:` snapshot
//! label, the "no tree yet" guard, the Ok/Err mapping — lives on the three
//! edit helpers in the parent module.

use super::*;

/// Lines `read_package_file` returns by default, when the caller does not ask
/// for a specific window.
///
/// Unlike `read_reference_file`'s `offset`/`limit` (where 0 means "the whole
/// file"), a package file is not bounded the same way: an authored
/// `.content.xml` reached 250 KB in a run that blew its context window. 500
/// lines is ~31,000 characters at the density observed in real forms (~62
/// bytes/line), well under `pipeline::run::LARGE_TOOL_REPLY_WARN_CHARS`.
pub(super) const DEFAULT_TEXT_WINDOW_LINES: usize = 500;

/// Hard ceiling on a windowed tool's *total* reply, however it was reached.
///
/// `DEFAULT_TEXT_WINDOW_LINES` bounds the default, but an explicit `limit` has
/// no ceiling of its own. This is the backstop that holds regardless: no
/// windowed reply can reach the scale of the incident that prompted it (873,000
/// characters) however large a limit is requested. Comfortably above the
/// default so it only ever engages on a large *explicit* request, never
/// silently on ordinary use.
pub(super) const MAX_TOTAL_REPLY_CHARS: usize = 400_000;

/// A bounded line-range slice of `content`, with a note appended when the
/// window does not reach the end.
///
/// `offset`/`limit` are both in lines; `limit == 0` (absent, or explicitly 0)
/// means [`DEFAULT_TEXT_WINDOW_LINES`], not "everything" — see that constant's
/// doc for why `read_package_file` cannot default to unbounded the way
/// `read_reference_file`'s `offset`/`limit` does. Does not itself
/// apply [`MAX_TOTAL_REPLY_CHARS`]: that is a property of the whole reply a
/// caller assembles, not of one source's window — see [`cap_total`].
///
/// `pub(super)` so it can be unit-tested directly, on synthetic content, rather
/// than only indirectly through a built package.
pub(super) fn windowed_text(content: &str, offset: usize, limit: usize) -> String {
    let limit = if limit == 0 {
        DEFAULT_TEXT_WINDOW_LINES
    } else {
        limit
    };
    let total = content.lines().count();

    // The whole source already fits in one window: return it byte for byte
    // rather than reconstructing through `lines().join("\n")`, which would
    // silently drop a trailing newline or normalize CRLF to LF even though
    // nothing was actually windowed.
    if offset == 0 && limit >= total {
        return content.to_string();
    }
    if offset >= total {
        return format!("[offset {offset} is past the end — this source has only {total} lines]");
    }

    let window: Vec<&str> = content.lines().skip(offset).take(limit).collect();
    let shown = window.len();
    let mut out = window.join("\n");
    if offset + shown < total {
        out.push_str(&format!(
            "\n[showing lines {}-{} of {total} — pass a higher offset to continue, or a \
             higher limit to read more at once]",
            offset + 1,
            offset + shown
        ));
    }
    out
}

/// Truncate `text` to at most [`MAX_TOTAL_REPLY_CHARS`], noting it when it
/// had to. The backstop `windowed_text` cannot provide on its own — see
/// [`MAX_TOTAL_REPLY_CHARS`]'s doc for why one is still needed after windowing.
pub(super) fn cap_total(text: String) -> String {
    if text.len() <= MAX_TOTAL_REPLY_CHARS {
        return text;
    }
    // `text` is UTF-8; cut lands mid-codepoint unless walked back to a
    // boundary.
    let mut cut = MAX_TOTAL_REPLY_CHARS;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n[reply truncated at {MAX_TOTAL_REPLY_CHARS} characters — narrow the request (a \
         smaller limit) instead of reading this much at once]",
        &text[..cut]
    )
}

impl ConversionAgent {
    pub async fn execute(&mut self, name: &str, input: &serde_json::Value) -> ToolReply {
        if let Some(refusal) = self.target_refusal(name) {
            return ToolReply::Error(refusal);
        }

        match name {
            // §1 extraction
            "get_source_info" => match self.source_documents(input) {
                Ok(documents) => {
                    let languages = dedup(documents.iter().map(|(_, l, _)| l.as_str()).collect());
                    let documents: Vec<_> = documents
                        .iter()
                        .map(|(name, language, path)| {
                            serde_json::json!({
                                "name": name,
                                "language": language,
                                "doc_path": path.display().to_string(),
                            })
                        })
                        .collect();
                    ToolReply::Text(
                        serde_json::json!({ "languages": languages, "documents": documents })
                            .to_string(),
                    )
                }
                Err(e) => ToolReply::Error(e),
            },
            // §2a structured tree (Redacto target)
            "set_structured" => {
                let v = input.get("nodes").cloned().unwrap_or_else(|| input.clone());
                let headers = match input.get("headers") {
                    None | Some(serde_json::Value::Null) => None,
                    Some(h) => match serde_json::from_value::<BTreeMap<String, String>>(h.clone()) {
                        Ok(h) => Some(h),
                        Err(e) => {
                            return ToolReply::Error(format!(
                                "headers must map a language code to the page header text: {e}"
                            ));
                        }
                    },
                };
                match serde_json::from_value::<Vec<StructuredNode>>(v) {
                    Ok(nodes) => {
                        let count = nodes.len();
                        self.structured = nodes;
                        self.structured_edited("AI: set structured tree");
                        if let Some(headers) = headers {
                            self.set_headers(headers);
                        }
                        ToolReply::Text(format!(
                            "OK: working structured tree set ({count} top-level nodes, page \
                             headers for {:?}).",
                            self.headers.keys().collect::<Vec<_>>()
                        ))
                    }
                    Err(e) => ToolReply::Error(format!("Invalid StructuredNode JSON: {e}")),
                }
            }
            "get_structured_outline" => {
                if self.structured.is_empty() {
                    return ToolReply::Error(NO_STRUCTURED_TREE.into());
                }
                ToolReply::Text(crate::structured_edit::outline(&self.structured))
            }
            "get_structured_node" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                match crate::structured_edit::resolve_mut(&mut self.structured, &path) {
                    Ok(node) => {
                        ToolReply::Text(serde_json::to_string_pretty(node).unwrap_or_default())
                    }
                    Err(e) => ToolReply::Error(e),
                }
            }
            "set_structured_field" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                let field = input["field"].as_str().unwrap_or_default().to_string();
                let value = input
                    .get("value")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let result =
                    crate::structured_edit::set_field(&mut self.structured, &path, &field, value);
                self.edit_structured(format_args!("set {field} on {path}"), result)
            }
            "set_structured_fields" => {
                let edits: Vec<(String, String, serde_json::Value)> = input["edits"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|e| {
                                (
                                    e["path"].as_str().unwrap_or_default().to_string(),
                                    e["field"].as_str().unwrap_or_default().to_string(),
                                    e.get("value").cloned().unwrap_or(serde_json::Value::Null),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let count = edits.len();
                let result = crate::structured_edit::set_fields(&mut self.structured, &edits);
                self.edit_structured(format_args!("set {count} field(s)"), result)
            }
            "replace_structured_node" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                let node = input
                    .get("node")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let result =
                    crate::structured_edit::replace_node(&mut self.structured, &path, node);
                self.edit_structured(format_args!("replace {path}"), result)
            }
            "insert_structured_node" => {
                let parent = input["parent_path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let node = input
                    .get("node")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let pos = match crate::structured_edit::parse_insert_pos(
                    input.get("position").unwrap_or(&serde_json::Value::Null),
                ) {
                    Ok(p) => p,
                    Err(e) => return ToolReply::Error(e),
                };
                let result =
                    crate::structured_edit::insert_node(&mut self.structured, &parent, node, pos);
                self.edit_structured(format_args!("insert into {parent}"), result)
            }
            "remove_structured_node" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                let result = crate::structured_edit::remove_node(&mut self.structured, &path);
                self.edit_structured(format_args!("remove {path}"), result)
            }
            "build_redacto_dump" => {
                if self.structured.is_empty() {
                    return ToolReply::Error(NO_STRUCTURED_TREE.into());
                }
                match self.build_redacto() {
                    Ok((dump, config)) => {
                        let validation = blueprint::validate_dump(&dump, &config);
                        ToolReply::Text(
                            serde_json::to_string_pretty(&serde_json::json!({
                                "document_id": config.document_id,
                                "title": config.title,
                                "languages": config.languages,
                                "headers": config.headers,
                                "footers": config.footers,
                                "assets": validation.counts.assets,
                                "asset_versions": validation.counts.asset_versions,
                                "document_versions": validation.counts.document_versions,
                                "rows": validation.counts.rows,
                                "asset_containers": validation.counts.asset_containers,
                                "styled_panels": validation.counts.styled_panels,
                                "header_assets": validation.counts.header_assets,
                                "footer_assets": validation.counts.footer_assets,
                                "problems": validation.problems,
                                "warnings": validation.warnings,
                            }))
                            .unwrap_or_default(),
                        )
                    }
                    Err(e) => ToolReply::Error(e),
                }
            }
            "review_redacto_output" => {
                if self.structured.is_empty() {
                    return ToolReply::Error(NO_STRUCTURED_TREE.into());
                }
                let (dump, _) = match self.build_redacto() {
                    Ok(pair) => pair,
                    Err(e) => return ToolReply::Error(e),
                };
                let checks = blueprint::check_redacto_output(&dump);
                ToolReply::Text(serde_json::to_string_pretty(&checks).unwrap_or_default())
            }
            // §2 multilingual AEM tree (AemNodeTranslated)
            "set_aem_translated" => {
                let v = input.get("root").cloned().unwrap_or_else(|| input.clone());
                match serde_json::from_value::<AemNodeTranslated>(v) {
                    Ok(node) => {
                        if let Some(aem) = self.target.aem_mut() {
                            aem.tree = Some(node);
                        }
                        self.aem_translated_edited("AI: set AEM (translated) tree");
                        ToolReply::Text("OK — working AEM tree set (package invalidated).".into())
                    }
                    Err(e) => ToolReply::Error(format!("Invalid AemNodeTranslated JSON: {e}")),
                }
            }
            "get_aem_translated" => self.read_aem(|root| {
                ToolReply::Text(serde_json::to_string_pretty(root).unwrap_or_default())
            }),
            "get_aem_translated_outline" => {
                self.read_aem(|root| ToolReply::Text(crate::aem_translated_edit::outline(root)))
            }
            "get_aem_translated_node" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                self.read_aem(
                    |root| match crate::aem_translated_edit::resolve_mut(root, &path) {
                        Ok(node) => {
                            ToolReply::Text(serde_json::to_string_pretty(node).unwrap_or_default())
                        }
                        Err(e) => ToolReply::Error(e),
                    },
                )
            }
            "set_aem_translated_field" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                let field = input["field"].as_str().unwrap_or_default().to_string();
                if field.is_empty() {
                    return ToolReply::Error("`field` must not be empty.".into());
                }
                let value = input
                    .get("value")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                self.edit_aem(format_args!("set {field} on {path}"), |root| {
                    crate::aem_translated_edit::set_field(root, &path, &field, value)
                })
            }
            "replace_aem_translated_node" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                let node = input
                    .get("node")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                self.edit_aem(format_args!("replace {path}"), |root| {
                    crate::aem_translated_edit::replace_node(root, &path, node)
                })
            }
            "insert_aem_translated_node" => {
                let parent = input["parent_path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let node = input
                    .get("node")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let pos = match crate::aem_translated_edit::parse_insert_pos(&input["position"]) {
                    Ok(p) => p,
                    Err(e) => return ToolReply::Error(e),
                };
                self.edit_aem(format_args!("insert into {parent}"), |root| {
                    crate::aem_translated_edit::insert_node(root, &parent, node, pos)
                })
            }
            "remove_aem_translated_node" => {
                let path = input["path"].as_str().unwrap_or_default().to_string();
                self.edit_aem(format_args!("remove {path}"), |root| {
                    crate::aem_translated_edit::remove_node(root, &path)
                })
            }

            // §5 output
            "build_aem_package" => {
                let cfg = match self.config() {
                    Ok(c) => c,
                    Err(e) => return ToolReply::Error(e),
                };
                let (aem, translations) = match self.lower_aem_translated() {
                    Ok(pair) => pair,
                    Err(e) => return ToolReply::Error(e),
                };
                // Re-emit each loaded node's fidelity passthrough (raw attrs +
                // unmodeled children) so a template's load→edit→save round-trip
                // preserves what the typed model doesn't represent. Empty for
                // from-XFA trees, so their output is unchanged.
                let passthrough = self
                    .aem_tree()
                    .map(|t| t.passthrough_map())
                    .unwrap_or_default();
                let pkg = blueprint::to_aem_package_from_node_with_passthrough(
                    &aem,
                    &cfg,
                    translations,
                    &passthrough,
                );
                let size = pkg.len();

                // Also build the bound variant, so binding to the schema is a
                // choice at download time rather than a re-run. It needs its own
                // lowering: `bindRef`s are only derived when `bind_to_xsd` is on.
                let mut bound_note = String::new();
                let bound = if cfg.bind_to_xsd || cfg.xsd_path.is_none() {
                    // Already bound, or the profile names no schema location.
                    None
                } else {
                    let mut bound_cfg = cfg.clone();
                    bound_cfg.bind_to_xsd = true;
                    match self.lower_aem_translated_with(&bound_cfg) {
                        Ok((bound_aem, bound_translations)) => {
                            let bound_pkg = blueprint::to_aem_package_from_node_with_passthrough(
                                &bound_aem,
                                &bound_cfg,
                                bound_translations,
                                &passthrough,
                            );
                            bound_note = format!(" With bindRefs: {} bytes.", bound_pkg.len());
                            Some(bound_pkg)
                        }
                        Err(e) => {
                            bound_note = format!(" The bindRef variant failed: {e}");
                            None
                        }
                    }
                };
                if let Some(aem) = self.target.aem_mut() {
                    aem.package = Some(pkg);
                    aem.package_bound = bound;
                }
                ToolReply::Text(format!("Built package ({size} bytes).{bound_note}"))
            }
            "get_package_info" => match self.target.aem().and_then(|s| s.package.as_ref()) {
                Some(pkg) => {
                    let files = crate::references::unzip_package(pkg).unwrap_or_default();
                    let paths: Vec<&String> = files.iter().map(|(p, _)| p).collect();
                    ToolReply::Text(format!(
                        "size: {} bytes\nfiles:\n{}",
                        pkg.len(),
                        serde_json::to_string_pretty(&paths).unwrap_or_default()
                    ))
                }
                None => ToolReply::Error(NO_PACKAGE.into()),
            },
            "read_package_file" => {
                let path = input["path"].as_str().unwrap_or_default();
                // See `windowed_text`: an authored .content.xml reached 250 KB
                // in the run that overflowed the window, so the default here
                // is a bounded window, not the whole file.
                let offset = input["offset"].as_u64().unwrap_or(0) as usize;
                let limit = input["limit"].as_u64().unwrap_or(0) as usize;
                match self.target.aem().and_then(|s| s.package.as_ref()) {
                    Some(pkg) => match crate::references::unzip_package(pkg) {
                        Ok(files) => match files.iter().find(|(p, _)| p == path) {
                            Some((_, content)) => {
                                ToolReply::Text(cap_total(windowed_text(content, offset, limit)))
                            }
                            None => ToolReply::Error(format!("No such file: {path:?}")),
                        },
                        Err(e) => ToolReply::Error(e),
                    },
                    None => ToolReply::Error(NO_PACKAGE.into()),
                }
            }
            "validate_aem_package" => {
                let Some(pkg) = self.target.aem().and_then(|s| s.package.clone()) else {
                    return ToolReply::Error(NO_PACKAGE.into());
                };
                match validate_package_bytes(&pkg) {
                    Ok(msg) => ToolReply::Text(msg),
                    Err(e) => ToolReply::Error(e),
                }
            }
            "review_output" => {
                let (aem, _) = match self.lower_aem_translated() {
                    Ok(pair) => pair,
                    Err(e) => return ToolReply::Error(e),
                };
                let config = match self.config() {
                    Ok(c) => c,
                    Err(e) => return ToolReply::Error(e),
                };
                let checks = blueprint::check_aem_output(&aem, &config);
                ToolReply::Text(serde_json::to_string_pretty(&checks).unwrap_or_default())
            }
            "generate_xsd" => {
                let p = match self.profile.clone() {
                    Some(p) if blueprint::has_xsd_config(&p) => p,
                    _ => return ToolReply::Error("This profile has no XSD config.".into()),
                };
                let mut cfg = match blueprint::load_xsd_config(&p) {
                    Ok(cfg) => cfg,
                    Err(e) => return ToolReply::Error(e),
                };
                // The AEM tree is the source of truth on an AEM run, so derive
                // the schema straight from it: that is the same tree the package
                // ships, and its bindRefs are the schema's element paths. Fall
                // back to the structured content only when no tree exists yet.
                let fragments = self
                    .config()
                    .map(|c| {
                        cfg.form_code = Some(c.form_code.clone());
                        c.fragments.clone()
                    })
                    .unwrap_or_default();

                if self.aem_translated().is_some() {
                    let (aem, _) = match self.lower_aem_translated_lenient() {
                        Ok(pair) => pair,
                        Err(e) => return ToolReply::Error(e),
                    };
                    return ToolReply::Text(blueprint::generate_xsd_string_from_aem(
                        &aem, &cfg, &fragments,
                    ));
                }

                let content = match self.derived_output_content() {
                    Ok(c) => c,
                    Err(e) => return ToolReply::Error(e),
                };
                let aem_config = match self.config() {
                    Ok(c) => c,
                    Err(e) => return ToolReply::Error(e),
                };
                ToolReply::Text(blueprint::to_xsd(&content, &aem_config, &cfg))
            }
            "generate_html" => {
                match self.profile.as_deref() {
                    Some(p) if blueprint::has_html_config(p) => {}
                    _ => return ToolReply::Error("This profile has no HTML config.".into()),
                };
                let content = match self.derived_output_content() {
                    Ok(c) => c,
                    Err(e) => return ToolReply::Error(e),
                };
                // Deliberately no custom_styles: the profile's logo and font
                // files load as base64 data URIs (`load_html_custom_styles`),
                // and a real form's assets alone run past 800KB of base64 —
                // invisible to a text model, and it once cost a stage over
                // 800,000 prompt tokens in a single reply. The agent is
                // comparing structure and content against the source page
                // images, not brand fidelity; a visually exact export is what
                // the CLI's own `to_html` call is for.
                let cfg = blueprint::HtmlConfig::default();
                ToolReply::Text(blueprint::to_html(&content, &cfg))
            }

            // §6 deploy + verify (network)
            "upload_to_aem" => {
                let Some(conn) = self.conn.clone() else {
                    return ToolReply::Error("No AEM connection configured.".into());
                };
                let Some(pkg) = self.target.aem().and_then(|s| s.package.clone()) else {
                    return ToolReply::Error(NO_PACKAGE.into());
                };
                let cfg = match self.config() {
                    Ok(c) => c,
                    Err(e) => return ToolReply::Error(e),
                };
                match crate::aem_client::upload_and_install_package(&conn, pkg, &cfg.form_code)
                    .await
                {
                    Ok(()) => {
                        if let Some(aem) = self.target.aem_mut() {
                            aem.uploaded = true;
                            aem.form_path = Some(form_jcr_path(&cfg));
                        }
                        ToolReply::Text(format!(
                            "Uploaded and installed on AEM.\n{}",
                            form_urls_text(&conn.host, &cfg)
                        ))
                    }
                    Err(e) => ToolReply::Error(e),
                }
            }
            "fetch_aem_form_html" => {
                let (Some(conn), Ok(cfg)) = (self.conn.clone(), self.config()) else {
                    return ToolReply::Error("No AEM connection / profile configured.".into());
                };
                let path = form_jcr_path(&cfg);
                match crate::aem_client::fetch_form_html(&conn, &path).await {
                    Ok(html) => ToolReply::Text(truncate(&html, 8000)),
                    Err(e) => ToolReply::Error(e),
                }
            }
            "fetch_aem_dor_pdf" => {
                let (Some(conn), Ok(cfg)) = (self.conn.clone(), self.config()) else {
                    return ToolReply::Error("No AEM connection / profile configured.".into());
                };
                let path = form_jcr_path(&cfg);
                match crate::aem_client::fetch_dor_pdf(&conn, &path).await {
                    Ok(pdf) => match render_pdf_pages(&pdf) {
                        Ok(images) => ToolReply::Image {
                            media_type: "image/jpeg",
                            images,
                        },
                        Err(e) => ToolReply::Error(e),
                    },
                    Err(e) => ToolReply::Error(e),
                }
            }
            "aem_form_urls" => {
                let (Some(conn), Ok(cfg)) = (self.conn.clone(), self.config()) else {
                    return ToolReply::Error("No AEM connection / profile configured.".into());
                };
                if !self.aem_uploaded() {
                    return ToolReply::Error(
                        "The form has not been uploaded in this run yet: run upload_to_aem first."
                            .into(),
                    );
                }
                ToolReply::Text(form_urls_text(&conn.host, &cfg))
            }
            "inspect_pdf" => {
                let Some(dir) = self.browser.as_ref().map(|b| b.output_dir().to_path_buf()) else {
                    return ToolReply::Error(
                        "No browser session in this run, so nothing has been downloaded. Use \
                         fetch_aem_dor_pdf to look at the Document of Record instead."
                            .into(),
                    );
                };
                let path = input["path"]
                    .as_str()
                    .map(str::trim)
                    .filter(|p| !p.is_empty());
                match path {
                    None => match crate::browser::list_output_files(&dir) {
                        Ok(files) if files.is_empty() => ToolReply::Text(
                            "The browser output directory is empty: nothing has been downloaded yet."
                                .into(),
                        ),
                        Ok(files) => ToolReply::Text(
                            files
                                .iter()
                                .map(|f| format!("{}  {} bytes  {}", f.name, f.size, f.modified))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        Err(e) => ToolReply::Error(e),
                    },
                    Some(path) => {
                        let file = match crate::browser::resolve_inside(&dir, path) {
                            Ok(f) => f,
                            Err(e) => return ToolReply::Error(e),
                        };
                        // Check the magic bytes before reading a possibly large
                        // download that turns out not to be a PDF at all.
                        let mut head = [0u8; 8];
                        let read = std::fs::File::open(&file).and_then(|mut f| {
                            use std::io::Read;
                            f.read(&mut head)
                        });
                        let n = match read {
                            Ok(n) => n,
                            Err(e) => {
                                return ToolReply::Error(format!("could not read {path:?}: {e}"));
                            }
                        };
                        if !head[..n].starts_with(b"%PDF") {
                            return ToolReply::Error(format!(
                                "{path:?} is not a PDF (starts with {:?})",
                                String::from_utf8_lossy(&head[..n])
                            ));
                        }
                        let bytes = match std::fs::read(&file) {
                            Ok(b) => b,
                            Err(e) => {
                                return ToolReply::Error(format!("could not read {path:?}: {e}"));
                            }
                        };
                        match render_pdf_pages(&bytes) {
                            Ok(images) => ToolReply::Image {
                                media_type: "image/jpeg",
                                images,
                            },
                            Err(e) => ToolReply::Error(e),
                        }
                    }
                }
            }

            // §7 references
            "list_reference_forms" => {
                let profile = self.profile.clone().unwrap_or_default();
                let list: Vec<_> = crate::references::list_references(&profile)
                    .into_iter()
                    .map(|r| serde_json::json!({"ref_id": r.ref_id, "label": r.label, "description": r.description, "pdf_count": r.pdf_count, "files": r.files}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&list).unwrap_or_default())
            }
            "search_references" => {
                let profile = self.profile.clone().unwrap_or_default();
                let query = input["query"].as_str().unwrap_or_default().to_string();
                if query.trim().is_empty() {
                    return ToolReply::Error(
                        "search_references requires a non-empty query — pass a description of the \
                         input form/section, not an empty string."
                            .into(),
                    );
                }
                let top_k = input["top_k"].as_u64().unwrap_or(3).max(1) as usize;
                let matcher = match self.matcher() {
                    Ok(m) => m,
                    Err(e) => return ToolReply::Error(e),
                };
                let hits: Vec<_> =
                    crate::references::search_references(&profile, &query, matcher, top_k)
                        .into_iter()
                        .map(|h| serde_json::json!({"ref_id": h.ref_id, "label": h.label, "where": h.location, "matched": h.matched, "score": h.score, "snippet": h.snippet}))
                        .collect();
                ToolReply::Text(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "grep_references" => {
                let profile = self.profile.clone().unwrap_or_default();
                let query = input["query"].as_str().unwrap_or_default();
                let regex = input["regex"].as_bool().unwrap_or(false);
                let hits: Vec<_> = crate::references::grep_references(&profile, query, regex)
                    .into_iter()
                    .map(|h| serde_json::json!({"ref_id": h.ref_id, "label": h.label, "where": h.location, "snippet": h.snippet}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "read_reference_file" => {
                let ref_id = input["ref_id"].as_str().unwrap_or_default();
                let path = input["path"].as_str().unwrap_or_default();
                let offset = input["offset"].as_u64().unwrap_or(0) as usize;
                let limit = input["limit"].as_u64().unwrap_or(0) as usize;
                match crate::references::read_reference_file(ref_id, path, offset, limit) {
                    Ok(t) => ToolReply::Text(t),
                    Err(e) => ToolReply::Error(e),
                }
            }
            "get_reference_package" => {
                let ref_id = input["ref_id"].as_str().unwrap_or_default();
                let files = crate::references::get_reference_package_files(ref_id);
                let paths: Vec<&String> = files.iter().map(|(p, _)| p).collect();
                ToolReply::Text(serde_json::to_string_pretty(&paths).unwrap_or_default())
            }
            "list_reference_docs" => {
                let profile = self.profile.clone().unwrap_or_default();
                let list: Vec<_> = crate::references::list_docs(&profile)
                    .into_iter()
                    .map(|d| serde_json::json!({"doc_id": d.doc_id, "label": d.label}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&list).unwrap_or_default())
            }
            "read_reference_doc" => {
                let doc_id = input["doc_id"].as_str().unwrap_or_default();
                let offset = input["offset"].as_u64().unwrap_or(0) as usize;
                let limit = input["limit"].as_u64().unwrap_or(0) as usize;
                match crate::references::read_doc(doc_id, offset, limit) {
                    Ok(t) => ToolReply::Text(t),
                    Err(e) => ToolReply::Error(e),
                }
            }
            "grep_reference_docs" => {
                let profile = self.profile.clone().unwrap_or_default();
                let query = input["query"].as_str().unwrap_or_default();
                let regex = input["regex"].as_bool().unwrap_or(false);
                let hits: Vec<_> = crate::references::grep_docs(&profile, query, regex)
                    .into_iter()
                    .map(|(doc_id, label, snippet)| serde_json::json!({"doc_id": doc_id, "label": label, "snippet": snippet}))
                    .collect();
                ToolReply::Text(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }

            // §8 control
            "get_schema" => {
                // Unknown/absent `kind` keeps returning the AEM schema, which is
                // what every caller predating the structured target expects.
                let schema = match input["kind"].as_str() {
                    Some("structured") => blueprint::structured_schema(),
                    _ => blueprint::aem_translated_schema(),
                };
                ToolReply::Text(serde_json::to_string_pretty(&schema).unwrap_or_default())
            }
            "get_profile_info" => match self.config() {
                Ok(c) => ToolReply::Text(format!(
                    "form_code: {}\nlanguages: {:?}\nmaster_language: {}\nform_path: {}\nform_dir: {}\nbind_to_xsd: {}\nuse_fragments: {}",
                    c.form_code,
                    c.languages,
                    c.master_language,
                    c.form_path,
                    c.form_dir,
                    c.bind_to_xsd,
                    c.use_fragments
                )),
                Err(e) => ToolReply::Error(e),
            },
            "submit_review" => {
                let approved = input["approved"].as_bool().unwrap_or(false);
                let report = input["report"].as_str().unwrap_or_default().to_string();
                self.review = Some(ReviewResult { approved, report });
                ToolReply::Text(if approved {
                    "Review recorded: approved.".into()
                } else {
                    "Review recorded: changes requested — returning to the author.".into()
                })
            }

            other if crate::u2s::is_u2s_tool(other) => match self.u2s_tools() {
                Ok(tools) => tools.call(other, input).await,
                Err(e) => ToolReply::Error(e),
            },

            // Browser tools are not catalog entries: their specs come from the
            // attached session, and so do their results.
            other if other.starts_with("browser_") => match self.browser.as_mut() {
                Some(browser) => browser.call(other, input).await,
                None => ToolReply::Error(
                    "Browser tools are not available in this run (browser verification is off or \
                     no AEM connection is configured). Verify with fetch_aem_dor_pdf instead."
                        .into(),
                ),
            },
            other => ToolReply::Error(format!("Unknown tool: {other}")),
        }
    }
}
