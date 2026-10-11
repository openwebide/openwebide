//! Native HTTP and clock primitives, shared by every execution host.
use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use openwebide_plugin_runtime::sdk::{HttpRequest, HttpResponse};

pub async fn request(payload: &str) -> Result<String, String> {
    let input: HttpRequest = serde_json::from_str(payload).map_err(|error| error.to_string())?;
    if input.url.len() > 4096 || input.headers.len() > 32 || input.body_base64.len() > 512 * 1024 {
        return Err("Plugin HTTP request exceeds its limit.".into());
    }
    let uri = input
        .url
        .parse::<hyper::Uri>()
        .map_err(|_| "Invalid plugin HTTP URL")?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return Err("Plugin HTTP requests require HTTP or HTTPS.".into());
    }
    openwebide_core::network::check_http_host(uri.host().ok_or("HTTP URL has no host")?)?;
    if input.method.eq_ignore_ascii_case("CONNECT") {
        return Err("Plugin HTTP does not provide TCP tunnels".into());
    }
    let body = base64::engine::general_purpose::STANDARD
        .decode(input.body_base64)
        .map_err(|error| error.to_string())?;
    let mut request = hyper::Request::builder()
        .method(input.method.as_str())
        .uri(uri);
    for (name, value) in input.headers {
        if (name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("proxy-connection"))
            && value.split(',').any(|header| {
                header
                    .trim()
                    .eq_ignore_ascii_case(openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER)
            })
        {
            return Err("Plugin transport identity cannot be a hop-by-hop header".into());
        }
        if name.len() > 128 || value.len() > 8192 {
            return Err("Plugin HTTP header exceeds its limit.".into());
        }
        request = request.header(name, value);
    }
    let request = request
        .header(openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER, "1")
        .body(Full::new(Bytes::from(body)))
        .map_err(|error| error.to_string())?;
    let response = crate::runs::http_client::ReqwestHttpClient::default()
        .send(request)
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .take(64)
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .filter(|value| value.len() <= 8192)
                .map(|value| (name.to_string(), value.to_owned()))
        })
        .collect();
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| error.to_string())?;
        if let Ok(data) = frame.into_data() {
            let remaining = (2 * 1024 * 1024usize).saturating_sub(bytes.len());
            bytes.extend_from_slice(&data[..data.len().min(remaining)]);
            if bytes.len() == 2 * 1024 * 1024 {
                break;
            }
        }
    }
    serde_json::to_string(&HttpResponse {
        status,
        headers,
        body_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn plugin_http_cannot_bootstrap_or_use_host_control_credentials() {
        use openwebide_core::plugins::execution::PLUGIN_HTTP_HEADER;
        use openwebide_plugin_runtime::sdk::{HttpRequest, HttpResponse};
        for pairing in [None, Some("paired-token".into())] {
            let directory = tempfile::tempdir().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let secret: std::sync::Arc<str> =
                "this_is_a_very_long_dummy_secret_32_bytes_min".into();
            let config = crate::ServerConfig::new(directory.path().into(), secret.clone(), pairing);
            let server = tokio::spawn(crate::run_server(listener, config));
            for marker in [None, Some(""), Some("0")] {
                let mut headers = std::collections::BTreeMap::new();
                if let Some(value) = marker {
                    headers.insert(PLUGIN_HTTP_HEADER.into(), value.into());
                }
                headers.insert("Authorization".into(), format!("Bearer {secret}"));
                headers.insert("Content-Type".into(), "application/json".into());
                for path in ["/secret", "/exec", "/plugins/invoke", "/scheduler/host"] {
                    let response = super::request(
                        &serde_json::to_string(&HttpRequest {
                            url: format!("http://127.0.0.1:{port}{path}"),
                            method: "POST".into(),
                            headers: headers.clone(),
                            body_base64: String::new(),
                        })
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                    let response: HttpResponse = serde_json::from_str(&response).unwrap();
                    assert_eq!(response.status, 403, "{path} {marker:?}");
                    assert!(!response.text().unwrap().contains(secret.as_ref()));
                }
                let response = super::request(
                    &serde_json::to_string(&HttpRequest {
                        url: format!("http://127.0.0.1:{port}/health"),
                        method: "GET".into(),
                        headers,
                        body_base64: String::new(),
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
                assert_eq!(
                    serde_json::from_str::<HttpResponse>(&response)
                        .unwrap()
                        .status,
                    200
                );
            }
            let response = super::request(
                &serde_json::to_string(&HttpRequest {
                    url: format!("http://127.0.0.1:{port}/health"),
                    method: "GET".into(),
                    headers: std::collections::BTreeMap::from([
                        ("Connection".into(), "Upgrade".into()),
                        ("Upgrade".into(), "websocket".into()),
                        ("Sec-WebSocket-Version".into(), "13".into()),
                        (
                            "Sec-WebSocket-Key".into(),
                            "dGhlIHNhbXBsZSBub25jZQ==".into(),
                        ),
                    ]),
                    body_base64: String::new(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                serde_json::from_str::<HttpResponse>(&response)
                    .unwrap()
                    .status,
                403
            );
            server.abort();
        }
    }
    #[tokio::test]
    async fn plugin_http_cannot_remove_identity_at_a_proxy_or_create_tcp_tunnels() {
        for headers in [
            std::collections::BTreeMap::from([(
                "Connection".into(),
                "keep-alive, X-OpenWebide-Plugin".into(),
            )]),
            std::collections::BTreeMap::from([(
                "Proxy-Connection".into(),
                "x-openwebide-plugin".into(),
            )]),
        ] {
            let input = super::HttpRequest {
                url: "http://127.0.0.1:1/".into(),
                method: "GET".into(),
                headers,
                body_base64: String::new(),
            };
            assert!(
                super::request(&serde_json::to_string(&input).unwrap())
                    .await
                    .unwrap_err()
                    .contains("hop-by-hop")
            );
        }
        let input = super::HttpRequest {
            url: "http://127.0.0.1:1/".into(),
            method: "CONNECT".into(),
            headers: Default::default(),
            body_base64: String::new(),
        };
        assert!(
            super::request(&serde_json::to_string(&input).unwrap())
                .await
                .unwrap_err()
                .contains("tunnels")
        );
    }
}
