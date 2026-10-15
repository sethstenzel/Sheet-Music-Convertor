//! Talking to Claude, either through the Claude Code CLI (subscription or API
//! key) or directly through the Anthropic Messages API.

pub mod api;
pub mod auth;
pub mod cli;
pub mod extract;
pub mod prompts;

use crate::config::{AppConfig, BackendKind};
use crate::model::project::Usage;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct ImageInput {
    pub media_type: String,
    pub data_b64: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub enum Part {
    Text(String),
    Image(ImageInput),
}

#[derive(Clone, Debug)]
pub struct Request {
    pub system: String,
    pub parts: Vec<Part>,
    /// Short label for logs ("transcribe", "proofread"…).
    pub label: String,
    pub max_output_tokens: u32,
}

#[derive(Clone, Debug, Default)]
pub struct RateLimit {
    /// Short description, e.g. "5-hour window 34% used".
    pub summary: String,
    pub status: String,
}

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub text: String,
    pub model: String,
    pub usage: Usage,
    pub rate_limit: Option<RateLimit>,
    pub stop_reason: String,
}

#[derive(Clone, Debug)]
pub enum Progress {
    /// Status line for the UI.
    Status(String),
    /// Number of characters of the answer received so far.
    Streaming(usize),
    RateLimit(RateLimit),
}

/// Cancellation shared between the UI and a running job. A job may run
/// several Claude processes at once; each registers its child process here.
#[derive(Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    children: Arc<Mutex<HashMap<u64, std::process::Child>>>,
    next_id: Arc<AtomicU64>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Ok(mut c) = self.children.lock() {
            for child in c.values_mut() {
                let _ = child.kill();
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Register a running child process; returns its handle id.
    pub(crate) fn add_child(&self, child: std::process::Child) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut c) = self.children.lock() {
            c.insert(id, child);
        }
        if self.is_cancelled() {
            let _ = self.with_child(id, |c| c.kill());
        }
        id
    }

    pub(crate) fn with_child<R>(&self, id: u64, f: impl FnOnce(&mut std::process::Child) -> R) -> Option<R> {
        self.children.lock().ok().and_then(|mut c| c.get_mut(&id).map(f))
    }

    pub(crate) fn take_child(&self, id: u64) -> Option<std::process::Child> {
        self.children.lock().ok().and_then(|mut c| c.remove(&id))
    }
}

pub trait Backend: Send + Sync {
    fn name(&self) -> String;
    fn send(&self, req: &Request, cancel: &Cancel, progress: &mut dyn FnMut(Progress)) -> Result<Response>;
}

/// Build the backend selected in the settings.
pub fn backend_from_config(cfg: &AppConfig) -> Box<dyn Backend> {
    match cfg.backend {
        BackendKind::ClaudeCodeSubscription | BackendKind::ClaudeCodeApiKey => Box::new(cli::CliBackend::from_config(cfg)),
        BackendKind::DirectApi => Box::new(api::ApiBackend::from_config(cfg)),
    }
}

/// Model choices offered in the UI: (value, label).
pub const MODEL_CHOICES: &[(&str, &str)] =
    &[("opus", "Opus (latest) — recommended"), ("fable", "Fable (most capable, slower)"), ("sonnet", "Sonnet (faster, lighter usage)")];

pub const EFFORT_CHOICES: &[&str] = &["low", "medium", "high", "xhigh", "max"];
