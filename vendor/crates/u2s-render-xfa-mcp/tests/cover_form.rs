//! A "cover form": ACYI_001_EN opens on a one-page selection mask and shows
//! the form proper only once "Proceed to form" is pressed, with "Back to
//! selection" on every page of it. Driving that round trip through the MCP
//! surface is the end-to-end proof that an agent can navigate such a form:
//! nothing of the form proper leaks onto the cover, the proceed button only
//! appears once the selections allow it, and the labels the form's scripts
//! write are what the pages say.

#[allow(dead_code)]
mod support;

use serde_json::{Value, json};
use u2s_render_test_harness::{Client, ServerUnderTest, call, structured};

const MASK: &str = "TopForm.Page_Mask.TopSection.Main_Selection";

fn server() -> ServerUnderTest {
    ServerUnderTest::locate("u2s-render-xfa-mcp", "xfa")
        .env(
            "U2S_FONT_DIR",
            u2s_test_assets::font_dir().display().to_string(),
        )
        .env(
            "U2S_BLOB_DIR",
            std::env::temp_dir()
                .join("u2s-xfa-cover-form-blobs")
                .display()
                .to_string(),
        )
}

struct Session {
    handle: String,
    revision: u64,
}

async fn mutate(c: &Client, s: &mut Session, tool: &str, mut args: Value) -> Value {
    args["session"] = json!(s.handle);
    args["expected_revision"] = json!(s.revision);
    let result = structured(&call(c, tool, args).await).clone();
    assert_eq!(result["warning"], Value::Null, "{tool} {result}");
    s.revision = result["revision"].as_u64().expect("revision");
    result
}

async fn page_text(c: &Client, s: &Session, page: u64) -> String {
    let result = call(
        c,
        "xfa_page_text",
        json!({ "session": s.handle, "revision": s.revision, "page": page, "limit": 20000 }),
    )
    .await;
    structured(&result)["text"]
        .as_str()
        .expect("text")
        .to_string()
}

async fn field(c: &Client, s: &Session, path: &str) -> Value {
    structured(
        &call(
            c,
            "xfa_field",
            json!({ "session": s.handle, "revision": s.revision, "field": path }),
        )
        .await,
    )
    .clone()
}

#[tokio::test]
async fn the_cover_leads_to_the_form_and_back() {
    support::require_fonts();
    let srv = server();
    let c = srv.connect().await;

    let form = u2s_test_assets::corpus_form("ACYI_001_EN.pdf");
    let opened = structured(
        &call(
            &c,
            "xfa_open",
            json!({ "doc_path": form.display().to_string() }),
        )
        .await,
    )
    .clone();
    assert_eq!(
        opened["warning"],
        Value::Null,
        "every script of the cover must run: {opened}"
    );
    assert_eq!(opened["page_count"], 1, "only the cover is laid out");
    let mut s = Session {
        handle: opened["session"].as_str().expect("session").to_string(),
        revision: opened["revision"].as_u64().expect("revision"),
    };

    let cover = page_text(&c, &s, 1).await;
    assert!(cover.contains("Please select the entity"), "{cover}");
    assert!(
        !cover.contains("Client details"),
        "the form proper must not leak onto the cover: {cover}"
    );

    // The option group the scripts read is kept `invisible`: its buttons
    // take their space but are neither drawn nor listed as visible.
    let hidden_option =
        field(&c, &s, "TopForm.Page_Mask.DoNotDeleteSection.RB_Select_Option.RB_1").await;
    assert_eq!(hidden_option["visible"], false, "{hidden_option}");
    assert!(
        hidden_option["positions"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{hidden_option}"
    );

    let proceed = format!("{MASK}.ProceedToForm.STP.AB_proceed_to_form");
    assert_eq!(
        field(&c, &s, &proceed).await["visible"],
        false,
        "nothing selected yet"
    );

    // The selections a person makes on the cover, in order: confirm, the
    // disclosing entity, the business area, the option within it.
    // The confirm box is pressed, as a person would: it has a box to press
    // (a width, where its margin used to swallow it), and the press toggles
    // it.
    let confirm = format!("{MASK}.Intro_I.CB_Confirm");
    let confirm_box = field(&c, &s, &confirm).await;
    assert!(
        confirm_box["positions"][0]["width"]
            .as_f64()
            .unwrap_or(0.0)
            > 5.0,
        "{confirm_box}"
    );
    let pressed = mutate(&c, &mut s, "xfa_click", json!({ "field": confirm })).await;
    assert_eq!(pressed["values_changed"], true, "{pressed}");
    assert_eq!(field(&c, &s, &confirm).await["value"], "1");
    mutate(&c, &mut s, "xfa_set", json!({ "field": format!("{MASK}.Intro_I.CL_Branch"), "value": "Australia, UBS AG Australia Branch" })).await;
    let other = format!("{MASK}.Intro_II.Intro_II_Select_GWMOther");
    let area = mutate(
        &c,
        &mut s,
        "xfa_click",
        json!({ "field": format!("{other}.Select_GWMOther.RB_Pre_Select_Option.RB_GWM_Other") }),
    )
    .await;
    assert!(
        area["appeared"]
            .as_array()
            .expect("appeared")
            .iter()
            .any(|f| f.as_str() == Some(&format!("{other}.SEL7.STP.RB_Select_Option.RB_7"))),
        "picking a business area reveals its options: {area}"
    );
    assert_eq!(
        field(&c, &s, &proceed).await["visible"],
        false,
        "an area alone is not a selection"
    );
    mutate(
        &c,
        &mut s,
        "xfa_click",
        json!({ "field": format!("{other}.SEL7.STP.RB_Select_Option.RB_7") }),
    )
    .await;
    let proceed_now = field(&c, &s, &proceed).await;
    assert_eq!(proceed_now["visible"], true, "{proceed_now}");
    let cover_now = page_text(&c, &s, 1).await;
    assert!(
        cover_now.contains("Proceed to form") && !cover_now.contains("*** Proceed"),
        "the button carries the label its initialize script set: {cover_now}"
    );

    let proceeded = mutate(&c, &mut s, "xfa_click", json!({ "field": proceed })).await;
    assert!(
        proceeded["page_count"].as_u64().expect("pages") >= 2,
        "{proceeded}"
    );
    let page1 = page_text(&c, &s, 1).await;
    assert!(page1.contains("Client details"), "{page1}");
    assert!(
        !page1.contains("Please select the entity"),
        "the cover must be gone: {page1}"
    );
    assert!(
        page1.contains("Last name, first name / Company"),
        "a label's initialize script replaces its placeholder: {page1}"
    );

    let back = "TopForm.MPs.SET.MP1.Footer.AB_Back_to_selection";
    let back_now = field(&c, &s, back).await;
    assert_eq!(back_now["visible"], true, "{back_now}");
    assert!(
        !back_now["positions"]
            .as_array()
            .expect("positions")
            .is_empty()
    );

    let returned = mutate(&c, &mut s, "xfa_click", json!({ "field": back })).await;
    assert_eq!(returned["page_count"], 1, "{returned}");
    assert!(
        page_text(&c, &s, 1)
            .await
            .contains("Please select the entity")
    );
    // The back button's script "restores initial values": it clears the
    // flags and the option group (assigning "" to the group's node sets its
    // value, as Acrobat does), so the cover is as first opened, with nothing
    // selected and the proceed button hidden again.
    assert_eq!(field(&c, &s, &proceed).await["visible"], false);
}
