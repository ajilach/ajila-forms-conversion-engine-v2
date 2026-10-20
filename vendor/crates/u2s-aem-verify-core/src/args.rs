//! The arguments of every verify tool, as types: each is both the tool's
//! input schema and what its arguments parse into. Shared by both verifier
//! binaries, which write only their own tool descriptions.
//!
//! A `///` on a field is the description the caller reads: prompt text.
//! Maintainer notes use `//`. Each tool's type closes its schema with
//! `#[schemars(deny_unknown_fields)]`; deserialization stays lenient, since
//! the transport adds arguments of its own.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use u2s_blob::{BlobOrPath, SourceArgs};
use u2s_verify_core::browser::ImageFormat;

use crate::interactive::{ControlKind, ControlsQuery, SetRequest};

/// A call's arguments as the tool's argument type.
pub fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, String> {
    u2s_schema::parse_tool_args(args).map_err(|err| format!("invalid arguments: {err}"))
}

/// The caller's own instance (`crate::instance`'s module doc explains why
/// this exists at all). Callers that omit it -- a human, a test, anything
/// with no `u2s-server` run behind it -- get
/// [`crate::instance::DEFAULT_INSTANCE_KEY`], one shared instance.
fn instance_key(instance_id: Option<&str>) -> String {
    instance_id
        .filter(|id| !id.is_empty())
        .unwrap_or(crate::instance::DEFAULT_INSTANCE_KEY)
        .to_owned()
}

/// `instance_id` as the interactive tools describe it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct InstanceArg {
    /// Any stable string identifying the calling agent/run -- the same value passed to verify_run/verify_status. Each value gets its own AEM+Chromium instance, which holds that caller's sessions. Usually injected by the server; omit to use one shared default instance.
    #[schemars(length(min = 1))]
    pub instance_id: Option<String>,
}

impl InstanceArg {
    pub fn key(&self) -> String {
        instance_key(self.instance_id.as_deref())
    }
}

/// The open form an interactive call acts on.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SessionArg {
    /// The session handle verify_open returned: one open form, in its own browser tab.
    #[schemars(length(min = 1))]
    pub session: String,
}

/// The revision a side-effecting interactive call is based on.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct BasedOn {
    /// The revision this call is based on.
    pub expected_revision: u64,
}

// ------------------------------------------------------------- top level

/// `verify_status`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct StatusArgs {
    /// Which caller's instance to report on -- the same value passed to verify_run. Omit to report on the shared default instance.
    #[schemars(length(min = 1))]
    pub instance_id: Option<String>,
}

impl StatusArgs {
    pub fn instance_key(&self) -> String {
        instance_key(self.instance_id.as_deref())
    }
}

/// `verify_package_check`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PackageCheckArgs {
    /// A u2s blob handle naming the FileVault package to check.
    #[schemars(length(min = 1))]
    pub package: Option<String>,
    /// A filesystem path to the package, for conformance test vectors.
    #[schemars(length(min = 1))]
    pub package_path: Option<String>,
}

impl PackageCheckArgs {
    pub fn package(self) -> Result<BlobOrPath, String> {
        BlobOrPath::from_parts(self.package, self.package_path, SourceArgs::PACKAGE)
    }
}

/// `verify_run`, with `L` the language argument a profile offers:
/// [`LanguageArg`] where the driver can open a form in a language,
/// [`NoLanguage`] where it cannot.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct VerifyRunArgs<L> {
    /// A u2s blob handle naming the FileVault package to verify.
    #[schemars(length(min = 1))]
    pub package: Option<String>,
    /// A filesystem path to the package, for conformance test vectors.
    #[schemars(length(min = 1))]
    pub package_path: Option<String>,
    /// Validate inputs and report Docker reachability only; default false.
    #[serde(default)]
    pub dry_run: bool,
    /// Field name to value. Each name must be the guide node's own `name` -- GuideBridge exposes it as a global carrying `.value`. Ignored when `submit` is false.
    // `length` only constrains strings and arrays; a map's minimum is
    // `minProperties`.
    #[schemars(extend("minProperties" = 1))]
    pub fill: Option<Map<String, Value>>,
    /// Fill and submit after rendering; default true.
    #[serde(default = "VerifyRunArgs::<L>::default_submit")]
    pub submit: bool,
    /// Any stable string identifying the calling agent/run. Two different values get two independent AEM+Chromium instances, so concurrent callers never share or block on one another's instance; the same value across calls reuses that caller's own instance. Omit to use a single shared default instance.
    #[schemars(length(min = 1))]
    pub instance_id: Option<String>,
    #[serde(flatten)]
    pub language: L,
}

impl<L> VerifyRunArgs<L> {
    fn default_submit() -> bool {
        true
    }

    pub fn instance_key(&self) -> String {
        instance_key(self.instance_id.as_deref())
    }
}

