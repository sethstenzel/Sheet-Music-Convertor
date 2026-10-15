//! MusicXML 4.0 (score-partwise) writer.
//!
//! MuseScore opens MusicXML natively, so this is the primary "MuseScore"
//! output; `.mscz`/`.pdf` are produced from it with the MuseScore CLI.

use super::{MusicXmlOptions, ScoreLayout};
use crate::model::notation::{Barline, Event, EventKind, Hairpin, PMeasure, ParsedSong, Pitch, PPQ};
use crate::model::project::SourceInfo;
use crate::model::score::{KeySig, Staff};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

// Page layout (tenths). 7 mm per 40 tenths, US Letter.
const MM: f32 = 7.0;
const TENTHS: f32 = 40.0;
const PAGE_H: f32 = 1596.0;
const PAGE_W: f32 = 1234.0;
const MARGIN: f32 = 80.0;

/// Escape text for XML content and attribute values.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' && c != '\r' => {}
            c => o.push(c),
        }
    }
    o
}

/// Tiny indenting XML writer.
struct W {
    s: String,
    depth: usize,
}

impl W {
    fn new() -> Self {
        W { s: String::new(), depth: 0 }
    }
    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.s.push_str("  ");
        }
    }
    /// `<tag attrs>` — `tag` may include attributes.
    fn open(&mut self, tag: &str) {
        self.indent();
        let _ = writeln!(self.s, "<{tag}>");
        self.depth += 1;
    }
    fn close(&mut self, name: &str) {
        self.depth = self.depth.saturating_sub(1);
        self.indent();
        let _ = writeln!(self.s, "</{name}>");
    }
    /// `<name attrs>text</name>` (text is escaped).
    fn leaf(&mut self, name: &str, attrs: &str, text: &str) {
        self.indent();
        if attrs.is_empty() {
            let _ = writeln!(self.s, "<{name}>{}</{name}>", esc(text));
        } else {
            let _ = writeln!(self.s, "<{name} {attrs}>{}</{name}>", esc(text));
        }
    }
    /// `<tag/>` — `tag` may include attributes.
    fn empty(&mut self, tag: &str) {
        self.indent();
        let _ = writeln!(self.s, "<{tag}/>");
    }
}

fn attr(name: &str, value: &str) -> String {
    format!("{name}=\"{}\"", esc(value))
}

// ---------------------------------------------------------------------------
// Part planning
// ---------------------------------------------------------------------------

struct VoiceAssign {
    voice_id: String,
    /// 0-based staff within the part.
    staff: usize,
    xml_voice: u32,
    stem: String,
}

struct PartPlan {
    id: String,
    name: String,
    abbrev: String,
    show_name: bool,
    program: u8,
    clefs: Vec<String>,
    voices: Vec<VoiceAssign>,
}

fn clef_for_voice(id: &str, name: &str, fallback: &str) -> String {
    let key = if name.trim().is_empty() { id } else { name };
    let k = key.trim().to_ascii_lowercase();
    if k.starts_with('s') || k.starts_with('a') || k.starts_with("melody") || k == "m" {
        "treble".into()
    } else if k.starts_with('t') {
        "treble8vb".into()
    } else if k.starts_with('b') {
        "bass".into()
    } else {
        fallback.to_string()
    }
}

