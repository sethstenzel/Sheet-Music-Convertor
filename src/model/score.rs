//! The song transcription format exchanged with Claude.
//!
//! Claude returns a [`Song`] as JSON. Notes are written in a compact token
//! language (see [`crate::model::notation`]) so that a whole hymn fits in a
//! single response and every measure can be checked mechanically.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Descriptive information printed on the page (titles, credits, copyright…).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct SongMeta {
    pub title: String,
    pub subtitle: String,
    /// Number of the song in its book (e.g. hymn "245"), as printed.
    pub number: String,
    /// Name of the hymn tune, e.g. "NICAEA".
    pub tune_name: String,
    /// Poetic meter, e.g. "8.7.8.7 D", "C.M.", "L.M."
    pub meter: String,
    pub lyricist: String,
    pub composer: String,
    pub arranger: String,
    pub translator: String,
    /// Full "Words:" credit line as printed.
    pub words_credit: String,
    /// Full "Music:" credit line as printed.
    pub music_credit: String,
    pub copyright: String,
    pub scripture: String,
    pub topics: Vec<String>,
    pub first_line: String,
    pub refrain_first_line: String,
    pub language: String,
    /// Any other printed text: footnotes, "Alternate tune: …", performance notes.
    pub other_text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct KeySig {
    /// Number of sharps (positive) or flats (negative), -7..=7.
    pub fifths: i32,
    /// "major" or "minor".
    pub mode: String,
}

impl Default for KeySig {
    fn default() -> Self {
        Self { fifths: 0, mode: "major".into() }
    }
}

impl KeySig {
    pub fn is_minor(&self) -> bool {
        self.mode.eq_ignore_ascii_case("minor")
    }

