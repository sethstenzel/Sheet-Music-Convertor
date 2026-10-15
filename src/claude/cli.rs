//! Claude Code CLI backend.
//!
//! Runs `claude -p` in headless mode with `--input-format stream-json`, which
//! accepts image content blocks, so page photos are sent directly without
//! giving Claude any file-system tools. Authentication is whatever Claude Code
//! is signed in with (subscription), a stored `claude setup-token` token, or an
//! API key, depending on the selected backend.

use super::{Backend, Cancel, Part, Progress, RateLimit, Request, Response};
use crate::config::{AppConfig, BackendKind};
use crate::model::project::Usage;
use crate::secrets::{self, Secret};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct CliBackend {
    pub exe: Option<PathBuf>,
    pub kind: BackendKind,
    pub model: String,
    pub effort: String,
    pub timeout: Duration,
}

impl CliBackend {
    pub fn from_config(cfg: &AppConfig) -> Self {
        Self {
            exe: resolve_claude(&cfg.claude_path),
            kind: cfg.backend,
            model: cfg.model.clone(),
            effort: cfg.effort.clone(),
            timeout: Duration::from_secs(cfg.timeout_secs.max(60)),
        }
    }
}

#[cfg(windows)]
const EXE_NAMES: &[&str] = &["claude.exe", "claude.cmd", "claude.bat"];
#[cfg(not(windows))]
const EXE_NAMES: &[&str] = &["claude"];

/// Find the Claude Code executable: explicit setting, PATH, then the usual
/// install locations. npm's `claude.cmd` shim is resolved to the real binary
/// so arguments are not mangled by cmd.exe.
pub fn resolve_claude(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return Some(unwrap_shim(&p).unwrap_or(p));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in EXE_NAMES {
                candidates.push(dir.join(name));
            }
        }
    }
    if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
        for name in EXE_NAMES {
            candidates.push(home.join(".local").join("bin").join(name));
            candidates.push(home.join(".claude").join("local").join(name));
        }
        #[cfg(windows)]
        candidates.push(home.join("AppData").join("Roaming").join("npm").join("claude.cmd"));
    }
    #[cfg(not(windows))]
    {
        candidates.push(PathBuf::from("/opt/homebrew/bin/claude"));
        candidates.push(PathBuf::from("/usr/local/bin/claude"));
        candidates.push(PathBuf::from("/usr/bin/claude"));
    }
    candidates.into_iter().find(|p| p.is_file()).map(|p| unwrap_shim(&p).unwrap_or(p))
}

/// For a Windows `.cmd` shim (npm), find the executable it launches.
fn unwrap_shim(p: &Path) -> Option<PathBuf> {
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    if ext != "cmd" && ext != "bat" {
        return None;
    }
    let text = std::fs::read_to_string(p).ok()?;
    let dir = p.parent()?;
    for line in text.lines() {
        if let Some(start) = line.find("\"%dp0%\\") {
            let rest = &line[start + 7..];
            let end = rest.find('"')?;
            let rel = &rest[..end];
            if rel.to_ascii_lowercase().ends_with(".exe") {
                let full = dir.join(rel.trim_start_matches('\\'));
                if full.is_file() {
                    return Some(full);
                }
            }
        }
    }
    None
}

/// A `Command` for the Claude executable with the console window hidden.
pub fn command(exe: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Apply authentication environment for the chosen backend.
pub fn apply_auth_env(cmd: &mut Command, kind: BackendKind) -> Result<()> {
    match kind {
        BackendKind::ClaudeCodeSubscription => {
            // Make sure an API key in the environment doesn't silently take over billing.
            cmd.env_remove("ANTHROPIC_API_KEY");
            cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
            if let Some(tok) = secrets::get(Secret::ClaudeOauthToken) {
                cmd.env("CLAUDE_CODE_OAUTH_TOKEN", tok);
            }
        }
        BackendKind::ClaudeCodeApiKey => {
            // A saved key wins; otherwise Claude Code's own Console sign-in is used.
            if let Some(key) = secrets::get(Secret::AnthropicApiKey) {
                cmd.env("ANTHROPIC_API_KEY", key);
            }
            cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
        }
        BackendKind::DirectApi => {
            let key = secrets::get(Secret::AnthropicApiKey)
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .ok_or_else(|| anyhow!("No Anthropic API key saved. Add one in Account settings."))?;
            cmd.env("ANTHROPIC_API_KEY", key);
        }
    }
    Ok(())
}

fn content_json(parts: &[Part]) -> Value {
    let blocks: Vec<Value> = parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => json!({"type": "text", "text": t}),
            Part::Image(img) => json!({
                "type": "image",
                "source": {"type": "base64", "media_type": img.media_type, "data": img.data_b64}
            }),
        })
        .collect();
    Value::Array(blocks)
}

