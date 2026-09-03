//! Conversion of a structured document into a [`RedactoDump`].

use uuid::Uuid;

use super::content::{render_block_html, render_footer_html, render_header_html};
use super::{
    AssetRow, AssetType, AssetVersionRow, DocumentRow, DocumentVersionRow, FooterField,
    INITIAL_VERSION, ObjectType, OwnerType, OwnershipRow, OwnershipType, RedactoComponent,
    RedactoConfig, RedactoConfiguration, RedactoDocumentMeta, RedactoDump, RelationRow, asset_ref,
};
use crate::structured::{StructuredNode, collect_footnote_nodes};

/// Number of columns a `layout-split-block` panel is designed for.
const SPLIT_PANEL_COLUMNS: usize = 2;

/// Generate the Redacto dump for a structured document.
pub fn generate_redacto_dump(nodes: &[StructuredNode], config: &RedactoConfig) -> RedactoDump {
    let markers: Vec<String> = collect_footnote_nodes(nodes)
        .iter()
        .filter_map(|f| f.marker.clone())
        .collect();

    // Every asset needs at least one variant, otherwise the dump would contain
    // `assets` rows with no `asset_version` and Redacto could not resolve them.
    let fallback;
    let config = if config.languages.is_empty() {
        fallback = RedactoConfig {
            languages: vec![config.master_language.clone()],
            ..config.clone()
        };
        &fallback
    } else {
        config
    };

    let mut builder = DumpBuilder::new(config, markers);
    let mut body = builder.walk(nodes);
    if let Some(panel) = builder.take_footnote_panel() {
        body.push(panel);
    }
    builder.finish(body)
}

/// Generate the Redacto dump and serialise it to SQL.
pub fn generate_redacto_sql(nodes: &[StructuredNode], config: &RedactoConfig) -> String {
    generate_redacto_dump(nodes, config).to_sql()
}

// ============================================================================
// Builder
// ============================================================================

/// Accumulates assets, components and warnings while walking the tree.
struct DumpBuilder<'a> {
    config: &'a RedactoConfig,
    markers: Vec<String>,
    assets: Vec<AssetRow>,
    asset_versions: Vec<AssetVersionRow>,
    /// Asset business identifiers of the footnote blocks, held back so they can
    /// be flushed into a trailing panel.
    footnote_refs: Vec<String>,
    warnings: Vec<String>,
}

impl<'a> DumpBuilder<'a> {
    fn new(config: &'a RedactoConfig, markers: Vec<String>) -> Self {
        Self {
            config,
            markers,
            assets: Vec::new(),
            asset_versions: Vec::new(),
            footnote_refs: Vec::new(),
            warnings: config.column_width_warnings(),
        }
    }

