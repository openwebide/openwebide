use super::*;

#[tokio::test]
async fn mutable_runtime_output_preserves_the_original_review_in_both_scopes() {
    for owner in ["user:1", "paired/user:1"] {
        let mut fixture = Fixture::with_mode(owner, "inputs");
        let installation = tempfile::tempdir().unwrap();
        let script = installation.path().join("server.py");
        let output = installation.path().join("runtime-output.log");
        std::fs::write(
            &script,
            format!(
                "import pathlib,sys\npathlib.Path(sys.argv[2]).write_text('started')\n{}",
                include_str!("../../../../../../tools/fixtures/commands_server.py"),
            ),
        )
        .unwrap();
        fixture.scope.server.args = vec![
            script.to_str().unwrap().into(),
            "inputs".into(),
            output.to_str().unwrap().into(),
        ];
        fixture.scope.server.runtime_inputs =
            Some(serde_json::from_value(json!({"argumentFiles":[0]})).unwrap());
        fixture.scope.server.project_inputs =
            Some(serde_json::from_value(json!({"files":["settings*.json"]})).unwrap());
        fixture.prepared.manifest.compatibility.plugin_api = 10;
        fixture.prepared.manifest.contributions.languages[0]
            .features
            .language_server = Some(fixture.scope.server.clone());
        std::fs::write(fixture.root.path().join("settings-old.json"), "{}").unwrap();
        let state = fixture.state().await;
        let original = state.language_review.as_ref().unwrap();
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "started");
        std::fs::write(&output, "mutable output changed during review").unwrap();
        let request = Request {
            request: fixture.input.request + 1,
            query: Query::ValidateAnalysis {
                context: original.context.clone(),
            },
            ..original.input.clone()
        };
        let result = fixture
            .host
            .query_package(fixture.scope.clone(), &request, &fixture.prepared)
            .await;
        fixture.host.shutdown();
        let response = result.expect("Mutable output invalidated the original language review");
        assert_eq!(response.server, original.context.server);
        assert!(fixture.root.path().join("settings-old.json").is_file());
        assert!(!fixture.root.path().join("settings-new.json").exists());
        assert!(!fixture.root.path().join("executions").exists());
    }
}