pub fn parse_rate_limit(v: &Value) -> Option<RateLimit> {
    let info = v.get("rate_limit_info")?;
    let status = info.get("status").and_then(Value::as_str).unwrap_or("").to_string();
    let mut parts = Vec::new();
    if let Some(w) = info.get("unifiedWindows").and_then(Value::as_object) {
        for (name, win) in w {
            if let Some(u) = win.get("utilization").and_then(Value::as_f64) {
                let label = match name.as_str() {
                    "five_hour" => "5-hour",
                    "seven_day" => "7-day",
                    other => other,
                };
                parts.push(format!("{label}: {:.0}% used", u * 100.0));
            }
        }
    }
    Some(RateLimit { summary: parts.join(" · "), status })
}

impl Backend for CliBackend {
    fn name(&self) -> String {
        format!("Claude Code ({})", self.model)
    }

    fn send(&self, req: &Request, cancel: &Cancel, progress: &mut dyn FnMut(Progress)) -> Result<Response> {
        let exe = self.exe.clone().ok_or_else(|| {
            anyhow!("Claude Code was not found. Install it (https://claude.com/claude-code) or set its path in Settings.")
        })?;
        let workdir = crate::config::claude_workdir();
        let sys_file = workdir.join(format!("system-{}.txt", uuid::Uuid::new_v4()));
        std::fs::write(&sys_file, &req.system).context("writing system prompt")?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(sys_file.clone());

        let mut cmd = command(&exe);
        cmd.current_dir(&workdir)
            .arg("-p")
            .args(["--input-format", "stream-json"])
            .args(["--output-format", "stream-json"])
            .arg("--verbose")
            .arg("--include-partial-messages")
            .args(["--model", &self.model])
            .args(["--tools", ""])
            .arg("--system-prompt-file")
            .arg(&sys_file)
            .args(["--setting-sources", ""])
            .arg("--strict-mcp-config")
            .arg("--no-session-persistence")
            .arg("--disable-slash-commands")
            .env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", req.max_output_tokens.to_string())
            .env("DISABLE_AUTOUPDATER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !self.effort.is_empty() {
            cmd.args(["--effort", &self.effort]);
        }
        apply_auth_env(&mut cmd, self.kind)?;

        progress(Progress::Status(format!("Starting Claude Code ({})…", self.model)));
        let mut child = cmd.spawn().with_context(|| format!("starting {}", exe.display()))?;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let mut stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
        let child_id = cancel.add_child(child);

        let message = json!({
            "type": "user",
            "message": {"role": "user", "content": content_json(&req.parts)}
        });
        let line = serde_json::to_string(&message)? + "\n";
        // Write stdin on a thread so a large payload can't deadlock with stdout.
        let writer = std::thread::spawn(move || -> std::io::Result<()> {
            stdin.write_all(line.as_bytes())?;
            stdin.flush()?;
            drop(stdin);
            Ok(())
        });
        let err_reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });

        // Watchdog for the timeout.
        let started = Instant::now();
        let timeout = self.timeout;
        let watchdog_cancel = cancel.clone();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done_w = done.clone();
        let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let timed_out_w = timed_out.clone();
        std::thread::spawn(move || {
            while !done_w.load(std::sync::atomic::Ordering::SeqCst) {
                if started.elapsed() > timeout {
                    timed_out_w.store(true, std::sync::atomic::Ordering::SeqCst);
                    let _ = watchdog_cancel.with_child(child_id, |c| c.kill());
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });

        let mut text_so_far = String::new();
        let mut final_text: Option<String> = None;
        let mut result_error: Option<String> = None;
        let mut response = Response::default();
        let mut last_report = Instant::now();

        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            match v.get("type").and_then(Value::as_str) {
                Some("system") => {
                    if v.get("subtype").and_then(Value::as_str) == Some("init") {
                        if let Some(m) = v.get("model").and_then(Value::as_str) {
                            response.model = m.to_string();
                            progress(Progress::Status(format!("Claude ({m}) is reading the pages…")));
                        }
                    }
                }
                Some("stream_event") => {
                    let ev = &v["event"];
                    if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                        if let Some(t) = ev["delta"]["text"].as_str() {
                            text_so_far.push_str(t);
                            if last_report.elapsed() > Duration::from_millis(250) {
                                progress(Progress::Streaming(text_so_far.len()));
                                last_report = Instant::now();
                            }
                        }
                    } else if ev["type"] == "content_block_start" && ev["content_block"]["type"] == "thinking" {
                        progress(Progress::Status("Claude is thinking…".into()));
                    } else if ev["type"] == "content_block_start" && ev["content_block"]["type"] == "text" {
                        progress(Progress::Status("Claude is writing the transcription…".into()));
                    }
                }
                Some("assistant") => {
                    if let Some(m) = v["message"]["model"].as_str() {
                        response.model = m.to_string();
                    }
                }
                Some("rate_limit_event") => {
                    if let Some(rl) = parse_rate_limit(&v) {
                        progress(Progress::RateLimit(rl.clone()));
                        response.rate_limit = Some(rl);
                    }
                }
                Some("result") => {
                    let is_error = v["is_error"].as_bool().unwrap_or(false);
                    let subtype = v["subtype"].as_str().unwrap_or("");
                    if let Some(r) = v["result"].as_str() {
                        final_text = Some(r.to_string());
                    }
                    if is_error || subtype.starts_with("error") {
                        result_error = Some(
                            v["result"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| v["errors"].as_array().map(|a| format!("{a:?}")))
                                .unwrap_or_else(|| subtype.to_string()),
                        );
                    }
                    response.stop_reason = v["stop_reason"].as_str().unwrap_or("").to_string();
                    let u = &v["usage"];
                    response.usage = Usage {
                        requests: 1,
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0)
                            + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                            + u["cache_read_input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
                    };
                }
                _ => {}
            }
        }

        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = writer.join();
        let status = cancel.take_child(child_id).map(|mut c| c.wait());
        let stderr_text = err_reader.join().unwrap_or_default();

        if cancel.is_cancelled() {
            bail!("Cancelled");
        }
        if timed_out.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("Claude did not finish within {} seconds (raise the timeout in Settings)", timeout.as_secs());
        }
        if let Some(err) = result_error {
            bail!("{}", friendly_error(&err));
        }
        let text = final_text.unwrap_or(text_so_far);
        if text.trim().is_empty() {
            let code = status.and_then(|s| s.ok()).and_then(|s| s.code());
            let detail = stderr_text.trim();
            bail!(
                "Claude Code returned no answer{}{}",
                code.map(|c| format!(" (exit code {c})")).unwrap_or_default(),
                if detail.is_empty() { String::new() } else { format!(": {}", friendly_error(detail)) }
            );
        }
        response.text = text;
        Ok(response)
    }
}

fn friendly_error(e: &str) -> String {
    let lower = e.to_ascii_lowercase();
    if lower.contains("not logged in")
        || lower.contains("/login")
        || lower.contains("invalid api key")
        || lower.contains("oauth") && lower.contains("expired")
    {
        format!("{e}\n\nSign in again from the Account screen.")
    } else if lower.contains("rate limit") || lower.contains("usage limit") || lower.contains("limit reached") {
        format!("{e}\n\nYour Claude usage limit was reached. Try again after it resets, or switch to an API key in Account.")
    } else {
        e.to_string()
    }
}

/// `claude --version`
pub fn version(exe: &Path) -> Result<String> {
    let out = command(exe).arg("--version").output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
