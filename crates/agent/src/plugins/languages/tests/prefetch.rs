use super::*;

#[test]
fn prefetch_uses_fresh_authority_and_exact_sources_on_both_hosts() {
    futures::executor::block_on(async {
        for profile in ["server", "paired"] {
            let (host, mut bindings, _, _) = setup(profile);
            let manifest = &mut bindings[0].prepared.manifest;
            manifest.compatibility.plugin_api = 9;
            manifest.executable = None;
            manifest.contributions.languages[0].features.syntax = false;
            manifest.contributions.languages[0].features.language_server = Some(
                serde_json::from_value(serde_json::json!({"command":"community-lsp"})).unwrap(),
            );
            let provider = LanguageRegistry::from_bindings(&bindings).providers[0].clone();
            let service = LanguageService::new(host.clone(), bindings, provider).unwrap();
            let document = LanguageDocumentRequest {
                document: "file".into(),
                path: "file.custom".into(),
                revision: 3,
                source: Arc::new("unsaved 文😀\r\n".into()),
                tab_width: 4,
            };
            for kind in [
                server_protocol::PrefetchKind::Diagnostics,
                server_protocol::PrefetchKind::Symbols,
            ] {
                service
                    .prefetch_intelligence("editor-a", document.clone(), kind)
                    .await
                    .unwrap();
            }
            let calls = host.server_calls.lock().unwrap().clone();
            assert_eq!(calls.len(), 2);
            assert_eq!(host.preparations.load(Ordering::Relaxed), 1);
            assert_eq!(host.grants.load(Ordering::Relaxed), 2);
            assert_eq!(host.starts.load(Ordering::Relaxed), 0);
            for call in &calls {
                assert!(call.input.query.is_prefetch());
                assert_eq!(call.input.source, *document.source);
                assert_eq!(call.input.revision, 3);
                assert_eq!(call.input.client, "editor-a");
                assert_eq!(call.prepared.host_id, profile);
            }
            assert!(calls[0].input.request < calls[1].input.request);
            host.deny_grant.store(true, Ordering::Relaxed);
            assert!(
                service
                    .prefetch_intelligence(
                        "editor-a",
                        document.clone(),
                        server_protocol::PrefetchKind::Symbols
                    )
                    .await
                    .is_err()
            );
            assert_eq!(host.server_calls.lock().unwrap().len(), 2);
            host.deny_grant.store(false, Ordering::Relaxed);
            host.current.store(false, Ordering::Relaxed);
            assert!(
                service
                    .prefetch_intelligence(
                        "editor-a",
                        document,
                        server_protocol::PrefetchKind::Symbols
                    )
                    .await
                    .is_err()
            );
            assert_eq!(host.server_calls.lock().unwrap().len(), 2);
        }
    });
}