    /// Human readable name such as "E♭ major".
    pub fn name(&self) -> String {
        const MAJ: [&str; 15] = ["C♭", "G♭", "D♭", "A♭", "E♭", "B♭", "F", "C", "G", "D", "A", "E", "B", "F♯", "C♯"];
        const MIN: [&str; 15] = ["A♭", "E♭", "B♭", "F", "C", "G", "D", "A", "E", "B", "F♯", "C♯", "G♯", "D♯", "A♯"];
        let i = (self.fifths.clamp(-7, 7) + 7) as usize;
        if self.is_minor() {
            format!("{} minor", MIN[i])
        } else {
            format!("{} major", MAJ[i])
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Tempo {
    /// Printed tempo word(s), e.g. "Majestically".
    pub text: String,
    /// Metronome value if printed (or a sensible suggestion if `suggested`).
    pub bpm: Option<f32>,
    /// Beat unit for the metronome mark in token notation ("q", "h", "q.").
    pub beat: String,
    /// True when no metronome mark was printed and `bpm` is Claude's suggestion.
    pub suggested: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Staff {
    /// "treble", "bass", "treble8vb" (tenor clef with 8 below), "alto", "tenor", "percussion".
    pub clef: String,
    pub name: String,
}

impl Default for Staff {
    fn default() -> Self {
        Self { clef: "treble".into(), name: String::new() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct VoiceDef {
    /// Short id used as key in each measure, e.g. "S", "A", "T", "B", "M" (melody).
    pub id: String,
    pub name: String,
    /// 1-based staff index.
    pub staff: u32,
    /// "up", "down" or "auto".
    pub stem: String,
    /// The voice only appears in some measures (e.g. a second voice in a keyboard part).
    pub optional: bool,
}

impl Default for VoiceDef {
    fn default() -> Self {
        Self { id: "S".into(), name: "Soprano".into(), staff: 1, stem: "auto".into(), optional: false }
    }
}

/// One voice's content in one measure.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct VoiceMeasure {
    /// Whitespace separated note tokens, e.g. `"G4:q E4:e( F4:e) G4:h"`.
    pub notes: String,
    /// Lyrics for this voice in this measure, keyed by verse number.
    /// Syllables align with the voice's sung notes (see notation docs).
    pub lyrics: BTreeMap<u32, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Measure {
    /// First measure that is an anacrusis (pickup).
    pub pickup: bool,
    /// Intentionally incomplete measure (split at a repeat, section or ending).
    pub partial: bool,
    /// Key change starting at this measure.
    pub key: Option<KeySig>,
    /// Time signature change starting at this measure.
    pub time: Option<String>,
    /// Barline at the end of the measure: "" (normal), "double", "final",
    /// "repeat-end", "dashed", "none".
    pub barline: String,
    /// A start-repeat barline at the beginning of this measure.
    pub repeat_start: bool,
    /// Volta (1st/2nd ending) this measure belongs to, e.g. "1" or "1,2".
    pub ending: Option<String>,
    /// Section / rehearsal label starting here, e.g. "Refrain", "Coda", "Amen".
    pub section: String,
    /// Other text printed above the staff at the start of the measure
    /// (e.g. "Unison", "D.C. al Fine", "Fine").
    pub text: String,
    /// The printed system (line) ends after this measure.
    pub line_break: bool,
    /// The printed page ends after this measure.
    pub page_break: bool,
    /// Content keyed by voice id.
    pub voices: BTreeMap<String, VoiceMeasure>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Stanza {
    pub number: u32,
    /// Full stanza text, lines separated by '\n'.
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Song {
    #[serde(flatten)]
    pub meta: SongMeta,
    pub key: KeySig,
    /// "4/4", "3/4", "6/8", "3/2", "C" (common time) or "C|" (cut time).
    pub time: String,
    pub tempo: Option<Tempo>,
    pub staves: Vec<Staff>,
    pub voices: Vec<VoiceDef>,
    /// Number of verses printed between the staves.
    pub verse_count: u32,
    pub measures: Vec<Measure>,
    /// Additional stanzas printed as text below the music.
    pub extra_stanzas: Vec<Stanza>,
    /// Claude's notes about anything uncertain or unreadable.
    pub transcription_notes: String,
}

impl Default for Song {
    fn default() -> Self {
        Self {
            meta: SongMeta::default(),
            key: KeySig::default(),
            time: "4/4".into(),
            tempo: None,
            staves: vec![Staff { clef: "treble".into(), name: String::new() }, Staff { clef: "bass".into(), name: String::new() }],
            voices: default_satb_voices(),
            verse_count: 1,
            measures: Vec::new(),
            extra_stanzas: Vec::new(),
            transcription_notes: String::new(),
        }
    }
}

pub fn default_satb_voices() -> Vec<VoiceDef> {
    vec![
        VoiceDef { id: "S".into(), name: "Soprano".into(), staff: 1, stem: "up".into(), optional: false },
        VoiceDef { id: "A".into(), name: "Alto".into(), staff: 1, stem: "down".into(), optional: false },
        VoiceDef { id: "T".into(), name: "Tenor".into(), staff: 2, stem: "up".into(), optional: false },
        VoiceDef { id: "B".into(), name: "Bass".into(), staff: 2, stem: "down".into(), optional: false },
    ]
}

impl Song {
    /// Display title, falling back to the first line.
    pub fn display_title(&self) -> String {
        if !self.meta.title.trim().is_empty() {
            self.meta.title.trim().to_string()
        } else if !self.meta.first_line.trim().is_empty() {
            self.meta.first_line.trim().to_string()
        } else {
            "Untitled".to_string()
        }
    }

    /// Highest verse number present in measure lyrics or `verse_count`.
    pub fn max_verse(&self) -> u32 {
        let mut m = self.verse_count;
        for meas in &self.measures {
            for vm in meas.voices.values() {
                if let Some((&k, _)) = vm.lyrics.iter().next_back() {
                    m = m.max(k);
                }
            }
        }
        m
    }

    pub fn voice(&self, id: &str) -> Option<&VoiceDef> {
        self.voices.iter().find(|v| v.id == id)
    }
}
