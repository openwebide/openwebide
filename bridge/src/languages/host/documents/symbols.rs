use super::*;
use protocol::results::WorkspaceSymbol;

const MAX_SYMBOLS: usize = 256;
const MAX_SYMBOL_BYTES: usize = 256 * 1024;

impl Documents {
    pub(super) async fn workspace_symbols(
        &mut self,
        session: &Session,
        input: &Request,
        sources: &mut BTreeMap<PathBuf, Arc<str>>,
        result: Value,
    ) -> Result<Value, String> {
        let values = if result.is_null() {
            vec![]
        } else {
            result
                .as_array()
                .cloned()
                .ok_or("Invalid workspace symbol list")?
        };
        if values.len() > MAX_SYMBOLS
            || serde_json::to_vec(&values)
                .map_err(|error| error.to_string())?
                .len()
                > MAX_SYMBOL_BYTES
        {
            return Err("Too many workspace symbols; narrow the search".into());
        }
        let document = self
            .documents
            .get_mut(&input.document)
            .ok_or("Language document is closed")?;
        document.symbols.clear();
        let mut symbols = Vec::new();
        for value in values {
            let name = value["name"]
                .as_str()
                .filter(|name| {
                    !name.is_empty() && name.len() <= 4096 && !name.chars().any(char::is_control)
                })
                .ok_or("Invalid workspace symbol name")?
                .to_owned();
            let kind = value["kind"]
                .as_u64()
                .and_then(|kind| u32::try_from(kind).ok())
                .filter(|kind| (1..=26).contains(kind))
                .ok_or("Invalid workspace symbol kind")?;
            let container = value["containerName"].as_str().map(str::to_owned);
            if container
                .as_ref()
                .is_some_and(|name| name.len() > 4096 || name.chars().any(char::is_control))
            {
                return Err("Invalid workspace symbol container".into());
            }
            let (target, id) = if value["location"]["range"].is_object() {
                (
                    Some(
                        results::target(
                            session,
                            &mut document.resources,
                            sources,
                            &value["location"],
                        )
                        .await?,
                    ),
                    None,
                )
            } else {
                if session
                    .capabilities
                    .pointer("/workspaceSymbolProvider/resolveProvider")
                    .and_then(Value::as_bool)
                    != Some(true)
                    || value["location"]["uri"]
                        .as_str()
                        .is_none_or(|uri| uri.len() > 4096 || url::Url::parse(uri).is_err())
                {
                    return Err("Workspace symbol has no supported location".into());
                }
                let id = format!("{:032x}", rand::random::<u128>());
                document.symbols.insert(id.clone(), value);
                (None, Some(id))
            };
            symbols.push(WorkspaceSymbol {
                name,
                kind,
                container,
                target,
                id,
            });
        }
        serde_json::to_value(symbols).map_err(|error| error.to_string())
    }
}
