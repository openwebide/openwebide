//! Cross-repository check against the actual first-party WASI artifact.
use base64::{Engine, engine::general_purpose::STANDARD};
use openwebide_core::plugins::{PluginManifest, panels::*};
use openwebide_plugin_runtime::{HostServices, Runtime, sdk};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Fixture {
    head: String,
    posts: Vec<Value>,
}
#[derive(Clone)]
struct Services(Arc<Mutex<Fixture>>);
impl HostServices for Services {
    fn request(&mut self, capability: &str, payload: &str) -> Result<String, String> {
        let mut fixture = self.0.lock().unwrap();
        let payload: Value = serde_json::from_str(payload).unwrap();
        if capability == "records" {
            assert_eq!(payload["collection"], "github");
            assert_eq!(payload["operation"]["action"], "list");
            return Ok(json!({"records":[{"id":1,"revision":1,"value":{
                "api":"http://fixture.invalid/api","repository":"owner/repo","token":"fixture-private-token"
            }}],"next":null}).to_string());
        }
        assert_eq!(capability, "http");
        let request: sdk::HttpRequest = serde_json::from_value(payload).unwrap();
        assert!(
            request
                .url
                .starts_with("http://fixture.invalid/api/repos/owner/repo/"),
            "Reference tests cannot contact external services"
        );
        let body = if request.method == "POST" {
            fixture.posts.push(
                serde_json::from_slice(&STANDARD.decode(request.body_base64).unwrap()).unwrap(),
            );
            "{}".into()
        } else {
            assert_eq!(request.method, "GET");
            assert!(request.body_base64.is_empty());
            if request.headers["accept"] == "application/vnd.github.diff" {
                "diff --git a/file b/file\n+new\n".into()
            } else if request.url.ends_with("reviews?per_page=30") {
                "[]".into()
            } else if request.url.ends_with("pulls/42") {
                json!({"number":42,"title":"A change","body":"Review this","state":"open","head":{"sha":fixture.head},"html_url":"https://github.com/owner/repo/pull/42"}).to_string()
            } else {
                json!([{"number":42,"title":"A change","user":{"login":"contributor"}}]).to_string()
            }
        };
        Ok(serde_json::to_string(&sdk::HttpResponse {
            status: 200,
            headers: Default::default(),
            body_base64: STANDARD.encode(body),
        })
        .unwrap())
    }
}
fn run(runtime: &Runtime, bytes: &[u8], services: Services, input: &PanelInput) -> PanelDocument {
    let outcome = runtime
        .panel(
            bytes,
            services,
            &["http".into(), "records".into()],
            &serde_json::to_string(input).unwrap(),
        )
        .unwrap();
    assert!(outcome.ok);
    serde_json::from_str(&outcome.content).unwrap()
}
#[test]
#[ignore = "Compile the marketplace PR Review package and set OPENWEBIDE_PR_REVIEW_COMPONENT and OPENWEBIDE_PR_REVIEW_MANIFEST"]
fn first_party_component_owns_views_and_publishing_through_public_sdk() {
    let bytes =
        std::fs::read(std::env::var("OPENWEBIDE_PR_REVIEW_COMPONENT").expect("component path"))
            .unwrap();
    let manifest: PluginManifest = serde_json::from_slice(
        &std::fs::read(std::env::var("OPENWEBIDE_PR_REVIEW_MANIFEST").expect("manifest path"))
            .unwrap(),
    )
    .unwrap();
    manifest.validate().unwrap();
    let runtime = Runtime::new().unwrap();
    runtime.validate_exports(&bytes, &manifest).unwrap();
    let definition = &manifest.contributions.panels[0];
    let fixture = Arc::new(Mutex::new(Fixture {
        head: "a".repeat(40),
        posts: Vec::new(),
    }));
    let services = Services(fixture.clone());
    let mut input = PanelInput {
        panel: "pull-requests".into(),
        instance: "a".repeat(32),
        request: "b".repeat(32),
        revision: 0,
        handler: "render".into(),
        action: None,
        values: Default::default(),
        state: Value::Null,
    };
    let listing = run(&runtime, &bytes, services.clone(), &input);
    validate_document(definition, &listing).unwrap();
    assert!(
        !serde_json::to_string(&listing)
            .unwrap()
            .contains("fixture-private-token")
    );
    assert!(fixture.lock().unwrap().posts.is_empty());
    input.revision = listing.revision;
    input.state = listing.state;
    input.request = "c".repeat(32);
    input.handler = "open".into();
    input.action = Some("open".into());
    input.values = [("number".into(), json!(42))].into();
    let detail = run(&runtime, &bytes, services.clone(), &input);
    validate_document(definition, &detail).unwrap();
    let submit = detail
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Form { id, submit, .. } if id == "review" => Some(submit),
            _ => None,
        })
        .unwrap();
    input.revision = detail.revision;
    input.state = detail.state.clone();
    input.request = "d".repeat(32);
    input.handler = "publish".into();
    input.action = Some("publish".into());
    input.values = submit.values.clone();
    input.values.insert("event".into(), json!("COMMENT"));
    input.values.insert("body".into(), json!("Looks good"));
    assert!(validate_submission(definition, &detail, &input, false).is_err());
    validate_submission(definition, &detail, &input, true).unwrap();
    let published = run(&runtime, &bytes, services.clone(), &input);
    validate_document(definition, &published).unwrap();
    assert!(
        !serde_json::to_string(&published)
            .unwrap()
            .contains("fixture-private-token")
    );
    assert_eq!(
        fixture.lock().unwrap().posts,
        json!([{"event":"COMMENT","body":"Looks good","commit_id":"a".repeat(40)}])
            .as_array()
            .unwrap()
            .clone()
    );
    fixture.lock().unwrap().head = "b".repeat(40);
    let failed = runtime.panel(
        &bytes,
        services,
        &["http".into(), "records".into()],
        &serde_json::to_string(&input).unwrap(),
    );
    assert!(failed.is_err() || failed.is_ok_and(|outcome| !outcome.ok));
    assert_eq!(fixture.lock().unwrap().posts.len(), 1);
}
