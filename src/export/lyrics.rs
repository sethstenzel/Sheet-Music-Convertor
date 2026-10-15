//! Plain-text lyrics (for printing, slides or projection software).

use crate::model::notation::{ParsedSong, Syllabic};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct LyricsOptions {
    /// Print the refrain after every verse instead of only after verse 1.
    pub repeat_refrain: bool,
    pub include_header: bool,
    /// Break lines where the printed music starts a new system.
    pub follow_systems: bool,
}

impl Default for LyricsOptions {
    fn default() -> Self {
        Self { repeat_refrain: true, include_header: true, follow_systems: true }
    }
}

/// Text of each verse and the refrain (if any).
pub struct LyricsText {
    pub verses: BTreeMap<u32, String>,
    pub refrain: String,
}

pub fn collect(song: &ParsedSong, follow_systems: bool) -> LyricsText {
    let verse_count = song.verse_count.max(1);
    let mut verses: BTreeMap<u32, String> = BTreeMap::new();
    let mut refrain = String::new();
    let mut seen_verse_specific = false;
    for m in &song.measures {
        // Which verses have words in this measure (any voice)?
        let mut present: BTreeMap<u32, Vec<(String, Syllabic)>> = BTreeMap::new();
        for v in &m.voices {
            for e in &v.events {
                for l in &e.lyrics {
                    present.entry(l.verse).or_default().push((l.text.clone(), l.syllabic));
                }
            }
        }
        if present.is_empty() {
            continue;
        }
        let is_refrain = verse_count > 1 && seen_verse_specific && present.len() == 1 && present.contains_key(&1);
        if present.len() > 1 {
            seen_verse_specific = true;
        }
        for (verse, sylls) in present {
            let target = if is_refrain { &mut refrain } else { verses.entry(verse).or_default() };
            for (text, syl) in sylls {
                target.push_str(&text);
                if matches!(syl, Syllabic::Single | Syllabic::End) {
                    target.push(' ');
                }
            }
        }
        if follow_systems && m.line_break {
            for t in verses.values_mut() {
                end_line(t);
            }
            end_line(&mut refrain);
        }
    }
    for t in verses.values_mut() {
        *t = tidy(t);
    }
    LyricsText { verses, refrain: tidy(&refrain) }
}

fn end_line(t: &mut String) {
    let trimmed = t.trim_end().len();
    t.truncate(trimmed);
    if !t.is_empty() && !t.ends_with('\n') {
        t.push('\n');
    }
}

fn tidy(t: &str) -> String {
    t.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

pub fn lyrics_text(song: &ParsedSong, opts: &LyricsOptions) -> String {
    let mut out = String::new();
    if opts.include_header {
        let m = &song.meta;
        let mut title = m.title.clone();
        if !m.number.is_empty() {
            title = format!("{} {}", m.number, title);
        }
        out.push_str(title.trim());
        out.push('\n');
        if !m.words_credit.is_empty() {
            out.push_str(&format!("Words: {}\n", m.words_credit));
        } else if !m.lyricist.is_empty() {
            out.push_str(&format!("Words: {}\n", m.lyricist));
        }
        if !m.music_credit.is_empty() {
            out.push_str(&format!("Music: {}\n", m.music_credit));
        } else if !m.composer.is_empty() {
            out.push_str(&format!("Music: {}\n", m.composer));
        }
        if !m.tune_name.is_empty() || !m.meter.is_empty() {
            out.push_str(&format!("Tune: {} {}\n", m.tune_name, m.meter).replace("  ", " "));
        }
        out.push('\n');
    }
    let text = collect(song, opts.follow_systems);
    let n = text.verses.len();
    for (i, (num, verse)) in text.verses.iter().enumerate() {
        out.push_str(&format!("{num}.\n{verse}\n\n"));
        if !text.refrain.is_empty() && (opts.repeat_refrain || i == 0 || i + 1 == n) {
            out.push_str(&format!("Refrain:\n{}\n\n", text.refrain));
        }
    }
    for st in &song.extra_stanzas {
        out.push_str(&format!("{}.\n{}\n\n", st.number, st.text.trim()));
        if !text.refrain.is_empty() && opts.repeat_refrain {
            out.push_str(&format!("Refrain:\n{}\n\n", text.refrain));
        }
    }
    if opts.include_header && !song.meta.copyright.is_empty() {
        out.push_str(&format!("{}\n", song.meta.copyright));
    }
    out.trim_end().to_string() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::notation::parse_song;
    use crate::model::score::{Measure, Song, VoiceMeasure};

    #[test]
    fn joins_syllables() {
        let mut song = Song::default();
        song.voices.truncate(1);
        song.verse_count = 2;
        let mut m = Measure::default();
        m.voices.insert(
            "S".into(),
            VoiceMeasure {
                notes: "C4:q D4:q E4:q F4:q".into(),
                lyrics: [(1, "A- maz- ing grace".to_string()), (2, "'Twas grace that taught".to_string())].into_iter().collect(),
            },
        );
        song.measures.push(m);
        let (p, _) = parse_song(&song);
        let t = collect(&p, true);
        assert_eq!(t.verses[&1], "Amazing grace");
        assert_eq!(t.verses[&2], "'Twas grace that taught");
    }
}
