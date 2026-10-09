//! The rules the UBS writer generates, as typed rules (`u2s_mapper_aem::script`)
//! rather than hand-escaped JSON in a template.
//!
//! Each builder returns the rules of one attribute; [`attribute_value`]
//! spells them as the attribute's value (a JCR multi-value of JSON objects),
//! and the XML writer escapes that once. A text that reaches a rule's
//! JavaScript as a string (a repeatable's subject, an option value) is
//! written as a JavaScript string literal by [`js_string`]; every layer
//! above it is the codec's.
//!
//! The JavaScript is the archetypes' own, character for character: the
//! deployed forms carry it, the feedback repository's fixers recognise it by
//! its text, and the decoder (`parser.rs`, `unexpand.rs`) folds it back.

use u2s_mapper_aem::script::{BodyOrder, EventScript, ScriptEvent, encode_script_list};

/// The marker opening every rule a generator owns.
fn generated(archetype: &str) -> String {
    format!(
        "// [{archetype}] Generated automatically. Do not edit: will be overwritten. Create your own different script."
    )
}

/// `rules` as the value of the attribute that holds them.
pub fn attribute_value(rules: &[EventScript]) -> String {
    encode_script_list(rules.iter().map(EventScript::to_json))
}

/// `text` as a JavaScript string literal, quotes included. JSON's string
/// escaping is valid JavaScript.
pub fn js_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string serialises")
}

const REPEATING_PANEL: &str = "repeating-panel";

/// A rule of the repeating-panel archetype: content first, no model.
fn repeating(field: &str, event: ScriptEvent, body: &str) -> EventScript {
    EventScript {
        field: field.into(),
        event,
        content: format!("{}\n{body}", generated(REPEATING_PANEL)),
        order: BodyOrder::ContentFirst,
        model: None,
        archetype: Some(REPEATING_PANEL.into()),
    }
}

/// The visibility rule of the repeating-panel archetype: the code editor's
/// order, with its model.
fn repeating_visible(field: &str, body: &str) -> EventScript {
    EventScript {
        archetype: Some(REPEATING_PANEL.into()),
        ..EventScript::event(
            field,
            ScriptEvent::Visibility,
            format!("{}\n{body}", generated(REPEATING_PANEL)),
        )
    }
}

/// What a repeatable's buttons drive.
pub struct Repeating<'a> {
    /// The repeating panel the Add button adds to.
    pub panel: &'a str,
    /// The signature panel the buttons also drive, if any.
    pub twin: Option<&'a str>,
    /// What one row is, announced by the accessibility helpers.
    pub subject: &'a str,
}

impl Repeating<'_> {
    /// The Remove button's click, visibility and initialize rules.
    pub fn remove(&self) -> [EventScript; 3] {
        let subject = js_string(self.subject);
        let mut click = String::from(
            "var repeatingPanel = this.parent;\nvar addButton = this.parent.parent.BT_Add;\nwindow.forms.ubs.removeInstance(repeatingPanel);",
        );
        if let Some(twin) = self.twin {
            click.push_str(&format!("\nwindow.forms.ubs.removeInstance({twin});"));
        }
        click.push_str(&format!(
            "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(repeatingPanel, {subject}, addButton);\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(repeatingPanel, {subject}, addButton, this);"
        ));
        if let Some(twin) = self.twin {
            click.push_str(&format!(
                "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons({twin}, {subject}, \"\", \"\");"
            ));
        }
        let shown = "this.parent.instanceIndex === this.parent.instanceManager.instances.length - 1 && this.parent.instanceManager.instances.length > this.parent.instanceManager.minOccur";
        [
            repeating("BT_Remove", ScriptEvent::Click, &click),
            repeating_visible("BT_Remove", &format!("{shown};")),
            repeating("BT_Remove", ScriptEvent::Initialize, &format!("this.visible = ({shown});")),
        ]
    }

    /// The Add button's click, visibility and initialize rules.
    pub fn add(&self) -> [EventScript; 3] {
        let (panel, subject) = (self.panel, js_string(self.subject));
        let mut click = format!("window.forms.ubs.addInstance(this.parent.{panel});");
        if let Some(twin) = self.twin {
            click.push_str(&format!("\nwindow.forms.ubs.addInstance({twin});"));
        }
        click.push_str(&format!(
            "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(this.parent.{panel}, {subject}, this);\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(this.parent.{panel}, {subject}, this, this.parent.{panel}.instanceManager.instances[this.parent.{panel}.instanceManager.instances.length - 1].BT_Remove);"
        ));
        if let Some(twin) = self.twin {
            click.push_str(&format!(
                "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons({twin}, {subject}, \"\", \"\");"
            ));
        }
        let shown = format!(
            "this.parent.{panel}.instanceManager.instances.length < this.parent.{panel}.instanceManager.maxOccur"
        );
        [
            repeating("BT_Add", ScriptEvent::Click, &click),
            repeating_visible("BT_Add", &format!("{shown};")),
            repeating("BT_Add", ScriptEvent::Initialize, &format!("this.visible = ({shown});")),
        ]
    }
}

