//! Standard MIDI File (type 1) writer for piano / organ playback.
//!
//! Layout of the file:
//! - track 0: conductor (title, copyright, time/key signatures, tempo map, markers)
//! - optional count-in track on channel 10 (GM percussion)
//! - one track per (non-muted) voice, each on its own channel
//!
//! Fermatas and the final ritardando are realised through tempo changes, so
//! bar lines stay aligned with the notation when the file is imported into a
//! notation program. Grace notes are not played (hymnals rarely use them and
//! they would disturb the rhythm of the other voices).

use super::{MidiOptions, VerseMode};
use crate::model::notation::{Barline, EventKind, ParsedSong, Syllabic, PPQ};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Playback order
// ---------------------------------------------------------------------------

/// Parse volta numbers such as "1", "1, 2", "1.–3." into a list.
fn parse_ending_numbers(s: &str) -> Vec<u32> {
    fn flush(cur: &mut String, pending: &mut Option<u32>, nums: &mut Vec<u32>) {
        if let Ok(n) = cur.parse::<u32>() {
            match pending.take() {
                Some(a) if a <= n => nums.extend(a..=n.min(a + 20)),
                _ => nums.push(n),
            }
        }
        cur.clear();
    }
    let mut nums = Vec::new();
    let mut pending = None;
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if c == '.' {
            continue;
        } else if c == '-' || c == '–' {
            if let Ok(n) = cur.parse::<u32>() {
                pending = Some(n);
            }
            cur.clear();
        } else {
            flush(&mut cur, &mut pending, &mut nums);
        }
    }
    flush(&mut cur, &mut pending, &mut nums);
    nums
}

/// Measure indices in the order they are played (repeats and voltas expanded
/// when `expand_repeats` is true). D.C./D.S. instructions are ignored.
pub fn playback_order(song: &ParsedSong, expand_repeats: bool) -> Vec<usize> {
    let n = song.measures.len();
    if !expand_repeats {
        return (0..n).collect();
    }
    let endings: Vec<Vec<u32>> =
        song.measures.iter().map(|m| m.ending.as_ref().map(|e| parse_ending_numbers(&e.numbers)).unwrap_or_default()).collect();
    let has_ending_for = |from: usize, pass: u32| endings[from..].iter().any(|e| e.contains(&pass));

    let mut order = Vec::new();
    let mut i = 0usize;
    let mut section_start = 0usize;
    let mut pass = 1u32;
    let mut taken: HashMap<usize, u32> = HashMap::new();
    let limit = n * 16 + 16;
    while i < n && order.len() < limit {
        let m = &song.measures[i];
        if m.repeat_start && i != section_start {
            section_start = i;
            pass = 1;
        }
        if !endings[i].is_empty() && !endings[i].contains(&pass) {
            i += 1;
            continue;
        }
        order.push(i);
        if m.barline == Barline::RepeatEnd {
            let t = taken.entry(i).or_insert(0);
            let jump = if !endings[i].is_empty() { *t < 8 && has_ending_for(section_start, pass + 1) } else { *t == 0 };
            if jump {
                *t += 1;
                pass += 1;
                i = section_start;
                continue;
            }
            section_start = i + 1;
            pass = 1;
        } else if let Some(e) = &m.ending {
            if e.stop.is_some() && !has_ending_for(i + 1, pass + 1) {
                // Last ending finished: a new section begins.
                section_start = i + 1;
                pass = 1;
            }
        }
        i += 1;
    }
    order
}

// ---------------------------------------------------------------------------
// SMF encoding helpers
// ---------------------------------------------------------------------------

fn vlq(mut v: u32, out: &mut Vec<u8>) {
    let mut buf = [0u8; 5];
    let mut i = 4;
    buf[i] = (v & 0x7f) as u8;
    v >>= 7;
    while v > 0 {
        i -= 1;
        buf[i] = ((v & 0x7f) as u8) | 0x80;
        v >>= 7;
    }
    out.extend_from_slice(&buf[i..]);
}

fn meta(kind: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![0xff, kind];
    vlq(data.len() as u32, &mut v);
    v.extend_from_slice(data);
    v
}

fn text_meta(kind: u8, text: &str) -> Vec<u8> {
    meta(kind, text.as_bytes())
}

