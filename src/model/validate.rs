//! Mechanical checks on a transcription. Errors are fed back to Claude in an
//! automatic correction pass; warnings are shown to the user for review.

use super::notation::{parse_song, ParsedSong, PPQ};
use super::score::Song;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Issue {
    pub severity: Severity,
    /// 0-based measure index in the song.
    pub measure: Option<usize>,
    pub voice: Option<String>,
    pub message: String,
}

impl Issue {
    pub fn location(&self) -> String {
        match (&self.measure, &self.voice) {
            (Some(m), Some(v)) => format!("measure index {m}, voice {v}"),
            (Some(m), None) => format!("measure index {m}"),
            (None, Some(v)) => format!("voice {v}"),
            (None, None) => "song".into(),
        }
    }

    pub fn describe(&self) -> String {
        format!("[{}] {}: {}", if self.severity == Severity::Error { "error" } else { "warning" }, self.location(), self.message)
    }
}

fn fmt_ticks(t: u32) -> String {
    let q = t as f32 / PPQ as f32;
    if (q - q.round()).abs() < 1e-3 {
        format!("{} quarter(s)", q.round() as i64)
    } else {
        format!("{q:.3} quarters")
    }
}

/// Typical comfortable ranges (MIDI) for hymn voices, with margin.
fn voice_range(id: &str, name: &str) -> Option<(i32, i32)> {
    let key = if !name.is_empty() { name.to_ascii_lowercase() } else { id.to_ascii_lowercase() };
    let k = key.as_str();
    if k.starts_with('s') || k.starts_with("melody") || k == "m" {
        Some((57, 84)) // A3..C6
    } else if k.starts_with('a') {
        Some((52, 77)) // E3..F5
    } else if k.starts_with('t') {
        Some((45, 72)) // A2..C5
    } else if k.starts_with('b') {
        Some((33, 67)) // A1..G4
    } else {
        None
    }
}

