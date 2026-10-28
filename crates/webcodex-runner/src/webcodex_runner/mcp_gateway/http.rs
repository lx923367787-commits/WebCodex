//! Streamable HTTP transport. No redirects, credential forwarding, or replay.
use super::*;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

pub(super) struct HttpConnection {
    client: Client,
    url: String,
    headers: HeaderMap,
    session: Option<HeaderValue>,
    next_id: u64,
}

impl HttpConnection {
    pub(super) fn connect(
        config: &McpGatewayProviderConfig,
        timeout: Duration,
    ) -> Result<Self, ProviderFailure> {
        let started = Instant::now();
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| ProviderFailure::before_send("provider_http_client_failed"))?;
        let mut headers = HeaderMap::new();
        for (name, value) in &config.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| ProviderFailure::before_send("provider_headers_invalid"))?;
            let mut value = HeaderValue::from_str(value)
                .map_err(|_| ProviderFailure::before_send("provider_headers_invalid"))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        let mut connection = Self {
            client,
            url: config.url.clone().unwrap_or_default(),
            headers,
            session: None,
            next_id: 1,
        };
        let initialized = connection.request("initialize", json!({
            "protocolVersion": MCP_PROTOCOL_VERSION, "capabilities": {},
            "clientInfo": {"name": "webcodex-runner-mcp-gateway", "version": env!("CARGO_PKG_VERSION")}
        }), timeout)?;
        validate_initialize_result(&initialized)?;
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|time| !time.is_zero())
            .ok_or_else(|| ProviderFailure::before_send("provider_timeout"))?;
        let response = connection.post(
            json!({"jsonrpc":"2.0", "method":"notifications/initialized", "params":{}}),
            remaining,
        )?;
        if !response.status().is_success() {
            return Err(ProviderFailure::before_send("provider_http_status"));
        }
        Ok(connection)
    }

    fn post(&self, message: Value, timeout: Duration) -> Result<Response, ProviderFailure> {
        let encoded = serde_json::to_vec(&message)
            .map_err(|_| ProviderFailure::before_send("provider_request_invalid"))?;
        if encoded.len() > MCP_GATEWAY_MAX_MESSAGE_BYTES {
            return Err(ProviderFailure::before_send("provider_request_too_large"));
        }
        let mut request = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
            .timeout(timeout)
            .body(encoded);
        if let Some(session) = &self.session {
            request = request.header("Mcp-Session-Id", session.clone());
        }
        request.send().map_err(|error| {
            if error.is_connect() {
                ProviderFailure::before_send("provider_http_connect_failed")
            } else {
                ProviderFailure::after_send(if error.is_timeout() {
                    "provider_timeout"
                } else {
                    "provider_http_failed"
                })
            }
        })
    }

    pub(super) fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ProviderFailure> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut response = self.post(
            json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
            timeout,
        )?;
        if !response.status().is_success() {
            // No response is replayed, including a server-expired session (404).
            return Err(ProviderFailure::after_send(
                match response.status().as_u16() {
                    401 | 403 => "provider_auth_required",
                    404 => "provider_http_session_unavailable",
                    _ => "provider_http_status",
                },
            ));
        }
        if method == "initialize" {
            if let Some(session) = response.headers().get("mcp-session-id") {
                if session.as_bytes().is_empty()
                    || session.as_bytes().len() > 256
                    || !session
                        .as_bytes()
                        .iter()
                        .all(|byte| (0x21..=0x7e).contains(byte))
                {
                    return Err(ProviderFailure::after_send("provider_http_session_invalid"));
                }
                let mut session = session.clone();
                session.set_sensitive(true);
                self.session = Some(session);
            }
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        if content_type.eq_ignore_ascii_case("text/event-stream") {
            return read_sse_response(response, id);
        }
        if !content_type.eq_ignore_ascii_case("application/json") {
            return Err(ProviderFailure::after_send("provider_http_content_type"));
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take((MCP_GATEWAY_MAX_MESSAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ProviderFailure::after_send("provider_http_read_failed"))?;
        if bytes.len() > MCP_GATEWAY_MAX_MESSAGE_BYTES {
            return Err(ProviderFailure::after_send("provider_message_too_large"));
        }
        let value = serde_json::from_slice(&bytes)
            .map_err(|_| ProviderFailure::after_send("provider_malformed_json"))?;
        validate_rpc_response(value, id)
    }
}

fn read_sse_response(response: impl Read, expected_id: u64) -> Result<Value, ProviderFailure> {
    let mut reader = BufReader::new(response);
    let mut data = Vec::new();
    let mut total = 0usize;
    let mut notifications = 0usize;
    loop {
        let mut line = Vec::new();
        let count = reader
            .by_ref()
            .take((MCP_GATEWAY_MAX_MESSAGE_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .map_err(|_| ProviderFailure::after_send("provider_http_read_failed"))?;
        total = total.saturating_add(count);
        if total > MCP_GATEWAY_MAX_MESSAGE_BYTES {
            return Err(ProviderFailure::after_send("provider_message_too_large"));
        }
        if count == 0 {
            return Err(ProviderFailure::after_send("provider_eof"));
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            if data.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_slice(&data)
                .map_err(|_| ProviderFailure::after_send("provider_malformed_json"))?;
            data.clear();
            if is_provider_notification(&value) {
                notifications += 1;
                if notifications > MCP_GATEWAY_MAX_IGNORED_NOTIFICATIONS {
                    return Err(ProviderFailure::after_send("provider_notification_flood"));
                }
                continue;
            }
            return validate_rpc_response(value, expected_id);
        }
        if let Some(value) = line.strip_prefix(b"data:") {
            if !data.is_empty() {
                data.push(b'\n');
            }
            data.extend_from_slice(value.strip_prefix(b" ").unwrap_or(value));
        }
    }
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
