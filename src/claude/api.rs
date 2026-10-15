//! Direct Anthropic Messages API backend (API key billing, no Claude Code needed).
//! Uses streaming so long transcriptions don't hit HTTP timeouts.

use super::{Backend, Cancel, Part, Progress, Request, Response};
use crate::config::AppConfig;
use crate::model::project::Usage;
use crate::secrets::{self, Secret};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

const API_URL: &str = "https://api.anthropic.com/v1/messages";

pub struct ApiBackend {
    pub model: String,
    pub effort: String,
    pub timeout: Duration,
}

impl ApiBackend {
    pub fn from_config(cfg: &AppConfig) -> Self {
        Self { model: model_id(&cfg.model), effort: cfg.effort.clone(), timeout: Duration::from_secs(cfg.timeout_secs.max(60)) }
    }
}

/// Map the friendly aliases used in settings to API model ids.
pub fn model_id(alias: &str) -> String {
    match alias.trim().to_ascii_lowercase().as_str() {
        "" | "opus" => "claude-opus-5-5".into(),
        "fable" => "claude-fable-5-1".into(),
        "sonnet" => "claude-sonnet-5-5".into(),
        "haiku" => "claude-haiku-5-5".into(),
        other => other.to_string(),
    }
}

/// (input, output) USD per million tokens.
fn pricing(model: &str) -> (f64, f64) {
    if model.contains("fable") || model.contains("mythos") {
        (10.0, 50.0)
    } else if model.contains("opus-5-5") {
        (4.0, 20.0)
    } else if model.contains("opus") {
        (5.0, 25.0)
    } else if model.contains("sonnet") {
        (2.0, 10.0)
    } else if model.contains("haiku-5") {
        (0.10, 0.50)
    } else {
        (1.0, 5.0)
    }
}

/// Models that accept the server-side refusal fallback ("default" form).
fn supports_default_fallback(model: &str) -> bool {
    matches!(model, "claude-fable-5-1" | "claude-opus-5-5" | "claude-opus-5" | "claude-sonnet-5-5")
}

impl Backend for ApiBackend {
    fn name(&self) -> String {
        format!("Anthropic API ({})", self.model)
    }

    fn send(&self, req: &Request, cancel: &Cancel, progress: &mut dyn FnMut(Progress)) -> Result<Response> {
        let key = secrets::get(Secret::AnthropicApiKey)
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
            .ok_or_else(|| anyhow!("No Anthropic API key saved. Add one in Account settings."))?;

        let content: Vec<Value> = req
            .parts
            .iter()
            .map(|p| match p {
                Part::Text(t) => json!({"type": "text", "text": t}),
                Part::Image(img) => json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": img.media_type, "data": img.data_b64}
                }),
            })
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_output_tokens,
            "stream": true,
            "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": content}],
            "thinking": {"type": "adaptive"},
        });
        if !self.effort.is_empty() {
            body["output_config"] = json!({"effort": self.effort});
        }
        let use_fallback = supports_default_fallback(&self.model);
        if use_fallback {
            body["fallbacks"] = json!("default");
        }

        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(30)).timeout_read(Duration::from_secs(300)).build();
        let mut request =
            agent.post(API_URL).set("x-api-key", &key).set("anthropic-version", "2023-06-01").set("content-type", "application/json");
        if use_fallback {
            request = request.set("anthropic-beta", "server-side-fallback-2026-07-01");
        }
        progress(Progress::Status(format!("Sending pages to {}…", self.model)));
        let resp = match request.send_json(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or(text);
                bail!("Anthropic API error {code}: {msg}");
            }
            Err(e) => bail!("Network error: {e}"),
        };

        let started = Instant::now();
        let mut out = Response { model: self.model.clone(), ..Default::default() };
        let mut text = String::new();
        let mut usage_in = 0u64;
        let mut usage_out = 0u64;
        let mut last_report = Instant::now();
        let reader = BufReader::new(resp.into_reader());
        for line in reader.lines() {
            if cancel.is_cancelled() {
                bail!("Cancelled");
            }
            if started.elapsed() > self.timeout {
                bail!("Claude did not finish within {} seconds", self.timeout.as_secs());
            }
            let line = line?;
            let Some(data) = line.strip_prefix("data:") else { continue };
            let Ok(v) = serde_json::from_str::<Value>(data.trim()) else { continue };
            match v["type"].as_str() {
                Some("message_start") => {
                    let u = &v["message"]["usage"];
                    usage_in = u["input_tokens"].as_u64().unwrap_or(0)
                        + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                        + u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    if let Some(m) = v["message"]["model"].as_str() {
                        out.model = m.to_string();
                    }
                    progress(Progress::Status("Claude is reading the pages…".into()));
                }
                Some("content_block_start") => {
                    if v["content_block"]["type"] == "thinking" {
                        progress(Progress::Status("Claude is thinking…".into()));
                    } else if v["content_block"]["type"] == "text" {
                        progress(Progress::Status("Claude is writing the transcription…".into()));
                    }
                }
                Some("content_block_delta") => {
                    if v["delta"]["type"] == "text_delta" {
                        if let Some(t) = v["delta"]["text"].as_str() {
                            text.push_str(t);
                            if last_report.elapsed() > Duration::from_millis(250) {
                                progress(Progress::Streaming(text.len()));
                                last_report = Instant::now();
                            }
                        }
                    }
                }
                Some("message_delta") => {
                    if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                        out.stop_reason = sr.to_string();
                    }
                    if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                        usage_out = o;
                    }
                }
                Some("error") => {
                    bail!("Anthropic API error: {}", v["error"]["message"].as_str().unwrap_or("unknown"));
                }
                _ => {}
            }
        }
        if out.stop_reason == "refusal" {
            bail!("Claude declined this request.");
        }
        let (pi, po) = pricing(&out.model);
        out.usage = Usage {
            requests: 1,
            input_tokens: usage_in,
            output_tokens: usage_out,
            cost_usd: usage_in as f64 * pi / 1e6 + usage_out as f64 * po / 1e6,
        };
        out.text = text;
        if out.text.trim().is_empty() {
            bail!("Claude returned an empty answer (stop reason: {})", out.stop_reason);
        }
        Ok(out)
    }
}