/// The language a form is opened in, for a profile whose driver can.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct LanguageArg {
    /// The language to open the form in: one of the codes its metadata declares for the entity (verify_package_check lists them, e.g. `de`). Default: the entity's first. A language the form does not declare is an error.
    #[schemars(length(min = 1))]
    pub language: Option<String>,
}

/// No language argument: the profile's driver opens a form in its one
/// language. The server still reads every call as
/// `VerifyRunArgs<LanguageArg>`, so a language sent anyway reaches the
/// driver, which refuses it by name.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct NoLanguage {}

// ----------------------------------------------------------- interactive

/// `verify_open`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OpenArgs {
    /// A u2s blob handle naming the FileVault package to open.
    #[schemars(length(min = 1))]
    pub package: Option<String>,
    /// A filesystem path to the package, for conformance test vectors.
    #[schemars(length(min = 1))]
    pub package_path: Option<String>,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

/// `verify_controls`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ControlsArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    /// The form's current revision, from verify_open or the last interactive call.
    pub revision: u64,
    /// Where the page starts; default 0.
    pub offset: Option<u64>,
    /// How many controls at most; default 100.
    #[schemars(range(min = 1, max = ControlsQuery::MAX_LIMIT))]
    pub limit: Option<u64>,
    /// Only controls of these kinds; omit for every kind.
    #[serde(default, deserialize_with = "kinds_arg")]
    #[schemars(length(min = 1))]
    pub kinds: Option<Vec<ControlKind>>,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

impl ControlsArgs {
    pub fn query(&self) -> Result<ControlsQuery, String> {
        ControlsQuery::new(
            self.offset,
            self.limit,
            self.kinds.clone().unwrap_or_default(),
        )
    }
}

