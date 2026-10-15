//! Application settings stored in the user's config directory.

use crate::export::ExportSettings;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BackendKind {
    /// Claude Code CLI signed in with a Claude subscription (Pro / Max / Team / Enterprise).
    #[default]
    ClaudeCodeSubscription,
    /// Claude Code CLI billed to an Anthropic Console API key.
    ClaudeCodeApiKey,
    /// Call the Anthropic Messages API directly with an API key (Claude Code not needed).
    DirectApi,
}

impl BackendKind {
    pub const ALL: [BackendKind; 3] = [BackendKind::ClaudeCodeSubscription, BackendKind::ClaudeCodeApiKey, BackendKind::DirectApi];

    pub fn label(self) -> &'static str {
        match self {
            BackendKind::ClaudeCodeSubscription => "Claude subscription (via Claude Code)",
            BackendKind::ClaudeCodeApiKey => "API key (via Claude Code)",
            BackendKind::DirectApi => "API key (direct Anthropic API)",
        }
    }

    pub fn uses_cli(self) -> bool {
        !matches!(self, BackendKind::DirectApi)
    }

    pub fn uses_api_key(self) -> bool {
        !matches!(self, BackendKind::ClaudeCodeSubscription)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Accuracy {
    /// One transcription pass plus automatic fixes for detected errors.
    Standard,
    /// Adds an independent proofreading pass against the images.
    #[default]
    High,
    /// Two independent transcriptions (full page and zoomed tiles) whose
    /// disagreements are settled against the image, then a proofreading pass.
    Maximum,
}

impl Accuracy {
    pub const ALL: [Accuracy; 3] = [Accuracy::Standard, Accuracy::High, Accuracy::Maximum];

    pub fn label(self) -> &'static str {
        match self {
            Accuracy::Standard => "Standard — transcribe + auto-fix",
            Accuracy::High => "High — adds a proofreading pass with zoomed detail",
            Accuracy::Maximum => "Maximum — two independent readings compared + proofreading",
        }
    }

    pub fn proofread_passes(self) -> u32 {
        match self {
            Accuracy::Standard => 0,
            Accuracy::High => 1,
            Accuracy::Maximum => 1,
        }
    }

    /// Zoomed tiles in the first transcription request.
    pub fn detail_in_transcription(self) -> bool {
        matches!(self, Accuracy::Maximum)
    }

    /// Zoomed tiles in proofreading and correction requests.
    pub fn detail_in_review(self) -> bool {
        !matches!(self, Accuracy::Standard)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VoiceLayout {
    /// Let Claude pick: SATB for hymns, keyboard for piano music, melody for lead sheets.
    #[default]
    Auto,
    /// Separate S/A/T/B voices (best for playback, editing and part practice).
    Satb,
    /// Piano / keyboard score: right and left hand, chords allowed.
    Keyboard,
    /// Melody line only (with lyrics and chord symbols).
    MelodyOnly,
}

impl VoiceLayout {
    pub const ALL: [VoiceLayout; 4] = [VoiceLayout::Auto, VoiceLayout::Satb, VoiceLayout::Keyboard, VoiceLayout::MelodyOnly];

    pub fn label(self) -> &'static str {
        match self {
            VoiceLayout::Auto => "Automatic (SATB for hymns, keyboard for piano music)",
            VoiceLayout::Satb => "Separate SATB voices",
            VoiceLayout::Keyboard => "Keyboard (right hand / left hand)",
            VoiceLayout::MelodyOnly => "Melody, lyrics and chord symbols only",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AppConfig {
    pub backend: BackendKind,
    /// Path to the `claude` executable; empty = auto-detect.
    pub claude_path: String,
    /// Model alias ("opus", "sonnet", "fable") or full model id.
    pub model: String,
    /// Reasoning effort: low, medium, high, xhigh, max.
    pub effort: String,
    pub accuracy: Accuracy,
    pub voice_layout: VoiceLayout,
    /// Max automatic correction rounds when the validator finds errors.
    pub max_fix_rounds: u32,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
    /// Longest image side sent to Claude, in pixels.
    pub max_image_dim: u32,
    /// Songs transcribed at the same time.
    pub parallel_jobs: u32,
    /// Path to MuseScore; empty = auto-detect.
    pub musescore_path: String,
    pub export: ExportSettings,
    pub recent_projects: Vec<PathBuf>,
    pub dark_mode: bool,
    pub ui_scale: f32,
    /// Show the welcome/sign-in guide on start.
    pub first_run_done: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::ClaudeCodeSubscription,
            claude_path: String::new(),
            model: "opus".into(),
            effort: "high".into(),
            accuracy: Accuracy::High,
            voice_layout: VoiceLayout::Auto,
            max_fix_rounds: 2,
            timeout_secs: 900,
            max_image_dim: 2000,
            parallel_jobs: 1,
            musescore_path: String::new(),
            export: ExportSettings::default(),
            recent_projects: Vec::new(),
            dark_mode: true,
            ui_scale: 1.0,
            first_run_done: false,
        }
    }
}

pub fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("org", "SheetMusicConvertor", "Sheet Music Convertor")
}

pub fn config_dir() -> PathBuf {
    project_dirs().map(|d| d.config_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    project_dirs().map(|d| d.data_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

/// Empty working directory for Claude Code runs, so no project CLAUDE.md or
/// settings are picked up.
pub fn claude_workdir() -> PathBuf {
    let d = data_dir().join("claude-workdir");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn default_projects_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.document_dir().map(|d| d.join("Sheet Music Convertor")))
        .unwrap_or_else(|| data_dir().join("projects"))
}

impl AppConfig {
    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> AppConfig {
        std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(Self::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn add_recent(&mut self, p: PathBuf) {
        self.recent_projects.retain(|x| x != &p);
        self.recent_projects.insert(0, p);
        self.recent_projects.truncate(10);
    }
}
