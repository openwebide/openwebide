//! Real terraform-ls through the shared host, without initializing infrastructure.
use anyhow::{Context, Result, ensure};
use openwebide_bridge::languages::host::{Host, Scope};
use openwebide_core::plugins::languages::{
    intelligence::{self as protocol, Position, Query, Response, results},
    source_digest,
};
use serde_json::json;
use std::{path::Path, time::Duration};

const VARIABLES: &str =
    "variable \"message\" {\r\n  type = string\r\n  default = \"disk\"\r\n}\r\n";
const UNSAVED: &str = "variable \"message\" {\r\n  type = number\r\n  default = 42\r\n}\r\n";
const MAIN: &str = "// 文😀\r\nlocals {\r\n  greeting = var.message\r\n}\r\noutput \"greeting\" {\r\n  value = local.greeting\r\n}\r\n";

struct Fixture {
    host: Host,
    scope: Scope,
    sequence: u64,
    client: &'static str,
}
impl Fixture {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            host: Host::default(),
            scope: Scope {
                owner: "fixture-owner".into(),
                project: 1,
                root: root.into(),
                provider: "community/terraform".into(),
                digest: "a".repeat(64),
                server: serde_json::from_value(json!({"command":"terraform-ls","args":["serve"]}))?,
            },
            sequence: 0,
            client: "editor-a",
        })
    }
    async fn query(
        &mut self,
        path: &str,
        language: &str,
        source: &str,
        revision: u64,
        query: Query,
    ) -> Result<Response> {
        self.sequence += 1;
        let input = protocol::Request {
            version: protocol::VERSION,
            client: self.client.into(),
            document: path.into(),
            request: self.sequence,
            revision,
            language: language.into(),
            path: path.into(),
            source_digest: source_digest(source),
            source: source.into(),
            query,
        };
        let mut scope = self.scope.clone();
        scope.server.language_id = Some(language.into());
        let response = self
            .host
            .query(scope, &input)
            .await
            .map_err(anyhow::Error::msg)?;
        response.validate(&input).map_err(anyhow::Error::msg)?;
        Ok(response)
    }
    async fn hover(&mut self, source: &str, revision: u64, expected: &str) -> Result<Response> {
        let position = point(
            source,
            source.find("var.message").context("Missing variable use")? + 4,
        )?;
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query(
                        "main.tf",
                        "terraform",
                        source,
                        revision,
                        Query::Hover { position },
                    )
                    .await?;
                if response.result.to_string().contains(expected) {
                    return Ok::<_, anyhow::Error>(response);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }
    async fn diagnostics(
        &mut self,
        source: &str,
        revision: u64,
        contains: Option<&str>,
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let response = self
                    .query("main.tf", "terraform", source, revision, Query::Synchronize)
                    .await?;
                if response.diagnostics_ready
                    && contains.map_or_else(
                        || response.diagnostics.is_empty(),
                        |expected| {
                            response
                                .diagnostics
                                .iter()
                                .any(|diagnostic| diagnostic.message.contains(expected))
                        },
                    )
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await?
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
    }
}
fn point(source: &str, byte: usize) -> Result<Position> {
    protocol::position(source, byte).map_err(anyhow::Error::msg)
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = tempfile::tempdir()?;
    for (path, source) in [("variables.tf", VARIABLES), ("main.tf", MAIN)] {
        std::fs::write(root.path().join(path), source)?;
    }
    for path in [
        "completion.tf",
        "format.tf",
        "probe.tfvars",
        "probe.tfcomponent.hcl",
        "probe.tfdeploy.hcl",
        "probe.tfquery.hcl",
        "probe.policy.hcl",
        "probe.policytest.hcl",
    ] {
        std::fs::write(root.path().join(path), "")?;
    }
    let mut fixture = Fixture::new(root.path())?;
    let result = check(&mut fixture, root.path()).await;
    fixture.host.shutdown();
    openwebide_bridge::exec::proc::wait_for_cleanup().await;
    result
}
async fn check(fixture: &mut Fixture, root: &Path) -> Result<()> {
    let first = fixture
        .query(
            "variables.tf",
            "terraform",
            VARIABLES,
            1,
            Query::Synchronize,
        )
        .await?;
    println!("{}", json!({"terraform_capabilities":first.capabilities}));
    let hover = fixture.hover(MAIN, 1, "string").await?;
    ensure!(
        hover.server == first.server,
        "Ordinary Terraform analysis restarted its service"
    );
    fixture
        .query("variables.tf", "terraform", UNSAVED, 2, Query::Synchronize)
        .await?;
    let changed = fixture.hover(MAIN, 1, "number").await?;
    ensure!(
        changed.server == first.server,
        "An unsaved variable edit restarted Terraform"
    );
    let position = point(MAIN, MAIN.find("var.message").unwrap() + 4)?;
    let definition = fixture
        .query(
            "main.tf",
            "terraform",
            MAIN,
            1,
            Query::Definition { position },
        )
        .await?;
    let targets: Vec<results::Target> = serde_json::from_value(definition.result)?;
    ensure!(
        targets
            .iter()
            .any(|target| target.path.as_deref() == Some("variables.tf")
                && target.source_digest.as_deref() == Some(&source_digest(UNSAVED))),
        "Terraform definition lost its unsaved source binding"
    );
    let symbols = fixture
        .query("variables.tf", "terraform", UNSAVED, 2, Query::Symbols)
        .await?;
    ensure!(
        symbols.result.to_string().contains("message"),
        "Variable symbol was missing"
    );
    let references = fixture
        .query(
            "variables.tf",
            "terraform",
            UNSAVED,
            2,
            Query::References {
                position: point(UNSAVED, UNSAVED.find("message").unwrap())?,
            },
        )
        .await?;
    let references: Vec<results::Target> = serde_json::from_value(references.result)?;
    ensure!(
        references
            .iter()
            .any(|target| target.path.as_deref() == Some("main.tf")),
        "Variable references omitted its current use"
    );
    let completion_source = "locals {\r\n  value = var.\r\n}\r\n";
    let completion = fixture
        .query(
            "completion.tf",
            "terraform",
            completion_source,
            1,
            Query::Completion {
                position: point(
                    completion_source,
                    completion_source.find("var.").unwrap() + 4,
                )?,
            },
        )
        .await?;
    ensure!(
        completion
            .result
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["label"]
                .as_str()
                .is_some_and(|label| label.contains("message")))),
        "Variable completion was missing: {}",
        completion.result
    );
    fixture
        .query(
            "completion.tf",
            "terraform",
            completion_source,
            1,
            Query::Close,
        )
        .await?;
    let broken = MAIN.replace("var.message", "var.missing");
    fixture.diagnostics(&broken, 2, Some("missing")).await?;
    fixture.diagnostics(MAIN, 3, None).await?;
    for (language, path, source) in [
        ("terraform-vars", "probe.tfvars", "message = 42\r\n"),
        (
            "terraform-stack",
            "probe.tfcomponent.hcl",
            "variable \"probe\" { type = string }\r\n",
        ),
        (
            "terraform-deploy",
            "probe.tfdeploy.hcl",
            "deployment \"probe\" {}\r\n",
        ),
        (
            "terraform-search",
            "probe.tfquery.hcl",
            "list \"probe\" \"example\" {}\r\n",
        ),
        (
            "terraform-policy",
            "probe.policy.hcl",
            "policy \"probe\" {}\r\n",
        ),
        (
            "terraform-policytest",
            "probe.policytest.hcl",
            "test \"probe\" {}\r\n",
        ),
    ] {
        let response = fixture
            .query(path, language, source, 1, Query::Symbols)
            .await?;
        ensure!(
            response.server == first.server,
            "{language} did not share its family's service"
        );
        let expected = if language == "terraform-vars" {
            "message"
        } else {
            "probe"
        };
        ensure!(
            response.result.to_string().contains(expected),
            "{language} omitted its document symbol: {}",
            response.result
        );
        fixture
            .query(path, language, source, 1, Query::Close)
            .await?;
    }
    let unformatted = "locals { value=42 }\r\n";
    let formatted = fixture
        .query(
            "format.tf",
            "terraform",
            unformatted,
            1,
            Query::Formatting {
                tab_width: 2,
                insert_spaces: true,
            },
        )
        .await?;
    let edits: Vec<results::TextEdit> = serde_json::from_value(formatted.result)?;
    ensure!(
        results::apply_edits(unformatted, &edits)
            .map_err(anyhow::Error::msg)?
            .contains("value = 42"),
        "Terraform CLI formatting returned no usable edits"
    );
    fixture.client = "editor-b";
    let other = VARIABLES
        .replace("string", "bool")
        .replace("\"disk\"", "true");
    fixture
        .query("variables.tf", "terraform", &other, 1, Query::Synchronize)
        .await?;
    let isolated = fixture.hover(MAIN, 1, "bool").await?;
    ensure!(
        isolated.server != first.server,
        "Clients shared one analysis generation"
    );
    fixture.client = "editor-a";
    fixture.hover(MAIN, 3, "number").await?;
    fixture.scope.owner = "other-owner".into();
    let owner = fixture.hover(MAIN, 1, "string").await?;
    ensure!(
        owner.server != isolated.server,
        "Accounts shared a native service"
    );
    fixture.scope.owner = "fixture-owner".into();
    let before_revoke = fixture.hover(MAIN, 3, "number").await?;
    fixture
        .host
        .revoke("fixture-owner", Some(1), Some("community/terraform"));
    fixture
        .query("variables.tf", "terraform", UNSAVED, 2, Query::Synchronize)
        .await?;
    let restarted = fixture.hover(MAIN, 3, "number").await?;
    ensure!(
        restarted.server != before_revoke.server,
        "Revocation reused its native generation"
    );
    for (path, source) in [("variables.tf", VARIABLES), ("main.tf", MAIN)] {
        ensure!(
            std::fs::read_to_string(root.join(path))? == source,
            "Analysis wrote {path} behind Save"
        );
    }
    for path in [
        "completion.tf",
        "format.tf",
        "probe.tfvars",
        "probe.tfcomponent.hcl",
        "probe.tfdeploy.hcl",
        "probe.tfquery.hcl",
        "probe.policy.hcl",
        "probe.policytest.hcl",
    ] {
        ensure!(
            std::fs::read(root.join(path))?.is_empty(),
            "Analysis wrote the draft {path}"
        );
    }
    ensure!(
        !root.join("terraform.tfstate").exists() && !root.join(".terraform").exists(),
        "The language probe initialized infrastructure"
    );
    println!(
        "{}",
        json!({"workspace_not_initialized":true,"real_terraform_lsp":true,"reviewable_formatting":true,"source_bound_definition":true,"source_files_unchanged":true,"symbols":true,"references":true,"completion":true,"diagnostics_and_clear":true,"seven_dialects_share_service":true,"client_owner_isolation":true,"revocation_restart":true,"unsaved_cross_file_hover":true})
    );
    Ok(())
}
