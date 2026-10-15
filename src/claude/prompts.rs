//! Prompts for every request the app makes.

use super::extract::song_json_with_indices;
use super::{ImageInput, Part};
use crate::config::VoiceLayout;
use crate::model::project::{BookInfo, CollectionKind};
use crate::model::score::Song;
use crate::model::validate::Issue;

/// The format specification shared by all transcription prompts.
pub const FORMAT_SPEC: &str = r#"
## Song JSON format

Return ONE JSON object (inside a single ```json fenced block) with these fields:

{
  "title": "Holy, Holy, Holy",            // as printed (fix ALL-CAPS to title case)
  "subtitle": "",
  "number": "1",                           // number of the song in the book, if printed
  "tune_name": "NICAEA",                   // hymn tune name if printed
  "meter": "11.12.12.10",                  // poetic meter if printed (e.g. "C.M.", "8.7.8.7 D")
  "lyricist": "Reginald Heber", "composer": "John B. Dykes", "arranger": "", "translator": "",
  "words_credit": "Reginald Heber, 1826",  // full credit lines exactly as printed
  "music_credit": "John B. Dykes, 1861",
  "copyright": "",                         // copyright / permission notice as printed
  "scripture": "", "topics": [], "language": "English",
  "first_line": "Holy, holy, holy! Lord God Almighty!",
  "refrain_first_line": "",
  "other_text": "",                        // any other printed text (footnotes, alternate tune, etc.)
  "key": {"fifths": 2, "mode": "major"},   // sharps positive, flats negative
  "time": "4/4",                           // "3/4", "6/8", "3/2", "C" = common, "C|" = cut time
  "tempo": {"text": "", "bpm": 92, "beat": "q", "suggested": true},
  "staves": [{"clef": "treble"}, {"clef": "bass"}],   // clefs: treble, bass, treble8vb, alto, tenor
  "voices": [
    {"id": "S", "name": "Soprano", "staff": 1, "stem": "up"},
    {"id": "A", "name": "Alto",    "staff": 1, "stem": "down"},
    {"id": "T", "name": "Tenor",   "staff": 2, "stem": "up"},
    {"id": "B", "name": "Bass",    "staff": 2, "stem": "down"}
  ],
  "verse_count": 4,                        // number of verses printed under/between the staves
  "measures": [ MEASURE, MEASURE, ... ],   // every measure in playing order as printed
  "extra_stanzas": [{"number": 5, "text": "line one\nline two"}],  // verses printed as text only
  "transcription_notes": ""                // anything uncertain, with measure indices
}

MEASURE:
{
  "pickup": false,        // true only for an opening anacrusis
  "partial": false,       // true for any other intentionally incomplete measure
                          //   (final measure completing a pickup, measures split at repeats/sections)
  "key": null,            // {"fifths":..,"mode":..} only where the key signature changes
  "time": null,           // e.g. "3/4" only where the time signature changes
  "repeat_start": false,  // start-repeat barline at the beginning of this measure
  "barline": "",          // barline at the END: "" normal, "double", "final", "repeat-end", "dashed"
  "ending": null,         // volta number(s) this measure is under, e.g. "1" or "2" or "1, 2"
  "section": "",          // label starting here, e.g. "Refrain", "Chorus", "Amen", "Coda"
  "text": "",             // other words above the staff here (e.g. "Unison", "D.C. al Fine", "Fine")
  "line_break": false,    // true on the LAST measure of each printed system (line of music)
  "page_break": false,    // true on the last measure of each page image
  "voices": {
    "S": {"notes": "D4:q", "lyrics": {"1": "Ho-", "2": "Ho-", "3": "Ho-", "4": "Ho-"}},
    "A": {"notes": "D4:q"},
    "T": {"notes": "F#3:q"},
    "B": {"notes": "D3:q"}
  }
}

## Note tokens

"notes" is a whitespace-separated list of tokens, one per note, chord or rest, in time order.

  PITCH:DURATION[MODIFIERS]       e.g.  G4:q   F#4:e.   Bb3:h~   E5:q^
  PITCH+PITCH:DURATION            chord on one stem, e.g. C4+E4:h
  r:DURATION                      rest, e.g. r:q
  R                               whole-measure rest (any time signature)
  gPITCH:DURATION                 grace note, e.g. gD5:e

PITCH = letter A–G, optional accidental (# sharp, b flat, x double-sharp, bb double-flat, n natural),
octave number. C4 is middle C. Treble-staff lines bottom→top: E4 G4 B4 D5 F5 (spaces F4 A4 C5 E5).
Bass-staff lines bottom→top: G2 B2 D3 F3 A3 (spaces A2 C3 E3 G3). Middle C (C4) is one ledger line
below the treble staff or one ledger line above the bass staff.

PITCHES ARE SOUNDING PITCHES: always apply the key signature (in every octave) and any accidental
earlier in the same measure on the same line/space. In D major every F and C is written F#/C#
unless a natural sign cancels it. An accidental lasts until the barline.

DURATION = w whole, h half, q quarter, e eighth, s 16th, t 32nd, x 64th, B breve; add "." per dot
(q. = dotted quarter, h.. = double-dotted half); add "/3" for a triplet member (e/3).

MODIFIERS (append in any order, no spaces):
  ~  tie to the next note (same pitch)      (  slur starts on this note    )  slur ends on this note
  ^  fermata    >  accent    '  staccato    _  tenuto    ,  breath mark after the note
  {mf} dynamic (pp p mp mf f ff fff sfz fp)   {<} crescendo hairpin starts   {>} diminuendo starts
  {|} hairpin ends    {c:G7} chord symbol above this note    {t:rit.} any other text at this note

## Lyrics

"lyrics" maps verse number → the syllables sung on THIS voice's notes in THIS measure, separated by
spaces. Syllables are assigned in order to the voice's sung notes, skipping rests, grace notes and
the second note of a tie.
  • End a syllable with "-" when the word continues on the next syllable: "Ho- ly, ho- ly"
  • Use "_" for a note that continues the previous syllable (melisma under a slur): "Lord _ God"
  • Use "*" for a note that has no syllable in this verse.
  • Keep the punctuation, apostrophes and capitalization exactly as printed. Do not include the
    verse numbers ("1.", "2.") themselves.
  • A hyphenated word may cross a barline: "glo-" at the end of one measure, "ry" in the next.
  • Lyrics usually belong to the soprano / melody voice. Put words on another voice only when that
    part prints its own different words (e.g. echo phrases in a refrain).
  • A refrain printed with a single line of words: give those words as verse "1" only.

## Rules

1. Transcribe exactly what is printed: every measure, note, rest, tie, slur, fermata, dynamic, repeat,
   ending, and every verse of the lyrics. Never simplify, never summarize, never skip verses.
2. Each voice's durations in a measure must add up EXACTLY to the time signature (e.g. 4/4 = four
   quarters) unless the measure is marked "pickup" or "partial". All voices in a measure must have
   the same total. Re-count every measure before answering.
3. Hymn engraving: on the treble staff, stems-up notes are Soprano, stems-down are Alto; on the bass
   staff, stems-up are Tenor, stems-down are Bass. When two parts share one notehead or one stem,
   write the note in BOTH voices. A whole/half note with two stems belongs to both voices.
4. Measures are listed in printed order (do not unroll repeats). Mark repeat signs and endings.
5. If a song spans several page images, continue the measures across pages in order and set
   "page_break" on the last measure of each page.
6. If something is genuinely unreadable, choose the most musically plausible reading and say so in
   "transcription_notes" with the measure index (0-based position in "measures").
7. Return strictly valid JSON: no comments, no trailing commas, verse keys as strings ("1"). Omit
   measure fields that have their default value (false / "" / null) to keep the answer short.
"#;

fn layout_instructions(layout: VoiceLayout) -> &'static str {
    match layout {
        VoiceLayout::Auto => {
            "Voice layout: choose the layout that matches the engraving. For four-part hymns use voices S, A, T, B \
             (two staves) exactly as in the example. For piano/keyboard music use staves treble+bass and voices \
             \"RH\" (staff 1), \"LH\" (staff 2), plus \"RH2\"/\"LH2\" with \"optional\": true where a hand has a second \
             independent rhythm; use chords (C4+E4:q) for notes on a shared stem. For a single melody line (lead sheet, \
             unison song) use one treble staff and voice \"M\" with chord symbols in {c:...}. For a choral piece with \
             accompaniment, give the choir parts their own voices and the piano RH/LH on additional staves."
        }
        VoiceLayout::Satb => {
            "Voice layout: use four separate voices S, A, T, B on two staves exactly as in the example, even when parts \
             share noteheads or stems (write the shared note in both voices). If a passage is in unison, write the \
             same notes in each voice that sings it."
        }
        VoiceLayout::Keyboard => {
            "Voice layout: keyboard score. Staves treble + bass; voices \"RH\" (staff 1, stem auto) and \"LH\" (staff 2, \
             stem auto). Write notes sharing a stem as chords (C4+E4+G4:q). Where a hand has a second, rhythmically \
             independent line add voice \"RH2\" or \"LH2\" with \"optional\": true and include it only in the measures \
             where it appears. Put lyrics on RH."
        }
        VoiceLayout::MelodyOnly => {
            "Voice layout: melody only. One treble staff and one voice \"M\" (\"name\": \"Melody\") containing the tune \
             (the soprano line in a hymn) with all verses of lyrics. Add chord symbols with {c:...} where printed."
        }
    }
}

/// System prompt for transcription requests.
pub fn transcription_system() -> String {
    format!(
        "You are an expert music engraver and copyist who transcribes printed sheet music — especially hymnals and \
         song books — from photographs into a precise machine-readable format. Accuracy matters more than speed: \
         read every staff note by note, check clefs, key signature, accidentals, durations, dots, ties and every \
         verse of lyrics, and verify your work before answering. Answer with the JSON only (a short sentence before \
         the code block is fine).\n{FORMAT_SPEC}"
    )
}

#[derive(Clone, Debug, Default)]
pub struct PageContext {
    pub book_page: String,
    pub ignore_top: bool,
    pub ignore_bottom: bool,
    pub note: String,
    pub cropped: bool,
}

#[derive(Clone, Debug)]
pub struct SongContext {
    pub kind: CollectionKind,
    pub book: BookInfo,
    pub title_hint: String,
    pub number_hint: String,
    pub instructions: String,
    pub layout: VoiceLayout,
    pub pages: Vec<PageContext>,
}

/// Images of one page: the full page plus optional zoomed tiles (label, image).
#[derive(Clone, Debug)]
pub struct PageImages {
    pub full: ImageInput,
    pub tiles: Vec<(String, ImageInput)>,
}

fn book_description(ctx: &SongContext) -> String {
    let b = &ctx.book;
    match ctx.kind {
        CollectionKind::SoloSheet => "The images show a single song on loose sheet music.".to_string(),
        kind => {
            let mut s = format!("The images are pages photographed from a {}", kind.noun());
            if !b.title.is_empty() {
                s.push_str(&format!(" titled \"{}\"", b.title));
            }
            let mut extra = Vec::new();
            if !b.publisher.is_empty() {
                extra.push(b.publisher.clone());
            }
            if !b.year.is_empty() {
                extra.push(b.year.clone());
            }
            if !b.denomination.is_empty() {
                extra.push(b.denomination.clone());
            }
            if !extra.is_empty() {
                s.push_str(&format!(" ({})", extra.join(", ")));
            }
            if !b.language.is_empty() && !b.language.eq_ignore_ascii_case("english") {
                s.push_str(&format!(". The text is in {}", b.language));
            }
            s.push('.');
            s
        }
    }
}

fn target_description(ctx: &SongContext) -> String {
    let mut parts = Vec::new();
    if !ctx.number_hint.trim().is_empty() {
        parts.push(format!("number {}", ctx.number_hint.trim()));
    }
    if !ctx.title_hint.trim().is_empty() {
        parts.push(format!("titled \"{}\"", ctx.title_hint.trim()));
    }
    if parts.is_empty() {
        "the song shown".into()
    } else {
        format!("the song {}", parts.join(", "))
    }
}

fn page_instructions(ctx: &SongContext) -> String {
    let n = ctx.pages.len();
    let mut s = String::new();
    for (i, p) in ctx.pages.iter().enumerate() {
        let mut lines = Vec::new();
        if p.ignore_top {
            lines.push(
                "the TOP of this page contains the end of a different song — ignore it and start where this song's music begins"
                    .to_string(),
            );
        }
        if p.ignore_bottom {
            lines.push(
                "the BOTTOM of this page contains the start of a different song — stop where this song ends and ignore everything below"
                    .to_string(),
            );
        }
        if i > 0 && !p.ignore_top {
            lines.push("this page continues the song from the previous page".into());
        }
        if i + 1 < n && !p.ignore_bottom {
            lines.push("the song continues on the next page".into());
        }
        if p.cropped {
            lines.push("the image has been cropped to the relevant area".into());
        }
        if !p.note.trim().is_empty() {
            lines.push(format!("note from the user: {}", p.note.trim()));
        }
        if !lines.is_empty() {
            s.push_str(&format!("• Page {}: {}.\n", i + 1, lines.join("; ")));
        }
    }
    s
}

fn push_page_images(parts: &mut Vec<Part>, ctx: &SongContext, images: &[PageImages]) {
    let n = images.len();
    for (i, img) in images.iter().enumerate() {
        let bp = ctx.pages.get(i).map(|p| p.book_page.trim().to_string()).unwrap_or_default();
        let bp = if bp.is_empty() { String::new() } else { format!(" (printed page {bp})") };
        parts.push(Part::Text(format!("Page image {} of {n}{bp}:", i + 1)));
        parts.push(Part::Image(img.full.clone()));
        for (label, tile) in &img.tiles {
            parts.push(Part::Text(format!("Page {} — zoomed detail ({label}; tiles overlap slightly):", i + 1)));
            parts.push(Part::Image(tile.clone()));
        }
    }
}

/// First pass: transcribe the song from the page images.
pub fn transcribe(ctx: &SongContext, images: &[PageImages]) -> Vec<Part> {
    let mut parts = Vec::new();
    parts.push(Part::Text(book_description(ctx)));
    push_page_images(&mut parts, ctx, images);
    let mut task = format!(
        "Transcribe {} completely into the song JSON format, including the title, every credit line, the \
         copyright notice, tune name, meter, all measures of all voices, and every verse of the lyrics.\n",
        target_description(ctx)
    );
    if images.len() > 1 {
        task.push_str(&format!("The song spans {} page images, in order.\n", images.len()));
    }
    if images.iter().any(|i| !i.tiles.is_empty()) {
        task.push_str(
            "Zoomed detail images of each page are included: use them to read small noteheads, accidentals, dots and \
             lyrics precisely, and the full page images for the overall structure.\n",
        );
    }
    let pi = page_instructions(ctx);
    if !pi.is_empty() {
        task.push_str("Page instructions:\n");
        task.push_str(&pi);
    }
    if ctx.kind.is_book() && ctx.number_hint.trim().is_empty() {
        task.push_str("Use the song number printed near the title (if any) as \"number\".\n");
    }
    task.push_str(layout_instructions(ctx.layout));
    task.push('\n');
    if !ctx.instructions.trim().is_empty() {
        task.push_str(&format!("Additional instructions from the user: {}\n", ctx.instructions.trim()));
    }
    task.push_str(
        "Work through the music system by system and measure by measure. Before answering, re-check every \
         measure's durations against the time signature and every accidental against the key signature.",
    );
    parts.push(Part::Text(task));
    parts
}

pub const PATCH_SPEC: &str = r#"Reply with a PATCH in a single ```json block:
{"patch": {
  "summary": "one or two sentences describing what you changed",
  "header":  { ...only top-level song fields that change, e.g. "title", "key", "verse_count"... },
  "replace": [ {"index": 12, "measure": { ...complete corrected MEASURE... }} ],
  "delete":  [ 30 ],
  "insert":  [ {"before": 5, "measures": [ ...complete new MEASUREs... ]} ]
}}
"index", "before" and "delete" refer to the "i" values in the transcription above (positions before the
patch). A replaced measure must be complete (all voices, all lyrics). Omit keys you don't need. If nothing
needs to change, reply with {"patch": {"summary": "No changes needed"}}."#;

fn current_song_block(song: &Song) -> String {
    format!("Current transcription (each measure has its index \"i\"):\n```json\n{}\n```", song_json_with_indices(song))
}

/// Automatic correction of validator errors.
pub fn fix_errors(ctx: &SongContext, images: &[PageImages], song: &Song, issues: &[Issue]) -> Vec<Part> {
    let mut parts = Vec::new();
    parts.push(Part::Text(book_description(ctx)));
    push_page_images(&mut parts, ctx, images);
    parts.push(Part::Text(current_song_block(song)));
    let list: Vec<String> = issues.iter().map(|i| format!("- {}", i.describe())).collect();
    parts.push(Part::Text(format!(
        "An automatic checker found these problems in the transcription of {}:\n{}\n\n\
         Look at the page images again for each listed measure and correct the transcription so it matches the \
         printed music exactly (durations must add up, lyrics must align with the sung notes). Fix only what is \
         wrong.\n\n{PATCH_SPEC}",
        target_description(ctx),
        list.join("\n")
    )));
    parts
}

/// Independent proofreading pass.
pub fn proofread(ctx: &SongContext, images: &[PageImages], song: &Song, round: u32, warnings: &[Issue]) -> Vec<Part> {
    let mut parts = Vec::new();
    parts.push(Part::Text(book_description(ctx)));
    push_page_images(&mut parts, ctx, images);
    parts.push(Part::Text(current_song_block(song)));
    let mut task = format!(
        "You are proofreading this transcription of {} (proofreading pass {round}). Compare it against the page \
         images measure by measure and voice by voice, as a meticulous copy editor would:\n\
         1. Title, number, credits, copyright, tune name, meter, key signature, time signature, tempo marking.\n\
         2. The number of measures in each printed system, pickups, repeats, endings and final barline.\n\
         3. Every pitch (check the octave, the key signature and accidentals), every duration and dot, ties, \
            slurs and fermatas, in every voice.\n\
         4. Every syllable of every verse, its alignment under the notes, hyphenation, melismas and punctuation; \
            and any stanzas printed below the music.\n\
         Assume nothing is correct until you have checked it against the image. Pay special attention to the \
         inner voices (alto and tenor), beamed eighth-note pairs, notes on ledger lines, notes sharing a stem, \
         and dotted rhythms — these are where transcription errors usually hide. Use the zoomed detail images.\n\
         Only change something when the image clearly shows it is wrong — count noteheads and ledger lines \
         carefully before adding or removing a note. If you are unsure, keep the current reading and mention \
         the doubt in the summary.\n",
        target_description(ctx)
    );
    if !warnings.is_empty() {
        let list: Vec<String> = warnings.iter().take(40).map(|i| format!("- {}", i.describe())).collect();
        task.push_str(&format!(
            "An automatic checker flagged these possible problems (they may be false alarms — check the image):\n{}\n",
            list.join("\n")
        ));
    }
    task.push('\n');
    task.push_str(PATCH_SPEC);
    parts.push(Part::Text(task));
    parts
}

/// Settle the measures where two independent transcriptions disagree.
pub fn adjudicate(ctx: &SongContext, images: &[PageImages], song: &Song, disagreements: &[(usize, String, String)]) -> Vec<Part> {
    let mut parts = Vec::new();
    parts.push(Part::Text(book_description(ctx)));
    push_page_images(&mut parts, ctx, images);
    parts.push(Part::Text(current_song_block(song)));
    let mut list = String::new();
    for (i, a, b) in disagreements {
        list.push_str(&format!("Measure i={i}\n  Reading A: {a}\n  Reading B: {b}\n"));
    }
    parts.push(Part::Text(format!(
        "Two independent transcriptions of {} were made. The transcription above is reading A. They disagree in \
         the measures listed below. For each one, look closely at that measure in the images (use the zoomed \
         details: count noteheads, check ledger lines, accidentals, dots, ties and lyric syllables) and decide what is \
         actually printed. Reading A or B may be right, or neither. Replace every listed measure where reading A is \
         wrong with the correct measure. Don't change other measures.\n\n{list}\n{PATCH_SPEC}",
        target_description(ctx)
    )));
    parts
}

/// A correction requested by the user in their own words.
pub fn user_fix(ctx: &SongContext, images: &[PageImages], song: &Song, request: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    parts.push(Part::Text(book_description(ctx)));
    push_page_images(&mut parts, ctx, images);
    parts.push(Part::Text(current_song_block(song)));
    parts.push(Part::Text(format!(
        "The user reviewed this transcription of {} and asks for the following change:\n\"\"\"\n{}\n\"\"\"\n\
         Measure numbers mentioned by the user are most likely the printed measure numbers (the pickup measure is \
         0, the first full measure is 1), not the \"i\" index — check the images to be sure. Make the requested \
         change, check the image to do it correctly, and keep everything else unchanged.\n\n{PATCH_SPEC}",
        target_description(ctx),
        request.trim()
    )));
    parts
}

pub fn detect_system() -> String {
    "You are an expert music librarian. You look at photographed pages of hymnals and song books and identify \
     which songs appear on which pages and where. Answer with JSON only."
        .into()
}

/// Detect songs on a sequence of pages.
pub fn detect_songs(kind: CollectionKind, book: &BookInfo, images: &[(String, ImageInput)], first_index: usize) -> Vec<Part> {
    let ctx = SongContext {
        kind,
        book: book.clone(),
        title_hint: String::new(),
        number_hint: String::new(),
        instructions: String::new(),
        layout: VoiceLayout::Auto,
        pages: Vec::new(),
    };
    let mut parts = vec![Part::Text(book_description(&ctx))];
    for (i, (label, img)) in images.iter().enumerate() {
        parts.push(Part::Text(format!("Page {} ({label}):", first_index + i)));
        parts.push(Part::Image(img.clone()));
    }
    parts.push(Part::Text(format!(
        r#"These are consecutive pages, in order. For each page list every song (or part of a song) that appears on it,
from top to bottom. A song that begins on one page and continues on the next appears on both pages.
Reply with a single ```json block:
{{"pages": [
  {{"page": {first_index}, "book_page": "printed page number or empty",
    "songs": [
      {{"number": "23", "title": "Title as printed (title case)", "first_line": "first line of verse 1",
        "position": "top" | "middle" | "bottom" | "full",
        "starts_on_this_page": true, "ends_on_this_page": false,
        "has_music": true}}
    ]}}
]}}
"position": where the song's portion sits on the page ("full" if it fills the page). Use "has_music": false
for text-only items (e.g. responsive readings, indexes). For a song continuing from an earlier page,
repeat its number and title."#
    )));
    parts
}

pub fn book_info_system() -> String {
    "You read title pages, copyright pages and covers of hymnals and song books and extract bibliographic \
     information. Answer with JSON only."
        .into()
}

pub fn book_info(images: &[ImageInput]) -> Vec<Part> {
    let mut parts = Vec::new();
    for img in images {
        parts.push(Part::Image(img.clone()));
    }
    parts.push(Part::Text(
        r#"Extract the book's details from these pages. Reply with a single ```json block:
{"title": "", "subtitle": "", "abbreviation": "", "publisher": "", "year": "", "edition": "", "editor": "",
 "denomination": "", "language": "", "isbn": "", "copyright": "", "notes": ""}
Leave fields empty when not shown. "year" is the publication or copyright year. "notes" may hold other useful
details (printing, compilers)."#
            .into(),
    ));
    parts
}
