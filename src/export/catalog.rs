//! Book-level exports: a CSV catalog and a hymnal-style index (by title,
//! first line, tune and meter).

use crate::model::project::Project;
use std::collections::BTreeMap;

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn catalog_csv(project: &Project) -> String {
    let mut out =
        String::from("Number,Title,First line,Refrain first line,Tune,Meter,Key,Time,Verses,Words,Music,Copyright,Book pages,Status\n");
    for e in &project.songs {
        let (first, refrain, tune, meter, key, time, verses, words, music, copyright) = match &e.song {
            Some(s) => (
                s.meta.first_line.clone(),
                s.meta.refrain_first_line.clone(),
                s.meta.tune_name.clone(),
                s.meta.meter.clone(),
                s.key.name(),
                s.time.clone(),
                s.verse_count.to_string(),
                if s.meta.words_credit.is_empty() { s.meta.lyricist.clone() } else { s.meta.words_credit.clone() },
                if s.meta.music_credit.is_empty() { s.meta.composer.clone() } else { s.meta.music_credit.clone() },
                s.meta.copyright.clone(),
            ),
            None => Default::default(),
        };
        let row = [
            e.display_number(),
            e.display_title(),
            first,
            refrain,
            tune,
            meter,
            key,
            time,
            verses,
            words,
            music,
            copyright,
            e.book_pages(),
            e.status.label().to_string(),
        ];
        out.push_str(&row.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

/// Index text as printed at the back of a hymnal.
pub fn index_text(project: &Project) -> String {
    let mut by_title: BTreeMap<String, String> = BTreeMap::new();
    let mut by_first: BTreeMap<String, String> = BTreeMap::new();
    let mut by_tune: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut by_meter: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in &project.songs {
        let num = e.display_number();
        let title = e.display_title();
        by_title.insert(format!("{}\u{0}{}", title.to_lowercase(), num), format!("{title}  {num}"));
        if let Some(s) = &e.song {
            if !s.meta.first_line.is_empty() {
                by_first.insert(format!("{}\u{0}{}", s.meta.first_line.to_lowercase(), num), format!("{}  {num}", s.meta.first_line));
            }
            if !s.meta.refrain_first_line.is_empty() {
                by_first.insert(
                    format!("{}\u{0}{}", s.meta.refrain_first_line.to_lowercase(), num),
                    format!("{} (refrain)  {num}", s.meta.refrain_first_line),
                );
            }
            if !s.meta.tune_name.is_empty() {
                by_tune.entry(s.meta.tune_name.to_uppercase()).or_default().push(num.clone());
            }
            if !s.meta.meter.is_empty() {
                by_meter.entry(s.meta.meter.clone()).or_default().push(if s.meta.tune_name.is_empty() {
                    num.clone()
                } else {
                    format!("{} {num}", s.meta.tune_name.to_uppercase())
                });
            }
        }
    }
    let mut out = String::new();
    let title = if project.book.title.is_empty() { project.name.clone() } else { project.book.title.clone() };
    out.push_str(&format!("{title}\n\n"));
    out.push_str("INDEX OF TITLES\n");
    for v in by_title.values() {
        out.push_str(&format!("  {v}\n"));
    }
    if !by_first.is_empty() {
        out.push_str("\nINDEX OF FIRST LINES\n");
        for v in by_first.values() {
            out.push_str(&format!("  {v}\n"));
        }
    }
    if !by_tune.is_empty() {
        out.push_str("\nALPHABETICAL INDEX OF TUNES\n");
        for (k, v) in &by_tune {
            out.push_str(&format!("  {k}  {}\n", v.join(", ")));
        }
    }
    if !by_meter.is_empty() {
        out.push_str("\nMETRICAL INDEX OF TUNES\n");
        for (k, v) in &by_meter {
            out.push_str(&format!("  {k}\n    {}\n", v.join(", ")));
        }
    }
    out
}
