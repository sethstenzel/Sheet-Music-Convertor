//! Pull JSON out of Claude's answers and apply transcription patches.

use crate::model::score::{Measure, Song};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// Extract the JSON object from an answer: the last ```json fenced block that
/// parses, otherwise the outermost `{ … }`.
pub fn extract_json(text: &str) -> Result<Value> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
        let body = &after[body_start..];
        let Some(end) = body.find("```") else { break };
        blocks.push(&body[..end]);
        rest = &body[end + 3..];
    }
    for b in blocks.iter().rev() {
        if let Ok(v) = parse_lenient(b) {
            if v.is_object() {
                return Ok(v);
            }
        }
    }
    let first = text.find('{').ok_or_else(|| anyhow!("Claude's answer contained no JSON"))?;
    let last = text.rfind('}').ok_or_else(|| anyhow!("Claude's answer contained no complete JSON object"))?;
    if last <= first {
        bail!("Claude's answer contained no complete JSON object");
    }
    parse_lenient(&text[first..=last]).context("Claude's JSON could not be parsed")
}

/// Parse JSON, tolerating trailing commas.
fn parse_lenient(s: &str) -> Result<Value> {
    match serde_json::from_str::<Value>(s.trim()) {
        Ok(v) => Ok(v),
        Err(e) => {
            let cleaned = strip_trailing_commas(s);
            serde_json::from_str::<Value>(cleaned.trim()).map_err(|_| anyhow!("{e}"))
        }
    }
}

fn strip_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Interpret an answer as a full song, a `{"song": …}` wrapper, or a patch
/// against `current`.
pub fn song_from_answer(text: &str, current: Option<&Song>) -> Result<(Song, String)> {
    let v = extract_json(text)?;
    if let Some(patch) = v.get("patch") {
        let cur = current.ok_or_else(|| anyhow!("Claude sent a patch but there is no transcription to patch"))?;
        let summary = patch.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
        let song = apply_patch(cur, patch)?;
        return Ok((song, summary));
    }
    let song_v = v.get("song").cloned().unwrap_or(v);
    let song: Song = serde_json::from_value(song_v).context("Claude's JSON does not match the song format")?;
    Ok((song, String::new()))
}

/// Apply a patch:
/// ```json
/// {"header": {...}, "replace": [{"index": 3, "measure": {...}}],
///  "delete": [7], "insert": [{"before": 5, "measures": [...]}], "summary": "..."}
/// ```
/// Indices refer to the measures *before* the patch.
pub fn apply_patch(song: &Song, patch: &Value) -> Result<Song> {
    let mut song = song.clone();
    if let Some(header) = patch.get("header").and_then(Value::as_object) {
        if !header.is_empty() {
            let mut sv = serde_json::to_value(&song)?;
            let measures = sv.as_object_mut().and_then(|o| o.remove("measures"));
            if let Some(obj) = sv.as_object_mut() {
                for (k, val) in header {
                    if k == "measures" {
                        continue;
                    }
                    obj.insert(k.clone(), val.clone());
                }
                if let Some(m) = measures {
                    obj.insert("measures".into(), m);
                }
            }
            song = serde_json::from_value(sv).context("patch header does not match the song format")?;
        }
    }

    let n = song.measures.len();
    let mut replaced: Vec<Option<Measure>> = vec![None; n];
    if let Some(reps) = patch.get("replace").and_then(Value::as_array) {
        for r in reps {
            let idx = r.get("index").and_then(Value::as_u64).ok_or_else(|| anyhow!("replace entry without index"))? as usize;
            if idx >= n {
                bail!("patch replaces measure index {idx} but the song has {n} measures");
            }
            let m: Measure = serde_json::from_value(r.get("measure").cloned().unwrap_or(Value::Null))
                .with_context(|| format!("replacement for measure {idx} is invalid"))?;
            replaced[idx] = Some(m);
        }
    }
    let mut deleted = vec![false; n];
    if let Some(dels) = patch.get("delete").and_then(Value::as_array) {
        for d in dels {
            if let Some(i) = d.as_u64() {
                if (i as usize) < n {
                    deleted[i as usize] = true;
                }
            }
        }
    }
    let mut inserts: Vec<(usize, Vec<Measure>)> = Vec::new();
    if let Some(ins) = patch.get("insert").and_then(Value::as_array) {
        for i in ins {
            let before = i.get("before").and_then(Value::as_u64).unwrap_or(n as u64).min(n as u64) as usize;
            let ms: Vec<Measure> = serde_json::from_value(i.get("measures").cloned().unwrap_or(Value::Array(vec![])))
                .context("inserted measures are invalid")?;
            inserts.push((before, ms));
        }
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..=n {
        for (before, ms) in &inserts {
            if *before == i {
                out.extend(ms.iter().cloned());
            }
        }
        if i < n && !deleted[i] {
            out.push(replaced[i].take().unwrap_or_else(|| song.measures[i].clone()));
        }
    }
    song.measures = out;
    Ok(song)
}

/// Serialize a song for a prompt, adding an `"i"` index to every measure so
/// patches can refer to measures unambiguously.
pub fn song_json_with_indices(song: &Song) -> String {
    let mut v = serde_json::to_value(song).unwrap_or(Value::Null);
    if let Some(ms) = v.get_mut("measures").and_then(Value::as_array_mut) {
        for (i, m) in ms.iter_mut().enumerate() {
            if let Some(obj) = m.as_object_mut() {
                // Put "i" first for readability.
                let mut new = serde_json::Map::new();
                new.insert("i".into(), Value::from(i));
                for (k, val) in std::mem::take(obj) {
                    new.insert(k, val);
                }
                *obj = new;
            }
        }
    }
    compact_defaults(&mut v);
    serde_json::to_string(&v).unwrap_or_default()
}

/// Remove empty / default fields to keep prompts short.
fn compact_defaults(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.retain(|_, val| match val {
                Value::Null => false,
                Value::Bool(false) => false,
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                _ => true,
            });
            for val in map.values_mut() {
                compact_defaults(val);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(compact_defaults),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::score::VoiceMeasure;

    #[test]
    fn extracts_fenced_json() {
        let t = "Here you go:\n```json\n{\"title\": \"X\", \"measures\": [],}\n```\nDone";
        let v = extract_json(t).unwrap();
        assert_eq!(v["title"], "X");
    }

    #[test]
    fn applies_patch() {
        let mut song = Song::default();
        for i in 0..4 {
            let mut m = Measure::default();
            m.voices.insert("S".into(), VoiceMeasure { notes: format!("C{}:w", i + 3), ..Default::default() });
            song.measures.push(m);
        }
        let patch = serde_json::json!({
            "header": {"title": "New"},
            "replace": [{"index": 1, "measure": {"voices": {"S": {"notes": "D4:w"}}}}],
            "delete": [2],
            "insert": [{"before": 0, "measures": [{"pickup": true, "voices": {"S": {"notes": "G3:q"}}}]}]
        });
        let s = apply_patch(&song, &patch).unwrap();
        assert_eq!(s.meta.title, "New");
        let notes: Vec<_> = s.measures.iter().map(|m| m.voices["S"].notes.clone()).collect();
        assert_eq!(notes, vec!["G3:q", "C3:w", "D4:w", "C6:w"]);
    }

    #[test]
    fn indices_roundtrip() {
        let mut song = Song::default();
        song.measures.push(Measure::default());
        let j = song_json_with_indices(&song);
        assert!(j.contains("\"i\":0"));
        let back: Song = serde_json::from_str(&j).unwrap();
        assert_eq!(back.measures.len(), 1);
    }
}