/// Validate a song. Returns the parsed song (for exporters) and all issues.
pub fn validate(song: &Song) -> (ParsedSong, Vec<Issue>) {
    let (parsed, problems) = parse_song(song);
    let mut issues: Vec<Issue> = problems
        .into_iter()
        .map(|p| Issue {
            severity: if p.is_error { Severity::Error } else { Severity::Warning },
            measure: p.measure,
            voice: p.voice,
            message: p.message,
        })
        .collect();

    if song.measures.is_empty() {
        issues.push(Issue { severity: Severity::Error, measure: None, voice: None, message: "No measures transcribed".into() });
        return (parsed, issues);
    }
    if !(-7..=7).contains(&song.key.fifths) {
        issues.push(Issue {
            severity: Severity::Error,
            measure: None,
            voice: None,
            message: format!("Key signature fifths {} must be between -7 and 7", song.key.fifths),
        });
    }
    for v in &song.voices {
        if v.staff == 0 || v.staff as usize > song.staves.len().max(1) {
            issues.push(Issue {
                severity: Severity::Error,
                measure: None,
                voice: Some(v.id.clone()),
                message: format!("Voice staff {} does not exist ({} staves declared)", v.staff, song.staves.len()),
            });
        }
    }
    if song.meta.title.trim().is_empty() {
        issues.push(Issue { severity: Severity::Warning, measure: None, voice: None, message: "Title is empty".into() });
    }

    let n = parsed.measures.len();
    for m in &parsed.measures {
        // Grace-only or empty voices are allowed to be absent but not to be short.
        let totals: Vec<(String, u32)> = m.voices.iter().map(|v| (v.id.clone(), v.total_ticks())).collect();
        let is_last = m.index + 1 == n;
        for (vid, t) in &totals {
            if *t == 0 {
                issues.push(Issue {
                    severity: Severity::Error,
                    measure: Some(m.index),
                    voice: Some(vid.clone()),
                    message: "Voice has no notes or rests (use a rest such as \"R\" for a whole-measure rest)".into(),
                });
                continue;
            }
            let short_ok = m.pickup || m.partial || is_last;
            if *t > m.expected_ticks {
                issues.push(Issue {
                    severity: Severity::Error,
                    measure: Some(m.index),
                    voice: Some(vid.clone()),
                    message: format!(
                        "Voice lasts {} but a {} measure holds {}",
                        fmt_ticks(*t),
                        m.time.label(),
                        fmt_ticks(m.expected_ticks)
                    ),
                });
            } else if *t < m.expected_ticks && !short_ok {
                issues.push(Issue {
                    severity: Severity::Error,
                    measure: Some(m.index),
                    voice: Some(vid.clone()),
                    message: format!(
                        "Voice lasts {} but a {} measure holds {} (if the measure is intentionally incomplete, set \"partial\": true)",
                        fmt_ticks(*t),
                        m.time.label(),
                        fmt_ticks(m.expected_ticks)
                    ),
                });
            }
        }
        // All voices in a measure must have the same length.
        if let (Some(max), Some(min)) = (totals.iter().map(|t| t.1).max(), totals.iter().map(|t| t.1).min()) {
            if max != min && max <= m.expected_ticks {
                let detail: Vec<String> = totals.iter().map(|(v, t)| format!("{v}={}", fmt_ticks(*t))).collect();
                issues.push(Issue {
                    severity: Severity::Error,
                    measure: Some(m.index),
                    voice: None,
                    message: format!("Voices have different lengths: {}", detail.join(", ")),
                });
            }
        }
        // Voices declared but missing.
        for v in &parsed.voices {
            if !v.optional && !m.voices.iter().any(|pv| pv.id == v.id) {
                issues.push(Issue {
                    severity: Severity::Warning,
                    measure: Some(m.index),
                    voice: Some(v.id.clone()),
                    message: "Voice is missing from this measure".into(),
                });
            }
        }
        // Range check.
        for v in &m.voices {
            if let Some((lo, hi)) = voice_range(&v.id, &v.name) {
                for e in &v.events {
                    for p in e.pitches() {
                        let midi = p.midi();
                        if midi < lo || midi > hi {
                            issues.push(Issue {
                                severity: Severity::Warning,
                                measure: Some(m.index),
                                voice: Some(v.id.clone()),
                                message: format!(
                                    "{} is unusually {} for {} (possible octave or clef error)",
                                    p.display(),
                                    if midi < lo { "low" } else { "high" },
                                    v.name
                                ),
                            });
                        }
                    }
                }
            }
        }
    }

    // Pickup / final measure should complement each other.
    if let (Some(first), Some(last)) = (parsed.measures.first(), parsed.measures.last()) {
        if first.pickup && n > 1 && first.time == last.time {
            let sum = first.actual_ticks + last.actual_ticks;
            if sum != first.expected_ticks && last.actual_ticks != last.expected_ticks {
                issues.push(Issue {
                    severity: Severity::Warning,
                    measure: Some(last.index),
                    voice: None,
                    message: "Final measure does not complete the pickup measure".into(),
                });
            }
        }
    }

    // Verse count consistency.
    let max_verse = song.max_verse();
    if max_verse > song.verse_count {
        issues.push(Issue {
            severity: Severity::Warning,
            measure: None,
            voice: None,
            message: format!("Lyrics contain verse {max_verse} but verse_count is {}", song.verse_count),
        });
    }

    // Large melodic leaps in the top voice often indicate misread pitches.
    if let Some(first_voice) = parsed.voices.first() {
        let mut prev: Option<i32> = None;
        for m in &parsed.measures {
            if let Some(v) = m.voices.iter().find(|v| v.id == first_voice.id) {
                for e in &v.events {
                    if let Some(p) = e.pitches().last() {
                        let midi = p.midi();
                        if let Some(pm) = prev {
                            if (midi - pm).abs() > 12 {
                                issues.push(Issue {
                                    severity: Severity::Warning,
                                    measure: Some(m.index),
                                    voice: Some(first_voice.id.clone()),
                                    message: format!("Leap larger than an octave to {}", p.display()),
                                });
                            }
                        }
                        prev = Some(midi);
                    }
                }
            }
        }
    }

    issues.sort_by_key(|i| (i.severity != Severity::Error, i.measure.unwrap_or(usize::MAX)));
    (parsed, issues)
}

pub fn error_count(issues: &[Issue]) -> usize {
    issues.iter().filter(|i| i.severity == Severity::Error).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::score::{Measure, VoiceMeasure};

    fn measure(notes: &[(&str, &str)]) -> Measure {
        let mut m = Measure::default();
        for (v, n) in notes {
            m.voices.insert(v.to_string(), VoiceMeasure { notes: n.to_string(), ..Default::default() });
        }
        m
    }

    #[test]
    fn detects_bad_lengths() {
        let mut song = Song::default();
        song.meta.title = "Test".into();
        song.time = "4/4".into();
        song.measures = vec![
            measure(&[("S", "C5:q D5:q E5:h"), ("A", "G4:w"), ("T", "E4:w"), ("B", "C3:w")]),
            measure(&[("S", "C5:q D5:q E5:q"), ("A", "G4:w"), ("T", "E4:w"), ("B", "C3:w")]),
        ];
        let (_, issues) = validate(&song);
        let errs: Vec<_> = issues.iter().filter(|i| i.severity == Severity::Error).collect();
        assert!(errs.iter().any(|e| e.measure == Some(1)), "{issues:?}");
        assert!(!errs.iter().any(|e| e.measure == Some(0)), "{issues:?}");
    }
}
