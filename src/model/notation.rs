//! Compact note-token language and its parser.
//!
//! A voice's notes in one measure are whitespace separated tokens:
//!
//! ```text
//! token     := ["g"] pitches ":" duration modifier*      (g = grace note)
//!            | "r:" duration modifier*                    (rest)
//!            | "R" [":" duration] modifier*               (whole-measure rest)
//! pitches   := pitch ("+" pitch)*                         (chord)
//! pitch     := A-G accidental? octave                      e.g. C4, F#4, Bb3, Ex5, Abb2, Bn3
//! accidental:= "#" | "##" | "x" | "b" | "bb" | "n"
//! duration  := ("B"|"w"|"h"|"q"|"e"|"s"|"t"|"x") "."* ["/" actual [":" normal]]
//! modifier  := "~"  tie to next note        "(" slur start     ")" slur end
//!            | "^"  fermata                 ">" accent         "'" staccato
//!            | "_"  tenuto                  "," breath mark after the note
//!            | "{" brace "}"
//! brace     := dynamic (pp, p, mp, mf, f, ff, sfz…) | "<" | ">" | "|" (hairpins start/stop)
//!            | "c:" chord-symbol | "t:" free text
//! ```
//!
//! Pitches are *sounding* pitches: the key signature and any earlier
//! accidentals in the measure are already applied (so in G major an F is
//! always written `F#4` unless it is actually a natural).
//!
//! Lyrics (per verse) are whitespace separated syllables that align with the
//! voice's sung notes (not rests, not grace notes, not the second note of a
//! tie). A syllable ending in "-" continues the word on the next syllable;
//! "_" means the note continues the previous syllable (melisma); "*" means
//! the note has no lyric in this verse.

use super::score::SongMeta;
use super::score::{KeySig, Song, Staff, Stanza, Tempo, VoiceDef};
use std::fmt;

/// MIDI-style ticks per quarter note used throughout the app.
pub const PPQ: u32 = 480;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pitch {
    /// 'C'..='B'
    pub step: char,
    /// -2..=2 semitones
    pub alter: i8,
    pub octave: i8,
}

impl Pitch {
    pub fn step_index(&self) -> i32 {
        match self.step {
            'C' => 0,
            'D' => 1,
            'E' => 2,
            'F' => 3,
            'G' => 4,
            'A' => 5,
            'B' => 6,
            _ => 0,
        }
    }

    pub fn step_semitone(step: char) -> i32 {
        match step {
            'C' => 0,
            'D' => 2,
            'E' => 4,
            'F' => 5,
            'G' => 7,
            'A' => 9,
            'B' => 11,
            _ => 0,
        }
    }

    /// MIDI note number (C4 = 60).
    pub fn midi(&self) -> i32 {
        (self.octave as i32 + 1) * 12 + Self::step_semitone(self.step) + self.alter as i32
    }

    /// Diatonic step number counted from C0 (used for spelling / transposition).
    pub fn diatonic(&self) -> i32 {
        self.octave as i32 * 7 + self.step_index()
    }

    pub fn from_diatonic(diatonic: i32, midi: i32) -> Pitch {
        const STEPS: [char; 7] = ['C', 'D', 'E', 'F', 'G', 'A', 'B'];
        let octave = diatonic.div_euclid(7);
        let step = STEPS[diatonic.rem_euclid(7) as usize];
        let natural = (octave + 1) * 12 + Self::step_semitone(step);
        Pitch { step, alter: (midi - natural) as i8, octave: octave as i8 }
    }

    /// Token spelling, e.g. "F#4".
    pub fn token(&self) -> String {
        let acc = match self.alter {
            -2 => "bb",
            -1 => "b",
            1 => "#",
            2 => "x",
            _ => "",
        };
        format!("{}{}{}", self.step, acc, self.octave)
    }

    /// Display name, e.g. "F♯4".
    pub fn display(&self) -> String {
        let acc = match self.alter {
            -2 => "𝄫",
            -1 => "♭",
            1 => "♯",
            2 => "𝄪",
            _ => "",
        };
        format!("{}{}{}", self.step, acc, self.octave)
    }
}

impl fmt::Display for Pitch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.token())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurBase {
    Breve,
    Whole,
    Half,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    SixtyFourth,
}

impl DurBase {
    pub fn ticks(self) -> u32 {
        match self {
            DurBase::Breve => PPQ * 8,
            DurBase::Whole => PPQ * 4,
            DurBase::Half => PPQ * 2,
            DurBase::Quarter => PPQ,
            DurBase::Eighth => PPQ / 2,
            DurBase::Sixteenth => PPQ / 4,
            DurBase::ThirtySecond => PPQ / 8,
            DurBase::SixtyFourth => PPQ / 16,
        }
    }

