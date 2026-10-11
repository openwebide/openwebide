use openwebide_plugin_sdk::{Plugin, Tool, Outcome, serde_json::{self, json}};
struct Fixture;
impl Plugin for Fixture {
    fn events()->Vec<String> { vec!["job_due".into()] }
    fn event(input:openwebide_plugin_sdk::EventInput)->Result<Outcome,String> {
        let conversations:Option<serde_json::Value> = if input.payload.get("discover")==Some(&json!(true)) {
            Some(openwebide_plugin_sdk::collections("conversations",&json!({"action":"list"}))?)
        } else { None };
        let operation=json!({"collection":"events","operation":{"action":"create","value":{"event":input.name,"payload":input.payload,"conversations":conversations}}});
        let response:serde_json::Value=openwebide_plugin_sdk::request("records",&operation)?;
        Ok(Outcome {ok:true,content:response.to_string(),summary:"Plugin-owned event".into()})
    }
    fn context(input: openwebide_plugin_sdk::ContextInput) -> Result<openwebide_plugin_sdk::ContextContribution, String> {
        if input.budget_bytes == 4 {
            let _:serde_json::Value=openwebide_plugin_sdk::request("completion",&json!({"system_prompt":"x","prompt":"y","max_output_tokens":64}))?;
        }
        let action = if input.budget_bytes == 1 { "create" } else { "list" };
        let operation = if action == "create" { json!({"action":"create","value":{"text":"mutation attempt"}}) } else { json!({"action":"list"}) };
        let _: serde_json::Value = openwebide_plugin_sdk::request("records", &json!({"collection":"notes","operation":operation}))?;
        Ok(openwebide_plugin_sdk::ContextContribution {
            prompt: (input.budget_bytes != 3).then(|| "Stored fact".into()),
            disabled_tools: vec![if input.budget_bytes == 3 { "another_plugin_tool" } else { "fixture_echo" }.into()],
        })
    }
    fn tools() -> Vec<Tool> {
        vec![Tool {
            name: "fixture_echo".into(), description: "Exercise the public host contract.".into(),
            parameters: json!({"type":"object","properties":{}}), requires_approval: false,
        }]
    }
    fn execute(name: &str, arguments: serde_json::Value) -> Result<Outcome, String> {
        match name {
            "fixture_echo" => {
                let response: serde_json::Value = if let Some(operation) = arguments.get("runs") {
                    openwebide_plugin_sdk::runs(operation)?
                } else if let Some(operation) = arguments.get("collections") {
                    openwebide_plugin_sdk::collections(operation["collection"].as_str().ok_or("Missing collection")?,&operation["operation"])?
                } else if let Some(operation) = arguments.get("jobs") {
                    openwebide_plugin_sdk::jobs(operation)?
                } else {
                    openwebide_plugin_sdk::request("records", &arguments)?
                };
                Ok(Outcome {ok: true, content: response.to_string(), summary: "Host capability response".into()})
            }
            "fixture_panic" => panic!("fixture trap"),
            "fixture_completion" => {
                let input=serde_json::from_value(arguments).map_err(|error|error.to_string())?;
                let response=openwebide_plugin_sdk::complete(&input)?;
                Ok(Outcome {ok:true,content:response.text,summary:"Text completion".into()})
            }
            "fixture_loop" => loop { std::hint::black_box(1); },
            "fixture_large_input" => {
                let response = openwebide_plugin_sdk::HttpResponse {
                    status: 200, headers: Default::default(),
                    body_base64: arguments["encoded"].as_str().ok_or("Missing encoded input")?.into(),
                };
                let mut bytes = response.bytes()?;
                bytes.truncate(16_384);
                Ok(Outcome {ok: true, content: String::from_utf8(bytes).map_err(|error| error.to_string())?, summary: "Decoded bounded response".into()})
            }
            _ => Err("Unknown fixture tool".into()),
        }
    }
}
openwebide_plugin_sdk::export!(Fixture);