fn tempo_meta(bpm: f64) -> Vec<u8> {
    let us = (60_000_000.0 / bpm.max(1.0)).round().clamp(1.0, 16_777_215.0) as u32;
    meta(0x51, &[(us >> 16) as u8, (us >> 8) as u8, us as u8])
}

/// Event priority at equal ticks: meta/setup first, then note-offs, then note-ons.
const PRIO_META: u8 = 0;
const PRIO_OFF: u8 = 1;
const PRIO_ON: u8 = 2;

#[derive(Default)]
struct Track {
    events: Vec<(u64, u8, Vec<u8>)>,
}

impl Track {
    fn push(&mut self, tick: u64, prio: u8, bytes: Vec<u8>) {
        self.events.push((tick, prio, bytes));
    }

    fn encode(mut self, end_tick: u64) -> Vec<u8> {
        self.events.sort_by_key(|e| (e.0, e.1));
        let mut body = Vec::new();
        let mut last = 0u64;
        for (tick, _, bytes) in &self.events {
            vlq((tick - last) as u32, &mut body);
            body.extend_from_slice(bytes);
            last = *tick;
        }
        let end = end_tick.max(last);
        vlq((end - last) as u32, &mut body);
        body.extend_from_slice(&[0xff, 0x2f, 0x00]);
        let mut out = b"MTrk".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend(body);
        out
    }
}

// ---------------------------------------------------------------------------
// Timeline
// ---------------------------------------------------------------------------

struct Seg {
    measure: usize,
    start: u64,
    len: u64,
    /// Verse number for lyrics (None for the introduction).
    verse: Option<u32>,
    /// First measure of a pass (intro or verse): ties are not carried into it.
    pass_start: bool,
    /// Part of the final ritardando.
    ritard: bool,
}

fn dynamic_level(d: &str) -> Option<f32> {
    Some(match d {
        "pppp" => 25.0,
        "ppp" => 32.0,
        "pp" => 40.0,
        "p" => 55.0,
        "mp" => 68.0,
        "mf" => 80.0,
        "f" => 95.0,
        "ff" => 110.0,
        "fff" => 120.0,
        "ffff" => 127.0,
        "fp" => 55.0,
        "sfp" => 55.0,
        _ => return None,
    })
}

fn is_sforzando(d: &str) -> bool {
    matches!(d, "sf" | "sfz" | "sffz" | "fz" | "rf" | "rfz" | "fp" | "sfp")
}

struct Held {
    start: u64,
    end: u64,
    vel: u8,
    staccato: bool,
    tenuto: bool,
}

fn emit_note(track: &mut Track, ch: u8, pitch: u8, h: &Held) {
    let len = h.end.saturating_sub(h.start).max(1);
    let sounding = if h.staccato {
        (len / 2).max(1)
    } else if h.tenuto {
        len - (len / 50).min(5).min(len - 1)
    } else {
        len - (len / 20).min(48).min(len - 1)
    };
    track.push(h.start, PRIO_ON, vec![0x90 | ch, pitch, h.vel.max(1)]);
    track.push(h.start + sounding, PRIO_OFF, vec![0x80 | ch, pitch, 0]);
}