    /// MusicXML `<type>` value.
    pub fn musicxml(self) -> &'static str {
        match self {
            DurBase::Breve => "breve",
            DurBase::Whole => "whole",
            DurBase::Half => "half",
            DurBase::Quarter => "quarter",
            DurBase::Eighth => "eighth",
            DurBase::Sixteenth => "16th",
            DurBase::ThirtySecond => "32nd",
            DurBase::SixtyFourth => "64th",
        }
    }

    pub fn token(self) -> char {
        match self {
            DurBase::Breve => 'B',
            DurBase::Whole => 'w',
            DurBase::Half => 'h',
            DurBase::Quarter => 'q',
            DurBase::Eighth => 'e',
            DurBase::Sixteenth => 's',
            DurBase::ThirtySecond => 't',
            DurBase::SixtyFourth => 'x',
        }
    }

    fn from_char(c: char) -> Option<DurBase> {
        Some(match c {
            'B' => DurBase::Breve,
            'w' => DurBase::Whole,
            'h' => DurBase::Half,
            'q' => DurBase::Quarter,
            'e' => DurBase::Eighth,
            's' => DurBase::Sixteenth,
            't' => DurBase::ThirtySecond,
            'x' => DurBase::SixtyFourth,
            _ => return None,
        })
    }

    /// Largest base whose plain duration is <= ticks.
    pub fn largest_fitting(ticks: u32) -> DurBase {
        for b in
            [DurBase::Breve, DurBase::Whole, DurBase::Half, DurBase::Quarter, DurBase::Eighth, DurBase::Sixteenth, DurBase::ThirtySecond]
        {
            if b.ticks() <= ticks {
                return b;
            }
        }
        DurBase::SixtyFourth
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Duration {
    pub base: DurBase,
    pub dots: u8,
    /// (actual, normal): a triplet is (3, 2).
    pub tuplet: Option<(u32, u32)>,
}

impl Duration {
    pub fn ticks(&self) -> u32 {
        let mut t = self.base.ticks();
        let mut add = self.base.ticks();
        for _ in 0..self.dots {
            add /= 2;
            t += add;
        }
        if let Some((actual, normal)) = self.tuplet {
            if actual > 0 {
                t = t * normal / actual;
            }
        }
        t
    }

    /// Duration without tuplet modification.
    pub fn untupled_ticks(&self) -> u32 {
        Duration { tuplet: None, ..*self }.ticks()
    }

    pub fn token(&self) -> String {
        let mut s = String::new();
        s.push(self.base.token());
        for _ in 0..self.dots {
            s.push('.');
        }
        if let Some((a, n)) = self.tuplet {
            if n == 2 && a == 3 {
                s.push_str("/3");
            } else {
                s.push_str(&format!("/{a}:{n}"));
            }
        }
        s
    }

    /// Duration for a plain number of ticks if representable with <=2 dots.
    pub fn from_ticks(ticks: u32) -> Option<Duration> {
        for base in [
            DurBase::Breve,
            DurBase::Whole,
            DurBase::Half,
            DurBase::Quarter,
            DurBase::Eighth,
            DurBase::Sixteenth,
            DurBase::ThirtySecond,
            DurBase::SixtyFourth,
        ] {
            for dots in 0..=2 {
                let d = Duration { base, dots, tuplet: None };
                if d.ticks() == ticks {
                    return Some(d);
                }
            }
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventKind {
    Note(Vec<Pitch>),
    Rest,
    /// Whole-measure rest (duration = measure length).
    MeasureRest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hairpin {
    Crescendo,
    Diminuendo,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syllabic {
    Single,
    Begin,
    Middle,
    End,
}

impl Syllabic {
    pub fn musicxml(self) -> &'static str {
        match self {
            Syllabic::Single => "single",
            Syllabic::Begin => "begin",
            Syllabic::Middle => "middle",
            Syllabic::End => "end",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lyric {
    pub verse: u32,
    pub text: String,
    pub syllabic: Syllabic,
    /// A melisma line follows (the syllable is held over following notes).
    pub extend: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    pub dur: Duration,
    /// Actual duration in ticks (for measure rests: the measure length).
    pub ticks: u32,
    pub grace: bool,
    /// Tie from this note to the next note of the same voice.
    pub tie_start: bool,
    /// This note is the continuation of a tie (computed).
    pub tie_stop: bool,
    pub slur_starts: u8,
    pub slur_stops: u8,
    pub fermata: bool,
    pub accent: bool,
    pub staccato: bool,
    pub tenuto: bool,
    pub breath: bool,
    pub dynamics: Vec<String>,
    pub hairpins: Vec<Hairpin>,
    pub chord_symbols: Vec<String>,
    pub texts: Vec<String>,
    pub lyrics: Vec<Lyric>,
    /// Original token (for diagnostics).
    pub source: String,
}

impl Event {
    pub fn pitches(&self) -> &[Pitch] {
        match &self.kind {
            EventKind::Note(p) => p,
            _ => &[],
        }
    }

    pub fn is_note(&self) -> bool {
        matches!(self.kind, EventKind::Note(_))
    }

    /// Serialize back to a token.
    pub fn token(&self) -> String {
        let mut s = String::new();
        if self.grace {
            s.push('g');
        }
        match &self.kind {
            EventKind::Note(ps) => {
                s.push_str(&ps.iter().map(|p| p.token()).collect::<Vec<_>>().join("+"));
                s.push(':');
                s.push_str(&self.dur.token());
            }
            EventKind::Rest => {
                s.push_str("r:");
                s.push_str(&self.dur.token());
            }
            EventKind::MeasureRest => s.push('R'),
        }
        if self.tie_start {
            s.push('~');
        }
        for _ in 0..self.slur_starts {
            s.push('(');
        }
        for _ in 0..self.slur_stops {
            s.push(')');
        }
        if self.fermata {
            s.push('^');
        }
        if self.accent {
            s.push('>');
        }
        if self.staccato {
            s.push('\'');
        }
        if self.tenuto {
            s.push('_');
        }
        if self.breath {
            s.push(',');
        }
        for d in &self.dynamics {
            s.push_str(&format!("{{{d}}}"));
        }
        for h in &self.hairpins {
            s.push_str(match h {
                Hairpin::Crescendo => "{<}",
                Hairpin::Diminuendo => "{>}",
                Hairpin::Stop => "{|}",
            });
        }
        for c in &self.chord_symbols {
            s.push_str(&format!("{{c:{c}}}"));
        }
        for t in &self.texts {
            s.push_str(&format!("{{t:{t}}}"));
        }
        s
    }
}

pub const DYNAMICS: &[&str] =
    &["pppp", "ppp", "pp", "p", "mp", "mf", "f", "ff", "fff", "ffff", "sf", "sfz", "sffz", "fp", "rfz", "rf", "fz", "sfp", "pf"];

fn parse_pitch(s: &str) -> Result<Pitch, String> {
    let mut chars = s.chars().peekable();
    let step = chars.next().ok_or("empty pitch")?;
    let step = step.to_ascii_uppercase();
    if !('A'..='G').contains(&step) {
        return Err(format!("'{s}': pitch must start with A-G"));
    }
    let rest: String = chars.collect();
    let rest = rest.replace('♯', "#").replace('♭', "b").replace('♮', "n").replace('𝄪', "x");
    let digit_pos = rest.find(|c: char| c.is_ascii_digit() || c == '-').ok_or_else(|| format!("'{s}': missing octave number"))?;
    let (acc, oct) = rest.split_at(digit_pos);
    let alter = match acc {
        "" | "n" => 0,
        "#" | "s" => 1,
        "##" | "x" => 2,
        "b" | "f" => -1,
        "bb" => -2,
        _ => return Err(format!("'{s}': unknown accidental '{acc}'")),
    };
    let octave: i8 = oct.parse().map_err(|_| format!("'{s}': bad octave '{oct}'"))?;
    if !(0..=9).contains(&octave) {
        return Err(format!("'{s}': octave {octave} out of range"));
    }
    Ok(Pitch { step, alter, octave })
}

fn parse_duration(s: &str) -> Result<(Duration, usize), String> {
    let mut it = s.char_indices().peekable();
    let (_, c) = it.next().ok_or("missing duration")?;
    let base = DurBase::from_char(c).ok_or_else(|| format!("unknown duration letter '{c}' (use w h q e s t x B)"))?;
    let mut dots = 0u8;
    let mut consumed = c.len_utf8();
    while let Some(&(i, ch)) = it.peek() {
        if ch == '.' {
            dots += 1;
            consumed = i + 1;
            it.next();
        } else {
            break;
        }
    }
    let mut tuplet = None;
    if s[consumed..].starts_with('/') {
        let tail = &s[consumed + 1..];
        let end = tail.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(tail.len());
        let spec = &tail[..end];
        let (a, n) = match spec.split_once(':') {
            Some((a, n)) => (a.parse::<u32>().ok(), n.parse::<u32>().ok()),
            None => {
                let a = spec.parse::<u32>().ok();
                let n = a.map(|a| match a {
                    3 => 2,
                    2 => 3,
                    5..=7 => 4,
                    9 => 8,
                    _ => a.saturating_sub(1).max(1),
                });
                (a, n)
            }
        };
        match (a, n) {
            (Some(a), Some(n)) if a > 0 && n > 0 => tuplet = Some((a, n)),
            _ => return Err(format!("bad tuplet '/{spec}'")),
        }
        consumed += 1 + end;
    }
    Ok((Duration { base, dots, tuplet }, consumed))
}

fn apply_modifiers(ev: &mut Event, mods: &str) -> Result<(), String> {
    let mut chars = mods.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '~' => ev.tie_start = true,
            '(' => ev.slur_starts += 1,
            ')' => ev.slur_stops += 1,
            '^' => ev.fermata = true,
            '>' => ev.accent = true,
            '\'' => ev.staccato = true,
            '_' => ev.tenuto = true,
            ',' => ev.breath = true,
            '{' => {
                let mut inner = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == '}' {
                        closed = true;
                        break;
                    }
                    inner.push(ch);
                }
                if !closed {
                    return Err(format!("unclosed '{{' in '{mods}'"));
                }
                let inner = inner.trim();
                if let Some(ch) = inner.strip_prefix("c:") {
                    ev.chord_symbols.push(ch.trim().to_string());
                } else if let Some(t) = inner.strip_prefix("t:") {
                    ev.texts.push(t.trim().to_string());
                } else if inner == "<" || inner.eq_ignore_ascii_case("cresc<") {
                    ev.hairpins.push(Hairpin::Crescendo);
                } else if inner == ">" {
                    ev.hairpins.push(Hairpin::Diminuendo);
                } else if inner == "|" || inner == "/" {
                    ev.hairpins.push(Hairpin::Stop);
                } else if DYNAMICS.contains(&inner) {
                    ev.dynamics.push(inner.to_string());
                } else if !inner.is_empty() {
                    ev.texts.push(inner.to_string());
                }
            }
            c if c.is_whitespace() => {}
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(())
}

fn blank_event(kind: EventKind, dur: Duration, source: &str) -> Event {
    Event {
        ticks: dur.ticks(),
        kind,
        dur,
        grace: false,
        tie_start: false,
        tie_stop: false,
        slur_starts: 0,
        slur_stops: 0,
        fermata: false,
        accent: false,
        staccato: false,
        tenuto: false,
        breath: false,
        dynamics: Vec::new(),
        hairpins: Vec::new(),
        chord_symbols: Vec::new(),
        texts: Vec::new(),
        lyrics: Vec::new(),
        source: source.to_string(),
    }
}

/// Parse a single token. `measure_ticks` is used for whole-measure rests.
pub fn parse_token(tok: &str, measure_ticks: u32) -> Result<Event, String> {
    let tok = tok.trim();
    if tok.is_empty() {
        return Err("empty token".into());
    }
    // Whole measure rest: "R", "R:w", "R^"
    if let Some(rest) = tok.strip_prefix('R') {
        let mods = match rest.strip_prefix(':') {
            Some(r) => {
                let (_, used) = parse_duration(r).map_err(|e| format!("'{tok}': {e}"))?;
                &r[used..]
            }
            None => rest,
        };
        let dur = Duration::from_ticks(measure_ticks).unwrap_or(Duration { base: DurBase::Whole, dots: 0, tuplet: None });
        let mut ev = blank_event(EventKind::MeasureRest, dur, tok);
        ev.ticks = measure_ticks;
        apply_modifiers(&mut ev, mods).map_err(|e| format!("'{tok}': {e}"))?;
        return Ok(ev);
    }
    let (grace, body) = match tok.strip_prefix('g') {
        Some(b) if b.starts_with(|c: char| ('A'..='G').contains(&c)) => (true, b),
        _ => (false, tok),
    };
    let (pitch_part, after) = body.split_once(':').ok_or_else(|| format!("'{tok}': expected PITCH:DURATION (e.g. G4:q)"))?;
    let (dur, used) = parse_duration(after).map_err(|e| format!("'{tok}': {e}"))?;
    let mods = &after[used..];
    let kind = if pitch_part == "r" || pitch_part == "z" {
        EventKind::Rest
    } else {
        let mut ps = Vec::new();
        for p in pitch_part.split('+') {
            ps.push(parse_pitch(p.trim()).map_err(|e| format!("'{tok}': {e}"))?);
        }
        ps.sort_by_key(|p| p.midi());
        ps.dedup();
        EventKind::Note(ps)
    };
    let mut ev = blank_event(kind, dur, tok);
    ev.grace = grace;
    if grace {
        ev.ticks = 0;
    }
    apply_modifiers(&mut ev, mods).map_err(|e| format!("'{tok}': {e}"))?;
    Ok(ev)
}

/// Parse a voice's note string for one measure.
pub fn parse_voice(notes: &str, measure_ticks: u32) -> Result<Vec<Event>, Vec<String>> {
    let mut out = Vec::new();
    let mut errs = Vec::new();
    for tok in notes.split_whitespace() {
        if tok == "|" || tok == "||" {
            continue;
        }
        match parse_token(tok, measure_ticks) {
            Ok(ev) => out.push(ev),
            Err(e) => errs.push(e),
        }
    }
    if errs.is_empty() {
        Ok(out)
    } else {
        Err(errs)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSig {
    pub beats: u32,
    pub beat_type: u32,
    /// "common" or "cut" symbol.
    pub symbol: Option<&'static str>,
}

impl TimeSig {
    pub fn parse(s: &str) -> Option<TimeSig> {
        let s = s.trim();
        match s {
            "C" | "c" | "common" => return Some(TimeSig { beats: 4, beat_type: 4, symbol: Some("common") }),
            "C|" | "c|" | "cut" | "¢" => return Some(TimeSig { beats: 2, beat_type: 2, symbol: Some("cut") }),
            _ => {}
        }
        let (a, b) = s.split_once('/')?;
        let beats: u32 = a.trim().parse().ok()?;
        let beat_type: u32 = b.trim().parse().ok()?;
        if beats == 0 || !matches!(beat_type, 1 | 2 | 4 | 8 | 16 | 32) {
            return None;
        }
        Some(TimeSig { beats, beat_type, symbol: None })
    }

    pub fn measure_ticks(&self) -> u32 {
        self.beats * PPQ * 4 / self.beat_type
    }

    pub fn is_compound(&self) -> bool {
        self.beat_type >= 8 && self.beats.is_multiple_of(3) && self.beats > 3
    }

    pub fn label(&self) -> String {
        match self.symbol {
            Some("common") => "C".into(),
            Some("cut") => "C|".into(),
            _ => format!("{}/{}", self.beats, self.beat_type),
        }
    }
}

impl Default for TimeSig {
    fn default() -> Self {
        TimeSig { beats: 4, beat_type: 4, symbol: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Barline {
    Regular,
    Double,
    Final,
    RepeatEnd,
    Dashed,
    None,
}

impl Barline {
    pub fn parse(s: &str) -> Barline {
        match s.trim().to_ascii_lowercase().as_str() {
            "double" | "light-light" => Barline::Double,
            "final" | "end" | "light-heavy" => Barline::Final,
            "repeat-end" | "repeat" | "end-repeat" | "repeat-both" => Barline::RepeatEnd,
            "dashed" => Barline::Dashed,
            "none" | "hidden" => Barline::None,
            _ => Barline::Regular,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EndingMark {
    /// e.g. "1" or "1, 2"
    pub numbers: String,
    pub start: bool,
    /// Ending ends on this measure: Some(true) = closed (with hook), Some(false) = open (discontinue).
    pub stop: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct PVoice {
    pub id: String,
    pub name: String,
    /// 0-based staff index.
    pub staff: usize,
    pub stem: String,
    pub events: Vec<Event>,
}

impl PVoice {
    pub fn total_ticks(&self) -> u32 {
        self.events.iter().map(|e| e.ticks).sum()
    }
}

#[derive(Clone, Debug)]
pub struct PMeasure {
    /// Position in the song (0-based).
    pub index: usize,
    /// Printed measure number (pickup = 0).
    pub number: i32,
    /// Measure does not count toward numbering (pickups, split measures).
    pub implicit: bool,
    pub pickup: bool,
    pub partial: bool,
    pub key_change: Option<KeySig>,
    pub time_change: Option<TimeSig>,
    pub key: KeySig,
    pub time: TimeSig,
    /// Nominal length according to the time signature.
    pub expected_ticks: u32,
    /// Actual length (longest voice).
    pub actual_ticks: u32,
    pub barline: Barline,
    pub repeat_start: bool,
    pub ending: Option<EndingMark>,
    pub section: String,
    pub text: String,
    pub line_break: bool,
    pub page_break: bool,
    /// Voices in song voice-definition order (only voices present in this measure).
    pub voices: Vec<PVoice>,
}

#[derive(Clone, Debug)]
pub struct ParsedSong {
    pub meta: SongMeta,
    pub key: KeySig,
    pub time: TimeSig,
    pub tempo: Option<Tempo>,
    pub staves: Vec<Staff>,
    pub voices: Vec<VoiceDef>,
    pub verse_count: u32,
    pub measures: Vec<PMeasure>,
    pub extra_stanzas: Vec<Stanza>,
}

impl ParsedSong {
    pub fn voice_index(&self, id: &str) -> Option<usize> {
        self.voices.iter().position(|v| v.id == id)
    }

    /// Voice id that carries the main lyrics (first voice with lyrics, else first voice).
    pub fn lyric_voice(&self) -> Option<String> {
        for m in &self.measures {
            for v in &m.voices {
                if v.events.iter().any(|e| !e.lyrics.is_empty()) {
                    return Some(v.id.clone());
                }
            }
        }
        self.voices.first().map(|v| v.id.clone())
    }

    pub fn tempo_bpm(&self) -> f32 {
        let bpm = self.tempo.as_ref().and_then(|t| t.bpm).unwrap_or(0.0);
        if bpm > 10.0 {
            // Convert to quarter-note bpm.
            let beat = self.tempo.as_ref().map(|t| t.beat.as_str()).unwrap_or("q");
            let factor = match beat {
                "h" => 2.0,
                "h." => 3.0,
                "q." => 1.5,
                "e" => 0.5,
                "w" => 4.0,
                _ => 1.0,
            };
            bpm * factor
        } else {
            // Sensible hymn default.
            if self.time.is_compound() {
                100.0 * 1.5
            } else if self.time.beat_type == 2 {
                160.0
            } else {
                96.0
            }
        }
    }
}

/// A problem found while parsing (location + message).
#[derive(Clone, Debug)]
pub struct ParseProblem {
    pub measure: Option<usize>,
    pub voice: Option<String>,
    pub message: String,
    pub is_error: bool,
}

fn staff_index_for(song: &Song, v: &VoiceDef) -> usize {
    let n = song.staves.len().max(1);
    (v.staff.max(1) as usize - 1).min(n - 1)
}

/// Build the parsed representation. Parsing never fails as a whole; problems
/// are collected so the UI and the automatic correction pass can show them.
pub fn parse_song(song: &Song) -> (ParsedSong, Vec<ParseProblem>) {
    let mut problems = Vec::new();
    let base_time = TimeSig::parse(&song.time).unwrap_or_else(|| {
        problems.push(ParseProblem {
            measure: None,
            voice: None,
            message: format!("Unrecognized time signature '{}', assuming 4/4", song.time),
            is_error: true,
        });
        TimeSig::default()
    });

    let mut voices = song.voices.clone();
    if voices.is_empty() {
        voices = super::score::default_satb_voices();
    }
    let mut staves = song.staves.clone();
    if staves.is_empty() {
        staves.push(Staff::default());
    }

    let mut measures = Vec::with_capacity(song.measures.len());
    let mut cur_time = base_time;
    let mut cur_key = song.key.clone();
    let mut number: i32 = 1;
    let mut prev_partial_open = false;

    for (idx, m) in song.measures.iter().enumerate() {
        let time_change = match &m.time {
            Some(t) if !t.trim().is_empty() => match TimeSig::parse(t) {
                Some(ts) => {
                    cur_time = ts;
                    Some(ts)
                }
                None => {
                    problems.push(ParseProblem {
                        measure: Some(idx),
                        voice: None,
                        message: format!("Unrecognized time signature '{t}'"),
                        is_error: true,
                    });
                    None
                }
            },
            _ => None,
        };
        let key_change = m.key.clone();
        if let Some(k) = &key_change {
            cur_key = k.clone();
        }
        let expected = cur_time.measure_ticks();

        let mut pvoices = Vec::new();
        for vid in m.voices.keys() {
            if !voices.iter().any(|v| &v.id == vid) {
                problems.push(ParseProblem {
                    measure: Some(idx),
                    voice: Some(vid.clone()),
                    message: format!("Voice '{vid}' is not declared in \"voices\""),
                    is_error: true,
                });
            }
        }
        for vdef in &voices {
            let Some(vm) = m.voices.get(&vdef.id) else { continue };
            let events = match parse_voice(&vm.notes, expected) {
                Ok(evs) => evs,
                Err(errs) => {
                    for e in errs {
                        problems.push(ParseProblem { measure: Some(idx), voice: Some(vdef.id.clone()), message: e, is_error: true });
                    }
                    parse_voice_lenient(&vm.notes, expected)
                }
            };
            pvoices.push(PVoice {
                id: vdef.id.clone(),
                name: vdef.name.clone(),
                staff: staff_index_for(song, vdef),
                stem: vdef.stem.clone(),
                events,
            });
        }
        // A whole-measure rest in a short (pickup / partial) measure lasts as
        // long as the other voices.
        if m.pickup || m.partial {
            let others =
                pvoices.iter().filter(|v| !v.events.iter().any(|e| e.kind == EventKind::MeasureRest)).map(|v| v.total_ticks()).max();
            if let Some(len) = others.filter(|&t| t > 0) {
                for v in &mut pvoices {
                    if v.events.len() == 1 && v.events[0].kind == EventKind::MeasureRest {
                        v.events[0].ticks = len;
                        if let Some(d) = Duration::from_ticks(len) {
                            v.events[0].dur = d;
                        }
                    }
                }
            }
        }
        let actual = pvoices.iter().map(|v| v.total_ticks()).max().unwrap_or(expected);

        let pickup = m.pickup || (idx == 0 && m.partial);
        let (this_number, implicit) = if pickup && idx == 0 {
            (0, true)
        } else if m.partial && prev_partial_open {
            // Second half of a split measure shares the first half's number.
            (number - 1, true)
        } else {
            number += 1;
            (number - 1, false)
        };
        prev_partial_open = m.partial && !implicit;

        measures.push(PMeasure {
            index: idx,
            number: this_number,
            implicit,
            pickup,
            partial: m.partial,
            key_change,
            time_change,
            key: cur_key.clone(),
            time: cur_time,
            expected_ticks: expected,
            actual_ticks: actual,
            barline: Barline::parse(&m.barline),
            repeat_start: m.repeat_start,
            ending: None,
            section: m.section.trim().to_string(),
            text: m.text.trim().to_string(),
            line_break: m.line_break,
            page_break: m.page_break,
            voices: pvoices,
        });
    }

    compute_endings(song, &mut measures);
    compute_ties(&mut measures, &voices, &mut problems);
    align_lyrics(song, &mut measures, &voices, &mut problems);

    let parsed = ParsedSong {
        meta: song.meta.clone(),
        key: song.key.clone(),
        time: base_time,
        tempo: song.tempo.clone(),
        staves,
        voices,
        verse_count: song.verse_count.max(1),
        measures,
        extra_stanzas: song.extra_stanzas.clone(),
    };
    (parsed, problems)
}

/// Parse what can be parsed, skipping bad tokens.
fn parse_voice_lenient(notes: &str, measure_ticks: u32) -> Vec<Event> {
    notes.split_whitespace().filter_map(|t| parse_token(t, measure_ticks).ok()).collect()
}

fn compute_endings(song: &Song, measures: &mut [PMeasure]) {
    let n = measures.len();
    for i in 0..n {
        let cur = song.measures[i].ending.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let Some(cur) = cur else { continue };
        let prev = if i > 0 { song.measures[i - 1].ending.as_deref().map(str::trim).filter(|s| !s.is_empty()) } else { None };
        let next = song.measures.get(i + 1).and_then(|m| m.ending.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let start = prev != Some(cur);
        let stop = if next != Some(cur) { Some(measures[i].barline == Barline::RepeatEnd) } else { None };
        measures[i].ending = Some(EndingMark { numbers: cur.to_string(), start, stop });
    }
}

/// Mark tie continuations and warn about ties that lead nowhere.
fn compute_ties(measures: &mut [PMeasure], voices: &[VoiceDef], problems: &mut Vec<ParseProblem>) {
    for vdef in voices {
        let mut pending: Option<(Vec<Pitch>, usize)> = None;
        for mi in 0..measures.len() {
            let Some(vi) = measures[mi].voices.iter().position(|v| v.id == vdef.id) else {
                if let Some((_, from)) = pending.take() {
                    problems.push(ParseProblem {
                        measure: Some(from),
                        voice: Some(vdef.id.clone()),
                        message: "Tie at end of measure but the voice is missing in the next measure".into(),
                        is_error: false,
                    });
                }
                continue;
            };
            let evs = &mut measures[mi].voices[vi].events;
            for ev in evs.iter_mut() {
                if ev.grace {
                    continue;
                }
                if let Some((pitches, from)) = pending.take() {
                    let cur = ev.pitches();
                    if !cur.is_empty() && pitches.iter().any(|p| cur.iter().any(|c| c.midi() == p.midi())) {
                        ev.tie_stop = true;
                    } else {
                        problems.push(ParseProblem {
                            measure: Some(from),
                            voice: Some(vdef.id.clone()),
                            message: format!("Tie from {} is not followed by the same pitch", ev.source),
                            is_error: false,
                        });
                    }
                }
                if ev.tie_start && ev.is_note() {
                    pending = Some((ev.pitches().to_vec(), mi));
                }
            }
        }
    }
}

fn align_lyrics(song: &Song, measures: &mut [PMeasure], voices: &[VoiceDef], problems: &mut Vec<ParseProblem>) {
    for vdef in voices {
        // Per verse: is the previous syllable unfinished (ended with '-')?
        let mut open_word: std::collections::BTreeMap<u32, (usize, usize, usize)> = Default::default();
        // Per verse: location of the last syllable (measure, voice idx, event idx) to set extend.
        let mut last_syl: std::collections::BTreeMap<u32, (usize, usize, usize)> = Default::default();
        for mi in 0..measures.len() {
            let Some(vm) = song.measures[mi].voices.get(&vdef.id) else { continue };
            let Some(vi) = measures[mi].voices.iter().position(|v| v.id == vdef.id) else { continue };
            for (&verse, text) in &vm.lyrics {
                let sylls: Vec<&str> = text.split_whitespace().collect();
                if sylls.is_empty() {
                    continue;
                }
                let eligible: Vec<usize> = measures[mi].voices[vi]
                    .events
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.is_note() && !e.grace && !e.tie_stop)
                    .map(|(i, _)| i)
                    .collect();
                if sylls.len() > eligible.len() {
                    problems.push(ParseProblem {
                        measure: Some(mi),
                        voice: Some(vdef.id.clone()),
                        message: format!(
                            "Verse {verse}: {} lyric syllables but only {} sung notes ({} )",
                            sylls.len(),
                            eligible.len(),
                            text
                        ),
                        is_error: true,
                    });
                }
                for (si, syl) in sylls.iter().enumerate() {
                    let Some(&ei) = eligible.get(si) else { break };
                    match *syl {
                        "_" => {
                            if let Some(&(lm, lv, le)) = last_syl.get(&verse) {
                                if let Some(l) = measures[lm].voices[lv].events[le].lyrics.iter_mut().find(|l| l.verse == verse) {
                                    l.extend = true;
                                }
                            }
                        }
                        "*" => {}
                        s => {
                            let hyphen_after = s.ends_with('-') && s.len() > 1;
                            let mut t = s.trim_end_matches('-').to_string();
                            if t.is_empty() {
                                t = s.to_string();
                            }
                            // Inner underscores mark elisions ("the_e-").
                            let t = t.replace('_', "‿");
                            let continuing = open_word.remove(&verse).is_some();
                            let syllabic = match (continuing, hyphen_after) {
                                (false, false) => Syllabic::Single,
                                (false, true) => Syllabic::Begin,
                                (true, true) => Syllabic::Middle,
                                (true, false) => Syllabic::End,
                            };
                            if hyphen_after {
                                open_word.insert(verse, (mi, vi, ei));
                            }
                            measures[mi].voices[vi].events[ei].lyrics.push(Lyric { verse, text: t, syllabic, extend: false });
                            last_syl.insert(verse, (mi, vi, ei));
                        }
                    }
                }
            }
        }
    }
}

/// Render a measure-by-measure plain text summary (used in prompts and the UI).
pub fn summarize(parsed: &ParsedSong) -> String {
    let mut s = String::new();
    for m in &parsed.measures {
        s.push_str(&format!("m{} (#{}{}):", m.index, m.number, if m.implicit { ", implicit" } else { "" }));
        for v in &m.voices {
            let toks: Vec<String> = v.events.iter().map(|e| e.token()).collect();
            s.push_str(&format!(" [{}] {}", v.id, toks.join(" ")));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::score::{Measure, VoiceMeasure};

    #[test]
    fn parses_tokens() {
        let e = parse_token("F#4:q.~(", 1920).unwrap();
        assert_eq!(e.pitches()[0], Pitch { step: 'F', alter: 1, octave: 4 });
        assert_eq!(e.ticks, 720);
        assert!(e.tie_start);
        assert_eq!(e.slur_starts, 1);

        let e = parse_token("C4+E4+G4:h^{mf}", 1920).unwrap();
        assert_eq!(e.pitches().len(), 3);
        assert!(e.fermata);
        assert_eq!(e.dynamics, vec!["mf".to_string()]);

        let e = parse_token("Bb3:e/3", 1920).unwrap();
        assert_eq!(e.ticks, 160);
        assert_eq!(e.pitches()[0].midi(), 58);

        let e = parse_token("R", 1440).unwrap();
        assert_eq!(e.ticks, 1440);

        assert!(parse_token("H4:q", 1920).is_err());
        assert!(parse_token("C4", 1920).is_err());
    }

    #[test]
    fn roundtrip_token() {
        for t in ["F#4:q.~(", "C4+E4+G4:h^{mf}", "r:e", "Bb3:e/3", "gD5:s"] {
            let e = parse_token(t, 1920).unwrap();
            let again = parse_token(&e.token(), 1920).unwrap();
            assert_eq!(e.kind, again.kind);
            assert_eq!(e.ticks, again.ticks);
        }
    }

    #[test]
    fn lyrics_and_ties() {
        let mut song = Song::default();
        song.time = "3/4".into();
        song.voices.truncate(1);
        let mut m1 = Measure::default();
        m1.voices.insert(
            "S".into(),
            VoiceMeasure { notes: "G4:q A4:q B4:q~".into(), lyrics: [(1, "A- maz- ing".to_string())].into_iter().collect() },
        );
        let mut m2 = Measure::default();
        m2.voices.insert(
            "S".into(),
            VoiceMeasure { notes: "B4:q C5:q( B5:q)".into(), lyrics: [(1, "grace _".to_string())].into_iter().collect() },
        );
        song.measures = vec![m1, m2];
        let (p, probs) = parse_song(&song);
        assert!(probs.iter().all(|p| !p.is_error), "{probs:?}");
        let e = &p.measures[0].voices[0].events;
        assert_eq!(e[0].lyrics[0].syllabic, Syllabic::Begin);
        assert_eq!(e[1].lyrics[0].syllabic, Syllabic::Middle);
        assert_eq!(e[2].lyrics[0].syllabic, Syllabic::End);
        let e2 = &p.measures[1].voices[0].events;
        assert!(e2[0].tie_stop);
        assert!(e2[0].lyrics.is_empty());
        assert_eq!(e2[1].lyrics[0].text, "grace");
        assert!(e2[1].lyrics[0].extend);
    }

    #[test]
    fn diatonic_roundtrip() {
        let p = Pitch { step: 'F', alter: 1, octave: 4 };
        assert_eq!(Pitch::from_diatonic(p.diatonic(), p.midi()), p);
    }
}
