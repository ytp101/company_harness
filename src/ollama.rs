//! Native Ollama wire format; histories include assistant tool calls and tool replies.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: FunctionCall,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}
impl ChatMessage {
    pub fn text(role: &str, content: String) -> Self {
        Self {
            role: role.into(),
            content,
            tool_calls: vec![],
            tool_name: None,
            thinking: None,
        }
    }
}
#[derive(Clone)]
pub struct Ollama {
    client: reqwest::Client,
    endpoint: String,
}
impl Ollama {
    pub fn new(endpoint: &str) -> Result<Self, String> {
        let url = reqwest::Url::parse(endpoint).map_err(|e| e.to_string())?;
        if url.scheme() != "http"
            || !matches!(
                url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            )
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(
                "Use a local HTTP Ollama endpoint (localhost, 127.0.0.1, or [::1]).".into(),
            );
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            endpoint: endpoint.trim_end_matches('/').into(),
        })
    }
    async fn json_response(response: reqwest::Response) -> Result<Value, String> {
        let status = response.status();
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if bytes.len() + chunk.len() > 2_000_000 {
                return Err("Ollama response exceeded 2 MB limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(format!(
                "Ollama {status}: {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("Malformed Ollama JSON: {e}"))?;
        if let Some(error) = value.get("error") {
            return Err(format!("Ollama: {error}"));
        }
        Ok(value)
    }
    pub async fn models(&self) -> Result<Vec<String>, String> {
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            let response = self
                .client
                .get(format!("{}/api/tags", self.endpoint))
                .send()
                .await
                .map_err(connection_error)?;
            let data = Self::json_response(response).await?;
            let models = data["models"].as_array().ok_or("Missing models array")?;
            Ok::<_, String>(
                models
                    .iter()
                    .filter_map(|m| m["name"].as_str().map(str::to_owned))
                    .collect(),
            )
        })
        .await;
        result.map_err(|_| "Model discovery timed out. Is Ollama running?".to_string())?
    }
    pub async fn supports_tools(&self, model: &str) -> Result<bool, String> {
        reject_cloud_model(model)?;
        let response = self
            .client
            .post(format!("{}/api/show", self.endpoint))
            .json(&json!({"model":model}))
            .send()
            .await
            .map_err(connection_error)?;
        let data = Self::json_response(response).await?;
        if data
            .get("remote_host")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
            || data
                .get("remote_model")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        {
            return Err(
                "Remote-backed Ollama models are not supported; choose a downloaded local model."
                    .into(),
            );
        }
        Ok(data["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|v| v == "tools")))
    }
    pub async fn chat(
        &self,
        model: &str,
        history: &[ChatMessage],
        tools: Vec<Value>,
        format: Option<Value>,
    ) -> Result<ChatMessage, String> {
        reject_cloud_model(model)?;
        let mut body = json!({"model":model,"messages":history,"stream":false,"tools":tools,"options":{"num_predict":2048,"num_ctx":16384,"temperature":0.2},"keep_alive":"5m"});
        if let Some(format) = format {
            body["format"] = format;
        }
        let response = self
            .client
            .post(format!("{}/api/chat", self.endpoint))
            .json(&body)
            .send()
            .await
            .map_err(connection_error)?;
        let data = Self::json_response(response).await?;
        if data["done"] != true {
            return Err("Ollama returned an incomplete non-streaming response".into());
        }
        if data["done_reason"] == "length" {
            return Err(
                "Model hit the 2048-token response limit; try a smaller task or another model"
                    .into(),
            );
        }
        let message: ChatMessage = serde_json::from_value(data["message"].clone())
            .map_err(|e| format!("Malformed chat message: {e}"))?;
        if message.role != "assistant" {
            return Err("Expected an assistant message from Ollama".into());
        }
        if message.content.trim().is_empty() && message.tool_calls.is_empty() {
            return Err(
                "Model returned an empty response. Choose a compatible chat/tool model.".into(),
            );
        }
        Ok(message)
    }
}
fn reject_cloud_model(model: &str) -> Result<(), String> {
    if model.ends_with("-cloud") || model.contains(":cloud") {
        return Err("Choose a downloaded local model, not an Ollama cloud model.".into());
    }
    Ok(())
}
fn connection_error(e: reqwest::Error) -> String {
    format!("Cannot complete local Ollama request: {e}. Check `ollama serve` and the model name.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::server;
    #[test]
    fn only_loopback_endpoints_are_accepted() {
        for endpoint in [
            "https://localhost:11434",
            "http://example.com",
            "http://localhost.evil",
            "http://user:pass@localhost:11434",
        ] {
            assert!(Ollama::new(endpoint).is_err());
        }
        for endpoint in [
            "http://localhost:11434",
            "http://127.0.0.1:11434",
            "http://[::1]:11434",
        ] {
            assert!(Ollama::new(endpoint).is_ok());
        }
    }
    #[tokio::test]
    async fn discovery_and_response_validation() {
        let (url, task)=server(vec![
            (200,json!({"models":[{"name":"test-model"}]})),
            (200,json!({"done":true,"message":{"role":"assistant","content":""}})),
            (200,json!({"done":true,"message":{"role":"assistant","tool_calls":[{"function":{"name":"get_company_profile","arguments":"not an object"}}]}})),
            (500,json!({"error":"model crashed"})),
            (200,json!({"done":true,"done_reason":"length","message":{"role":"assistant","content":"partial"}})),
        ]).await;
        let client = Ollama::new(&url).unwrap();
        assert_eq!(client.models().await.unwrap(), vec!["test-model"]);
        assert!(
            client
                .chat("test", &[], vec![], None)
                .await
                .unwrap_err()
                .contains("empty response")
        );
        let response = client.chat("test", &[], vec![], None).await.unwrap();
        assert!(
            crate::tools::dispatch(
                crate::message::Role::Ceo,
                &response.tool_calls[0].function.name,
                &response.tool_calls[0].function.arguments
            )
            .is_err()
        );
        assert!(
            client
                .chat("test", &[], vec![], None)
                .await
                .unwrap_err()
                .contains("model crashed")
        );
        assert!(
            client
                .chat("test", &[], vec![], None)
                .await
                .unwrap_err()
                .contains("token response limit")
        );
        assert!(task.await.unwrap()[0].0.starts_with("GET /api/tags"));
    }
}
