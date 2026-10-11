//! Public component bindings and tool types. No application internals are linked.
pub mod bindings {
    wit_bindgen::generate!({path: "wit", world: "plugin", pub_export_macro: true});
}

use serde::{Deserialize, Serialize};
pub use serde_json;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionProfile {
    #[default]
    Primary,
    Fast,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionRequest {
    pub system_prompt: String,
    pub prompt: String,
    #[serde(default)]
    pub profile: CompletionProfile,
    pub max_output_tokens: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionResponse {
    pub text: String,
}
/// Bounded text generation through the host's configured primary or fast model.
/// Plugins own prompts, interpretation and fallback behavior. Tools are disabled.
pub fn complete(input: &CompletionRequest) -> Result<CompletionResponse, String> {
    request("completion", input)
}

/// General HTTP transport. Bodies use base64 so binary data stays portable.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRequest {
    pub url: String,
    pub method: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body_base64: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body_base64: String,
}
impl HttpResponse {
    pub fn bytes(&self) -> Result<Vec<u8>, String> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(&self.body_base64)
            .map_err(|error| error.to_string())
    }
    pub fn text(&self) -> Result<String, String> {
        String::from_utf8(self.bytes()?).map_err(|error| error.to_string())
    }
}
pub fn http(request: &HttpRequest) -> Result<HttpResponse, String> {
    self::request("http", request)
}

/// General record CRUD. The host attaches user, project and plugin identity;
/// callers cannot choose another account or namespace. Operation is an object
/// with an `action` of list/read/create/update/delete and its arguments.
pub fn records<T: Serialize, R: serde::de::DeserializeOwned>(
    collection: &str,
    operation: &T,
) -> Result<R, String> {
    request(
        "records",
        &serde_json::json!({"collection": collection, "operation": operation}),
    )
}

/// CRUD for app-visible project collections, using the same operation envelope
/// as private records. This requires a separate `collections` grant. The host
/// supplies project/account scope, schema validation and revision checks; plugin
/// code supplies feature policy such as searching and formatting.
pub fn collections<T: Serialize, R: serde::de::DeserializeOwned>(
    collection: &str,
    operation: &T,
) -> Result<R, String> {
    request(
        "collections",
        &serde_json::json!({"collection": collection, "operation": operation}),
    )
}

/// Durable one-shot event jobs. Operations are list/read/schedule/cancel/delete.
/// Delete removes terminal jobs and their retained idempotency keys.
/// The host supplies scope, idempotency, leases and immutable program snapshots;
/// plugins calculate due times and recurrence and interpret event results.
pub fn jobs<T: Serialize, R: serde::de::DeserializeOwned>(operation: &T) -> Result<R, String> {
    request("jobs", operation)
}

/// Durable prompt submissions. Operations are list/read/submit/cancel/delete.
/// The host validates conversation scope and queues prompts; plugins decide when
/// to submit them and how to interpret status or completion results.
/// Submit may include up to eight `prerequisites`: scoped record revisions checked
/// atomically before creating a new run. Identical-key retries return existing work.
pub fn runs<T: Serialize, R: serde::de::DeserializeOwned>(operation: &T) -> Result<R, String> {
    request("runs", operation)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPrerequisite {
    /// Requires the corresponding declared and approved `records` or `collections` grant.
    pub capability: String,
    pub collection: String,
    pub id: i64,
    pub revision: i64,
}

/// The host independently validates names, schemas and requested capabilities.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub requires_approval: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub ok: bool,
    pub content: String,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextInput {
    pub budget_bytes: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextContribution {
    pub prompt: Option<String>,
    pub disabled_tools: Vec<String>,
}

/// Host-triggered callback data. Authority remains outside this envelope.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventInput {
    pub name: String,
    pub payload: serde_json::Value,
}

/// Plugins implement their feature behavior here.
pub trait Plugin {
    fn tools() -> Vec<Tool> {
        Vec::new()
    }
    /// Event names must exactly match the executable manifest contributions.
    fn events() -> Vec<String> {
        Vec::new()
    }
    fn execute(_name: &str, _arguments: serde_json::Value) -> Result<Outcome, String> {
        Err("This plugin does not provide tool handlers".into())
    }
    /// Read-only planning hook, evaluated before model tool selection. The host
    /// enforces its context budget and denies mutating capability calls.
    fn context(_input: ContextInput) -> Result<ContextContribution, String> {
        Ok(ContextContribution::default())
    }
    /// Event handlers own their behavior and use the same granted primitives as
    /// tools. They are not advertised as tools to the model.
    fn event(_input: EventInput) -> Result<Outcome, String> {
        Err("This plugin does not handle host events".into())
    }
}

/// Call a granted general host capability. The host supplies authority.
pub fn request<T: Serialize, R: for<'de> Deserialize<'de>>(
    capability: &str,
    payload: &T,
) -> Result<R, String> {
    let input = serde_json::to_string(payload).map_err(|error| error.to_string())?;
    let response = bindings::openwebide::plugin::host::request(capability, &input)?;
    serde_json::from_str(&response).map_err(|error| error.to_string())
}

/// Export a plugin implementation using the public versioned component contract.
#[macro_export]
macro_rules! export {
    ($plugin:ty) => {
        struct OpenWebIdeComponent;
        impl $crate::bindings::Guest for OpenWebIdeComponent {
            fn tools() -> String {
                $crate::serde_json::to_string(&<$plugin as $crate::Plugin>::tools())
                    .expect("serializable tool definitions")
            }
            fn events() -> String {
                $crate::serde_json::to_string(&<$plugin as $crate::Plugin>::events())
                    .expect("serializable event definitions")
            }
            fn execute(name: String, arguments: String) -> Result<String, String> {
                let arguments = $crate::serde_json::from_str(&arguments)
                    .map_err(|error| error.to_string())?;
                let result = <$plugin as $crate::Plugin>::execute(&name, arguments)?;
                $crate::serde_json::to_string(&result).map_err(|error| error.to_string())
            }
            fn context(input: String) -> Result<String, String> {
                let input = $crate::serde_json::from_str(&input)
                    .map_err(|error| error.to_string())?;
                let result = <$plugin as $crate::Plugin>::context(input)?;
                $crate::serde_json::to_string(&result).map_err(|error| error.to_string())
            }
            fn event(input: String) -> Result<String, String> {
                let input = $crate::serde_json::from_str(&input)
                    .map_err(|error| error.to_string())?;
                let result = <$plugin as $crate::Plugin>::event(input)?;
                $crate::serde_json::to_string(&result).map_err(|error| error.to_string())
            }
        }
        $crate::bindings::export!(OpenWebIdeComponent with_types_in $crate::bindings);
    };
}
