//! Transposition of a parsed song with correct enharmonic spelling.
//!
//! Pitches are moved by a diatonic interval (old tonic → new tonic) plus the
//! exact number of semitones, so that e.g. C major +2 gives D major with
//! F♯ rather than G♭.

use super::notation::{EventKind, ParsedSong, Pitch};
use super::score::KeySig;

/// Map a fifths value into -6..=5, preferring flats for the 6-accidental keys
/// (G♭ major / E♭ minor rather than F♯ major / D♯ minor).
fn normalize_fifths(f: i32) -> i32 {
    let r = f.rem_euclid(12);
    if r > 5 {
        r - 12
    } else {
        r
    }
}

/// Diatonic step index (0 = C … 6 = B) of the key's tonic.
fn tonic_step(key: &KeySig) -> i32 {
    // Each fifth is 4 diatonic steps up; the relative minor is 2 steps below.
    let major = (4 * key.fifths).rem_euclid(7);
    if key.is_minor() {
        (major - 2).rem_euclid(7)
    } else {
        major
    }
}

/// Key signature after transposing `key` by `semitones`.
pub fn transpose_key_name(key: &KeySig, semitones: i32) -> KeySig {
    KeySig { fifths: normalize_fifths(key.fifths + 7 * semitones), mode: key.mode.clone() }
}

/// Number of diatonic steps to move pitches when transposing `key` by `semitones`.
fn diatonic_steps(key: &KeySig, new_key: &KeySig, semitones: i32) -> i32 {
    let base = (tonic_step(new_key) - tonic_step(key)).rem_euclid(7);
    // Pick the octave so the step count best matches the semitone distance.
    let target = semitones as f64 * 7.0 / 12.0;
    let mut best = base;
    let mut best_err = f64::MAX;
    for k in -12..=12 {
        let cand = base + 7 * k;
        let err = (cand as f64 - target).abs();
        if err < best_err {
            best_err = err;
            best = cand;
        }
    }
    best
}

fn transpose_pitch(p: &Pitch, steps: i32, semitones: i32) -> Pitch {
    let midi = p.midi() + semitones;
    let d = p.diatonic() + steps;
    let cand = Pitch::from_diatonic(d, midi);
    if (-2..=2).contains(&cand.alter) {
        return cand;
    }
    // Respell enharmonically: try neighbouring letters, smallest |alter| wins.
    let mut best = cand;
    for delta in [-1, 1, -2, 2] {
        let c = Pitch::from_diatonic(d + delta, midi);
        if c.alter.abs() < best.alter.abs() {
            best = c;
        }
    }
    best
}

const STEPS: [char; 7] = ['C', 'D', 'E', 'F', 'G', 'A', 'B'];

/// Transpose a note name such as "Bb" or "F#" (no octave). Returns None if not a note name.
fn transpose_note_name(name: &str, steps: i32, semitones: i32, prefer_flats: bool) -> Option<String> {
    let mut chars = name.chars();
    let step = chars.next()?.to_ascii_uppercase();
    if !('A'..='G').contains(&step) {
        return None;
    }
    let acc: String = chars.collect();
    let alter: i32 = match acc.as_str() {
        "" => 0,
        "#" | "♯" => 1,
        "##" | "x" | "𝄪" => 2,
        "b" | "♭" => -1,
        "bb" | "𝄫" => -2,
        _ => return None,
    };
    let p = Pitch { step, alter: alter as i8, octave: 4 };
    let mut t = transpose_pitch(&p, steps, semitones);
    if t.alter.abs() > 1 {
        // Use the simple spelling of the pitch class.
        let pc = (p.midi() + semitones).rem_euclid(12);
        const SHARPS: [(char, i8); 12] =
            [('C', 0), ('C', 1), ('D', 0), ('D', 1), ('E', 0), ('F', 0), ('F', 1), ('G', 0), ('G', 1), ('A', 0), ('A', 1), ('B', 0)];
        const FLATS: [(char, i8); 12] =
            [('C', 0), ('D', -1), ('D', 0), ('E', -1), ('E', 0), ('F', 0), ('G', -1), ('G', 0), ('A', -1), ('A', 0), ('B', -1), ('B', 0)];
        let (s, a) = if prefer_flats { FLATS[pc as usize] } else { SHARPS[pc as usize] };
        t = Pitch { step: s, alter: a, octave: 4 };
    }
    let acc = match t.alter {
        -2 => "bb",
        -1 => "b",
        1 => "#",
        2 => "##",
        _ => "",
    };
    debug_assert!(STEPS.contains(&t.step));
    Some(format!("{}{}", t.step, acc))
}