fn plan_parts(song: &ParsedSong, layout: ScoreLayout) -> Vec<PartPlan> {
    let nstaves = song.staves.len().max(1);
    let staff_of = |s: u32| (s.max(1) as usize - 1).min(nstaves - 1);
    match layout {
        ScoreLayout::Hymn => {
            let mut per_staff = vec![0u32; nstaves];
            let voices = song
                .voices
                .iter()
                .map(|v| {
                    let st = staff_of(v.staff);
                    let k = per_staff[st];
                    per_staff[st] += 1;
                    VoiceAssign { voice_id: v.id.clone(), staff: st, xml_voice: st as u32 * 4 + k + 1, stem: v.stem.clone() }
                })
                .collect();
            let named = song.staves.first().map(|s| s.name.trim().to_string()).unwrap_or_default();
            let clefs =
                if song.staves.is_empty() { vec!["treble".to_string()] } else { song.staves.iter().map(|s| s.clef.clone()).collect() };
            vec![PartPlan {
                id: "P1".into(),
                show_name: !named.is_empty(),
                name: if named.is_empty() { "Piano".into() } else { named },
                abbrev: if nstaves > 1 { "Pno.".into() } else { String::new() },
                program: 1,
                clefs,
                voices,
            }]
        }
        ScoreLayout::OpenScore => song
            .voices
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let fallback = song.staves.get(staff_of(v.staff)).map(|s: &Staff| s.clef.as_str()).unwrap_or("treble");
                let name = if v.name.trim().is_empty() { v.id.clone() } else { v.name.clone() };
                PartPlan {
                    id: format!("P{}", i + 1),
                    abbrev: name.chars().take(1).collect::<String>() + ".",
                    name,
                    show_name: true,
                    program: 53,
                    clefs: vec![clef_for_voice(&v.id, &v.name, fallback)],
                    voices: vec![VoiceAssign { voice_id: v.id.clone(), staff: 0, xml_voice: 1, stem: "auto".into() }],
                }
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Song-wide precomputation
// ---------------------------------------------------------------------------

/// Per (measure, voice id, event): which pitches start / stop a tie.
type TieMap = HashMap<(usize, String, usize), (Vec<bool>, Vec<bool>)>;

fn compute_tie_map(song: &ParsedSong) -> TieMap {
    let mut map: TieMap = HashMap::new();
    for vdef in &song.voices {
        // (measure, event index) of the previous non-grace event.
        let mut prev: Option<(usize, usize)> = None;
        for (mi, m) in song.measures.iter().enumerate() {
            let Some(v) = m.voices.iter().find(|v| v.id == vdef.id) else {
                prev = None;
                continue;
            };
            for (ei, e) in v.events.iter().enumerate() {
                if e.grace {
                    continue;
                }
                if let Some((pm, pe)) = prev {
                    let pv = song.measures[pm].voices.iter().find(|v| v.id == vdef.id).unwrap();
                    let p = &pv.events[pe];
                    if p.tie_start && e.tie_stop && p.is_note() && e.is_note() {
                        let pp = p.pitches().to_vec();
                        let cp = e.pitches().to_vec();
                        let mut starts = vec![false; pp.len()];
                        let mut stops = vec![false; cp.len()];
                        for (i, a) in pp.iter().enumerate() {
                            for (j, b) in cp.iter().enumerate() {
                                if a.midi() == b.midi() {
                                    starts[i] = true;
                                    stops[j] = true;
                                }
                            }
                        }
                        let entry = map.entry((pm, vdef.id.clone(), pe)).or_insert_with(|| (vec![false; pp.len()], vec![false; pp.len()]));
                        for (i, s) in starts.iter().enumerate() {
                            entry.0[i] |= *s;
                        }
                        let entry = map.entry((mi, vdef.id.clone(), ei)).or_insert_with(|| (vec![false; cp.len()], vec![false; cp.len()]));
                        for (j, s) in stops.iter().enumerate() {
                            entry.1[j] |= *s;
                        }
                    }
                }
                prev = Some((mi, ei));
            }
        }
    }
    map
}

/// First lyric location (measure, voice id, event) for each verse number.
fn first_lyric_locations(song: &ParsedSong) -> HashMap<u32, (usize, String, usize)> {
    let mut out = HashMap::new();
    for (mi, m) in song.measures.iter().enumerate() {
        for vdef in &song.voices {
            let Some(v) = m.voices.iter().find(|v| v.id == vdef.id) else { continue };
            for (ei, e) in v.events.iter().enumerate() {
                for l in &e.lyrics {
                    out.entry(l.verse).or_insert_with(|| (mi, v.id.clone(), ei));
                }
            }
        }
    }
    out
}

/// Accidental alteration implied by the key signature for a step.
fn key_alter(fifths: i32, step: char) -> i8 {
    const SHARPS: [char; 7] = ['F', 'C', 'G', 'D', 'A', 'E', 'B'];
    const FLATS: [char; 7] = ['B', 'E', 'A', 'D', 'G', 'C', 'F'];
    if fifths > 0 {
        if SHARPS[..fifths.min(7) as usize].contains(&step) {
            return 1;
        }
    } else if fifths < 0 && FLATS[..(-fifths).min(7) as usize].contains(&step) {
        return -1;
    }
    0
}

fn accidental_name(alter: i8) -> &'static str {
    match alter {
        -2 => "flat-flat",
        -1 => "flat",
        1 => "sharp",
        2 => "double-sharp",
        _ => "natural",
    }
}

/// Which notes need a printed accidental in one measure of one part.
/// Key: (voice id, event index, pitch index).
fn compute_accidentals(m: &PMeasure, mi: usize, plan: &PartPlan, ties: &TieMap) -> HashMap<(String, usize, usize), &'static str> {
    let mut out = HashMap::new();
    let nstaves = plan.clefs.len().max(1);
    for staff in 0..nstaves {
        // (onset, voice order, event index, pitch index, pitch, tied-in)
        let mut notes: Vec<(u32, usize, usize, usize, Pitch, bool, String)> = Vec::new();
        for (order, va) in plan.voices.iter().enumerate() {
            if va.staff != staff {
                continue;
            }
            let Some(v) = m.voices.iter().find(|v| v.id == va.voice_id) else { continue };
            let mut t = 0u32;
            for (ei, e) in v.events.iter().enumerate() {
                let stops = ties.get(&(mi, v.id.clone(), ei)).map(|x| x.1.clone());
                for (pi, p) in e.pitches().iter().enumerate() {
                    let tied = stops.as_ref().and_then(|s| s.get(pi).copied()).unwrap_or(false);
                    notes.push((t, order, ei, pi, *p, tied, v.id.clone()));
                }
                t += e.ticks;
            }
        }
        notes.sort_by_key(|n| (n.0, n.1, n.2, n.3));
        let mut state: HashMap<(char, i8), i8> = HashMap::new();
        for (_, _, ei, pi, p, tied, vid) in notes {
            let cur = *state.get(&(p.step, p.octave)).unwrap_or(&key_alter(m.key.fifths, p.step));
            if p.alter != cur && !tied {
                out.insert((vid, ei, pi), accidental_name(p.alter));
            }
            state.insert((p.step, p.octave), p.alter);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Element helpers
// ---------------------------------------------------------------------------

fn write_clef(w: &mut W, number: Option<usize>, clef: &str) {
    let tag = match number {
        Some(n) => format!("clef number=\"{n}\""),
        None => "clef".into(),
    };
    w.open(&tag);
    let (sign, line, oct): (&str, Option<u8>, i8) = match clef.trim().to_ascii_lowercase().as_str() {
        "bass" | "f" => ("F", Some(4), 0),
        "bass8vb" => ("F", Some(4), -1),
        "treble8vb" | "tenor8vb" | "treble-8" | "g8vb" => ("G", Some(2), -1),
        "treble8va" => ("G", Some(2), 1),
        "alto" | "c3" => ("C", Some(3), 0),
        "tenor" | "c4" => ("C", Some(4), 0),
        "percussion" => ("percussion", None, 0),
        _ => ("G", Some(2), 0),
    };
    w.leaf("sign", "", sign);
    if let Some(l) = line {
        w.leaf("line", "", &l.to_string());
    }
    if oct != 0 {
        w.leaf("clef-octave-change", "", &oct.to_string());
    }
    w.close("clef");
}

fn write_key(w: &mut W, key: &KeySig) {
    w.open("key");
    w.leaf("fifths", "", &key.fifths.clamp(-7, 7).to_string());
    let mode = if key.mode.trim().is_empty() { "major".to_string() } else { key.mode.trim().to_ascii_lowercase() };
    w.leaf("mode", "", &mode);
    w.close("key");
}

fn write_time(w: &mut W, time: &crate::model::notation::TimeSig) {
    match time.symbol {
        Some(sym) => w.open(&format!("time symbol=\"{sym}\"")),
        None => w.open("time"),
    }
    w.leaf("beats", "", &time.beats.to_string());
    w.leaf("beat-type", "", &time.beat_type.to_string());
    w.close("time");
}

fn normalize_ending_numbers(s: &str) -> String {
    let parts: Vec<&str> = s.split(|c: char| c == ',' || c.is_whitespace()).map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        "1".into()
    } else {
        parts.join(", ")
    }
}

/// Parse a chord symbol such as "G7", "F#m7b5", "Bb/D".
fn parse_chord_symbol(s: &str) -> Option<(char, i8, String, Option<(char, i8)>)> {
    let s = s.trim();
    let mut chars = s.chars();
    let root = chars.next()?.to_ascii_uppercase();
    if !('A'..='G').contains(&root) {
        return None;
    }
    let rest: String = chars.collect();
    let (body, bass) = match rest.rsplit_once('/') {
        Some((b, bass)) if !bass.is_empty() => (b.to_string(), Some(bass.to_string())),
        _ => (rest, None),
    };
    let take_alter = |t: &str| -> (i8, usize) {
        let mut alter = 0i8;
        let mut used = 0usize;
        for c in t.chars() {
            match c {
                '#' | '♯' => alter += 1,
                'b' | '♭' => alter -= 1,
                _ => break,
            }
            used += c.len_utf8();
            if alter.abs() >= 2 {
                break;
            }
        }
        (alter, used)
    };
    // Avoid reading "b" of a kind like "b5" only when it follows the root directly: "Bb" is B flat.
    let (alter, used) = take_alter(&body);
    let kind = body[used..].to_string();
    let bass = match bass {
        Some(b) => {
            let mut bc = b.chars();
            let step = bc.next()?.to_ascii_uppercase();
            if !('A'..='G').contains(&step) {
                return None;
            }
            let tail: String = bc.collect();
            let (ba, _) = take_alter(&tail);
            Some((step, ba))
        }
        None => None,
    };
    Some((root, alter, kind, bass))
}

fn chord_kind(kind: &str) -> &'static str {
    match kind.trim() {
        "" | "maj" | "M" | "major" => "major",
        "m" | "min" | "-" | "minor" => "minor",
        "7" | "dom7" => "dominant",
        "maj7" | "M7" | "Δ" | "Δ7" | "ma7" => "major-seventh",
        "m7" | "min7" | "-7" | "mi7" => "minor-seventh",
        "dim" | "°" | "o" => "diminished",
        "dim7" | "°7" | "o7" => "diminished-seventh",
        "m7b5" | "ø" | "ø7" | "min7b5" | "-7b5" => "half-diminished",
        "aug" | "+" => "augmented",
        "sus4" | "sus" => "suspended-fourth",
        "sus2" => "suspended-second",
        "6" | "maj6" => "major-sixth",
        "m6" | "min6" => "minor-sixth",
        "9" => "dominant-ninth",
        "maj9" | "M9" => "major-ninth",
        "m9" | "min9" => "minor-ninth",
        "11" => "dominant-11th",
        "13" => "dominant-13th",
        "mmaj7" | "m(maj7)" | "minmaj7" => "major-minor",
        "aug7" | "+7" | "7#5" => "augmented-seventh",
        "5" => "power",
        _ => "other",
    }
}

fn write_harmony(w: &mut W, symbol: &str, staff: Option<usize>) -> bool {
    let Some((root, alter, kind, bass)) = parse_chord_symbol(symbol) else { return false };
    w.open("harmony");
    w.open("root");
    w.leaf("root-step", "", &root.to_string());
    if alter != 0 {
        w.leaf("root-alter", "", &alter.to_string());
    }
    w.close("root");
    let k = chord_kind(&kind);
    w.leaf("kind", &attr("text", &kind), k);
    if let Some((bs, ba)) = bass {
        w.open("bass");
        w.leaf("bass-step", "", &bs.to_string());
        if ba != 0 {
            w.leaf("bass-alter", "", &ba.to_string());
        }
        w.close("bass");
    }
    if let Some(s) = staff {
        w.leaf("staff", "", &(s + 1).to_string());
    }
    w.close("harmony");
    true
}

/// A `<direction>` with one or more direction-types given as raw writers.
fn write_direction(
    w: &mut W,
    placement: &str,
    voice: Option<u32>,
    staff: Option<usize>,
    sound_tempo: Option<f32>,
    body: impl FnOnce(&mut W),
) {
    w.open(&format!("direction placement=\"{placement}\""));
    body(w);
    if let Some(v) = voice {
        w.leaf("voice", "", &v.to_string());
    }
    if let Some(s) = staff {
        w.leaf("staff", "", &(s + 1).to_string());
    }
    if let Some(t) = sound_tempo {
        w.empty(&format!("sound tempo=\"{}\"", (t * 100.0).round() / 100.0));
    }
    w.close("direction");
}

fn words(w: &mut W, text: &str, extra_attrs: &str) {
    w.open("direction-type");
    w.leaf("words", extra_attrs, text);
    w.close("direction-type");
}

fn beat_unit(beat: &str) -> (&'static str, bool) {
    match beat.trim() {
        "h" => ("half", false),
        "h." => ("half", true),
        "q." => ("quarter", true),
        "e" => ("eighth", false),
        "e." => ("eighth", true),
        "w" => ("whole", false),
        _ => ("quarter", false),
    }
}

// ---------------------------------------------------------------------------
// Note writing
// ---------------------------------------------------------------------------

struct NoteCtx<'a> {
    mi: usize,
    va: &'a VoiceAssign,
    multi_staff: bool,
    ties: &'a TieMap,
    accidentals: &'a HashMap<(String, usize, usize), &'static str>,
    first_lyric: &'a HashMap<u32, (usize, String, usize)>,
    verse_numbers: bool,
}