    /// Walk a sibling list, producing its components.
    ///
    /// Consecutive content blocks are collected into a run and flushed into a
    /// single `assetContainer` whenever a panel interrupts them or the list
    /// ends — the shape Redacto documents are authored in.
    fn walk(&mut self, nodes: &[StructuredNode]) -> Vec<RedactoComponent> {
        let mut components = Vec::new();
        let mut run: Vec<String> = Vec::new();

        for node in nodes {
            match node {
                StructuredNode::Heading(_)
                | StructuredNode::Paragraph(_)
                | StructuredNode::List(_)
                | StructuredNode::Table(_)
                | StructuredNode::Notice(_)
                | StructuredNode::Html(_) => {
                    if let Some(reference) = self.emit_asset(node) {
                        run.push(reference);
                    }
                }
                StructuredNode::Footnote(_) => {
                    if let Some(reference) = self.emit_asset(node) {
                        self.footnote_refs.push(reference);
                    }
                }
                StructuredNode::GridLayout(grid) => {
                    flush_run(&mut run, &mut components);
                    if grid.columns != SPLIT_PANEL_COLUMNS {
                        self.warnings.push(format!(
                            "grid layout with {} columns rendered as a {} panel ({SPLIT_PANEL_COLUMNS} columns)",
                            grid.columns, self.config.grid_panel_style
                        ));
                    }
                    let nested: Vec<RedactoComponent> = grid
                        .elements
                        .iter()
                        .flat_map(|element| self.walk(std::slice::from_ref(&element.node)))
                        .collect();
                    if !nested.is_empty() {
                        components.push(RedactoComponent::StyledPanel {
                            id: new_id(),
                            style: self.config.grid_panel_style.clone(),
                            components: nested,
                        });
                    }
                }
                StructuredNode::Group(group) if group.column_flow => {
                    // The source laid this section out as a multi-column text
                    // flow. Redacto reproduces it with a panel whose CSS sets
                    // `column-count`, so the children stay a single ordered run
                    // and the renderer balances them across the columns.
                    flush_run(&mut run, &mut components);
                    let nested = self.walk(&group.children);
                    if !nested.is_empty() {
                        components.push(RedactoComponent::StyledPanel {
                            id: new_id(),
                            style: self.config.column_panel_style.clone(),
                            components: nested,
                        });
                    }
                }
                StructuredNode::Group(group) => {
                    // A group is a purely structural wrapper; inline it so its
                    // children stay in the surrounding run.
                    let nested = self.walk(&group.children);
                    merge_nested(nested, &mut run, &mut components);
                }
                StructuredNode::Conditional(conditional) => {
                    self.warnings.push(
                        "conditional content flattened: the Redacto target has no conditionals"
                            .to_string(),
                    );
                    let nested = self.walk(std::slice::from_ref(conditional.content.as_ref()));
                    merge_nested(nested, &mut run, &mut components);
                }
                StructuredNode::Repeatable(repeatable) => {
                    self.warnings.push(
                        "repeatable content emitted once: the Redacto target has no repeatables"
                            .to_string(),
                    );
                    let nested = self.walk(std::slice::from_ref(repeatable.item.as_ref()));
                    merge_nested(nested, &mut run, &mut components);
                }
                StructuredNode::Field(field) => {
                    let label = field
                        .label
                        .as_ref()
                        .map(|l| l.plain_text_in(&self.config.master_language))
                        .filter(|l| !l.trim().is_empty())
                        .unwrap_or_else(|| field.name.to_string());
                    self.warnings.push(format!(
                        "field '{label}' skipped: the Redacto target supports text-only documents"
                    ));
                }
                StructuredNode::Image(image) => {
                    let alt = image.alt_text.clone().unwrap_or_else(|| "unnamed".into());
                    self.warnings.push(format!(
                        "image '{alt}' skipped: image assets are not generated"
                    ));
                }
                StructuredNode::Empty => {}
            }
        }

        flush_run(&mut run, &mut components);
        self.join_adjacent_column_panels(components)
    }

    /// Fuse column panels that ended up next to each other into one panel.
    ///
    /// Nothing separates two adjacent column panels, so they are one continuous
    /// multi-column flow in the source and the break between them is an
    /// artefact of how the section happened to be grouped. Joining them lets
    /// the CSS balance the whole flow across the columns instead of restarting
    /// the balance halfway through.
    fn join_adjacent_column_panels(
        &self,
        components: Vec<RedactoComponent>,
    ) -> Vec<RedactoComponent> {
        let mut out: Vec<RedactoComponent> = Vec::with_capacity(components.len());

        for component in components {
            match component {
                RedactoComponent::StyledPanel {
                    id,
                    style,
                    components: nested,
                } if style == self.config.column_panel_style => match out.pop() {
                    Some(RedactoComponent::StyledPanel {
                        id: open_id,
                        style: open_style,
                        components: mut open,
                    }) if open_style == style => {
                        open.extend(nested);
                        out.push(RedactoComponent::StyledPanel {
                            id: open_id,
                            style,
                            components: join_adjacent_runs(open),
                        });
                    }
                    previous => {
                        out.extend(previous);
                        out.push(RedactoComponent::StyledPanel {
                            id,
                            style,
                            components: nested,
                        });
                    }
                },
                other => out.push(other),
            }
        }

        out
    }

    /// Create the `assets` row and one `asset_version` per language for a
    /// content block, returning its [`asset_ref`] identifier.
    fn emit_asset(&mut self, node: &StructuredNode) -> Option<String> {
        let asset_pk = new_id();
        let asset_id = new_id();

        let mut versions = Vec::with_capacity(self.config.languages.len());
        for language in &self.config.languages {
            let content = render_block_html(node, language, &self.markers)?;
            versions.push(AssetVersionRow {
                id: new_id(),
                created: self.config.created.clone(),
                language: language.clone(),
                version: INITIAL_VERSION,
                status: self.config.status,
                content,
                asset_fk_id: asset_pk.clone(),
            });
        }

        self.assets.push(AssetRow {
            id: asset_pk,
            created: self.config.created.clone(),
            asset_id: asset_id.clone(),
            asset_type: AssetType::Text,
        });
        self.asset_versions.extend(versions);

        Some(asset_ref(&asset_id, INITIAL_VERSION))
    }

