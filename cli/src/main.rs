//! The `blueprint` command line.
//!
//! Two modes over one engine. Bare arguments run the deterministic pipeline —
//! parse, render, export — with no model involved. The `convert` subcommand runs
//! the AI conversion the desktop app runs (Analyst → Author → Reviewer, over the
//! `pipeline` controller and the shared `runner` transport), which is why the app
//! and this binary cannot drift apart.

mod console;
mod convert;

use blueprint::{
    FieldLabelMap, GraphSelection, GraphState, HtmlConfig, OutputTarget, PipelineConfig, PipelineEvent,
    PipelineStep, build_field_label_map, generate_dot, run_pipeline,
};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use log::info;
use std::path::{Path, PathBuf};

/// CLI render mode, wrapping the library's [`blueprint::RenderMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RenderMode {
    Plain,
    Labelled,
    Annotated,
}

/// Blueprint - XFA PDF document processor
#[derive(Parser, Debug)]
#[command(name = "blueprint")]
#[command(about = "Process and analyze XFA PDF documents", long_about = None)]
// A subcommand replaces the deterministic run entirely, rather than adding to
// it: the two modes share no arguments, and the document list is only required
// by the bare form.
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path(s) to the PDF document(s). Multiple files of the same document in
    /// different languages can be passed for multilingual output.
    #[arg(value_name = "DOCUMENT", required = true)]
    documents: Vec<PathBuf>,

    /// Render mode(s) for output images. Can be specified multiple times.
    /// Modes: plain, labelled, annotated
    #[arg(long = "render", value_enum)]
    render_modes: Vec<RenderMode>,

    /// Scale factor for rendering (default: 1.5)
    #[arg(short, long, default_value = "1.5")]
    scale: f32,

    /// Enable specific analysis modules (can be specified multiple times)
    /// Example: --module ubs --module custom
    #[arg(long = "module")]
    modules: Vec<String>,

    /// Export the structured form as JSON
    #[arg(long)]
    structured: bool,

    /// Export the form as a standalone HTML file with embedded CSS and JavaScript
    #[arg(long)]
    html: bool,

    /// Export the form as AEM Adaptive Forms JCR content XML.
    #[arg(long)]
    aem: bool,

    /// Export the form as an XSD (XML Schema Definition) file.
    #[arg(long)]
    xsd: bool,

    /// Export the form as a PostgreSQL dump for the Redacto platform.
    ///
    /// Intended for documents without input fields; any field encountered is
    /// skipped and reported as a warning.
    #[arg(long)]
    redacto: bool,

    /// Name of an embedded profile containing per-output configs.
    #[arg(long)]
    profile: Option<String>,

    /// Export a GraphViz DOT file showing the interactive decision flow
    #[arg(long)]
    graphviz: bool,

    /// Dump the raw XFA XML content to a file and exit
    #[arg(long)]
    dump_xfa: bool,
}

/// The modes that do something other than the deterministic export run.
///
/// The size gap between the variants is clap's business, not a cost: exactly one
/// is built, once, from the process arguments.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
enum Command {
    /// Convert a form with the AI agent: the desktop app's pipeline, headless.
    Convert(convert::ConvertArgs),

    /// List the conversion sessions a `convert --feedback` run can resume.
    Sessions,

    /// The Docker-hosted verifiers the Author and Reviewer check their output
    /// with: the AEM Forms instance for an AEM run, Postgres for a Redacto run.
    Verify(VerifyArgs),
}

#[derive(ClapArgs, Debug)]
struct VerifyArgs {
    #[command(subcommand)]
    action: VerifyAction,

    /// Which target's verifier to check: aem or redacto.
    #[arg(long, default_value = "aem", value_parser = convert::parse_target, global = true)]
    target: OutputTarget,
}

#[derive(Subcommand, Debug)]
enum VerifyAction {
    /// Pull the public images the verifiers run (headless Chromium, Postgres).
    /// Needs a network connection once. The AEM image is private: pull it by
    /// hand after `az acr login` (see docker/aem/README.md).
    Prepare,
    /// The preflight a run performs: the settings, Docker, the images, the AEM
    /// data volume and pdfium.
    Check,
}