/// A conditional panel's rules: shown while any `(field, value)` trigger
/// holds. The visibility expression only runs when a trigger changes, so the
/// initialize rule repeats it, as an assignment, for a freshly opened form.
pub fn show(panel: &str, triggers: &[(String, String)]) -> (EventScript, EventScript) {
    let condition = triggers
        .iter()
        .map(|(field, value)| format!("{field}.value == {}", js_string(value)))
        .collect::<Vec<_>>()
        .join(" || ");
    let visible = EventScript {
        model: Some("SHOW_EXPRESSION".into()),
        ..EventScript::event(
            panel,
            ScriptEvent::Visibility,
            format!(
                "if ({condition}) {{\n  window.forms.ubs.showAFShowDor(this);\n  true;\n}} else {{\n  window.forms.ubs.hideAFHideDor(this);\n  false;\n}}\n"
            ),
        )
    };
    let init = EventScript::event(
        panel,
        ScriptEvent::Initialize,
        format!(
            "if ({condition}) {{\n  window.forms.ubs.showAFShowDor(this);\n  this.visible = true;\n}} else {{\n  window.forms.ubs.hideAFHideDor(this);\n  this.visible = false;\n}}\n"
        ),
    );
    (visible, init)
}

/// A panel a configurator choice decides, and the repeatables in it.
pub struct ResetTarget {
    pub panel: String,
    pub repeats: Vec<String>,
}

const CONFIGURATOR_RESET: &str = "configurator-reset-on-change";

/// A configurator choice's value-commit rule: every panel it decides is
/// emptied, its repeatables first, so no option shows another's data.
pub fn configurator_reset(choice: &str, targets: &[ResetTarget]) -> EventScript {
    let mut content = generated(CONFIGURATOR_RESET);
    for target in targets {
        for repeat in &target.repeats {
            content.push_str(&format!("\nwindow.forms.ubs.resetAllPanelInstances({repeat});"));
        }
        content.push_str(&format!("\n{}.resetData();", target.panel));
    }
    EventScript {
        archetype: Some(CONFIGURATOR_RESET.into()),
        ..EventScript::event(choice, ScriptEvent::ValueCommit, content)
    }
}

// A fragment's Initialize rules. AEM's Expression Editor hands back exactly
// ONE SCRIPTMODEL per field and event: two sharing `this` and `Initialize` are
// joined by a bare comma into invalid JSON, and the editor then fails the
// moment it loads the field, taking the page's editor data with it. So each
// of these is one rule, however many statements it carries.

/// An address fragment's initialize rule (feedback #102): City and Country
/// must be optional. The fragment's own default makes them mandatory, so this
/// overrides it. Both fields sit one level down in `PN_AddressBlock` and are
/// passed as arguments, which is why they need the `this.` prefix: a bare or
/// panel-qualified reference does not resolve.
pub const ADDRESS_INIT: &str = "window.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.TXT_City_AddressBlock, false);\nwindow.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.DD_Country_AddressBlock, false);";

