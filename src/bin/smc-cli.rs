//! Command-line companion to the desktop app, for scripting and batch work.
//!
//! ```text
//! smc-cli transcribe [options] PAGE.jpg [PAGE2.jpg …]
//!     --out DIR              where to write files (default: current directory)
//!     --title TEXT           title hint            --number N      number in the book
//!     --book TEXT            book title (marks the pages as coming from a hymnal)
//!     --ignore-top P         ignore the song at the top of page P (1-based, repeatable)
//!     --ignore-bottom P      ignore the song at the bottom of page P (repeatable)
//!     --accuracy standard|high|maximum      --model opus|sonnet|fable
//!     --note TEXT            instructions for Claude
//! smc-cli export SONG.json [--out DIR]     re-export a saved transcription
//! smc-cli check SONG.json                  run the validator
//! ```
//! Uses the connection configured in the desktop app (subscription or API key).

use anyhow::{anyhow, bail, Result};
use sheet_music_convertor::claude::{backend_from_config, Cancel};
use sheet_music_convertor::config::{Accuracy, AppConfig};
use sheet_music_convertor::export::{self, ExportSettings};
use sheet_music_convertor::model::project::{BookInfo, CollectionKind, Project, SongEntry, SongPage, SourceInfo};
use sheet_music_convertor::model::score::Song;
use sheet_music_convertor::model::validate::validate;
use sheet_music_convertor::pipeline::{self, Env, Event, SongInput};
use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        println!(
            "{}",
            include_str!("smc-cli.rs").lines().skip(3).take(13).map(|l| l.trim_start_matches("//! ")).collect::<Vec<_>>().join("\n")
        );
        return Ok(());
    };
    match cmd.as_str() {
        "transcribe" => transcribe(&args[1..]),
        "export" => {
            let (song, out) = load_song_args(&args[1..])?;
            write_exports(&song, &out, &SourceInfo::default())
        }
        "check" => {
            let (song, _) = load_song_args(&args[1..])?;
            let (_, issues) = validate(&song);
            for i in &issues {
                println!("{}", i.describe());
            }
            println!("{} issue(s)", issues.len());
            Ok(())
        }
        other => bail!("unknown command '{other}' (use transcribe, export or check)"),
    }
}

fn load_song_args(args: &[String]) -> Result<(Song, PathBuf)> {
    let mut out = PathBuf::from(".");
    let mut file = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--out" {
            out = PathBuf::from(args.get(i + 1).ok_or_else(|| anyhow!("--out needs a value"))?);
            i += 2;
        } else {
            file = Some(PathBuf::from(&args[i]));
            i += 1;
        }
    }
    let file = file.ok_or_else(|| anyhow!("missing SONG.json"))?;
    let song: Song = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
    Ok((song, out))
}