    /// Wrap the collected footnote assets into their trailing styled panel.
    fn take_footnote_panel(&mut self) -> Option<RedactoComponent> {
        let refs = std::mem::take(&mut self.footnote_refs);
        if refs.is_empty() {
            return None;
        }
        Some(RedactoComponent::StyledPanel {
            id: new_id(),
            style: self.config.footnote_panel_style.clone(),
            components: vec![RedactoComponent::AssetContainer {
                id: new_id(),
                assets: refs,
            }],
        })
    }

    /// Build one furniture text asset (header or footer) from already-rendered
    /// per-language HTML bodies, aligned 1:1 with `self.config.languages`.
    fn emit_furniture_asset(&mut self, contents: Vec<String>) -> Vec<RedactoComponent> {
        let config = self.config;
        let asset_pk = new_id();
        let asset_id = new_id();

        for (language, content) in config.languages.iter().zip(contents) {
            self.asset_versions.push(AssetVersionRow {
                id: new_id(),
                created: config.created.clone(),
                language: language.clone(),
                version: INITIAL_VERSION,
                status: config.status,
                content,
                asset_fk_id: asset_pk.clone(),
            });
        }
        self.assets.push(AssetRow {
            id: asset_pk,
            created: config.created.clone(),
            asset_id: asset_id.clone(),
            asset_type: AssetType::Text,
        });

        vec![RedactoComponent::AssetContainer {
            id: new_id(),
            assets: vec![asset_ref(&asset_id, INITIAL_VERSION)],
        }]
    }

    /// Emit the text asset for the page header.
    ///
    /// The section carries one asset with one version per document language:
    /// Redacto fails a render when a referenced asset has no version for the
    /// requested language, so a language without header text of its own takes
    /// the master language's rather than being left out. A header no language
    /// has any text for stays empty and gets no asset at all.
    fn header_section(&mut self) -> Vec<RedactoComponent> {
        let config = self.config;
        let non_blank = |language: &str| -> Option<&str> {
            config
                .headers
                .get(language)
                .map(String::as_str)
                .filter(|v| !v.trim().is_empty())
        };
        let resolve =
            |language: &str| non_blank(language).or_else(|| non_blank(&config.master_language));

        if config.languages.iter().all(|l| resolve(l).is_none()) {
            return Vec::new();
        }

        // A language the master cannot cover either still needs a version, so
        // it renders as empty rather than failing the whole document.
        let contents: Vec<String> = config
            .languages
            .iter()
            .map(|language| match resolve(language) {
                Some(text) => render_header_html(text),
                None => super::content::SPACER.to_string(),
            })
            .collect();

        self.emit_furniture_asset(contents)
    }

    /// Emit the text asset for the page footer.
    ///
    /// Mirrors [`header_section`](Self::header_section)'s per-language
    /// fallback, but the unit of "has content" is the whole field record: a
    /// language falls back to the master's fields only when NONE of its own
    /// fields have a value — mixing one language's version with another's
    /// form id would be nonsensical. The footer is skipped entirely only when
    /// no language, including the master, has any field content at all.
    /// Whenever the section IS built, every language's asset carries the
    /// legacy page-number counter after its field spans, even a language whose
    /// own and master fallback fields are both entirely blank, since
    /// pagination must keep working regardless.
    fn footer_section(&mut self) -> Vec<RedactoComponent> {
        let config = self.config;
        let has_content =
            |fields: &[FooterField]| fields.iter().any(|f| !f.value.trim().is_empty());
        let non_blank = |language: &str| -> Option<&[FooterField]> {
            config
                .footers
                .get(language)
                .map(Vec::as_slice)
                .filter(|fields| has_content(fields))
        };
        let resolve =
            |language: &str| non_blank(language).or_else(|| non_blank(&config.master_language));

        if config.languages.iter().all(|l| resolve(l).is_none()) {
            return Vec::new();
        }

        let contents: Vec<String> = config
            .languages
            .iter()
            .map(|language| render_footer_html(resolve(language).unwrap_or(&[])))
            .collect();

        self.emit_furniture_asset(contents)
    }