/// Write a Standard MIDI File (type 1).
pub fn write_midi(song: &ParsedSong, opts: &MidiOptions) -> Vec<u8> {
    let order = playback_order(song, opts.expand_repeats);
    let passes = match opts.verses {
        VerseMode::Once => 1,
        // Verses under the music plus stanzas printed below it.
        VerseMode::AllVerses => (song.verse_count + song.extra_stanzas.len() as u32).max(1),
        VerseMode::Times(n) => n.max(1),
    };
    let mlen = |mi: usize| -> u64 {
        let m = &song.measures[mi];
        let t = if m.actual_ticks > 0 { m.actual_ticks } else { m.expected_ticks };
        t.max(1) as u64
    };
    let beat_ticks = |ts: &crate::model::notation::TimeSig| -> u64 {
        let b = (PPQ * 4 / ts.beat_type.max(1)) as u64;
        if ts.is_compound() {
            b * 3
        } else {
            b
        }
    };

    // ---- Build the timeline -------------------------------------------------
    let mut segs: Vec<Seg> = Vec::new();
    let mut markers: Vec<(u64, String)> = Vec::new();
    let mut clicks: Vec<(u64, u8)> = Vec::new();
    let mut t: u64 = 0;

    if opts.count_in && !order.is_empty() {
        let ts = song.measures[order[0]].time;
        let full = ts.measure_ticks() as u64;
        let bt = beat_ticks(&ts);
        markers.push((0, "Count-in".into()));
        let mut b = 0;
        while b < full {
            clicks.push((b, if b == 0 { 76 } else { 77 }));
            b += bt;
        }
        t += full;
    }

    if opts.intro_measures > 0 && !order.is_empty() {
        let k = (opts.intro_measures as usize).min(order.len());
        markers.push((t, "Intro".into()));
        for (j, &mi) in order[order.len() - k..].iter().enumerate() {
            let len = mlen(mi);
            segs.push(Seg { measure: mi, start: t, len, verse: None, pass_start: j == 0, ritard: false });
            t += len;
        }
        let last = *order.last().unwrap();
        t += beat_ticks(&song.measures[last].time);
    }

    for v in 1..=passes {
        markers.push((t, format!("Verse {v}")));
        for (j, &mi) in order.iter().enumerate() {
            let len = mlen(mi);
            let m = &song.measures[mi];
            if !m.section.is_empty() && j > 0 {
                markers.push((t, m.section.clone()));
            }
            segs.push(Seg {
                measure: mi,
                start: t,
                len,
                verse: Some(v),
                pass_start: j == 0,
                ritard: opts.final_ritard && v == passes && j + 2 >= order.len(),
            });
            t += len;
        }
    }
    let total = t;

    // ---- Global dynamics timeline (dynamics apply to all voices) ------------
    let default_level = 80.0f32;
    let mut dyn_points: Vec<(u64, f32)> = vec![(0, default_level)];
    for seg in &segs {
        if seg.pass_start {
            dyn_points.push((seg.start, default_level));
        }
        let m = &song.measures[seg.measure];
        for pv in &m.voices {
            let mut local = 0u64;
            for e in &pv.events {
                for d in &e.dynamics {
                    if let Some(l) = dynamic_level(d) {
                        dyn_points.push((seg.start + local, l));
                    }
                }
                local += e.ticks as u64;
            }
        }
    }
    dyn_points.sort_by_key(|p| p.0); // stable: later entries at the same tick win
    let level_at = |tick: u64| -> f32 {
        let idx = dyn_points.partition_point(|p| p.0 <= tick);
        if idx == 0 {
            default_level
        } else {
            dyn_points[idx - 1].1
        }
    };

    // ---- Tempo map ------------------------------------------------------------
    let base_bpm = opts.tempo_bpm.filter(|b| *b > 10.0).unwrap_or_else(|| song.tempo_bpm()) as f64;
    let mut tempo_points: Vec<(u64, f64)> = vec![(0, base_bpm)];
    let rit: Vec<&Seg> = segs.iter().filter(|s| s.ritard).collect();
    if let (Some(first), Some(last)) = (rit.first(), rit.last()) {
        let r_start = first.start;
        let r_end = last.start + last.len;
        let bt = beat_ticks(&song.measures[first.measure].time).max(1);
        let span = (r_end - r_start).max(1) as f64;
        let mut b = r_start;
        while b < r_end {
            let frac = (b - r_start) as f64 / span;
            tempo_points.push((b, base_bpm * (1.0 - 0.3 * frac)));
            b += bt;
        }
    }
    let mut fermatas: Vec<(u64, u64)> = Vec::new();
    if opts.fermata_factor > 1.0 {
        for seg in &segs {
            for pv in &song.measures[seg.measure].voices {
                let mut local = 0u64;
                for e in &pv.events {
                    if e.fermata && !e.grace && e.ticks > 0 {
                        fermatas.push((seg.start + local, seg.start + local + e.ticks as u64));
                    }
                    local += e.ticks as u64;
                }
            }
        }
    }
    let mut boundaries: Vec<u64> = tempo_points.iter().map(|p| p.0).collect();
    for &(s, e) in &fermatas {
        boundaries.push(s);
        boundaries.push(e);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut tempo_events: Vec<(u64, f64)> = Vec::new();
    for &b in &boundaries {
        let idx = tempo_points.partition_point(|p| p.0 <= b);
        let base = if idx == 0 { base_bpm } else { tempo_points[idx - 1].1 };
        let held = fermatas.iter().any(|&(s, e)| s <= b && b < e);
        let eff = if held { base / opts.fermata_factor as f64 } else { base };
        if tempo_events.last().map(|l| (l.1 - eff).abs() > 1e-6).unwrap_or(true) {
            tempo_events.push((b, eff));
        }
    }

    // ---- Conductor track ------------------------------------------------------
    let mut conductor = Track::default();
    let title = if song.meta.title.trim().is_empty() { "Untitled" } else { song.meta.title.trim() };
    conductor.push(0, PRIO_META, text_meta(0x03, title));
    if !song.meta.copyright.trim().is_empty() {
        conductor.push(0, PRIO_META, text_meta(0x02, song.meta.copyright.trim()));
    }
    let mut last_ts = None;
    let mut last_key = None;
    for seg in &segs {
        let m = &song.measures[seg.measure];
        let ts = (m.time.beats, m.time.beat_type);
        if last_ts != Some(ts) {
            let dd = m.time.beat_type.max(1).trailing_zeros() as u8;
            let cc = if m.time.is_compound() { 36 } else { 24 };
            conductor.push(seg.start, PRIO_META, meta(0x58, &[m.time.beats as u8, dd, cc, 8]));
            last_ts = Some(ts);
        }
        let key = (m.key.fifths.clamp(-7, 7), m.key.is_minor());
        if last_key != Some(key) {
            conductor.push(seg.start, PRIO_META, meta(0x59, &[key.0 as i8 as u8, key.1 as u8]));
            last_key = Some(key);
        }
    }
    if segs.is_empty() {
        conductor.push(0, PRIO_META, meta(0x58, &[4, 2, 24, 8]));
    }
    for (tick, bpm) in &tempo_events {
        conductor.push(*tick, PRIO_META, tempo_meta(*bpm));
    }
    for (tick, name) in &markers {
        conductor.push(*tick, PRIO_META, text_meta(0x06, name));
    }

    let mut tracks: Vec<Track> = vec![conductor];

    // ---- Count-in ---------------------------------------------------------------
    if !clicks.is_empty() {
        let mut tr = Track::default();
        tr.push(0, PRIO_META, text_meta(0x03, "Count-in"));
        for (tick, note) in &clicks {
            tr.push(*tick, PRIO_ON, vec![0x99, *note, 100]);
            tr.push(*tick + (PPQ as u64 / 8), PRIO_OFF, vec![0x89, *note, 0]);
        }
        tracks.push(tr);
    }

    // ---- Voice tracks -----------------------------------------------------------
    let lyric_voice = song.lyric_voice();
    let max_lyric_verse = song
        .measures
        .iter()
        .flat_map(|m| m.voices.iter())
        .flat_map(|v| v.events.iter())
        .flat_map(|e| e.lyrics.iter().map(|l| l.verse))
        .max()
        .unwrap_or(0);
    let mut lyric_events: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut lyric_track_index: Option<usize> = None;

    let mut channel: u8 = 0;
    for vdef in &song.voices {
        if opts.mute_voices.iter().any(|m| m == &vdef.id) {
            continue;
        }
        if channel == 9 {
            channel += 1;
        }
        if channel > 15 {
            break;
        }
        let ch = channel;
        channel += 1;

        let emphasized = opts.emphasize_voice.as_deref().map(|e| e == vdef.id);
        let mut tr = Track::default();
        let name = if vdef.name.is_empty() { vdef.id.as_str() } else { vdef.name.as_str() };
        tr.push(0, PRIO_META, text_meta(0x03, name));
        tr.push(0, PRIO_META, vec![0xc0 | ch, opts.program.min(127)]);
        let volume = match emphasized {
            Some(true) => 127,
            Some(false) => 70,
            None => 100,
        };
        tr.push(0, PRIO_META, vec![0xb0 | ch, 7, volume]);
        if emphasized == Some(true) {
            tr.push(0, PRIO_META, vec![0xb0 | ch, 10, 64]);
        }

        let is_lyric_voice = lyric_voice.as_deref() == Some(vdef.id.as_str());
        if is_lyric_voice {
            lyric_track_index = Some(tracks.len());
        }

        let mut held: HashMap<u8, Held> = HashMap::new();
        let close_all = |held: &mut HashMap<u8, Held>, tr: &mut Track| {
            let mut keys: Vec<u8> = held.keys().copied().collect();
            keys.sort_unstable();
            for k in keys {
                if let Some(h) = held.remove(&k) {
                    emit_note(tr, ch, k, &h);
                }
            }
        };

        for seg in &segs {
            if seg.pass_start {
                close_all(&mut held, &mut tr);
            }
            let m = &song.measures[seg.measure];
            let Some(pv) = m.voices.iter().find(|v| v.id == vdef.id) else {
                close_all(&mut held, &mut tr);
                continue;
            };

            // Which verse's lyrics to show in this measure.
            let lyric_verse = if is_lyric_voice {
                seg.verse.filter(|_| max_lyric_verse > 0).map(|v| {
                    let target = v.min(max_lyric_verse);
                    let has_target = pv.events.iter().any(|e| e.lyrics.iter().any(|l| l.verse == target));
                    if has_target {
                        target
                    } else {
                        1
                    }
                })
            } else {
                None
            };

            let mut local = 0u64;
            for e in &pv.events {
                if e.grace {
                    continue;
                }
                let on = seg.start + local;
                let off = on + e.ticks as u64;
                local += e.ticks as u64;
                match &e.kind {
                    EventKind::Note(ps) => {
                        let mut vel = level_at(on) * opts.velocity.max(1) as f32 / 80.0;
                        match emphasized {
                            Some(true) => vel += 30.0,
                            Some(false) => vel = (vel - 35.0).max(20.0),
                            None => {}
                        }
                        if e.accent {
                            vel += 15.0;
                        }
                        if e.dynamics.iter().any(|d| is_sforzando(d)) {
                            vel += 20.0;
                        }
                        let vel = vel.round().clamp(1.0, 127.0) as u8;
                        let midis: Vec<u8> = ps.iter().map(|p| p.midi().clamp(0, 127) as u8).collect();
                        // Close held notes that this event does not continue.
                        let keys: Vec<u8> = held.keys().copied().collect();
                        for k in keys {
                            if !(e.tie_stop && midis.contains(&k)) {
                                if let Some(h) = held.remove(&k) {
                                    emit_note(&mut tr, ch, k, &h);
                                }
                            }
                        }
                        for &p in &midis {
                            if let Some(h) = held.get_mut(&p) {
                                h.end = off;
                                h.staccato = e.staccato;
                                h.tenuto = e.tenuto;
                            } else {
                                held.insert(p, Held { start: on, end: off, vel, staccato: e.staccato, tenuto: e.tenuto });
                            }
                        }
                        if !e.tie_start {
                            for &p in &midis {
                                if let Some(h) = held.remove(&p) {
                                    emit_note(&mut tr, ch, p, &h);
                                }
                            }
                        }
                        if let Some(lv) = lyric_verse {
                            if !e.tie_stop {
                                if let Some(l) = e.lyrics.iter().find(|l| l.verse == lv) {
                                    let suffix = match l.syllabic {
                                        Syllabic::Begin | Syllabic::Middle => "-",
                                        Syllabic::Single | Syllabic::End => " ",
                                    };
                                    lyric_events.push((on, text_meta(0x05, &format!("{}{}", l.text, suffix))));
                                }
                            }
                        }
                    }
                    EventKind::Rest | EventKind::MeasureRest => close_all(&mut held, &mut tr),
                }
            }
        }
        close_all(&mut held, &mut tr);
        tracks.push(tr);
    }

    // Lyrics go on the lyric voice's track, or the conductor track if it is muted.
    let li = lyric_track_index.unwrap_or(0);
    for (tick, bytes) in lyric_events {
        tracks[li].push(tick, PRIO_META, bytes);
    }

    // ---- Assemble -----------------------------------------------------------------
    let mut out = b"MThd".to_vec();
    out.extend_from_slice(&6u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
    out.extend_from_slice(&(PPQ as u16).to_be_bytes());
    for tr in tracks {
        out.extend(tr.encode(total));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::notation::parse_song;
    use crate::model::score::{Measure, Song, Tempo, VoiceMeasure};

    // ---- tiny SMF reader -------------------------------------------------------
    struct Ev {
        tick: u64,
        status: u8,
        data: Vec<u8>,
        meta: Option<u8>,
    }
    struct Smf {
        format: u16,
        division: u16,
        tracks: Vec<Vec<Ev>>,
    }

    fn read_vlq(b: &[u8], i: &mut usize) -> u32 {
        let mut v = 0u32;
        loop {
            let c = b[*i];
            *i += 1;
            v = (v << 7) | (c & 0x7f) as u32;
            if c & 0x80 == 0 {
                return v;
            }
        }
    }

    fn read_smf(b: &[u8]) -> Smf {
        assert_eq!(&b[0..4], b"MThd");
        assert_eq!(u32::from_be_bytes(b[4..8].try_into().unwrap()), 6);
        let format = u16::from_be_bytes([b[8], b[9]]);
        let ntracks = u16::from_be_bytes([b[10], b[11]]);
        let division = u16::from_be_bytes([b[12], b[13]]);
        let mut i = 14;
        let mut tracks = Vec::new();
        for _ in 0..ntracks {
            assert_eq!(&b[i..i + 4], b"MTrk");
            let len = u32::from_be_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
            let mut j = i + 8;
            let end = j + len;
            let mut tick = 0u64;
            let mut evs = Vec::new();
            let mut running = 0u8;
            let mut saw_eot = false;
            while j < end {
                tick += read_vlq(b, &mut j) as u64;
                let mut status = b[j];
                if status & 0x80 != 0 {
                    j += 1;
                } else {
                    status = running;
                }
                if status == 0xff {
                    let kind = b[j];
                    j += 1;
                    let l = read_vlq(b, &mut j) as usize;
                    if kind == 0x2f {
                        saw_eot = true;
                    }
                    evs.push(Ev { tick, status, data: b[j..j + l].to_vec(), meta: Some(kind) });
                    j += l;
                } else {
                    running = status;
                    let n = match status & 0xf0 {
                        0xc0 | 0xd0 => 1,
                        _ => 2,
                    };
                    evs.push(Ev { tick, status, data: b[j..j + n].to_vec(), meta: None });
                    j += n;
                }
            }
            assert!(saw_eot, "missing end of track");
            assert_eq!(j, end);
            tracks.push(evs);
            i = end;
        }
        assert_eq!(i, b.len());
        Smf { format, division, tracks }
    }

    /// Length of the file in seconds according to the tempo map (end-of-track).
    fn seconds(smf: &Smf) -> f64 {
        let end = smf.tracks.iter().flat_map(|t| t.iter()).map(|e| e.tick).max().unwrap_or(0);
        let mut tempos: Vec<(u64, u32)> = smf.tracks[0]
            .iter()
            .filter(|e| e.meta == Some(0x51))
            .map(|e| (e.tick, ((e.data[0] as u32) << 16) | ((e.data[1] as u32) << 8) | e.data[2] as u32))
            .collect();
        tempos.sort_by_key(|t| t.0);
        let mut secs = 0.0;
        let mut last_tick = 0u64;
        let mut us = 500_000u32;
        for (t, u) in tempos {
            if t > end {
                break;
            }
            secs += (t - last_tick) as f64 * us as f64 / smf.division as f64 / 1e6;
            last_tick = t;
            us = u;
        }
        secs + (end - last_tick) as f64 * us as f64 / smf.division as f64 / 1e6
    }

    fn note_counts(smf: &Smf) -> HashMap<u8, (usize, usize)> {
        let mut m: HashMap<u8, (usize, usize)> = HashMap::new();
        for t in &smf.tracks {
            for e in t {
                let ch = e.status & 0x0f;
                match e.status & 0xf0 {
                    0x90 if e.data[1] > 0 => m.entry(ch).or_default().0 += 1,
                    0x80 => m.entry(ch).or_default().1 += 1,
                    0x90 => m.entry(ch).or_default().1 += 1,
                    _ => {}
                }
            }
        }
        m
    }

    // ---- song builders ----------------------------------------------------------
    struct M<'a> {
        s: &'a str,
        b: &'a str,
        barline: &'a str,
        repeat_start: bool,
        ending: Option<&'a str>,
    }

    fn m<'a>(s: &'a str, b: &'a str) -> M<'a> {
        M { s, b, barline: "", repeat_start: false, ending: None }
    }

    fn build(measures: &[M], bpm: f32, verses: u32) -> ParsedSong {
        let mut song = Song::default();
        song.meta.title = "Test".into();
        song.voices = vec![
            crate::model::score::VoiceDef { id: "S".into(), name: "Soprano".into(), staff: 1, stem: "up".into(), optional: false },
            crate::model::score::VoiceDef { id: "B".into(), name: "Bass".into(), staff: 2, stem: "down".into(), optional: false },
        ];
        song.tempo = Some(Tempo { text: String::new(), bpm: Some(bpm), beat: "q".into(), suggested: false });
        song.verse_count = verses;
        for x in measures {
            let mut meas = Measure::default();
            meas.voices.insert("S".into(), VoiceMeasure { notes: x.s.into(), ..Default::default() });
            meas.voices.insert("B".into(), VoiceMeasure { notes: x.b.into(), ..Default::default() });
            meas.barline = x.barline.into();
            meas.repeat_start = x.repeat_start;
            meas.ending = x.ending.map(|e| e.to_string());
            song.measures.push(meas);
        }
        let (p, probs) = parse_song(&song);
        assert!(probs.iter().all(|p| !p.is_error), "{probs:?}");
        p
    }

    fn plain_opts() -> MidiOptions {
        MidiOptions { verses: VerseMode::Once, final_ritard: false, fermata_factor: 1.0, ..Default::default() }
    }

    #[test]
    fn header_and_tracks() {
        let p = build(&[m("C5:q D5:q E5:h", "C3:w"), m("F5:w", "F2:w")], 120.0, 1);
        let bytes = write_midi(&p, &plain_opts());
        let smf = read_smf(&bytes);
        assert_eq!(smf.format, 1);
        assert_eq!(smf.division, 480);
        assert_eq!(smf.tracks.len(), 3); // conductor + S + B
        let counts = note_counts(&smf);
        assert_eq!(counts[&0], (4, 4));
        assert_eq!(counts[&1], (2, 2));
        assert!((seconds(&smf) - 4.0).abs() < 1e-6, "{}", seconds(&smf));
    }

    #[test]
    fn ties_merge_and_verses_repeat() {
        let p = build(&[m("C5:h G5:h~", "C3:w"), m("G5:w", "G2:w")], 120.0, 3);
        let opts = MidiOptions { verses: VerseMode::AllVerses, ..plain_opts() };
        let smf = read_smf(&write_midi(&p, &opts));
        let counts = note_counts(&smf);
        // Two soprano notes per pass (the tied G sounds once), three passes.
        assert_eq!(counts[&0], (6, 6));
        assert!((seconds(&smf) - 12.0).abs() < 1e-6);
        let markers: Vec<String> =
            smf.tracks[0].iter().filter(|e| e.meta == Some(0x06)).map(|e| String::from_utf8(e.data.clone()).unwrap()).collect();
        assert_eq!(markers, vec!["Verse 1", "Verse 2", "Verse 3"]);
    }

    #[test]
    fn repeats_expand() {
        let p = build(&[m("C5:w", "C3:w"), M { barline: "repeat-end", ..m("D5:w", "D3:w") }, m("E5:w", "E3:w")], 120.0, 1);
        assert_eq!(playback_order(&p, true), vec![0, 1, 0, 1, 2]);
        assert_eq!(playback_order(&p, false), vec![0, 1, 2]);

        let p = build(
            &[
                m("C5:w", "C3:w"),
                M { repeat_start: true, ..m("D5:w", "D3:w") },
                M { barline: "repeat-end", ..m("E5:w", "E3:w") },
                m("F5:w", "F3:w"),
            ],
            120.0,
            1,
        );
        assert_eq!(playback_order(&p, true), vec![0, 1, 2, 1, 2, 3]);

        let p = build(
            &[
                m("C5:w", "C3:w"),
                M { barline: "repeat-end", ending: Some("1"), ..m("D5:w", "D3:w") },
                M { barline: "final", ending: Some("2"), ..m("E5:w", "E3:w") },
            ],
            120.0,
            1,
        );
        assert_eq!(playback_order(&p, true), vec![0, 1, 0, 2]);
        let smf = read_smf(&write_midi(&p, &plain_opts()));
        assert!((seconds(&smf) - 8.0).abs() < 1e-6);

        let p = build(
            &[
                m("C5:w", "C3:w"),
                M { barline: "repeat-end", ending: Some("1, 2"), ..m("D5:w", "D3:w") },
                M { ending: Some("3"), ..m("E5:w", "E3:w") },
                m("F5:w", "F3:w"),
            ],
            120.0,
            1,
        );
        assert_eq!(playback_order(&p, true), vec![0, 1, 0, 1, 0, 2, 3]);
    }

    #[test]
    fn fermata_lengthens() {
        let plain = build(&[m("C5:h D5:h", "C3:w"), m("E5:w", "C3:w")], 120.0, 1);
        let ferm = build(&[m("C5:h D5:h^", "C3:w"), m("E5:w", "C3:w")], 120.0, 1);
        let opts = MidiOptions { fermata_factor: 2.0, ..plain_opts() };
        let a = seconds(&read_smf(&write_midi(&plain, &opts)));
        let b = seconds(&read_smf(&write_midi(&ferm, &opts)));
        assert!((a - 4.0).abs() < 1e-6);
        assert!((b - 5.0).abs() < 1e-6, "{b}");
    }

    #[test]
    fn ritard_intro_countin_emphasis() {
        let p = build(&[m("C5:w", "C3:w"), m("D5:w", "D3:w"), m("E5:w", "E3:w")], 120.0, 1);
        let opts = MidiOptions { final_ritard: true, intro_measures: 1, count_in: true, emphasize_voice: Some("B".into()), ..plain_opts() };
        let smf = read_smf(&write_midi(&p, &opts));
        // conductor + count-in + 2 voices
        assert_eq!(smf.tracks.len(), 4);
        // count-in 4 beats + intro 4 + pause 1 + 12 beats, ritard makes it longer than 21 beats @120.
        assert!(seconds(&smf) > 10.5);
        let counts = note_counts(&smf);
        assert_eq!(counts[&9], (4, 4));
        // Bass (channel 1) velocities are louder than soprano (channel 0).
        let vel =
            |ch: u8| smf.tracks.iter().flat_map(|t| t.iter()).find(|e| e.status == 0x90 | ch && e.data[1] > 0).map(|e| e.data[1]).unwrap();
        assert!(vel(1) > vel(0));
    }

    #[test]
    fn muted_voice_and_lyrics() {
        let mut song = Song::default();
        song.voices.truncate(1);
        song.verse_count = 2;
        let mut meas = Measure::default();
        meas.voices.insert(
            "S".into(),
            VoiceMeasure {
                notes: "C5:h D5:h".into(),
                lyrics: [(1, "A- men".to_string()), (2, "Hal- le".to_string())].into_iter().collect(),
            },
        );
        song.measures.push(meas);
        let p = parse_song(&song).0;
        let smf = read_smf(&write_midi(&p, &MidiOptions { verses: VerseMode::AllVerses, ..plain_opts() }));
        let lyrics: Vec<String> = smf
            .tracks
            .iter()
            .flat_map(|t| t.iter())
            .filter(|e| e.meta == Some(0x05))
            .map(|e| String::from_utf8(e.data.clone()).unwrap())
            .collect();
        assert_eq!(lyrics, vec!["A-", "men ", "Hal-", "le "]);

        let smf = read_smf(&write_midi(&p, &MidiOptions { mute_voices: vec!["S".into()], ..plain_opts() }));
        assert_eq!(smf.tracks.len(), 1);
    }

    #[test]
    fn ending_numbers() {
        assert_eq!(parse_ending_numbers("1"), vec![1]);
        assert_eq!(parse_ending_numbers("1, 2"), vec![1, 2]);
        assert_eq!(parse_ending_numbers("1.-3."), vec![1, 2, 3]);
    }
}