/// Tuplet start/stop flags per event of a voice in one measure.
fn tuplet_flags(events: &[Event]) -> Vec<(bool, bool)> {
    let mut flags = vec![(false, false); events.len()];
    let mut group: Option<((u32, u32), u32, u32, usize)> = None; // (ratio, sum, target, last index)
    for (i, e) in events.iter().enumerate() {
        if e.grace {
            continue;
        }
        match (e.dur.tuplet, group) {
            (Some(r), Some((gr, sum, target, _))) if gr == r && sum < target => {
                let sum = sum + e.ticks;
                group = Some((gr, sum, target, i));
                if sum >= target {
                    flags[i].1 = true;
                    group = None;
                }
            }
            (t, g) => {
                if let Some((_, _, _, last)) = g {
                    flags[last].1 = true;
                }
                group = None;
                if let Some(r) = t {
                    let target = r.1 * e.dur.untupled_ticks();
                    flags[i].0 = true;
                    if e.ticks >= target {
                        flags[i].1 = true;
                    } else {
                        group = Some((r, e.ticks, target, i));
                    }
                }
            }
        }
    }
    if let Some((_, _, _, last)) = group {
        flags[last].1 = true;
    }
    flags
}

fn write_event_directions(w: &mut W, e: &Event, ctx: &NoteCtx) {
    let staff = if ctx.multi_staff { Some(ctx.va.staff) } else { None };
    let voice = Some(ctx.va.xml_voice);
    for sym in &e.chord_symbols {
        if !write_harmony(w, sym, staff) {
            write_direction(w, "above", voice, staff, None, |w| words(w, sym, ""));
        }
    }
    for d in &e.dynamics {
        write_direction(w, "below", voice, staff, None, |w| {
            w.open("direction-type");
            w.open("dynamics");
            w.empty(d);
            w.close("dynamics");
            w.close("direction-type");
        });
    }
    for h in &e.hairpins {
        let ty = match h {
            Hairpin::Crescendo => "crescendo",
            Hairpin::Diminuendo => "diminuendo",
            Hairpin::Stop => "stop",
        };
        write_direction(w, "below", voice, staff, None, |w| {
            w.open("direction-type");
            w.empty(&format!("wedge type=\"{ty}\" number=\"1\""));
            w.close("direction-type");
        });
    }
    for t in &e.texts {
        write_direction(w, "above", voice, staff, None, |w| words(w, t, "font-style=\"italic\""));
    }
}