/// `blueprint verify prepare|check`.
fn verify_command(args: VerifyArgs) -> Result<(), Box<dyn std::error::Error>> {
    let settings = runner::AppSettings::load();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match args.action {
            VerifyAction::Prepare => {
                let report =
                    agent::u2s::pull_verifier_images(&settings.aem_verify, &settings.redacto_verify).await?;
                println!("{report}");
            }
            VerifyAction::Check => {
                let report = match args.target {
                    OutputTarget::Aem => agent::u2s::aem_verify_readiness(&settings.aem_verify).await?,
                    OutputTarget::Redacto => {
                        agent::u2s::redacto_verify_readiness(&settings.redacto_verify).await?
                    }
                };
                println!("{report}");
                println!("Ready.");
            }
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Print every recorded conversion session, newest first.
fn list_sessions() {
    let sessions = agent::db::list_all_sessions();
    if sessions.is_empty() {
        println!("No conversion sessions recorded yet.");
        return;
    }
    for s in sessions {
        println!(
            "{}  {}  {:<10}  {:>3} edit(s)  {}",
            agent::db::format_timestamp(&s.created_at),
            s.session_id,
            s.profile.as_deref().unwrap_or("-"),
            s.edit_count,
            s.label,
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let mut args = Args::parse();

    // The AI modes are their own thing end to end; the deterministic run below
    // never sees them.
    match args.command.take() {
        Some(Command::Convert(convert_args)) => return convert::run(convert_args),
        Some(Command::Sessions) => {
            list_sessions();
            return Ok(());
        }
        Some(Command::Verify(verify_args)) => return verify_command(verify_args),
        None => {}
    }

    // Validate that all paths exist up-front.
    for doc_path in &args.documents {
        if !doc_path.exists() {
            eprintln!("Error: document not found: {}", doc_path.display());
            std::process::exit(1);
        }
    }

    // ─── --dump-xfa: extract raw XML and exit ────────────────────────────────
    if args.dump_xfa {
        for doc_path in &args.documents {
            let doc_name = doc_stem(doc_path);
            match blueprint::extract_xfa_from_pdf(doc_path)? {
                Some(xfa_data) => {
                    let xml_path = PathBuf::from(format!("{}.xml", doc_name));
                    std::fs::write(&xml_path, &xfa_data)
                        .map_err(|e| format!("Failed to write XFA XML: {}", e))?;
                    info!("XFA dumped to {}", xml_path.display());
                }
                None => info!(
                    "{} is not an XFA PDF (no XFA data to dump)",
                    doc_path.display()
                ),
            }
        }
        return Ok(());
    }

    if !args.modules.is_empty() {
        info!("Enabled modules: {:?}", args.modules);
    }

    // ─── Read all files into memory ──────────────────────────────────────────
    let files: Vec<(String, Vec<u8>)> = args
        .documents
        .iter()
        .map(|p| {
            let name = doc_stem(p).to_string();
            let bytes = std::fs::read(p)?;
            Ok((name, bytes))
        })
        .collect::<Result<_, std::io::Error>>()?;

    // Derive a base name for output files from the first document.
    let base_name = files.first().map(|(n, _)| n.as_str()).unwrap_or("document");

    // ─── Build pipeline config from CLI args ─────────────────────────────────
    let config = PipelineConfig {
        scale: args.scale,
        render_plain: args.render_modes.contains(&RenderMode::Plain),
        render_annotated: args.render_modes.contains(&RenderMode::Annotated),
        render_labelled: args.render_modes.contains(&RenderMode::Labelled),
    };

    // ─── Load profile fonts ────────────────────────────────────────────────
    if let Some(ref profile_name) = args.profile {
        blueprint::load_profile_fonts(profile_name)?;
    }

    // ─── Run the pipeline ────────────────────────────────────────────────────
    // The callback fires for every step change, states-found notification, and
    // individual render completion.  Renders are saved to disk immediately.
    let output = run_pipeline(&files, &config, |event| match event {
        PipelineEvent::StepChanged(step) => {
            let msg = match step {
                PipelineStep::Parsing => "Parsing PDF(s)...",
                PipelineStep::ExhaustiveSearching => "Discovering form states...",
                PipelineStep::Flattening => "Rendering plain/annotated images...",
                PipelineStep::Structuring => "Structuring form content...",
                PipelineStep::Merging => "Merging outputs...",
                PipelineStep::Complete => "Done.",
            };
            info!("{}", msg);
        }

        PipelineEvent::StatesFound { file, count } => {
            info!("  {}: {} unique states found", file, count);
        }

        PipelineEvent::PlainRender { label, images } => {
            for (page, image) in images.iter().enumerate() {
                let path = PathBuf::from(format!("{}_{}_p{}.plain.png", base_name, label, page));
                match image.save(&path) {
                    Ok(()) => info!("  Saved: {}", path.display()),
                    Err(e) => eprintln!("Warning: failed to save {}: {}", path.display(), e),
                }
            }
        }

        PipelineEvent::AnnotatedRender { label, images } => {
            for (page, image) in images.iter().enumerate() {
                let path =
                    PathBuf::from(format!("{}_{}_p{}.annotated.png", base_name, label, page));
                match image.save(&path) {
                    Ok(()) => info!("  Saved: {}", path.display()),
                    Err(e) => eprintln!("Warning: failed to save {}: {}", path.display(), e),
                }
            }
        }

        PipelineEvent::LabelledRender { label, images } => {
            for (page, image) in images.iter().enumerate() {
                let path = PathBuf::from(format!("{}_{}_p{}.labelled.png", base_name, label, page));
                match image.save(&path) {
                    Ok(()) => info!("  Saved: {}", path.display()),
                    Err(e) => eprintln!("Warning: failed to save {}: {}", path.display(), e),
                }
            }
        }

        PipelineEvent::Warning(msg) => eprintln!("Warning: {}", msg),
    })?;

    // ─── Post-pipeline: structured / HTML / AEM / GraphViz output ────────────
    let is_multilingual = args.documents.len() > 1;
    let merged_name = strip_language_suffix(base_name).to_string();
    let suffix = if is_multilingual {
        "multilingual"
    } else {
        "merged"
    };

    // Structured JSON
    if args.structured {
        let json = serde_json::to_string_pretty(&output.merged)
            .map_err(|e| format!("Failed to serialize JSON: {}", e))?;
        let json_path = PathBuf::from(format!("{}_{}.json", merged_name, suffix));
        std::fs::write(&json_path, json).map_err(|e| format!("Failed to write JSON: {}", e))?;
        info!("Structured JSON: {}", json_path.display());
    }

    // HTML
    if args.html {
        let profile_name = require_profile_name(args.profile.as_deref())?;
        let custom_styles = Some(
            blueprint::load_html_custom_styles(profile_name)
                .map_err(|e| format!("Failed to load HTML profile: {e}"))?,
        );
        let html_config = HtmlConfig {
            custom_styles,
            ..HtmlConfig::default()
        };
        let html = blueprint::to_html(&output.merged.content, &html_config);
        let html_path = PathBuf::from(format!("{}_{}.html", merged_name, suffix));
        std::fs::write(&html_path, html).map_err(|e| format!("Failed to write HTML: {}", e))?;
        info!("HTML: {}", html_path.display());
    }

    // AEM package
    if args.aem {
        let profile_name = require_profile_name(args.profile.as_deref())?;
        let aem_config = blueprint::load_aem_config(profile_name, &output.merged.context)
            .map_err(|e| format!("Failed to load AEM profile: {e}"))?;
        let aem_zip = blueprint::to_aem_package(&output.merged.content, &aem_config);
        let aem_path = PathBuf::from(format!("{}_{}.zip", merged_name, suffix));
        std::fs::write(&aem_path, aem_zip)
            .map_err(|e| format!("Failed to write AEM package: {}", e))?;
        info!("AEM package: {}", aem_path.display());
    }

    // GraphViz decision-flow DOT file
    if args.graphviz {
        let graph_states: Vec<GraphState> = output
            .state_labels
            .iter()
            .map(|(label, selections)| GraphState {
                selections: selections.iter().map(GraphSelection::from).collect(),
                label: label.clone(),
            })
            .collect();
        let field_labels: FieldLabelMap = build_field_label_map(&output.merged.content);
        let dot = generate_dot(&graph_states, &field_labels);
        let dot_path = PathBuf::from(format!("{}_flow.dot", merged_name));
        std::fs::write(&dot_path, dot)
            .map_err(|e| format!("Failed to write GraphViz DOT file: {}", e))?;
        info!("GraphViz DOT: {}", dot_path.display());
    }

    // XSD schema
    if args.xsd {
        let profile_name = require_profile_name(args.profile.as_deref())?;
        let mut xsd_config = blueprint::load_xsd_config(profile_name)
            .map_err(|e| format!("Failed to load XSD profile: {e}"))?;
        // Extract form code from merged name (e.g. "AAAI_019" → "AAAI")
        let form_code = merged_name.split('_').next().unwrap_or(&merged_name);
        xsd_config.form_code = Some(form_code.to_string());
        let aem_config = blueprint::load_aem_config(profile_name, &output.merged.context)
            .map_err(|e| format!("Failed to load AEM profile for XSD generation: {e}"))?;
        let xsd = blueprint::to_xsd(&output.merged.content, &aem_config, &xsd_config);
        let xsd_path = PathBuf::from(format!("{}_{}.xsd", merged_name, suffix));
        std::fs::write(&xsd_path, xsd).map_err(|e| format!("Failed to write XSD: {}", e))?;
        info!("XSD: {}", xsd_path.display());
    }

    // Redacto PostgreSQL dump
    if args.redacto {
        let profile_name = require_profile_name(args.profile.as_deref())?;
        let (dump, resolved) = blueprint::to_redacto_dump_for_profile(
            profile_name,
            &output.contexts,
            &output.merged.content,
        )?;
        for warning in &dump.warnings {
            eprintln!("Warning: {}", warning);
        }
        // A contentless dump is still valid SQL, so say so rather than letting
        // an empty document look like a successful conversion.
        let validation = blueprint::validate_dump(&dump, &resolved);
        for problem in &validation.problems {
            eprintln!("Problem: {}", problem);
        }
        let sql_path = PathBuf::from(format!("{}_{}.sql", merged_name, suffix));
        std::fs::write(&sql_path, dump.to_sql())
            .map_err(|e| format!("Failed to write Redacto SQL: {}", e))?;
        info!("Redacto SQL: {}", sql_path.display());
    }

    Ok(())
}

// ─── Filename helpers ─────────────────────────────────────────────────────────

/// Extract the file stem of a path as `&str`, falling back to `"document"`.
fn doc_stem(path: &Path) -> &str {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document")
}

/// Strip language suffixes like `_DE`, `_EN`, etc. from a document name
/// to get a base name suitable for merged output files.
fn strip_language_suffix(name: &str) -> &str {
    const SUFFIXES: &[&str] = &[
        "_DE", "_EN", "_FR", "_IT", "_ES", "_de", "_en", "_fr", "_it", "_es",
    ];
    for suffix in SUFFIXES {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

// ─── Profile loading ──────────────────────────────────────────────────────────

fn require_profile_name(profile_name: Option<&str>) -> Result<&str, Box<dyn std::error::Error>> {
    profile_name.ok_or_else(|| "No profile specified (use --profile <name>)".into())
}

#[cfg(test)]
mod tests {
    use super::require_profile_name;

    #[test]
    fn embedded_xsd_loader_succeeds_for_existing_profile() {
        let config = blueprint::load_xsd_config("ubs").expect("load embedded xsd config");
        assert!(
            config.registered_types.contains_key("AddressType"),
            "expected AddressType from embedded ubs xsd/types"
        );
    }

    #[test]
    fn require_profile_name_errors_when_missing() {
        let err = require_profile_name(None).expect_err("expected missing profile error");
        let msg = err.to_string();
        assert!(
            msg.contains("No profile specified"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn embedded_html_loader_errors_when_profile_missing() {
        let err = blueprint::load_html_custom_styles("missing-profile")
            .expect_err("expected missing html profile error");
        let msg = err.to_string();
        assert!(
            msg.contains("has no html/ subdirectory") || msg.contains("has no config.toml"),
            "unexpected error message: {msg}"
        );
    }
}