    /// Assemble the document, ownership and relation rows around the assets.
    fn finish(mut self, body: Vec<RedactoComponent>) -> RedactoDump {
        // Build the furniture before the ownership and relation rows below:
        // they iterate `self.assets`, so the header and footer assets pick up
        // the same ORIGIN ownership and relation rows as the body's.
        let header = self.header_section();
        let footer = self.footer_section();

        let config = self.config;
        let document_pk = new_id();

        let configuration = RedactoConfiguration {
            schema: config.schema.clone(),
            document: RedactoDocumentMeta {
                id: config.document_id.clone(),
                title: config.title.clone(),
                style: config.style.clone(),
            },
            // Left empty: the analysis recovers one master-page header without
            // distinguishing the first page, and an absent `firstHeader` makes
            // Redacto use the normal header on the first page too.
            first_header: Vec::new(),
            header,
            body,
            footer,
        };

        let document_versions = config
            .languages
            .iter()
            .map(|language| DocumentVersionRow {
                id: new_id(),
                created: config.created.clone(),
                language: language.clone(),
                version: INITIAL_VERSION,
                status: config.status,
                document_fk_id: document_pk.clone(),
            })
            .collect();

        // The document owner row is mandatory: without it Redacto rejects every
        // authoring write against the document.
        let mut ownerships = vec![OwnershipRow {
            id: new_id(),
            created: config.created.clone(),
            owner_id: config.owner_id.clone(),
            owner_type: OwnerType::User,
            ownership_type: OwnershipType::Owner,
            object_id: config.document_id.clone(),
            object_type: ObjectType::Document,
        }];
        let mut relations = Vec::with_capacity(self.assets.len());
        for asset in &self.assets {
            let reference = asset_ref(&asset.asset_id, INITIAL_VERSION);
            ownerships.push(OwnershipRow {
                id: new_id(),
                created: config.created.clone(),
                owner_id: config.document_id.clone(),
                owner_type: OwnerType::Document,
                ownership_type: OwnershipType::Origin,
                object_id: reference.clone(),
                object_type: ObjectType::Text,
            });
            relations.push(RelationRow {
                id: new_id(),
                created: config.created.clone(),
                relates_to: config.document_id.clone(),
                object_id: reference,
                object_type: ObjectType::Text,
            });
        }

        RedactoDump {
            assets: self.assets,
            asset_versions: self.asset_versions,
            documents: vec![DocumentRow {
                id: document_pk,
                created: config.created.clone(),
                document_id: config.document_id.clone(),
                form_path: config.form_path.clone(),
                configuration,
            }],
            document_versions,
            ownerships,
            relations,
            warnings: self.warnings,
        }
    }
}

// ============================================================================
// Component-run helpers
// ============================================================================

/// Flush a run of asset references into an `assetContainer`.
fn flush_run(run: &mut Vec<String>, components: &mut Vec<RedactoComponent>) {
    if run.is_empty() {
        return;
    }
    components.push(RedactoComponent::AssetContainer {
        id: new_id(),
        assets: std::mem::take(run),
    });
}

/// Collapse consecutive `assetContainer` components into a single one.
///
/// Used when two panels are joined: each contributed its own run, and the two
/// runs meeting at the seam are one ordered run. The first container keeps its
/// identifier.
fn join_adjacent_runs(components: Vec<RedactoComponent>) -> Vec<RedactoComponent> {
    let mut out: Vec<RedactoComponent> = Vec::with_capacity(components.len());
    let mut run: Vec<String> = Vec::new();
    let mut run_id: Option<String> = None;

    for component in components {
        match component {
            RedactoComponent::AssetContainer { id, assets } => {
                run_id.get_or_insert(id);
                run.extend(assets);
            }
            panel => {
                if let Some(id) = run_id.take() {
                    out.push(RedactoComponent::AssetContainer {
                        id,
                        assets: std::mem::take(&mut run),
                    });
                }
                out.push(panel);
            }
        }
    }
    if let Some(id) = run_id {
        out.push(RedactoComponent::AssetContainer { id, assets: run });
    }

    out
}

/// Splice the components of an inlined wrapper into the surrounding run.
///
/// A leading `assetContainer` continues the open run instead of starting a new
/// component, so a `Group` of paragraphs stays a single container.
fn merge_nested(
    nested: Vec<RedactoComponent>,
    run: &mut Vec<String>,
    components: &mut Vec<RedactoComponent>,
) {
    for (index, component) in nested.into_iter().enumerate() {
        match component {
            RedactoComponent::AssetContainer { assets, .. } if index == 0 => run.extend(assets),
            RedactoComponent::AssetContainer { id, assets } => {
                flush_run(run, components);
                components.push(RedactoComponent::AssetContainer { id, assets });
            }
            panel => {
                flush_run(run, components);
                components.push(panel);
            }
        }
    }
}

/// Fresh identifier for a row primary key, asset business id or component id.
fn new_id() -> String {
    Uuid::new_v4().to_string()
}
