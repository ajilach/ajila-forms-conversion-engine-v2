//! Shared fixture used across the integration tests: one form JSON document
//! that exercises every `Node` variant, every enum, and the cross-node
//! relationships `AemForm::validate` checks (a repeatable expressed as
//! ordinary nested components, translations in two languages).
//!
//! Under this crate's redesigned model (see `u2s_aem::model::Node`'s own
//! doc), a panel, a repeatable's outer/inner pair, a fragment reference and
//! a toolbar action are all `Node::Component` — this fixture authors them
//! directly, the way the Conversion Agent now does, rather than relying on
//! a Rust-side expansion that no longer exists.

use serde_json::{Value, json};

pub fn sample_form_json() -> Value {
    json!({
        "metadata": {
            "form_name": "SampleForm",
            "title": { "en": "Sample Form", "de": "Beispielformular" },
            "master_language": "en",
            "languages": ["en", "de"],
            "dor": "generate",
            "data_model": { "kind": "xml_schema", "root_element": "SampleFormRoot" },
            "toolbar": [
                {
                    "type": "Component",
                    "common": {
                        "name": "PrevButton",
                        "resource_type": "fd/af/components/actions/previtemnav"
                    },
                    "properties": { "title": { "kind": "single", "value": "Previous" } },
                    "children": []
                },
                {
                    "type": "Component",
                    "common": {
                        "name": "NextButton",
                        "resource_type": "fd/af/components/actions/nextitemnav"
                    },
                    "properties": { "title": { "kind": "single", "value": "Next" } },
                    "children": []
                },
                {
                    "type": "Component",
                    "common": {
                        "name": "SubmitButton",
                        "resource_type": "fd/af/components/actions/submit"
                    },
                    "properties": { "title": { "kind": "single", "value": "Submit" } },
                    "children": []
                }
            ]
        },
        "pages": [
            {
                "name": "PageOne",
                "properties": {
                    "jcr:title": { "kind": "text", "value": { "en": "Page One", "de": "Seite Eins" } },
                    "panelSetType": { "kind": "single", "value": "wizard" }
                },
                "children": [
                    {
                        "type": "Component",
                        "common": {
                            "name": "ConditionalPanel",
                            "resource_type": "fd/af/components/panel"
                        },
                        "properties": {
                            "jcr:title": { "kind": "text", "value": { "en": "Conditional", "de": "Bedingt" } }
                        },
                        "children": [
                            {
                                "type": "Component",
                                "common": {
                                    "name": "VisibilityRules",
                                    "jcr_name": "fd:rules"
                                },
                                "properties": {},
                                "children": [
                                    {
                                        "type": "Component",
                                        "common": {
                                            "name": "VisibleRule",
                                            "jcr_name": "fd:visible"
                                        },
                                        "properties": {
                                            "trigger": { "kind": "single", "value": "CountryDropdown" },
                                            "value": { "kind": "single", "value": "ch" }
                                        },
                                        "children": []
                                    }
                                ]
                            },
                            {
                                "type": "TextField",
                                "common": {
                                    "name": "SwissOnlyField",
                                    "resource_type": "fd/af/components/controls/textbox"
                                },
                                "field": {
                                    "label": { "en": "Swiss field", "de": "Schweizer Feld" },
                                    "mandatory": true
                                },
                                "layout": { "width": 6 },
                                "input": "single_line",
                                "max_chars": 40,
                                "autofill": "email"
                            }
                        ]
                    },
                    {
                        "type": "Dropdown",
                        "common": {
                            "name": "CountryDropdown",
                            "resource_type": "fd/af/components/controls/dropdownlist"
                        },
                        "field": { "label": { "en": "Country", "de": "Land" } },
                        "layout": { "width": 6 },
                        "options": [
                            { "value": "ch", "label": { "en": "Switzerland", "de": "Schweiz" } },
                            { "value": "de", "label": { "en": "Germany", "de": "Deutschland" } }
                        ],
                        "filtering_allowed": false,
                        "sort": "ascending"
                    },
                    {
                        "type": "Component",
                        "common": {
                            "name": "Dependants",
                            "resource_type": "fd/af/components/panel"
                        },
                        "properties": {
                            "jcr:title": { "kind": "text", "value": { "en": "Dependants", "de": "Angehörige" } }
                        },
                        "children": [
                            {
                                "type": "Component",
                                "common": {
                                    "name": "DependantsInstance",
                                    "jcr_name": "Dependants_instance",
                                    "resource_type": "fd/af/components/panel"
                                },
                                "properties": {
                                    "minOccur": { "kind": "single", "value": "0" },
                                    "maxOccur": { "kind": "single", "value": "4" }
                                },
                                "children": [
                                    {
                                        "type": "TextField",
                                        "common": {
                                            "name": "DependantName",
                                            "resource_type": "fd/af/components/controls/textbox"
                                        },
                                        "field": {
                                            "label": { "en": "Name", "de": "Name" },
                                            "mandatory": true
                                        },
                                        "layout": { "width": 12 },
                                        "input": "single_line"
                                    },
                                    {
                                        "type": "Component",
                                        "common": {
                                            "name": "RemoveDependant",
                                            "resource_type": "fd/af/components/controls/removebutton"
                                        },
                                        "properties": { "jcr:title": { "kind": "single", "value": "Remove" } },
                                        "children": []
                                    }
                                ]
                            },
                            {
                                "type": "Component",
                                "common": {
                                    "name": "AddDependant",
                                    "resource_type": "fd/af/components/controls/tertiarybutton"
                                },
                                "properties": { "jcr:title": { "kind": "single", "value": "Add" } },
                                "children": []
                            }
                        ]
                    },
                    {
                        "type": "NumberField",
                        "common": {
                            "name": "Amount",
                            "resource_type": "fd/af/components/controls/numericbox"
                        },
                        "field": { "label": { "en": "Amount", "de": "Betrag" } },
                        "layout": { "width": 6 },
                        "format": { "kind": "named", "value": "currency" }
                    },
                    {
                        "type": "DatePicker",
                        "common": {
                            "name": "BirthDate",
                            "resource_type": "fd/af/components/controls/datepicker"
                        },
                        "field": { "label": { "en": "Date of birth", "de": "Geburtsdatum" } },
                        "layout": { "width": 6 },
                        "default_to_current_date": false,
                        "format": { "kind": "pattern", "value": "YYYY-MM-DD" },
                        "year_range": { "before_today": 100, "after_today": 0 }
                    },
                    {
                        "type": "Checkbox",
                        "common": {
                            "name": "AgreeTerms",
                            "resource_type": "fd/af/components/controls/checkbox"
                        },
                        "field": { "label": { "en": "I agree", "de": "Ich stimme zu" } },
                        "layout": { "width": 12 },
                        "options": [
                            { "value": "yes", "label": { "en": "Yes", "de": "Ja" } }
                        ],
                        "alignment": "horizontal"
                    },
                    {
                        "type": "RadioButton",
                        "common": {
                            "name": "Salutation",
                            "resource_type": "fd/af/components/controls/radiobutton"
                        },
                        "field": { "label": { "en": "Salutation", "de": "Anrede" } },
                        "layout": { "width": 6 },
                        "options": [
                            { "value": "mr", "label": { "en": "Mr", "de": "Herr" } },
                            { "value": "ms", "label": { "en": "Ms", "de": "Frau" } }
                        ],
                        "alignment": "vertical"
                    },
                    {
                        "type": "StaticText",
                        "common": {
                            "name": "IntroText",
                            "resource_type": "fd/af/components/controls/textdraw"
                        },
                        "layout": { "width": 12 },
                        "content": { "en": "<p>Welcome</p>", "de": "<p>Willkommen</p>" },
                        "heading_level": "H2"
                    },
                    {
                        "type": "Signature",
                        "common": {
                            "name": "ApplicantSignature",
                            "resource_type": "fd/af/components/controls/scribble"
                        },
                        "field": { "label": { "en": "Signature", "de": "Unterschrift" } },
                        "layout": { "width": 6 }
                    },
                    {
                        "type": "Component",
                        "common": {
                            "name": "AddressFragment",
                            "resource_type": "fd/af/components/panel"
                        },
                        "properties": {
                            "jcr:title": { "kind": "text", "value": { "en": "Address", "de": "Adresse" } },
                            "fragRef": {
                                "kind": "single",
                                "value": "/content/dam/formsanddocuments/global/address"
                            }
                        },
                        "children": []
                    }
                ]
            }
        ]
    })
}
