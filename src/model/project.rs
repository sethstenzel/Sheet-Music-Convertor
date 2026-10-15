//! Projects: a book (or single sheet) with the songs taken from its pages.
//!
//! A project lives in a folder:
//! ```text
//! MyHymnal/
//!   project.json
//!   images/      copies of the page photos (deduplicated by content hash)
//!   exports/     default export location
//! ```

use super::score::Song;
use super::validate::Issue;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const PROJECT_FILE: &str = "project.json";
pub const IMAGES_DIR: &str = "images";
const MAX_REVISIONS: usize = 30;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CollectionKind {
    /// A single song on one or more loose sheets.
    SoloSheet,
    #[default]
    Hymnal,
    /// Songbook, chorus book, choir collection…
    SongBook,
    Other,
}

impl CollectionKind {
    pub const ALL: [CollectionKind; 4] =
        [CollectionKind::SoloSheet, CollectionKind::Hymnal, CollectionKind::SongBook, CollectionKind::Other];

    pub fn label(self) -> &'static str {
        match self {
            CollectionKind::SoloSheet => "Solo sheet (single song)",
            CollectionKind::Hymnal => "Hymnal",
            CollectionKind::SongBook => "Song book / collection",
            CollectionKind::Other => "Other",
        }
    }

    pub fn is_book(self) -> bool {
        !matches!(self, CollectionKind::SoloSheet)
    }

    pub fn noun(self) -> &'static str {
        match self {
            CollectionKind::SoloSheet => "sheet",
            CollectionKind::Hymnal => "hymnal",
            CollectionKind::SongBook => "song book",
            CollectionKind::Other => "collection",
        }
    }
}

/// Information about the book the songs come from.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct BookInfo {
    pub title: String,
    pub subtitle: String,
    pub abbreviation: String,
    pub publisher: String,
    pub year: String,
    pub edition: String,
    pub editor: String,
    pub denomination: String,
    pub language: String,
    pub isbn: String,
    pub copyright: String,
    pub notes: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SongPage {
    /// File name inside the project's images folder.
    pub image: String,
    /// Original file path the image was imported from (informational).
    pub original_path: String,
    /// Page number printed in the book, if known.
    pub book_page: String,
    /// The top of this page holds the end of a different song: ignore it.
    pub ignore_top: bool,
    /// The bottom of this page holds the start of a different song: ignore it.
    pub ignore_bottom: bool,
    /// Clockwise rotation applied before processing: 0, 90, 180, 270.
    pub rotation: u16,
    /// Optional crop rectangle in normalized coordinates [x0, y0, x1, y1] (after rotation).
    pub crop: Option<[f32; 4]>,
    /// Improve contrast / convert to grayscale before sending.
    pub enhance: bool,
    /// Free-form hint for Claude about this page.
    pub note: String,
}