/// Reads `kinds`, refusing an unknown kind by listing the valid ones.
fn kinds_arg<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<ControlKind>>, D::Error> {
    use serde::de::Error as _;
    let Some(kinds) = Option::<Value>::deserialize(d)? else {
        return Ok(None);
    };
    Vec::<ControlKind>::deserialize(&kinds)
        .map(Some)
        .map_err(|_| {
            D::Error::custom(format!(
                "kinds must be an array of {}, got {kinds}",
                ControlKind::ALL
                    .iter()
                    .map(|kind| serde_json::to_string(kind).expect("a kind serialises"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// `verify_field`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FieldArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    /// The form's current revision.
    pub revision: u64,
    /// A control's `field` (its SOM expression), from verify_controls.
    #[schemars(length(min = 1))]
    pub field: String,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

/// How many controls one batched `verify_set` may set.
pub const MAX_SETS_PER_CALL: usize = 25;

/// `verify_set`. Either `field` and `value`, or `fields`:
/// [`SetArgs::requests`] is their one reading.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct SetArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    #[serde(flatten)]
    pub based_on: BasedOn,
    /// A control's `field` (its SOM expression), from verify_controls.
    #[schemars(length(min = 1))]
    pub field: Option<String>,
    /// What to set it to -- one of the control's own option values (an array for a multi-select checkbox), or free text/a number.
    // `deserialize_with` so that a `null` value is a value, as it is inside
    // `fields`, rather than the same as leaving `value` out.
    #[serde(default, deserialize_with = "present")]
    pub value: Option<Value>,
    /// Several controls to set in order, instead of `field`/`value`.
    #[schemars(length(min = 1, max = MAX_SETS_PER_CALL))]
    pub fields: Option<Vec<SetEntry>>,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SetEntry {
    /// A control's `field` (its SOM expression), from verify_controls.
    #[schemars(length(min = 1))]
    pub field: String,
    /// What to set it to, as `value` above.
    pub value: Value,
}

fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// What one `verify_set` call sets.
#[derive(Debug, Clone, PartialEq)]
pub enum SetRequests {
    One { field: String, value: Value },
    Many(Vec<SetRequest>),
}

impl SetArgs {
    /// The sets this call asks for: one, or a batch of 1 to
    /// [`MAX_SETS_PER_CALL`], never both.
    pub fn requests(self) -> Result<SetRequests, String> {
        match (self.fields, self.field, self.value) {
            (Some(_), Some(_), _) => {
                Err("pass either `field`/`value` or `fields`, not both".to_owned())
            }
            (Some(fields), None, _) => {
                if fields.is_empty() {
                    return Err("fields must name at least one control".to_owned());
                }
                if fields.len() > MAX_SETS_PER_CALL {
                    return Err(format!(
                        "fields names {} controls; set at most {MAX_SETS_PER_CALL} per call",
                        fields.len()
                    ));
                }
                fields
                    .into_iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        if entry.field.is_empty() {
                            return Err(format!("fields[{index}] has no `field`"));
                        }
                        Ok(SetRequest {
                            field: entry.field,
                            value: entry.value,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(SetRequests::Many)
            }
            (None, None, _) => Err("missing required argument: field".to_owned()),
            (None, Some(_), None) => Err("missing required argument: value".to_owned()),
            (None, Some(field), Some(value)) => Ok(SetRequests::One { field, value }),
        }
    }
}

/// `verify_click`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ClickArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    #[serde(flatten)]
    pub based_on: BasedOn,
    /// The button's `field` (its SOM expression), from verify_controls.
    #[schemars(length(min = 1))]
    pub field: String,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

/// `verify_next`, `verify_prev`, `verify_reset` and `verify_submit`: a
/// step on the open form that takes nothing else.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct StepArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    #[serde(flatten)]
    pub based_on: BasedOn,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

/// `verify_goto`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct GotoArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    #[serde(flatten)]
    pub based_on: BasedOn,
    /// A panel's `som` from `panels`.
    #[schemars(length(min = 1))]
    pub panel: String,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

/// The image a `verify_screenshot` returns. `png` is the default, matching
/// every screenshot `verify_run` itself has always taken
/// (`page.screenshot_full_page()`, PNG); `jpeg` always uses quality 82, the
/// same vision-payload quality `u2s-render-core::limits::Limits`'s own
/// default settled on, rather than exposing a third knob nothing has needed
/// yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScreenshotFormat {
    #[default]
    Png,
    Jpeg,
}

impl From<ScreenshotFormat> for ImageFormat {
    fn from(format: ScreenshotFormat) -> Self {
        match format {
            ScreenshotFormat::Png => ImageFormat::Png,
            ScreenshotFormat::Jpeg => ImageFormat::Jpeg { quality: 82 },
        }
    }
}

/// `verify_screenshot`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ScreenshotArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    /// The form's current revision.
    pub revision: u64,
    /// Image format; default png.
    pub format: Option<ScreenshotFormat>,
    /// A control's `field` (its SOM expression), to crop the screenshot to just that control; omit for the whole page.
    #[schemars(length(min = 1))]
    pub field: Option<String>,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

/// `verify_close`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CloseArgs {
    #[serde(flatten)]
    pub session: SessionArg,
    #[serde(flatten)]
    pub instance: InstanceArg,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(args: Value) -> Result<SetRequests, String> {
        parse_args::<SetArgs>(&args)?.requests()
    }

    #[test]
    fn verify_set_takes_one_set_or_a_batch_never_both() {
        let base = json!({ "session": "s", "expected_revision": 0 });
        let with = |extra: Value| {
            let mut args = base.clone();
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            args
        };
        assert_eq!(
            set(with(json!({ "field": "a", "value": null }))),
            Ok(SetRequests::One {
                field: "a".to_owned(),
                value: Value::Null
            })
        );
        assert!(matches!(
            set(with(json!({ "fields": [{ "field": "a", "value": 1 }, { "field": "b", "value": "x" }] }))),
            Ok(SetRequests::Many(requests)) if requests.len() == 2
        ));
        assert!(
            set(with(json!({ "field": "a", "value": 1, "fields": [] })))
                .unwrap_err()
                .contains("not both")
        );
        assert!(
            set(with(json!({ "field": "a" })))
                .unwrap_err()
                .contains("value")
        );
        assert!(set(with(json!({}))).unwrap_err().contains("field"));
        assert!(
            set(with(json!({ "fields": [] })))
                .unwrap_err()
                .contains("at least one")
        );
        let many: Vec<Value> = (0..26)
            .map(|i| json!({ "field": format!("f{i}"), "value": i }))
            .collect();
        assert!(
            set(with(json!({ "fields": many })))
                .unwrap_err()
                .contains("at most 25")
        );
        assert!(
            set(with(json!({ "fields": [{ "field": "", "value": 1 }] })))
                .unwrap_err()
                .contains("fields[0]")
        );
    }

    #[test]
    fn an_unknown_kind_is_refused_listing_the_valid_ones() {
        let err = parse_args::<ControlsArgs>(
            &json!({ "session": "s", "revision": 0, "kinds": ["slider"] }),
        )
        .unwrap_err();
        assert!(
            err.contains("kinds must be") && err.contains("\"radio\""),
            "{err}"
        );
    }

    #[test]
    fn verify_run_reads_its_defaults_and_its_language() {
        let run =
            parse_args::<VerifyRunArgs<LanguageArg>>(&json!({ "package": "h", "language": "de" }))
                .unwrap();
        assert!(!run.dry_run && run.submit);
        assert_eq!(run.language.language.as_deref(), Some("de"));
        assert_eq!(run.instance_key(), crate::instance::DEFAULT_INSTANCE_KEY);
    }

    #[test]
    fn a_profile_without_languages_offers_none() {
        let schema = u2s_schema::tool_input_schema::<VerifyRunArgs<NoLanguage>>();
        assert!(schema["properties"].get("language").is_none(), "{schema}");
        let schema = u2s_schema::tool_input_schema::<VerifyRunArgs<LanguageArg>>();
        assert!(schema["properties"].get("language").is_some(), "{schema}");
    }
}