fn transcribe(args: &[String]) -> Result<()> {
    let mut cfg = AppConfig::load();
    let mut out = PathBuf::from(".");
    let mut pages: Vec<PathBuf> = Vec::new();
    let mut ignore_top = Vec::new();
    let mut ignore_bottom = Vec::new();
    let (mut title, mut number, mut note) = (String::new(), String::new(), String::new());
    let mut book = BookInfo::default();
    let mut i = 0;
    let val = |i: usize| args.get(i + 1).cloned().ok_or_else(|| anyhow!("{} needs a value", args[i]));
    while i < args.len() {
        match args[i].as_str() {
            "--out" => out = PathBuf::from(val(i)?),
            "--title" => title = val(i)?,
            "--number" => number = val(i)?,
            "--book" => book.title = val(i)?,
            "--note" => note = val(i)?,
            "--model" => cfg.model = val(i)?,
            "--ignore-top" => ignore_top.push(val(i)?.parse::<usize>()?),
            "--ignore-bottom" => ignore_bottom.push(val(i)?.parse::<usize>()?),
            "--accuracy" => {
                cfg.accuracy = match val(i)?.as_str() {
                    "standard" => Accuracy::Standard,
                    "high" => Accuracy::High,
                    "maximum" | "max" => Accuracy::Maximum,
                    o => bail!("unknown accuracy '{o}'"),
                }
            }
            p => {
                pages.push(PathBuf::from(p));
                i += 1;
                continue;
            }
        }
        i += 2;
    }
    if pages.is_empty() {
        bail!("give at least one page image");
    }
    std::fs::create_dir_all(&out)?;
    // A throwaway project folder inside the output directory holds the page copies.
    let proj_dir = out.join(".smc-pages");
    let project =
        Project::create(&proj_dir, "cli", if book.title.is_empty() { CollectionKind::SoloSheet } else { CollectionKind::Hymnal })?;
    let mut song_pages: Vec<SongPage> = Vec::new();
    for (n, p) in pages.iter().enumerate() {
        let mut pg = project.import_image(p)?;
        pg.ignore_top = ignore_top.contains(&(n + 1));
        pg.ignore_bottom = ignore_bottom.contains(&(n + 1));
        song_pages.push(pg);
    }
    let input = SongInput {
        kind: project.kind,
        book: book.clone(),
        images_dir: project.images_dir(),
        pages: song_pages.clone(),
        title_hint: title,
        number_hint: number,
        instructions: note,
        current: None,
    };
    let backend = backend_from_config(&cfg);
    let cancel = Cancel::default();
    let env = Env { backend: backend.as_ref(), cfg: &cfg, cancel: &cancel };
    let started = std::time::Instant::now();
    let mut emit = |e: Event| match e {
        Event::Status(s) => eprintln!("[{:>4}s] {s}", started.elapsed().as_secs()),
        Event::Log(s) => eprintln!("[{:>4}s] {s}", started.elapsed().as_secs()),
        Event::RateLimit(r) => eprintln!("[{:>4}s] usage: {}", started.elapsed().as_secs(), r.summary),
        Event::Streaming(_) => {}
    };
    let outcome = pipeline::transcribe(&env, &input, &mut emit)?;
    for (k, (label, s)) in outcome.revisions.iter().enumerate() {
        std::fs::write(out.join(format!("revision-{k}-{}.json", export::sanitize_filename(label))), serde_json::to_string_pretty(s)?)?;
    }
    std::fs::write(out.join("song.json"), serde_json::to_string_pretty(&outcome.song)?)?;
    for i in &outcome.issues {
        println!("{}", i.describe());
    }
    let entry = SongEntry { pages: song_pages, song: Some(outcome.song.clone()), ..Default::default() };
    let source = SourceInfo { kind: project.kind, book, pages: entry.book_pages() };
    write_exports(&outcome.song, &out, &source)?;
    println!(
        "Done in {}s: {} requests, {} output tokens{}",
        started.elapsed().as_secs(),
        outcome.usage.requests,
        outcome.usage.output_tokens,
        if outcome.usage.cost_usd > 0.0 { format!(", ≈${:.2} API-equivalent", outcome.usage.cost_usd) } else { String::new() }
    );
    Ok(())
}

fn write_exports(song: &Song, out: &std::path::Path, source: &SourceInfo) -> Result<()> {
    let settings = ExportSettings::default();
    let parsed = export::prepare(song, 0);
    let stem = export::file_stem("{number} {title}", &song.meta.number, &song.display_title(), "");
    std::fs::create_dir_all(out)?;
    let xml = out.join(format!("{stem}.musicxml"));
    std::fs::write(&xml, export::musicxml::write_musicxml(&parsed, &settings.xml, source))?;
    std::fs::write(out.join(format!("{stem}.mid")), export::midi::write_midi(&parsed, &settings.midi_opts))?;
    std::fs::write(out.join(format!("{stem}.txt")), export::lyrics::lyrics_text(&parsed, &Default::default()))?;
    println!("Wrote {}", xml.display());
    if let Some(ms) = export::musescore::find(&AppConfig::load().musescore_path) {
        let mscz = out.join(format!("{stem}.mscz"));
        match export::musescore::convert(&ms, &xml, &mscz) {
            Ok(()) => println!("Wrote {}", mscz.display()),
            Err(e) => eprintln!("MuseScore conversion failed: {e:#}"),
        }
    }
    Ok(())
}