fn write_event(w: &mut W, e: &Event, ei: usize, ctx: &NoteCtx, tuplet: (bool, bool), slur_stack: &mut Vec<u32>) {
    write_event_directions(w, e, ctx);

    // Slur numbers: stops first (LIFO), then starts.
    let mut slur_stop_nums = Vec::new();
    for _ in 0..e.slur_stops {
        if let Some(n) = slur_stack.pop() {
            slur_stop_nums.push(n);
        }
    }
    let mut slur_start_nums = Vec::new();
    for _ in 0..e.slur_starts {
        let n = (1..=16).find(|n| !slur_stack.contains(n)).unwrap_or(1);
        slur_stack.push(n);
        slur_start_nums.push(n);
    }

    let tie_info = ctx.ties.get(&(ctx.mi, ctx.va.voice_id.clone(), ei));
    let pitches: Vec<Option<Pitch>> = match &e.kind {
        EventKind::Note(ps) => ps.iter().copied().map(Some).collect(),
        _ => vec![None],
    };

    for (pi, p) in pitches.iter().enumerate() {
        w.open("note");
        if e.grace {
            w.empty("grace");
        }
        if pi > 0 {
            w.empty("chord");
        }
        match (&e.kind, p) {
            (EventKind::Note(_), Some(p)) => {
                w.open("pitch");
                w.leaf("step", "", &p.step.to_string());
                if p.alter != 0 {
                    w.leaf("alter", "", &p.alter.to_string());
                }
                w.leaf("octave", "", &p.octave.to_string());
                w.close("pitch");
            }
            (EventKind::MeasureRest, _) => w.empty("rest measure=\"yes\""),
            _ => w.empty("rest"),
        }
        if !e.grace {
            w.leaf("duration", "", &e.ticks.to_string());
        }
        let tie_start = tie_info.map(|t| t.0.get(pi).copied().unwrap_or(false)).unwrap_or(false);
        let tie_stop = tie_info.map(|t| t.1.get(pi).copied().unwrap_or(false)).unwrap_or(false);
        if tie_stop {
            w.empty("tie type=\"stop\"");
        }
        if tie_start {
            w.empty("tie type=\"start\"");
        }
        w.leaf("voice", "", &ctx.va.xml_voice.to_string());
        if e.kind != EventKind::MeasureRest {
            w.leaf("type", "", e.dur.base.musicxml());
            for _ in 0..e.dur.dots {
                w.empty("dot");
            }
        }
        if let Some(acc) = ctx.accidentals.get(&(ctx.va.voice_id.clone(), ei, pi)) {
            w.leaf("accidental", "", acc);
        }
        if let Some((actual, normal)) = e.dur.tuplet {
            w.open("time-modification");
            w.leaf("actual-notes", "", &actual.to_string());
            w.leaf("normal-notes", "", &normal.to_string());
            w.close("time-modification");
        }
        if e.is_note() && (ctx.va.stem == "up" || ctx.va.stem == "down") {
            w.leaf("stem", "", &ctx.va.stem);
        }
        if ctx.multi_staff {
            w.leaf("staff", "", &(ctx.va.staff + 1).to_string());
        }

        // Notations
        let first = pi == 0;
        let has_artic = first && (e.accent || e.staccato || e.tenuto || e.breath);
        let has_notations = tie_start
            || tie_stop
            || (first && (!slur_stop_nums.is_empty() || !slur_start_nums.is_empty() || tuplet.0 || tuplet.1 || e.fermata || has_artic));
        if has_notations {
            w.open("notations");
            if tie_stop {
                w.empty("tied type=\"stop\"");
            }
            if tie_start {
                w.empty("tied type=\"start\"");
            }
            if first {
                for n in &slur_stop_nums {
                    w.empty(&format!("slur type=\"stop\" number=\"{n}\""));
                }
                for n in &slur_start_nums {
                    w.empty(&format!("slur type=\"start\" number=\"{n}\""));
                }
                if tuplet.0 {
                    w.empty("tuplet type=\"start\" bracket=\"yes\"");
                }
                if tuplet.1 {
                    w.empty("tuplet type=\"stop\"");
                }
                if has_artic {
                    w.open("articulations");
                    if e.accent {
                        w.empty("accent");
                    }
                    if e.staccato {
                        w.empty("staccato");
                    }
                    if e.tenuto {
                        w.empty("tenuto");
                    }
                    if e.breath {
                        w.empty("breath-mark");
                    }
                    w.close("articulations");
                }
                if e.fermata {
                    w.leaf("fermata", "type=\"upright\"", "");
                }
            }
            w.close("notations");
        }

        if first && !e.grace {
            for l in &e.lyrics {
                w.open(&format!("lyric number=\"{}\"", l.verse));
                w.leaf("syllabic", "", l.syllabic.musicxml());
                let mut text = l.text.clone();
                if ctx.verse_numbers {
                    if let Some((m0, v0, e0)) = ctx.first_lyric.get(&l.verse) {
                        if *m0 == ctx.mi && *v0 == ctx.va.voice_id && *e0 == ei {
                            text = format!("{}. {}", l.verse, text);
                        }
                    }
                }
                w.leaf("text", "", &text);
                if l.extend {
                    w.empty("extend");
                }
                w.close("lyric");
            }
        }
        w.close("note");
    }
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn write_header(w: &mut W, song: &ParsedSong, opts: &MusicXmlOptions, source: &SourceInfo) {
    let meta = &song.meta;
    let title = if meta.title.trim().is_empty() { meta.first_line.trim() } else { meta.title.trim() };

    w.open("work");
    if !meta.number.trim().is_empty() {
        w.leaf("work-number", "", meta.number.trim());
    }
    w.leaf("work-title", "", title);
    w.close("work");
    if !meta.number.trim().is_empty() {
        w.leaf("movement-number", "", meta.number.trim());
    }
    w.leaf("movement-title", "", title);

    w.open("identification");
    let composer = if !meta.composer.trim().is_empty() { meta.composer.trim() } else { meta.music_credit.trim() };
    let lyricist = if !meta.lyricist.trim().is_empty() { meta.lyricist.trim() } else { meta.words_credit.trim() };
    for (ty, val) in
        [("composer", composer), ("lyricist", lyricist), ("arranger", meta.arranger.trim()), ("translator", meta.translator.trim())]
    {
        if !val.is_empty() {
            w.leaf("creator", &attr("type", ty), val);
        }
    }
    if !meta.copyright.trim().is_empty() {
        w.leaf("rights", "", meta.copyright.trim());
    }
    w.open("encoding");
    w.leaf("software", "", "Sheet Music Convertor");
    w.leaf("encoding-date", "", &chrono::Local::now().format("%Y-%m-%d").to_string());
    w.empty("supports element=\"accidental\" type=\"yes\"");
    w.empty("supports element=\"beam\" type=\"no\"");
    w.empty("supports element=\"stem\" type=\"yes\"");
    w.empty("supports attribute=\"new-system\" element=\"print\" type=\"yes\" value=\"yes\"");
    w.empty("supports attribute=\"new-page\" element=\"print\" type=\"yes\" value=\"yes\"");
    w.close("encoding");
    let src = source.describe();
    if !src.is_empty() {
        w.leaf("source", "", &src);
    }
    let mut misc: Vec<(&str, String)> = vec![
        ("tune-name", meta.tune_name.trim().to_string()),
        ("meter", meta.meter.trim().to_string()),
        ("subtitle", meta.subtitle.trim().to_string()),
        ("scripture", meta.scripture.trim().to_string()),
        ("topics", meta.topics.join("; ")),
        ("first-line", meta.first_line.trim().to_string()),
        ("refrain-first-line", meta.refrain_first_line.trim().to_string()),
        ("words-credit", meta.words_credit.trim().to_string()),
        ("music-credit", meta.music_credit.trim().to_string()),
        ("language", meta.language.trim().to_string()),
        ("other-text", meta.other_text.trim().to_string()),
    ];
    if source.kind.is_book() {
        misc.extend([
            ("book-type", source.kind.noun().to_string()),
            ("book-title", source.book.title.trim().to_string()),
            ("book-publisher", source.book.publisher.trim().to_string()),
            ("book-year", source.book.year.trim().to_string()),
            ("book-edition", source.book.edition.trim().to_string()),
            ("book-isbn", source.book.isbn.trim().to_string()),
            ("book-page", source.pages.trim().to_string()),
        ]);
    }
    let misc: Vec<_> = misc.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    if !misc.is_empty() {
        w.open("miscellaneous");
        for (k, v) in misc {
            w.leaf("miscellaneous-field", &attr("name", k), &v);
        }
        w.close("miscellaneous");
    }
    w.close("identification");

    w.open("defaults");
    w.open("scaling");
    w.leaf("millimeters", "", &MM.to_string());
    w.leaf("tenths", "", &TENTHS.to_string());
    w.close("scaling");
    w.open("page-layout");
    w.leaf("page-height", "", &PAGE_H.to_string());
    w.leaf("page-width", "", &PAGE_W.to_string());
    w.open("page-margins type=\"both\"");
    for side in ["left-margin", "right-margin", "top-margin", "bottom-margin"] {
        w.leaf(side, "", &MARGIN.to_string());
    }
    w.close("page-margins");
    w.close("page-layout");
    w.close("defaults");

    if opts.credits {
        let credit = |w: &mut W, ty: Option<&str>, x: f32, y: f32, justify: &str, valign: &str, size: u32, text: &str| {
            if text.trim().is_empty() {
                return;
            }
            w.open("credit page=\"1\"");
            if let Some(t) = ty {
                w.leaf("credit-type", "", t);
            }
            w.leaf(
                "credit-words",
                &format!("default-x=\"{x}\" default-y=\"{y}\" justify=\"{justify}\" valign=\"{valign}\" font-size=\"{size}\""),
                text.trim(),
            );
            w.close("credit");
        };
        let top = PAGE_H - MARGIN;
        let center = PAGE_W / 2.0;
        let left = MARGIN;
        let right = PAGE_W - MARGIN;
        credit(w, Some("title"), center, top, "center", "top", 22, title);
        credit(w, Some("subtitle"), center, top - 50.0, "center", "top", 14, &meta.subtitle);
        if !meta.number.trim().is_empty() {
            credit(w, None, left, top, "left", "top", 20, meta.number.trim());
        }
        let words_line = if !meta.words_credit.trim().is_empty() { meta.words_credit.trim().to_string() } else { lyricist.to_string() };
        let music_line = if !meta.music_credit.trim().is_empty() { meta.music_credit.trim().to_string() } else { composer.to_string() };
        credit(w, Some("lyricist"), left, top - 95.0, "left", "bottom", 10, &words_line);
        credit(w, Some("composer"), right, top - 95.0, "right", "bottom", 10, &music_line);
        let tune: Vec<&str> = [meta.tune_name.trim(), meta.meter.trim()].into_iter().filter(|s| !s.is_empty()).collect();
        if !tune.is_empty() {
            credit(w, None, right, top - 70.0, "right", "bottom", 9, &tune.join("   "));
        }
        credit(w, Some("rights"), center, MARGIN, "center", "bottom", 8, &meta.copyright);
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Write the song as MusicXML 4.0 partwise.
pub fn write_musicxml(song: &ParsedSong, opts: &MusicXmlOptions, source: &SourceInfo) -> String {
    let mut w = W::new();
    w.s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n");
    w.s.push_str(
        "<!DOCTYPE score-partwise PUBLIC \"-//Recordare//DTD MusicXML 4.0 Partwise//EN\" \"http://www.musicxml.org/dtds/partwise.dtd\">\n",
    );
    w.open("score-partwise version=\"4.0\"");
    write_header(&mut w, song, opts, source);

    let parts = plan_parts(song, opts.layout);

    w.open("part-list");
    let grouped = parts.len() > 1;
    if grouped {
        w.open("part-group type=\"start\" number=\"1\"");
        w.leaf("group-symbol", "", "bracket");
        w.leaf("group-barline", "", "yes");
        w.close("part-group");
    }
    for p in &parts {
        w.open(&format!("score-part id=\"{}\"", p.id));
        if p.show_name {
            w.leaf("part-name", "", &p.name);
        } else {
            w.leaf("part-name", "print-object=\"no\"", &p.name);
        }
        if !p.abbrev.is_empty() {
            if p.show_name {
                w.leaf("part-abbreviation", "", &p.abbrev);
            } else {
                w.leaf("part-abbreviation", "print-object=\"no\"", &p.abbrev);
            }
        }
        w.open(&format!("score-instrument id=\"{}-I1\"", p.id));
        w.leaf("instrument-name", "", &p.name);
        w.close("score-instrument");
        w.open(&format!("midi-instrument id=\"{}-I1\"", p.id));
        let idx: usize = p.id.trim_start_matches('P').parse().unwrap_or(1);
        let mut channel = idx as u32;
        if channel >= 10 {
            channel += 1; // skip percussion channel
        }
        w.leaf("midi-channel", "", &channel.min(16).to_string());
        w.leaf("midi-program", "", &p.program.to_string());
        w.leaf("volume", "", "80");
        w.close("midi-instrument");
        w.close("score-part");
    }
    if grouped {
        w.empty("part-group type=\"stop\" number=\"1\"");
    }
    w.close("part-list");

    let ties = compute_tie_map(song);
    let first_lyric = first_lyric_locations(song);

    for (part_idx, plan) in parts.iter().enumerate() {
        write_part(&mut w, song, opts, plan, part_idx == 0, &ties, &first_lyric);
    }

    w.close("score-partwise");
    w.s
}

fn write_part(
    w: &mut W,
    song: &ParsedSong,
    opts: &MusicXmlOptions,
    plan: &PartPlan,
    is_first_part: bool,
    ties: &TieMap,
    first_lyric: &HashMap<u32, (usize, String, usize)>,
) {
    w.open(&format!("part id=\"{}\"", plan.id));
    let nstaves = plan.clefs.len().max(1);
    let multi_staff = nstaves > 1;
    let mut slur_stacks: HashMap<String, Vec<u32>> = HashMap::new();
    let mut used_numbers: HashSet<String> = HashSet::new();
    let n_measures = song.measures.len();

    for (mi, m) in song.measures.iter().enumerate() {
        // Unique measure number.
        let mut number = m.number.to_string();
        if used_numbers.contains(&number) {
            let mut k = 1;
            while used_numbers.contains(&format!("{}X{k}", m.number)) {
                k += 1;
            }
            number = format!("{}X{k}", m.number);
        }
        used_numbers.insert(number.clone());
        if m.implicit {
            w.open(&format!("measure number=\"{number}\" implicit=\"yes\""));
        } else {
            w.open(&format!("measure number=\"{number}\""));
        }

        // System / page breaks requested by the previous measure.
        if opts.keep_line_breaks && mi > 0 {
            let prev = &song.measures[mi - 1];
            if prev.page_break {
                w.empty("print new-page=\"yes\"");
            } else if prev.line_break {
                w.empty("print new-system=\"yes\"");
            }
        }

        // Left barline: repeat start and/or ending start.
        let ending_start = m.ending.as_ref().filter(|e| e.start);
        if m.repeat_start || ending_start.is_some() {
            w.open("barline location=\"left\"");
            if m.repeat_start {
                w.leaf("bar-style", "", "heavy-light");
            }
            if let Some(e) = ending_start {
                let nums = normalize_ending_numbers(&e.numbers);
                let label = format!("{}.", nums.replace(", ", ". "));
                w.leaf("ending", &format!("number=\"{}\" type=\"start\"", esc(&nums)), &label);
            }
            if m.repeat_start {
                w.empty("repeat direction=\"forward\"");
            }
            w.close("barline");
        }

        // Attributes.
        if mi == 0 || m.key_change.is_some() || m.time_change.is_some() {
            w.open("attributes");
            if mi == 0 {
                w.leaf("divisions", "", &PPQ.to_string());
                write_key(w, &m.key);
                write_time(w, &m.time);
                if multi_staff {
                    w.leaf("staves", "", &nstaves.to_string());
                }
                for (i, c) in plan.clefs.iter().enumerate() {
                    write_clef(w, if multi_staff { Some(i + 1) } else { None }, c);
                }
            } else {
                if let Some(k) = &m.key_change {
                    write_key(w, k);
                }
                if let Some(t) = &m.time_change {
                    write_time(w, t);
                }
            }
            w.close("attributes");
        }

        // Measure-level directions (first part only).
        if is_first_part {
            let top_staff = if multi_staff { Some(0) } else { None };
            if mi == 0 {
                let bpm = song.tempo_bpm();
                let tempo = song.tempo.clone().unwrap_or_default();
                let show_metronome = !tempo.suggested && tempo.bpm.map(|b| b > 10.0).unwrap_or(false);
                let text = tempo.text.trim().to_string();
                if text.is_empty() && !show_metronome {
                    // Playback tempo only: a bare <sound> avoids an empty text object in MuseScore.
                    w.empty(&format!("sound tempo=\"{}\"", (bpm * 100.0).round() / 100.0));
                } else {
                    write_direction(w, "above", None, top_staff, Some(bpm), |w| {
                        if !text.is_empty() {
                            w.open("direction-type");
                            w.leaf("words", "font-weight=\"bold\"", &text);
                            w.close("direction-type");
                        }
                        if show_metronome {
                            let (unit, dot) = beat_unit(&tempo.beat);
                            w.open("direction-type");
                            if text.is_empty() {
                                w.open("metronome");
                            } else {
                                w.open("metronome parentheses=\"yes\"");
                            }
                            w.leaf("beat-unit", "", unit);
                            if dot {
                                w.empty("beat-unit-dot");
                            }
                            w.leaf("per-minute", "", &format!("{}", tempo.bpm.unwrap_or(bpm).round()));
                            w.close("metronome");
                            w.close("direction-type");
                        }
                    });
                }
            }
            if !m.section.is_empty() {
                let s = m.section.clone();
                write_direction(w, "above", None, top_staff, None, |w| words(w, &s, "font-weight=\"bold\""));
            }
            if !m.text.is_empty() {
                let t = m.text.clone();
                write_direction(w, "above", None, top_staff, None, |w| words(w, &t, "font-style=\"italic\""));
            }
            if opts.extra_stanzas && mi + 1 == n_measures && !song.extra_stanzas.is_empty() {
                let text = song
                    .extra_stanzas
                    .iter()
                    .map(|s| if s.number > 0 { format!("{}. {}", s.number, s.text.trim()) } else { s.text.trim().to_string() })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let bottom = if multi_staff { Some(nstaves - 1) } else { None };
                write_direction(w, "below", None, bottom, None, |w| words(w, &text, "justify=\"left\""));
            }
        }

        // Voices.
        let accidentals = compute_accidentals(m, mi, plan, ties);
        let measure_len = if m.actual_ticks > 0 { m.actual_ticks } else { m.expected_ticks };
        let mut pos: u32 = 0;
        for staff in 0..nstaves {
            let assigns: Vec<&VoiceAssign> = plan.voices.iter().filter(|v| v.staff == staff).collect();
            let mut wrote_any = false;
            for va in assigns.iter() {
                let Some(pv) = m.voices.iter().find(|v| v.id == va.voice_id) else { continue };
                if pv.events.is_empty() {
                    continue;
                }
                if pos > 0 {
                    w.open("backup");
                    w.leaf("duration", "", &pos.to_string());
                    w.close("backup");
                    pos = 0;
                }
                let ctx = NoteCtx { mi, va, multi_staff, ties, accidentals: &accidentals, first_lyric, verse_numbers: opts.verse_numbers };
                let flags = tuplet_flags(&pv.events);
                let stack = slur_stacks.entry(va.voice_id.clone()).or_default();
                for (ei, e) in pv.events.iter().enumerate() {
                    write_event(w, e, ei, &ctx, flags[ei], stack);
                    pos += e.ticks;
                }
                wrote_any = true;
            }
            if !wrote_any {
                // Keep empty staves well-formed with a whole-measure rest.
                if pos > 0 {
                    w.open("backup");
                    w.leaf("duration", "", &pos.to_string());
                    w.close("backup");
                }
                let voice = assigns.first().map(|v| v.xml_voice).unwrap_or(staff as u32 * 4 + 1);
                w.open("note");
                w.empty("rest measure=\"yes\"");
                w.leaf("duration", "", &measure_len.to_string());
                w.leaf("voice", "", &voice.to_string());
                if multi_staff {
                    w.leaf("staff", "", &(staff + 1).to_string());
                }
                w.close("note");
                pos = measure_len;
            }
        }

        // Right barline.
        let ending_stop = m.ending.as_ref().and_then(|e| e.stop.map(|closed| (e, closed)));
        if m.barline != Barline::Regular || ending_stop.is_some() {
            w.open("barline location=\"right\"");
            let style = match m.barline {
                Barline::Regular => "regular",
                Barline::Double => "light-light",
                Barline::Final | Barline::RepeatEnd => "light-heavy",
                Barline::Dashed => "dashed",
                Barline::None => "none",
            };
            w.leaf("bar-style", "", style);
            if let Some((e, closed)) = ending_stop {
                let nums = normalize_ending_numbers(&e.numbers);
                let ty = if closed { "stop" } else { "discontinue" };
                w.empty(&format!("ending number=\"{}\" type=\"{ty}\"", esc(&nums)));
            }
            if m.barline == Barline::RepeatEnd {
                w.empty("repeat direction=\"backward\"");
            }
            w.close("barline");
        }

        w.close("measure");
    }
    w.close("part");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{MusicXmlOptions, ScoreLayout};
    use crate::model::project::{CollectionKind, SourceInfo};
    use crate::model::score::{KeySig, Measure, Song, Stanza, Tempo, VoiceMeasure};
    use crate::model::validate::{error_count, validate};
    use std::collections::BTreeMap;

    fn vm(notes: &str, lyrics: &[(u32, &str)]) -> VoiceMeasure {
        VoiceMeasure { notes: notes.into(), lyrics: lyrics.iter().map(|(k, v)| (*k, v.to_string())).collect::<BTreeMap<_, _>>() }
    }

    fn measure(parts: &[(&str, VoiceMeasure)]) -> Measure {
        let mut m = Measure::default();
        for (id, v) in parts {
            m.voices.insert(id.to_string(), v.clone());
        }
        m
    }

    fn test_song() -> Song {
        let mut song = Song::default();
        song.meta.title = "Amazing Grace & <Friends>".into();
        song.meta.number = "245".into();
        song.meta.tune_name = "NEW BRITAIN".into();
        song.meta.meter = "C.M.".into();
        song.meta.words_credit = "Words: John Newton, 1779".into();
        song.meta.music_credit = "Music: American folk melody".into();
        song.meta.copyright = "Public domain".into();
        song.key = KeySig { fifths: 1, mode: "major".into() };
        song.time = "3/4".into();
        song.tempo = Some(Tempo { text: "Gently".into(), bpm: Some(88.0), beat: "q".into(), suggested: false });
        song.verse_count = 2;

        let mut m0 = measure(&[
            ("S", vm("D4:q", &[(1, "A-"), (2, "Through")])),
            ("A", vm("B3:q", &[])),
            ("T", vm("G3:q", &[])),
            ("B", vm("G2:q", &[])),
        ]);
        m0.pickup = true;

        let m1 = measure(&[
            ("S", vm("G4:q{c:G} B4:e( G4:e) B4:q~", &[(1, "maz- ing _ grace"), (2, "ma- ny dan- gers")])),
            ("A", vm("B3:q D4:q D4:q", &[])),
            ("T", vm("D4:q D4:q G3:q", &[])),
            ("B", vm("G2:q{mf} G2:q G2:q", &[])),
        ]);

        let mut m2 = measure(&[
            ("S", vm("B4:h A4:e/3 G4:e/3 A4:e/3^", &[(1, "how sweet the"), (2, "toils and snares")])),
            ("A", vm("D4:h D4:q", &[])),
            ("T", vm("G3:h F#3:q", &[])),
            ("B", vm("G2:h D3:q{c:D7/F#}", &[])),
        ]);
        m2.line_break = true;

        let mut m3 = measure(&[
            ("S", vm("gB4:s A4:q F#4:q( E4:q)", &[(1, "sound that _")])),
            ("A", vm("F#4:q D4:q C4:q", &[])),
            ("T", vm("A3:q A3:q A3:q", &[])),
            ("B", vm("D3:q D3:q A2:q", &[])),
        ]);
        m3.key = Some(KeySig { fifths: 2, mode: "major".into() });
        m3.repeat_start = true;
        m3.section = "Refrain".into();

        let mut m4 =
            measure(&[("S", vm("D4:h.", &[(1, "saved")])), ("A", vm("D4:h.", &[])), ("T", vm("R", &[])), ("B", vm("r:q D3:h", &[]))]);
        m4.ending = Some("1".into());
        m4.barline = "repeat-end".into();

        let mut m5 = measure(&[("S", vm("D4:h^", &[(1, "me.")])), ("A", vm("D4:h", &[])), ("T", vm("A3:h", &[])), ("B", vm("D3:h", &[]))]);
        m5.ending = Some("2".into());
        m5.barline = "final".into();
        m5.partial = true;

        song.measures = vec![m0, m1, m2, m3, m4, m5];
        song.extra_stanzas = vec![Stanza { number: 3, text: "When we've been there\nten thousand years".into() }];
        song
    }

    fn source() -> SourceInfo {
        let mut s = SourceInfo { kind: CollectionKind::Hymnal, ..Default::default() };
        s.book.title = "Test Hymnal".into();
        s.book.year = "1950".into();
        s.pages = "12".into();
        s
    }

    fn parse(xml: &str) -> roxmltree::Document<'_> {
        let opts = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
        roxmltree::Document::parse_with_options(xml, opts).expect("well-formed XML")
    }

    fn child_text<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
        n.children().find(|c| c.has_tag_name(name)).and_then(|c| c.text())
    }

    /// Walk every measure checking the timeline: backups never go negative,
    /// no voice runs past the measure, and every voice fills the measure.
    fn check_timeline(doc: &roxmltree::Document, expected: &[u32]) {
        for part in doc.descendants().filter(|n| n.has_tag_name("part")) {
            let measures: Vec<_> = part.children().filter(|n| n.has_tag_name("measure")).collect();
            assert_eq!(measures.len(), expected.len());
            for (mi, m) in measures.iter().enumerate() {
                let mut pos: i64 = 0;
                let mut max_pos: i64 = 0;
                let mut voice_sums: HashMap<String, i64> = HashMap::new();
                for c in m.children().filter(|c| c.is_element()) {
                    let dur: i64 = child_text(c, "duration").map(|d| d.parse().unwrap()).unwrap_or(0);
                    match c.tag_name().name() {
                        "note" => {
                            let is_chord = c.children().any(|x| x.has_tag_name("chord"));
                            let is_grace = c.children().any(|x| x.has_tag_name("grace"));
                            if is_grace {
                                assert!(child_text(c, "duration").is_none(), "grace note with duration");
                            }
                            if !is_chord && !is_grace {
                                pos += dur;
                                let v = child_text(c, "voice").unwrap().to_string();
                                *voice_sums.entry(v).or_default() += dur;
                            }
                        }
                        "backup" => {
                            pos -= dur;
                            assert!(pos >= 0, "backup below zero in measure {mi}");
                        }
                        "forward" => pos += dur,
                        _ => {}
                    }
                    max_pos = max_pos.max(pos);
                }
                assert_eq!(max_pos as u32, expected[mi], "measure {mi} length");
                for (v, s) in voice_sums {
                    assert_eq!(s as u32, expected[mi], "measure {mi} voice {v} length");
                }
            }
        }
    }

    fn build(layout: ScoreLayout) -> (String, Vec<u32>) {
        let song = test_song();
        let (parsed, issues) = validate(&song);
        assert_eq!(error_count(&issues), 0, "{issues:#?}");
        let opts = MusicXmlOptions { layout, ..Default::default() };
        let xml = write_musicxml(&parsed, &opts, &source());
        let lens = parsed.measures.iter().map(|m| m.actual_ticks).collect();
        (xml, lens)
    }

    #[test]
    fn hymn_layout_is_well_formed_and_timed() {
        let (xml, lens) = build(ScoreLayout::Hymn);
        let doc = parse(&xml);
        assert_eq!(lens, vec![480, 1440, 1440, 1440, 1440, 960]);
        check_timeline(&doc, &lens);

        let root = doc.root_element();
        assert_eq!(root.attribute("version"), Some("4.0"));
        let div = doc.descendants().find(|n| n.has_tag_name("divisions")).unwrap();
        assert_eq!(div.text(), Some("480"));
        assert_eq!(doc.descendants().filter(|n| n.has_tag_name("part")).count(), 1);
        assert_eq!(doc.descendants().find(|n| n.has_tag_name("staves")).unwrap().text(), Some("2"));

        // Title escaping and metadata.
        let wt = doc.descendants().find(|n| n.has_tag_name("work-title")).unwrap();
        assert_eq!(wt.text(), Some("Amazing Grace & <Friends>"));
        assert!(xml.contains("Amazing Grace &amp; &lt;Friends&gt;"));
        assert!(doc.descendants().any(|n| n.has_tag_name("miscellaneous-field") && n.attribute("name") == Some("tune-name")));
        assert!(doc.descendants().any(|n| n.has_tag_name("source") && n.text() == Some("Test Hymnal (1950), p. 12")));

        // Pickup measure numbering.
        let measures: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("measure")).collect();
        assert_eq!(measures[0].attribute("number"), Some("0"));
        assert_eq!(measures[0].attribute("implicit"), Some("yes"));
        let nums: HashSet<_> = measures.iter().map(|m| m.attribute("number").unwrap()).collect();
        assert_eq!(nums.len(), measures.len(), "measure numbers unique");

        // Voice numbers: S=1, A=2, T=5, B=6.
        let voices: HashSet<_> = doc.descendants().filter(|n| n.has_tag_name("note")).filter_map(|n| child_text(n, "voice")).collect();
        assert_eq!(voices, ["1", "2", "5", "6"].into_iter().collect());

        // Lyrics with verse numbers, syllabic and extend.
        let lyrics: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("lyric")).collect();
        assert!(lyrics.iter().any(|l| l.attribute("number") == Some("1") && child_text(*l, "text") == Some("1. A")));
        assert!(lyrics.iter().any(|l| l.attribute("number") == Some("2") && child_text(*l, "text") == Some("2. Through")));
        assert!(lyrics.iter().any(|l| child_text(*l, "syllabic") == Some("middle")));
        assert!(lyrics.iter().any(|l| l.children().any(|c| c.has_tag_name("extend"))));

        // Ties across the barline.
        assert!(xml.contains("<tie type=\"start\"/>"));
        assert!(xml.contains("<tie type=\"stop\"/>"));
        assert!(xml.contains("<tied type=\"start\"/>"));

        // Triplet.
        assert_eq!(doc.descendants().filter(|n| n.has_tag_name("time-modification")).count(), 3);
        assert!(xml.contains("<tuplet type=\"start\" bracket=\"yes\"/>"));
        assert!(xml.contains("<tuplet type=\"stop\"/>"));

        // Slurs, fermata, grace, chord symbols, dynamics.
        assert!(xml.contains("<slur type=\"start\" number=\"1\"/>"));
        assert!(xml.contains("<slur type=\"stop\" number=\"1\"/>"));
        assert!(xml.contains("<fermata type=\"upright\"></fermata>"));
        assert!(xml.contains("<grace/>"));
        let harmonies: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("harmony")).collect();
        assert_eq!(harmonies.len(), 2);
        assert!(xml.contains("<kind text=\"7\">dominant</kind>"));
        assert!(xml.contains("<bass-step>F</bass-step>"));
        assert!(xml.contains("<mf/>"));

        // Key change in measure index 3 and natural accidental on C4 in D major.
        let m3 = measures[3];
        let fifths: Vec<_> = m3.descendants().filter(|n| n.has_tag_name("fifths")).collect();
        assert_eq!(fifths[0].text(), Some("2"));
        assert!(m3.descendants().any(|n| n.has_tag_name("accidental") && n.text() == Some("natural")));
        // F#4 in D major / F#3 in G major are not marked.
        assert!(!doc.descendants().any(|n| n.has_tag_name("accidental") && n.text() == Some("sharp")));

        // Repeats and endings.
        assert!(xml.contains("<repeat direction=\"forward\"/>"));
        assert!(xml.contains("<repeat direction=\"backward\"/>"));
        assert!(xml.contains("<ending number=\"1\" type=\"start\">1.</ending>"));
        assert!(xml.contains("<ending number=\"1\" type=\"stop\"/>"));
        assert!(xml.contains("<ending number=\"2\" type=\"start\">2.</ending>"));
        assert!(xml.contains("<ending number=\"2\" type=\"discontinue\"/>"));

        // Line break after measure index 2.
        assert!(m3.children().any(|n| n.has_tag_name("print") && n.attribute("new-system") == Some("yes")));

        // Measure rest in the tenor of the last measure.
        assert!(xml.contains("<rest measure=\"yes\"/>"));

        // Tempo and section text.
        assert!(xml.contains("<sound tempo=\"88\"/>"));
        assert!(xml.contains("<per-minute>88</per-minute>"));
        assert!(xml.contains(">Refrain</words>"));
        assert!(xml.contains("3. When we&apos;ve been there\nten thousand years"));

        // Credits.
        assert!(doc.descendants().any(|n| n.has_tag_name("credit-type") && n.text() == Some("title")));
        assert!(doc.descendants().any(|n| n.has_tag_name("credit-type") && n.text() == Some("rights")));
    }

    #[test]
    fn open_score_layout() {
        let (xml, lens) = build(ScoreLayout::OpenScore);
        let doc = parse(&xml);
        check_timeline(&doc, &lens);
        assert_eq!(doc.descendants().filter(|n| n.has_tag_name("part")).count(), 4);
        assert!(xml.contains("<part-group type=\"start\" number=\"1\">"));
        assert!(xml.contains("<group-symbol>bracket</group-symbol>"));
        // Tenor gets the octave-treble clef, bass the F clef.
        assert!(xml.contains("<clef-octave-change>-1</clef-octave-change>"));
        let parts: Vec<_> = doc.descendants().filter(|n| n.has_tag_name("part")).collect();
        let bass_sign = parts[3].descendants().find(|n| n.has_tag_name("sign")).unwrap();
        assert_eq!(bass_sign.text(), Some("F"));
        // Single-staff parts don't emit <staff>.
        assert!(!doc.descendants().any(|n| n.has_tag_name("staff")));
        // All notes in voice 1.
        assert!(doc.descendants().filter(|n| n.has_tag_name("note")).all(|n| child_text(n, "voice") == Some("1")));
        // Lyrics stay with the soprano part.
        assert!(parts[0].descendants().any(|n| n.has_tag_name("lyric")));
        assert!(!parts[1].descendants().any(|n| n.has_tag_name("lyric")));
    }

    #[test]
    fn chord_symbol_parsing() {
        assert_eq!(parse_chord_symbol("G7"), Some(('G', 0, "7".into(), None)));
        assert_eq!(parse_chord_symbol("Bbm7"), Some(('B', -1, "m7".into(), None)));
        assert_eq!(parse_chord_symbol("F#m7b5"), Some(('F', 1, "m7b5".into(), None)));
        assert_eq!(parse_chord_symbol("C/E"), Some(('C', 0, "".into(), Some(('E', 0)))));
        assert_eq!(parse_chord_symbol("D/F#"), Some(('D', 0, "".into(), Some(('F', 1)))));
        assert_eq!(chord_kind("m7b5"), "half-diminished");
        assert_eq!(chord_kind("weird"), "other");
        assert!(parse_chord_symbol("N.C.").is_none());
    }

    #[test]
    fn tuplet_grouping() {
        let evs: Vec<Event> = ["C4:e/3", "D4:e/3", "E4:e/3", "F4:q", "G4:q/3", "A4:e/3"]
            .iter()
            .map(|t| crate::model::notation::parse_token(t, 1920).unwrap())
            .collect();
        let f = tuplet_flags(&evs);
        assert_eq!(f[0], (true, false));
        assert_eq!(f[2], (false, true));
        assert_eq!(f[3], (false, false));
        assert_eq!(f[4], (true, false));
        assert_eq!(f[5], (false, true));
    }

    #[test]
    fn escapes_xml() {
        assert_eq!(esc("a&b<c>\"d'"), "a&amp;b&lt;c&gt;&quot;d&apos;");
    }
}
