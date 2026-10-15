# Sheet Music Convertor

A desktop app (Rust + egui) that turns photos of hymnal pages and other sheet music into
**MuseScore** scores (`.mscz`), **MusicXML**, **PDF**, **piano MIDI**, part-practice MIDI and
lyrics text. Claude does the reading; the app does the checking, correcting and exporting.

It uses your **Claude subscription** through [Claude Code](https://claude.com/claude-code)
(recommended), or an Anthropic API key, either through Claude Code or directly.

## Features

- **Projects for hymnals, song books or single sheets.** When you create a project, the app asks
  what you are converting. For a book it saves the title, publisher, year, edition and so on with
  every song. Claude can fill these in from a photo of the title page.
- **Songs over several pages.** Add a song's pages one at a time, in order. On each page you can:
  - tick *Ignore song at the top* or *Ignore song at the bottom* when another song shares the page
  - rotate or crop it
  - turn contrast clean-up (shadow removal) on or off
  - add a note for Claude, e.g. "only the second song on this page"
- **Page inbox with automatic song detection.** Drop in photos of many pages and Claude lists which
  songs are on which pages, including songs that carry over to the next page. The app then creates
  the songs with the right pages and ignore settings.
- **Accurate multi-pass transcription.**
  1. Claude transcribes into a compact, checkable note format.
  2. A built-in validator checks:
     - measure lengths in every voice
     - that voices line up
     - lyric syllables against sung notes
     - ties, voice ranges, pickup/final measure balance, and verse counts
  3. Claude is asked to fix any errors.
  4. In High and Maximum accuracy, Claude proofreads the result against zoomed tiles of the photo
     at near-native resolution. Maximum also compares two independent transcriptions first.
  5. The version with the fewest errors is kept, and every version stays in History.
- **Everything on the page.** Title, hymn number, tune name, meter, words and music credits, and
  copyright. Every verse between the staves, plus stanzas printed below the music. Repeats,
  endings, fermatas, dynamics, chord symbols, section labels (Refrain, Amen) and printed line breaks.
- **Review side by side.** The photo sits next to the transcription. With MuseScore installed you
  also get an engraved preview. There is:
  - a piano roll and a measure-by-measure editor with live checking
  - an "Ask Claude to correct something" box for plain-language fixes
  - a JSON editor and full revision history
- **Exports:**
  - `.mscz` and PDF (through MuseScore's command line)
  - MusicXML, in either *hymn* layout (2 staves, SATB) or *open score* layout (one staff per voice)
  - Piano or organ MIDI that plays every verse, with an introduction, fermatas held and a final
    ritardando
  - Part-practice MIDI with one voice brought out, for choir rehearsal
  - Lyrics as text for slides
  - Transposition to any key
  - Book catalog (CSV) and a hymnal-style index of titles, first lines, tunes and meters
- **Batch work.** Transcribe all drafts or export all songs. Jobs run in the background; you can
  choose how many run at once.
- **Command line.** `smc-cli` transcribes and exports without the GUI, for scripts.

## Requirements

- Rust 1.80+ to build: `cargo build --release`
- For subscription use, **Claude Code** must be installed and signed in. The app can do the sign-in
  for you:
  - Windows: `irm https://claude.ai/install.ps1 | iex`
  - macOS / Linux: `curl -fsSL https://claude.ai/install.sh | bash`
  - or `npm install -g @anthropic-ai/claude-code`
- **MuseScore Studio 4** (optional, free) for `.mscz`, PDF and the notation preview. Without it
  you get `.musicxml` files, which MuseScore opens directly.

## Signing in

Open **Account** (it opens automatically the first time):

| Option | How it signs in |
|---|---|
| Claude subscription (via Claude Code) — *recommended* | **Sign in with Claude…** opens a window that runs `claude auth login --claudeai`. Your browser shows the Claude sign-in page. The app watches `claude auth status` and turns green once you're in. *Advanced:* create a dedicated long-lived token with `claude setup-token` and paste it; it is kept in the OS credential manager and passed to Claude Code as `CLAUDE_CODE_OAUTH_TOKEN`. |
| API key (via Claude Code) | Paste a Console API key, or sign Claude Code in with `claude auth login --console`. |
| API key (direct API) | Paste a Console API key. The app calls the Messages API itself; Claude Code is not needed. |

Keys and tokens are stored in Windows Credential Manager, the macOS Keychain, or the Secret
Service on Linux. They are never written to project files.

The app runs Claude Code headless: `claude -p --input-format stream-json`, with all tools
disabled, its own system prompt, no project settings, and an empty working directory. Images
go in as base64 content blocks, so Claude never gets file or shell access. The top bar shows
your subscription usage (5-hour and 7-day windows) as reported by Claude Code.

## Workflow

1. **Project → New project** and choose *Solo sheet*, *Hymnal*, *Song book* or *Other*.
2. Add a song (➕), type its number/title if you know them, and add its pages one at a time.
   Tick *Ignore song at top/bottom* where neighbouring songs share a page.
3. Click **Transcribe**. A hymn usually takes 1–3 minutes per pass.
4. **Review**: compare with the photo, click problems to jump to the measure, edit tokens or ask
   Claude in plain words, then **Mark as reviewed**.
5. **Export**, or use **Export all transcribed songs** on the book page.

Tips for photos: flat page, even light, phone parallel to the page, whole page in frame,
highest resolution. HEIC photos need converting to JPEG first.

## Accuracy modes

| Mode | Requests | What happens |
|---|---|---|
| Standard | 1 + fixes | Transcribe, validate, auto-fix errors |
| High (default) | 2 + fixes | + proofreading pass with zoomed tiles of each page |
| Maximum | 4 + fixes | Two independent transcriptions in parallel (full page and zoomed tiles). The measures where they disagree are settled against the image, then a proofreading pass runs. |

Two independent readings rarely make the *same* mistake, so their disagreements point straight at the
uncertain notes. This is why Maximum mode catches errors in inner voices that a single proofreading
pass can miss.

## The note format

Claude writes each voice's measure as tokens. You can edit these in the measure editor.

```
G4:q   F#4:e.   Bb3:h~   C4+E4+G4:h   r:q   R   gD5:e   E4:e/3
```

- **Pitch:** letter, accidental (`#` `b` `x` `bb` `n`), octave (C4 = middle C). Pitches are
  sounding pitches, with the key signature already applied.
- **Duration:** `w h q e s t x B`, `.` for each dot, `/3` for triplets. `R` is a whole-measure rest.
- **Modifiers:**
  - `~` tie
  - `(` `)` slur start and end
  - `^` fermata, `>` accent, `'` staccato, `_` tenuto, `,` breath mark
  - `{mf}` dynamic; `{<}` / `{>}` / `{|}` hairpin start (crescendo / diminuendo) and stop
  - `{c:G7}` chord symbol, `{t:rit.}` text
- **Lyrics** (per verse) are syllables matched to the sung notes in order: `glo- ry` splits a word
  across notes, `_` holds the previous syllable over the next note (melisma), and `*` leaves a
  note without a syllable.

## Command line

```
smc-cli transcribe --number 35 --book "The Methodist Hymnal" --ignore-bottom 1 page25.jpg --out out/
smc-cli check out/song.json
smc-cli export out/song.json --out out/
```

## Project layout on disk

```
My Hymnal/
  project.json   book info, songs, page settings, transcriptions, history
  images/        imported page photos (deduplicated)
  exports/       default export folder
```

## Development

```
cargo test     # parser, validator, MusicXML, MIDI, transposition, patching, lyrics
cargo run      # desktop app
cargo run --bin smc-cli -- transcribe …
```

Source map: `src/model` (song format, parser, validator, transposition), `src/claude` (CLI and
API backends, sign-in, prompts, JSON patching), `src/pipeline.rs` (multi-pass transcription),
`src/export` (MusicXML, MIDI, MuseScore, lyrics, catalog), `src/ui` (egui screens).