/// Split a leading note name (letter + accidentals) from the rest of a string.
fn split_note_name(s: &str) -> Option<(&str, &str)> {
    let first = s.chars().next()?;
    if !('A'..='G').contains(&first) {
        return None;
    }
    let mut end = first.len_utf8();
    let rest = &s[end..];
    for c in rest.chars() {
        if matches!(c, '#' | 'b' | '♯' | '♭') {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    // "Bb" ok; but avoid eating the 'b' of nothing else - accidentals only.
    Some((&s[..end], &s[end..]))
}

/// Transpose a chord symbol such as "G7/B" or "Bbmaj7". Unknown text is returned unchanged.
pub fn transpose_chord_symbol(sym: &str, steps: i32, semitones: i32, prefer_flats: bool) -> String {
    let (main, bass) = match sym.rsplit_once('/') {
        Some((m, b)) if split_note_name(b).map(|(_, r)| r.is_empty()).unwrap_or(false) => (m, Some(b)),
        _ => (sym, None),
    };
    let Some((root, quality)) = split_note_name(main) else { return sym.to_string() };
    let Some(new_root) = transpose_note_name(root, steps, semitones, prefer_flats) else { return sym.to_string() };
    let mut out = format!("{new_root}{quality}");
    if let Some(b) = bass {
        match transpose_note_name(b, steps, semitones, prefer_flats) {
            Some(nb) => out.push_str(&format!("/{nb}")),
            None => out.push_str(&format!("/{b}")),
        }
    }
    out
}

/// Transpose by `semitones`, choosing the new key signature with the simplest spelling.
pub fn transpose(song: &ParsedSong, semitones: i32) -> ParsedSong {
    if semitones == 0 {
        return song.clone();
    }
    let mut out = song.clone();
    let new_key = transpose_key_name(&song.key, semitones);
    // The interval is derived from the song's main key and used everywhere so
    // that key changes inside the song move by the same interval.
    let steps = diatonic_steps(&song.key, &new_key, semitones);
    out.key = new_key;

    for m in &mut out.measures {
        m.key = transpose_key_name(&m.key, semitones);
        if let Some(k) = &m.key_change {
            m.key_change = Some(transpose_key_name(k, semitones));
        }
        let prefer_flats = m.key.fifths < 0;
        for v in &mut m.voices {
            for e in &mut v.events {
                if let EventKind::Note(ps) = &mut e.kind {
                    for p in ps.iter_mut() {
                        *p = transpose_pitch(p, steps, semitones);
                    }
                    ps.sort_by_key(|p| p.midi());
                }
                for c in &mut e.chord_symbols {
                    *c = transpose_chord_symbol(c, steps, semitones, prefer_flats);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::notation::parse_song;
    use crate::model::score::{Measure, Song, VoiceMeasure};

    fn song_with(key: i32, mode: &str, notes: &str) -> ParsedSong {
        let mut s = Song::default();
        s.key = KeySig { fifths: key, mode: mode.into() };
        s.voices.truncate(1);
        let mut m = Measure::default();
        m.voices.insert("S".into(), VoiceMeasure { notes: notes.into(), ..Default::default() });
        s.measures = vec![m];
        parse_song(&s).0
    }

    fn pitches(p: &ParsedSong) -> Vec<String> {
        p.measures[0].voices[0].events.iter().flat_map(|e| e.pitches().iter().map(|p| p.token())).collect()
    }

    #[test]
    fn c_to_d() {
        let s = song_with(0, "major", "C4:q E4:q F4:q B4:q");
        let t = transpose(&s, 2);
        assert_eq!(t.key.fifths, 2);
        assert_eq!(pitches(&t), vec!["D4", "F#4", "G4", "C#5"]);
    }

    #[test]
    fn eb_down_to_c() {
        let s = song_with(-3, "major", "Eb4:q G4:q Bb4:q Ab4:q");
        let t = transpose(&s, -3);
        assert_eq!(t.key.fifths, 0);
        assert_eq!(pitches(&t), vec!["C4", "E4", "G4", "F4"]);
    }

    #[test]
    fn g_up_tritone_prefers_flats() {
        let s = song_with(1, "major", "G4:q F#4:q");
        let t = transpose(&s, 6);
        assert_eq!(t.key.fifths, -5); // Db major
        assert_eq!(pitches(&t), vec!["Db5", "C5"]);
        // F major + 1 → Gb (−6) rather than F# (+6)
        assert_eq!(transpose_key_name(&KeySig { fifths: -1, mode: "major".into() }, 1).fifths, -6);
    }

    #[test]
    fn octave_crossing() {
        let s = song_with(0, "major", "B3:q");
        let t = transpose(&s, 1);
        // C major +1 → Db major: B3 → C4
        assert_eq!(t.measures[0].voices[0].events[0].pitches()[0].midi(), 60);
        assert_eq!(pitches(&t), vec!["C4"]);
    }

    #[test]
    fn minor_keys() {
        let s = song_with(0, "minor", "A3:q C4:q G#4:q");
        let t = transpose(&s, 2); // A minor → B minor
        assert_eq!(t.key.fifths, 2);
        assert_eq!(pitches(&t), vec!["B3", "D4", "A#4"]);
    }

    #[test]
    fn chord_symbols() {
        assert_eq!(transpose_chord_symbol("G7/B", 1, 2, false), "A7/C#");
        assert_eq!(transpose_chord_symbol("Bbmaj7", 1, 2, false), "Cmaj7");
        assert_eq!(transpose_chord_symbol("Dm", -2, -3, true), "Bm");
        assert_eq!(transpose_chord_symbol("N.C.", 1, 2, false), "N.C.");
        let s = song_with(1, "major", "G4:q{c:G7/B}");
        let t = transpose(&s, 2);
        assert_eq!(t.measures[0].voices[0].events[0].chord_symbols[0], "A7/C#");
    }

    #[test]
    fn octave_up() {
        let s = song_with(-1, "major", "F4:q Bb4:q");
        let t = transpose(&s, 12);
        assert_eq!(t.key.fifths, -1);
        assert_eq!(pitches(&t), vec!["F5", "Bb5"]);
    }
}
