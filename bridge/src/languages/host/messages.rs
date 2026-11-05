use super::super::rpc::ServerMessage;
use super::documents::Diagnostics;
use super::*;
use tokio::sync::mpsc;

pub(super) fn pump(
    client: Client,
    mut events: mpsc::Receiver<ServerMessage>,
    diagnostics: Arc<Mutex<BTreeMap<String, Diagnostics>>>,
    root: &url::Url,
    settings: Value,
) -> tokio::task::AbortHandle {
    let root = root.to_string();
    tokio::spawn(async move {
        while let Some(message)=events.recv().await {
            if message.method=="textDocument/publishDiagnostics" {
                publish_diagnostics(&mut diagnostics.lock().unwrap(), &message.params);
                continue;
            }
            let Some(id)=message.id else {continue;};
            let result=match message.method.as_str() {
                "workspace/configuration"=>Some(Value::Array(message.params["items"].as_array().map_or_else(Vec::new,|items|items.iter().map(|item|configuration(&settings,item["section"].as_str())).collect()))),
                "workspace/workspaceFolders"=>Some(json!([{"uri":root,"name":"Project"}])),
                "window/workDoneProgress/create"=>Some(Value::Null),
                _=>None,
            };
            let response=result.map_or_else(||json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Client does not support this request"}}),|result|json!({"jsonrpc":"2.0","id":id,"result":result}));
            if client.send(response).await.is_err() {break;}
        }
    }).abort_handle()
}

fn publish_diagnostics(snapshots: &mut BTreeMap<String, Diagnostics>, params: &Value) {
    let Some(uri) = params["uri"].as_str() else {
        return;
    };
    let Some(snapshot) = snapshots.get_mut(uri) else {
        return;
    };
    // LSP permits an omitted version. Such notifications describe the currently
    // open document; explicit versions must match exactly. Source changes clear
    // this slot before synchronization, and editor publication remains bound to
    // the request's source, revision and editing-client identity.
    if let Some(version) = params.get("version").filter(|value| !value.is_null())
        && version.as_i64() != Some(i64::from(snapshot.version))
    {
        return;
    }
    let Some(items) = params["diagnostics"]
        .as_array()
        .filter(|items| items.len() <= protocol::MAX_DIAGNOSTICS)
    else {
        return;
    };
    snapshot.raw = items.clone();
    snapshot.ready = true;
}

fn configuration(settings: &Value, section: Option<&str>) -> Value {
    let Some(section) = section.filter(|section| !section.is_empty()) else {
        return settings.clone();
    };
    if let Some(value) = settings.get(section) {
        return value.clone();
    }
    let mut value = settings;
    for key in section.split('.') {
        let Some(next) = value.get(key) else {
            return Value::Null;
        };
        value = next;
    }
    value.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versionless_diagnostics_use_only_open_documents_and_explicit_versions_reject_stale_results()
    {
        let uri = "file:///project/file.ts";
        let mut snapshots = BTreeMap::from([(
            uri.into(),
            Diagnostics {
                version: 4,
                raw: vec![],
                ready: false,
            },
        )]);
        let current = json!({"uri":uri,"diagnostics":[{"message":"current"}]});
        publish_diagnostics(&mut snapshots, &current);
        assert!(snapshots[uri].ready);
        assert_eq!(snapshots[uri].raw[0]["message"], "current");
        for version in [json!(3), json!(5), json!("4"), json!(4.5)] {
            publish_diagnostics(
                &mut snapshots,
                &json!({"uri":uri,"version":version,"diagnostics":[]}),
            );
            assert_eq!(snapshots[uri].raw.len(), 1);
        }
        publish_diagnostics(
            &mut snapshots,
            &json!({"uri":uri,"version":4,"diagnostics":[]}),
        );
        assert!(snapshots[uri].ready);
        assert!(snapshots[uri].raw.is_empty());
        snapshots.remove(uri);
        publish_diagnostics(&mut snapshots, &current);
        assert!(snapshots.is_empty());
    }
}