impl Default for SongPage {
    fn default() -> Self {
        Self {
            image: String::new(),
            original_path: String::new(),
            book_page: String::new(),
            ignore_top: false,
            ignore_bottom: false,
            rotation: 0,
            crop: None,
            enhance: true,
            note: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SongStatus {
    #[default]
    Draft,
    Queued,
    Working,
    NeedsReview,
    Reviewed,
    Failed,
}

impl SongStatus {
    pub fn label(self) -> &'static str {
        match self {
            SongStatus::Draft => "Draft",
            SongStatus::Queued => "Queued",
            SongStatus::Working => "Working…",
            SongStatus::NeedsReview => "Needs review",
            SongStatus::Reviewed => "Reviewed",
            SongStatus::Failed => "Failed",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            SongStatus::Draft => "○",
            SongStatus::Queued => "⏳",
            SongStatus::Working => "⟳",
            SongStatus::NeedsReview => "◐",
            SongStatus::Reviewed => "✔",
            SongStatus::Failed => "✖",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Revision {
    /// RFC3339 timestamp.
    pub at: String,
    pub label: String,
    pub song: Song,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Usage {
    pub requests: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.requests += other.requests;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cost_usd += other.cost_usd;
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SongEntry {
    pub id: String,
    /// Title / number hints typed by the user before transcription.
    pub title_hint: String,
    pub number_hint: String,
    pub pages: Vec<SongPage>,
    pub status: SongStatus,
    /// Instructions for Claude for the whole song.
    pub instructions: String,
    pub song: Option<Song>,
    pub history: Vec<Revision>,
    pub issues: Vec<Issue>,
    pub last_error: String,
    pub usage: Usage,
    /// Log lines from the last transcription run.
    pub log: Vec<String>,
}

impl Default for SongEntry {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title_hint: String::new(),
            number_hint: String::new(),
            pages: Vec::new(),
            status: SongStatus::Draft,
            instructions: String::new(),
            song: None,
            history: Vec::new(),
            issues: Vec::new(),
            last_error: String::new(),
            usage: Usage::default(),
            log: Vec::new(),
        }
    }
}

impl SongEntry {
    pub fn display_title(&self) -> String {
        if let Some(s) = &self.song {
            let t = s.display_title();
            if t != "Untitled" {
                return t;
            }
        }
        if !self.title_hint.trim().is_empty() {
            return self.title_hint.trim().to_string();
        }
        "New song".to_string()
    }

    pub fn display_number(&self) -> String {
        if let Some(s) = &self.song {
            if !s.meta.number.trim().is_empty() {
                return s.meta.number.trim().to_string();
            }
        }
        self.number_hint.trim().to_string()
    }

    /// Set a new current song, remembering the previous one in history.
    pub fn push_revision(&mut self, label: &str, song: Song) {
        self.history.push(Revision { at: chrono::Local::now().to_rfc3339(), label: label.to_string(), song: song.clone() });
        if self.history.len() > MAX_REVISIONS {
            let excess = self.history.len() - MAX_REVISIONS;
            self.history.drain(0..excess);
        }
        self.song = Some(song);
    }

    pub fn book_pages(&self) -> String {
        let pages: Vec<&str> = self.pages.iter().map(|p| p.book_page.trim()).filter(|p| !p.is_empty()).collect();
        match pages.len() {
            0 => String::new(),
            1 => pages[0].to_string(),
            _ => format!("{}–{}", pages[0], pages[pages.len() - 1]),
        }
    }
}

/// Source description handed to exporters.
#[derive(Clone, Debug, Default)]
pub struct SourceInfo {
    pub kind: CollectionKind,
    pub book: BookInfo,
    pub pages: String,
}

impl SourceInfo {
    /// One-line description, e.g. "Hymns of Faith (1950), p. 12".
    pub fn describe(&self) -> String {
        let mut s = String::new();
        if self.kind.is_book() && !self.book.title.is_empty() {
            s.push_str(&self.book.title);
            if !self.book.year.is_empty() {
                s.push_str(&format!(" ({})", self.book.year));
            }
            if !self.pages.is_empty() {
                s.push_str(&format!(", p. {}", self.pages));
            }
        }
        s
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Project {
    pub version: u32,
    pub name: String,
    pub kind: CollectionKind,
    pub book: BookInfo,
    pub songs: Vec<SongEntry>,
    /// Loose pages imported for automatic song detection.
    pub inbox: Vec<SongPage>,
    pub usage: Usage,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            version: 1,
            name: "Untitled project".into(),
            kind: CollectionKind::Hymnal,
            book: BookInfo::default(),
            songs: Vec::new(),
            inbox: Vec::new(),
            usage: Usage::default(),
            dir: PathBuf::new(),
        }
    }
}

impl Project {
    pub fn create(dir: &Path, name: &str, kind: CollectionKind) -> Result<Project> {
        std::fs::create_dir_all(dir.join(IMAGES_DIR)).with_context(|| format!("creating {}", dir.display()))?;
        let p = Project { name: name.to_string(), kind, dir: dir.to_path_buf(), ..Default::default() };
        p.save()?;
        Ok(p)
    }

    pub fn open(dir_or_file: &Path) -> Result<Project> {
        let dir =
            if dir_or_file.is_file() { dir_or_file.parent().map(Path::to_path_buf).unwrap_or_default() } else { dir_or_file.to_path_buf() };
        let text =
            std::fs::read_to_string(dir.join(PROJECT_FILE)).with_context(|| format!("reading {}", dir.join(PROJECT_FILE).display()))?;
        let mut p: Project = serde_json::from_str(&text).context("parsing project.json")?;
        p.dir = dir;
        // A crash mid-transcription leaves songs "working": reset them.
        for s in &mut p.songs {
            if matches!(s.status, SongStatus::Working | SongStatus::Queued) {
                s.status = if s.song.is_some() { SongStatus::NeedsReview } else { SongStatus::Draft };
            }
        }
        Ok(p)
    }

    pub fn save(&self) -> Result<()> {
        let path = self.dir.join(PROJECT_FILE);
        let tmp = self.dir.join("project.json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn images_dir(&self) -> PathBuf {
        self.dir.join(IMAGES_DIR)
    }

    pub fn image_path(&self, name: &str) -> PathBuf {
        self.images_dir().join(name)
    }

    pub fn exports_dir(&self) -> PathBuf {
        self.dir.join("exports")
    }

    /// Copy an image into the project (deduplicated by content hash) and
    /// return a new page referencing it.
    pub fn import_image(&self, src: &Path) -> Result<SongPage> {
        let bytes = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
        let hash = Sha256::digest(&bytes);
        let hex: String = hash.iter().take(10).map(|b| format!("{b:02x}")).collect();
        let ext = src.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_else(|| "jpg".into());
        let name = format!("{hex}.{ext}");
        let dest = self.image_path(&name);
        std::fs::create_dir_all(self.images_dir())?;
        if !dest.exists() {
            std::fs::write(&dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
        }
        Ok(SongPage { image: name, original_path: src.display().to_string(), ..Default::default() })
    }

    pub fn song(&self, id: &str) -> Option<&SongEntry> {
        self.songs.iter().find(|s| s.id == id)
    }

    pub fn song_mut(&mut self, id: &str) -> Option<&mut SongEntry> {
        self.songs.iter_mut().find(|s| s.id == id)
    }

    pub fn source_info(&self, entry: &SongEntry) -> SourceInfo {
        SourceInfo { kind: self.kind, book: self.book.clone(), pages: entry.book_pages() }
    }

    /// Sort songs by their number (numeric where possible), then title.
    pub fn sort_songs(&mut self) {
        fn key(s: &SongEntry) -> (u64, String, String) {
            let n = s.display_number();
            let digits: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
            (digits.parse().unwrap_or(u64::MAX), n, s.display_title())
        }
        self.songs.sort_by_key(key);
    }
}
