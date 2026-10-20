//! What every corpus form looks like by default, recorded so that a change
//! to the layout or scripting engine shows exactly which forms it moves.
//!
//! Each form has a baseline file in `tests/baselines/` holding its page
//! sizes, the text of each page, every control's value, visibility and box,
//! and a hash of each page's pixels at 72 dpi. The test fails when a form no
//! longer matches its baseline and prints what moved. After reviewing the
//! differences, `U2S_UPDATE_BASELINE=1 cargo test -p u2s-render-xfa --test
//! corpus_baseline` records the new state; `U2S_BASELINE_FORMS=A,B` limits
//! either to the named forms (file stems).
//!
//! The forms' scripts see a fixed clock (`SOURCE_DATE_EPOCH`), since many
//! of them stamp today's date.
//!
//! The baseline is not a reference rendering: it records what the engine
//! did, right or wrong, so that every change to it is a reviewed one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use u2s_render_xfa::states::StateSpec;
use u2s_render_xfa::{ImageFormat, Limits, Renderer, Target, fonts};

/// Forms whose default rendering differs between runs, with why. They are
/// recorded but not compared; fixing the cause must remove them from here.
const NONDETERMINISTIC: &[(&str, &str)] = &[];

/// The instant the forms' scripts see as now (2026-01-15 12:00 UTC), so that
/// a form stamping today's date renders the same every day.
const SOURCE_DATE_EPOCH: &str = "1768478400";

/// Positions are compared at this resolution, in points, so that a change
/// in the last bits of a float does not count as a move.
const POSITION_STEP: f32 = 0.1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Baseline {
    /// Width and height of each page, in points.
    pages: Vec<(f32, f32)>,
    page_text: Vec<String>,
    /// FNV-1a over each page's RGBA pixels at 72 dpi.
    raster: Vec<String>,
    controls: BTreeMap<String, ControlState>,
    warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ControlState {
    value: Option<String>,
    visible: bool,
    /// `[page, x, y, width, height]`, rounded to `POSITION_STEP`.
    positions: Vec<[f32; 5]>,
}

fn round(v: f32) -> f32 {
    (v / POSITION_STEP).round() * POSITION_STEP
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn baseline_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/baselines")
}

fn record(renderer: &Renderer, form: &Path) -> Baseline {
    let target = Target::doc(form, &StateSpec::default());
    let info = renderer.info(&target).expect("info");
    let mut page_text = Vec::new();
    let mut raster = Vec::new();
    for page in 1..=info.page_count {
        let text = renderer.page_text(&target, page, None, Some(usize::MAX)).expect("page text");
        assert!(!text.truncated, "page {page} text was truncated");
        page_text.push(text.text);
        let (rendered, _) = renderer
            .render_page(&target, page, Some(72.0), None, ImageFormat::Png)
            .expect("render");
        let pixels = image::load_from_memory(&rendered.data).expect("decode").to_rgba8();
        raster.push(fnv1a(pixels.as_raw()));
    }
    let controls = renderer
        .controls(&target)
        .expect("controls")
        .controls
        .into_iter()
        .map(|c| {
            let positions = c
                .positions
                .iter()
                .map(|p| [p.page as f32, round(p.x), round(p.y), round(p.width), round(p.height)])
                .collect();
            (
                c.field,
                ControlState {
                    value: c.value,
                    visible: c.visible,
                    positions,
                },
            )
        })
        .collect();
    Baseline {
        pages: info.pages.iter().map(|g| (g.width_pt, g.height_pt)).collect(),
        page_text,
        raster,
        controls,
        warning: info.warning,
    }
}

/// What differs between the recorded and the current state, one line each.
fn differences(was: &Baseline, now: &Baseline) -> Vec<String> {
    let mut out = Vec::new();
    if was.pages != now.pages {
        out.push(format!("pages {:?} -> {:?}", was.pages, now.pages));
    }
    for (i, (a, b)) in was.page_text.iter().zip(&now.page_text).enumerate() {
        if a != b {
            out.push(format!("page {} text changed", i + 1));
        }
    }
    let changed_rasters: Vec<usize> = was
        .raster
        .iter()
        .zip(&now.raster)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i + 1)
        .collect();
    if !changed_rasters.is_empty() {
        out.push(format!("pixels changed on pages {changed_rasters:?}"));
    }
    let mut moved = 0;
    let mut max_move: f32 = 0.0;
    for (field, a) in &was.controls {
        match now.controls.get(field) {
            None => out.push(format!("control {field} is gone")),
            Some(b) => {
                if a.value != b.value {
                    out.push(format!("{field} value {:?} -> {:?}", a.value, b.value));
                }
                if a.visible != b.visible {
                    out.push(format!("{field} visible {} -> {}", a.visible, b.visible));
                }
                if a.positions != b.positions {
                    moved += 1;
                    for (p, q) in a.positions.iter().zip(&b.positions) {
                        for k in 1..5 {
                            max_move = max_move.max((p[k] - q[k]).abs());
                        }
                    }
                }
            }
        }
    }
    for field in now.controls.keys().filter(|f| !was.controls.contains_key(*f)) {
        out.push(format!("control {field} is new"));
    }
    if moved > 0 {
        out.push(format!("{moved} controls moved or resized, by up to {max_move:.1}pt"));
    }
    if was.warning != now.warning {
        out.push(format!("warning {:?} -> {:?}", was.warning, now.warning));
    }
    out
}

#[test]
fn every_corpus_form_matches_its_baseline() {
    // SAFETY: the only test in this binary, and it sets the variable before
    // the renderer's worker thread exists.
    unsafe { std::env::set_var("SOURCE_DATE_EPOCH", SOURCE_DATE_EPOCH) };
    fonts::register_dir_once(u2s_test_assets::font_dir(), None).expect("register the test fonts");
    let update = std::env::var_os("U2S_UPDATE_BASELINE").is_some();
    // `U2S_BASELINE_FORMS=AAKO_019_DE,AAAA_019_DE` checks only those forms.
    let only = std::env::var("U2S_BASELINE_FORMS").ok();
    let renderer = Renderer::new(Limits::default());
    let dir = baseline_dir();
    std::fs::create_dir_all(&dir).expect("baseline dir");

    let mut forms: Vec<PathBuf> = std::fs::read_dir(u2s_test_assets::corpus_dir())
        .expect("corpus dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "pdf"))
        .filter(|p| {
            only.as_ref()
                .is_none_or(|only| p.file_stem().is_some_and(|s| only.split(',').any(|o| s == o)))
        })
        .collect();
    forms.sort();

    let mut failures = Vec::new();
    for form in &forms {
        let name = form.file_stem().expect("stem").to_string_lossy().into_owned();
        let file = dir.join(format!("{name}.json"));
        let now = record(&renderer, form);
        if update {
            let json = serde_json::to_string_pretty(&now).expect("serialize");
            std::fs::write(&file, json + "\n").expect("write baseline");
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            failures.push(format!("{name}: no baseline; record one with U2S_UPDATE_BASELINE=1"));
            continue;
        };
        let was: Baseline = serde_json::from_str(&text).expect("parse baseline");
        let diff = differences(&was, &now);
        if NONDETERMINISTIC.iter().any(|(form, _)| *form == name) {
            continue;
        }
        if !diff.is_empty() {
            failures.push(format!("{name}:\n  {}", diff.join("\n  ")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} forms differ from their baseline \
         (review, then rerun with U2S_UPDATE_BASELINE=1):\n{}",
        failures.len(),
        forms.len(),
        failures.join("\n")
    );
}
