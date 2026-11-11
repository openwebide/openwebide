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
    // open document. The open and its full-content synchronization describe the
    // same bytes. Accept either explicit version, but never let an earlier one
    // replace a later publication. Actual source changes reset both bounds.
    // Editor publication still binds source, revision and editing client.
    let version = if let Some(version) = params.get("version").filter(|value| !value.is_null()) {
        let Some(version) = version
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .filter(|version| (snapshot.minimum_version..=snapshot.version).contains(version))
        else {
            return;
        };
        version
    } else {
        snapshot.version
    };
    let Some(items) = params["diagnostics"]
        .as_array()
        .filter(|items| items.len() <= protocol::MAX_DIAGNOSTICS)
    else {
        return;
    };
    snapshot.raw = items.clone();
    snapshot.minimum_version = version;
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
    fn identical_open_versions_remain_valid_without_allowing_diagnostics_to_move_backwards() {
        let uri = "file:///project/file.cpp";
        let mut snapshots = BTreeMap::from([(
            uri.into(),
            Diagnostics {
                version: 8,
                minimum_version: 7,
                raw: vec![],
                ready: false,
            },
        )]);
        let publish = |version, message| json!({"uri":uri,"version":version,"diagnostics":[{"message":message}]});
        publish_diagnostics(&mut snapshots, &publish(6, "stale"));
        publish_diagnostics(&mut snapshots, &publish(9, "future"));
        assert!(!snapshots[uri].ready);
        publish_diagnostics(&mut snapshots, &publish(7, "opening"));
        assert_eq!(snapshots[uri].raw[0]["message"], "opening");
        publish_diagnostics(&mut snapshots, &publish(8, "current"));
        publish_diagnostics(&mut snapshots, &publish(7, "late opening"));
        assert_eq!(snapshots[uri].raw[0]["message"], "current");
        snapshots.insert(
            uri.into(),
            Diagnostics {
                version: 10,
                minimum_version: 9,
                raw: vec![],
                ready: false,
            },
        );
        publish_diagnostics(&mut snapshots, &publish(8, "previous source"));
        assert!(!snapshots[uri].ready);
        publish_diagnostics(&mut snapshots, &publish(9, "reopened"));
        publish_diagnostics(&mut snapshots, &json!({"uri":uri,"diagnostics":[]}));
        publish_diagnostics(&mut snapshots, &publish(9, "late after versionless"));
        assert!(snapshots[uri].ready);
        assert!(snapshots[uri].raw.is_empty());
    }

    #[test]
    fn versionless_diagnostics_use_only_open_documents_and_explicit_versions_reject_stale_results()
    {
        let uri = "file:///project/file.ts";
        let mut snapshots = BTreeMap::from([(
            uri.into(),
            Diagnostics {
                version: 4,
                minimum_version: 4,
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
