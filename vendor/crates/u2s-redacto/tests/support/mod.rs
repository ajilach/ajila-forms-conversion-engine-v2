//! One shared fixture, shaped like a real (small) UBS Redacto document --
//! close to `redacto-AAEV_019.sql`'s own `documents.configuration` in
//! `~/Documents/ajila-redacto-platform/conversion/output-sql/`, trimmed to
//! what a unit test needs: a header, an intro paragraph, a two-column body
//! section and a footer, in two languages.

pub fn sample_document_json() -> serde_json::Value {
    serde_json::json!({
        "metadata": {
            "document_id": "aaev_019",
            "title": "AAEV_019",
            "style": "default.css",
            "form_path": "/content/forms/af/redacto-documents/aaev_019",
            "master_language": "en",
            "languages": ["en", "de"],
            "owner_id": "admin",
            "status": "draft"
        },
        "assets": [
            {
                "key": "header",
                "kind": "text",
                "content": {
                    "en": "<div class=\"right preserve-spaces\"><p>Valid from 02.01.2018</p><p><strong>UBS Europe SE</strong></p></div>",
                    "de": "<div class=\"right preserve-spaces\"><p>Gültig ab 02.01.2018</p><p><strong>UBS Europe SE</strong></p></div>"
                }
            },
            {
                "key": "intro",
                "kind": "text",
                "content": {
                    "en": "<h1>Investing in US Securities</h1><p>Qualified Intermediary rules apply.</p>",
                    "de": "<h1>Investieren in US-Wertschriften</h1><p>Die Qualified-Intermediary-Regeln gelten.</p>"
                }
            },
            {
                "key": "detail",
                "kind": "text",
                "content": {
                    "en": "<ul><li>The partnership is a direct account holder.</li></ul>",
                    "de": "<ul><li>Die Personengesellschaft ist direkte Kontoinhaberin.</li></ul>"
                }
            },
            {
                "key": "footer",
                "kind": "text",
                "content": {
                    "en": "<span class=\"redacto-reading-order\"><span class=\"footer-form-id\">66300</span> <span class=\"footer-language\">EN</span></span><span class=\"right\">Page <span class=\"page-number\"></span>/<span class=\"page-count\"></span></span>",
                    "de": "<span class=\"redacto-reading-order\"><span class=\"footer-form-id\">66300</span> <span class=\"footer-language\">DE</span></span><span class=\"right\">Seite <span class=\"page-number\"></span>/<span class=\"page-count\"></span></span>"
                }
            }
        ],
        "header": [
            { "type": "assetContainer", "assets": ["header"] }
        ],
        "body": [
            { "type": "assetContainer", "assets": ["intro"] },
            {
                "type": "styledPanel",
                "style": "layout-split",
                "components": [
                    { "type": "assetContainer", "assets": ["detail"] }
                ]
            }
        ],
        "footer": [
            { "type": "assetContainer", "assets": ["footer"] }
        ]
    })
}
