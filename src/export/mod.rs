//! Exporters: MusicXML (opened natively by MuseScore), MuseScore .mscz/.pdf via
//! the MuseScore command line, MIDI, lyrics text and book catalogs.

pub mod catalog;
pub mod lyrics;
pub mod midi;
pub mod musescore;
pub mod musicxml;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ScoreLayout {
    /// Two staves (treble + bass) with SATB as two voices per staff, as printed in hymnals.
    #[default]
    Hymn,
    /// One staff per voice (choir "open score"): S, A, T (treble 8vb), B.
    OpenScore,
}

impl ScoreLayout {
    pub fn label(self) -> &'static str {
        match self {
            ScoreLayout::Hymn => "Hymn (2 staves, as printed)",
            ScoreLayout::OpenScore => "Open score (one staff per voice)",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct MusicXmlOptions {
    pub layout: ScoreLayout,
    /// Prefix the first syllable of each verse with "1.", "2." …
    pub verse_numbers: bool,
    /// Keep the system (line) breaks found on the printed page.
    pub keep_line_breaks: bool,
    /// Add title/composer/lyricist/copyright credits to the page.
    pub credits: bool,
    /// Write text-only stanzas below the music.
    pub extra_stanzas: bool,
}

impl Default for MusicXmlOptions {
    fn default() -> Self {
        Self { layout: ScoreLayout::Hymn, verse_numbers: true, keep_line_breaks: true, credits: true, extra_stanzas: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VerseMode {
    /// Play the music once.
    Once,
    /// Play the music once per verse (hymn accompaniment).
    #[default]
    AllVerses,
    /// Play a fixed number of times.
    Times(u32),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct MidiOptions {
    /// General MIDI program (0 = Acoustic Grand Piano, 19 = Church Organ).
    pub program: u8,
    /// Override the tempo (quarter notes per minute). None = use the song's tempo.
    pub tempo_bpm: Option<f32>,
    pub verses: VerseMode,
    /// Play the last N measures as an introduction before verse 1 (0 = none).
    pub intro_measures: u32,
    /// Hold fermatas this many times longer (1.0 = ignore fermatas).
    pub fermata_factor: f32,
    /// Slow down over the last two measures of the final verse.
    pub final_ritard: bool,
    /// Voice id to bring out (louder) for part practice; others softer.
    pub emphasize_voice: Option<String>,
    /// Voice ids to leave out entirely.
    pub mute_voices: Vec<String>,
    /// Base note velocity 1..=127.
    pub velocity: u8,
    /// Insert a one-measure count-in click (GM percussion) before the music.
    pub count_in: bool,
    /// Expand repeats and voltas.
    pub expand_repeats: bool,
}

impl Default for MidiOptions {
    fn default() -> Self {
        Self {
            program: 0,
            tempo_bpm: None,
            verses: VerseMode::AllVerses,
            intro_measures: 0,
            fermata_factor: 1.75,
            final_ritard: true,
            emphasize_voice: None,
            mute_voices: Vec::new(),
            velocity: 80,
            count_in: false,
            expand_repeats: true,
        }
    }
}

/// Common General MIDI instruments for hymn playback.
pub const GM_INSTRUMENTS: &[(u8, &str)] = &[
    (0, "Acoustic Grand Piano"),
    (1, "Bright Acoustic Piano"),
    (4, "Electric Piano"),
    (6, "Harpsichord"),
    (19, "Church Organ"),
    (16, "Drawbar Organ"),
    (20, "Reed Organ"),
    (48, "String Ensemble"),
    (52, "Choir Aahs"),
    (53, "Voice Oohs"),
    (24, "Nylon Guitar"),
    (46, "Harp"),
    (56, "Trumpet"),
    (73, "Flute"),
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ExportSettings {
    pub musicxml: bool,
    pub mscz: bool,
    pub pdf: bool,
    pub midi: bool,
    /// One extra MIDI file per voice with that voice emphasized (choir practice).
    pub midi_parts: bool,
    pub lyrics_txt: bool,
    /// Transpose by this many semitones (0 = original key).
    pub transpose_semitones: i32,
    pub xml: MusicXmlOptions,
    pub midi_opts: MidiOptions,
    /// File name pattern: {number}, {title}, {book}
    pub filename_pattern: String,
    /// Empty = project's exports folder.
    pub output_dir: String,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            musicxml: true,
            mscz: true,
            pdf: false,
            midi: true,
            midi_parts: false,
            lyrics_txt: false,
            transpose_semitones: 0,
            xml: MusicXmlOptions::default(),
            midi_opts: MidiOptions::default(),
            filename_pattern: "{number} {title}".into(),
            output_dir: String::new(),
        }
    }
}

/// Make a string safe to use as a file name.
pub fn sanitize_filename(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "song".into()
    } else {
        collapsed.chars().take(120).collect()
    }
}

pub fn file_stem(pattern: &str, number: &str, title: &str, book: &str) -> String {
    let s = pattern.replace("{number}", number).replace("{title}", title).replace("{book}", book);
    sanitize_filename(s.trim_matches(|c: char| c == '-' || c.is_whitespace()))
}

use crate::model::notation::ParsedSong;
use crate::model::project::{Project, SongEntry, SourceInfo};
use crate::model::score::Song;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct ExportResult {
    pub files: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// Parse a song for export, applying transposition.
pub fn prepare(song: &Song, transpose_semitones: i32) -> ParsedSong {
    let (parsed, _) = crate::model::validate::validate(song);
    if transpose_semitones != 0 {
        crate::model::transpose::transpose(&parsed, transpose_semitones)
    } else {
        parsed
    }
}

pub fn output_dir(project: &Project, settings: &ExportSettings) -> PathBuf {
    if settings.output_dir.trim().is_empty() {
        project.exports_dir()
    } else {
        PathBuf::from(settings.output_dir.trim())
    }
}

pub fn song_file_stem(project: &Project, entry: &SongEntry, settings: &ExportSettings) -> String {
    let book = if project.book.abbreviation.is_empty() { project.book.title.clone() } else { project.book.abbreviation.clone() };
    let number = entry.display_number();
    let pattern = if number.is_empty() { settings.filename_pattern.replace("{number}", "") } else { settings.filename_pattern.clone() };
    file_stem(&pattern, &number, &entry.display_title(), &book)
}

/// Export one song in every format selected in `settings`.
pub fn export_song(project: &Project, entry: &SongEntry, settings: &ExportSettings, musescore_exe: Option<&Path>) -> Result<ExportResult> {
    let song = entry.song.as_ref().ok_or_else(|| anyhow!("\"{}\" has not been transcribed yet", entry.display_title()))?;
    let parsed = prepare(song, settings.transpose_semitones);
    let source: SourceInfo = project.source_info(entry);
    let dir = output_dir(project, settings);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut stem = song_file_stem(project, entry, settings);
    if settings.transpose_semitones != 0 {
        stem = format!("{stem} ({})", parsed.key.name());
    }
    let mut res = ExportResult::default();

    let xml_path = dir.join(format!("{stem}.musicxml"));
    let needs_xml = settings.musicxml || settings.mscz || settings.pdf;
    if needs_xml {
        let xml = musicxml::write_musicxml(&parsed, &settings.xml, &source);
        std::fs::write(&xml_path, xml).with_context(|| format!("writing {}", xml_path.display()))?;
        if settings.musicxml {
            res.files.push(xml_path.clone());
        }
    }
    for (wanted, ext) in [(settings.mscz, "mscz"), (settings.pdf, "pdf")] {
        if !wanted {
            continue;
        }
        match musescore_exe {
            Some(ms) => {
                let out = dir.join(format!("{stem}.{ext}"));
                match musescore::convert(ms, &xml_path, &out) {
                    Ok(()) => res.files.push(out),
                    Err(e) => res.warnings.push(format!("{e:#}")),
                }
            }
            None => {
                res.warnings.push(format!("MuseScore was not found, so no .{ext} file was made (the .musicxml file opens in MuseScore)."))
            }
        }
    }
    if needs_xml && !settings.musicxml {
        let _ = std::fs::remove_file(&xml_path);
    }
    if settings.midi {
        let p = dir.join(format!("{stem}.mid"));
        std::fs::write(&p, midi::write_midi(&parsed, &settings.midi_opts))?;
        res.files.push(p);
    }
    if settings.midi_parts {
        for v in &parsed.voices {
            let mut opts = settings.midi_opts.clone();
            opts.emphasize_voice = Some(v.id.clone());
            let name = if v.name.is_empty() { v.id.clone() } else { v.name.clone() };
            let p = dir.join(format!("{stem} - {} part.mid", sanitize_filename(&name)));
            std::fs::write(&p, midi::write_midi(&parsed, &opts))?;
            res.files.push(p);
        }
    }
    if settings.lyrics_txt {
        let p = dir.join(format!("{stem}.txt"));
        std::fs::write(&p, lyrics::lyrics_text(&parsed, &lyrics::LyricsOptions::default()))?;
        res.files.push(p);
    }
    Ok(res)
}

/// Render notation preview pages (PNG) with MuseScore into `dir`.
pub fn render_preview(
    parsed: &ParsedSong,
    source: &SourceInfo,
    xml_opts: &MusicXmlOptions,
    musescore_exe: &Path,
    dir: &Path,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
    let xml = dir.join("preview.musicxml");
    std::fs::write(&xml, musicxml::write_musicxml(parsed, xml_opts, source))?;
    let png = dir.join("preview.png");
    musescore::convert(musescore_exe, &xml, &png)?;
    let pages = musescore::png_pages(&png);
    if pages.is_empty() {
        return Err(anyhow!("MuseScore produced no preview image"));
    }
    Ok(pages)
}