/// The generic address fragment's initialize rule: its unused fields hidden,
/// its country list loaded for the form's mandator and language, city and
/// country not mandatory. `affrg_AddressGeneric1` carried two Initialize
/// rules of its own (the hides and the country list); they are folded in
/// here, not appended as a second rule.
pub const ADDRESS_GENERIC_INIT: &str = "window.forms.ubs.hideAFHideDor(TXT_StreetNumber);\nwindow.forms.ubs.hideAFHideDor(TXT_AdditionalDetails_AddressBlock);\nwindow.forms.ubs.hideAFHideDor(TXT_PostalCodeCity);\nwindow.forms.ubs.hideAFHideDor(TXT_State_AddressBlock);\nwindow.forms.ubs.hideAFHideDor(TXT_District_AddressBlock_APAC);\nvar formMetadata = window.forms.ubs.getFormMetadata();\nwindow.forms.ubs.referencedata.getCountryList(formMetadata.mandator, formMetadata.language, 'DOMICILE_INDIVIDUAL', DD_Country_AddressBlock);\nwindow.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.TXT_City_AddressBlock, false);\nwindow.com.ajila.forms.ubs.components.setMandatory(this.PN_AddressBlock.DD_Country_AddressBlock, false);";

/// A German form's banking relationship (feedback #104): `0319` and
/// disabled. Italy and every other entity share the same fragment but keep
/// what they have, so the writer gates this on the entity, not the fragment.
pub const BANKING_DEFAULT_DE_INIT: &str =
    "TXT_BankingRelationship1.value = '0319';\nTXT_BankingRelationship1.enabled = false;";

/// The initialize rule a fragment carries, by what it is: `None` for one
/// that carries none. `guide_path` is the fragment's own; the address rules
/// name it, the banking one and a partner generic's do as written.
pub fn fragment_init(
    frag_ref: &str,
    guide_path: &str,
    german_entity: bool,
    partner_calls: &[String],
) -> Option<EventScript> {
    let base = frag_ref.rsplit('/').next().unwrap_or(frag_ref).to_lowercase();
    if base.ends_with("affrg_addressgeneric1") {
        Some(EventScript::event(guide_path, ScriptEvent::Initialize, ADDRESS_GENERIC_INIT))
    } else if base.contains("address") && !base.contains("formofaddress") && !base.contains("form_address") {
        Some(EventScript::event(guide_path, ScriptEvent::Initialize, ADDRESS_INIT))
    } else if base.ends_with("affrg_bankingrelationship1") && german_entity {
        Some(banking_default_de())
    } else if !partner_calls.is_empty() {
        Some(EventScript::event(
            guide_path,
            ScriptEvent::Initialize,
            partner_calls.join("\n"),
        ))
    } else {
        None
    }
}

/// The German banking relationship's initialize rule, on whichever node
/// holds the fragment.
pub fn banking_default_de() -> EventScript {
    EventScript::event("this", ScriptEvent::Initialize, BANKING_DEFAULT_DE_INIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Remove button's click rule, as the deployed forms carry it: one
    /// list item, content first, the subject a JavaScript string.
    #[test]
    fn a_remove_click_rule_is_the_archetypes_own() {
        let [click, ..] = Repeating { panel: "RCP_X_repeat", twin: None, subject: "Client" }.remove();
        assert_eq!(
            attribute_value(&[click]),
            r#"[{"script":{"content":"// [repeating-panel] Generated automatically. Do not edit: will be overwritten. Create your own different script.\\nvar repeatingPanel = this.parent;\\nvar addButton = this.parent.parent.BT_Add;\\nwindow.forms.ubs.removeInstance(repeatingPanel);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(repeatingPanel\, \\"Client\\"\, addButton);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(repeatingPanel\, \\"Client\\"\, addButton\, this);"\,"event":"Click"\,"field":"BT_Remove"}\,"nodeName":"SCRIPTMODEL"\,"version":1\,"enabled":true\,"_archetype":"repeating-panel"}]"#
        );
    }

    #[test]
    fn a_show_rule_compares_with_the_value_as_a_string() {
        let (visible, _) = show("PN_A", &[("RB_X".into(), "a\"b".into()), ("CB_Y".into(), "true".into())]);
        assert!(visible.content.starts_with(r#"if (RB_X.value == "a\"b" || CB_Y.value == "true") {"#));
        assert_eq!(visible.model.as_deref(), Some("SHOW_EXPRESSION"));
    }
}
