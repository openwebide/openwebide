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
                let Some(uri)=message.params["uri"].as_str() else {continue;};
                let Some(version)=message.params["version"].as_i64().and_then(|version|i32::try_from(version).ok()) else {continue;};
                let Some(items)=message.params["diagnostics"].as_array().filter(|items|items.len()<=protocol::MAX_DIAGNOSTICS) else {continue;};
                let mut snapshots=diagnostics.lock().unwrap();
                if let Some(snapshot)=snapshots.get_mut(uri).filter(|snapshot|snapshot.version==version) {snapshot.raw=items.clone();snapshot.ready=true;}
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
