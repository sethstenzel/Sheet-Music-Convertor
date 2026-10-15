//! MuseScore command-line integration: convert MusicXML to .mscz / .pdf / .png
//! and open files in MuseScore Studio.

#[allow(unused_imports)]
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate MuseScore Studio (4.x preferred, 3.x accepted).
pub fn find(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim().trim_matches('"');
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        return p.is_file().then_some(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        for base in [
            std::env::var_os("ProgramFiles"),
            std::env::var_os("ProgramFiles(x86)"),
            std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Programs").into_os_string()),
        ]
        .into_iter()
        .flatten()
        {
            let base = PathBuf::from(base);
            for (dir, exe) in
                [("MuseScore 4", "MuseScore4.exe"), ("MuseScore Studio 4", "MuseScore4.exe"), ("MuseScore 3", "MuseScore3.exe")]
            {
                candidates.push(base.join(dir).join("bin").join(exe));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for app in ["MuseScore 4.app", "MuseScore Studio 4.app", "MuseScore Studio.app", "MuseScore 3.app"] {
            candidates.push(PathBuf::from("/Applications").join(app).join("Contents/MacOS/mscore"));
            if let Some(home) = directories::BaseDirs::new() {
                candidates.push(home.home_dir().join("Applications").join(app).join("Contents/MacOS/mscore"));
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["MuseScore4.exe", "MuseScore3.exe", "mscore4portable", "musescore4", "mscore4", "musescore", "mscore", "mscore3"] {
                candidates.push(dir.join(name));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for p in
            ["/usr/bin/mscore4portable", "/usr/bin/musescore", "/usr/bin/mscore", "/var/lib/flatpak/exports/bin/org.musescore.MuseScore"]
        {
            candidates.push(PathBuf::from(p));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn hidden(exe: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// Convert `input` (MusicXML) to `output`; the format follows the output
/// extension (.mscz, .mscx, .pdf, .png, .mid, .mp3…).
pub fn convert(musescore: &Path, input: &Path, output: &Path) -> Result<()> {
    let out = hidden(musescore).arg("-o").arg(output).arg(input).output().with_context(|| format!("running {}", musescore.display()))?;
    let produced = output.exists() || (output.extension().and_then(|e| e.to_str()) == Some("png") && !png_pages(output).is_empty());
    if !produced {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("MuseScore could not create {}: {}", output.display(), err.lines().rev().take(5).collect::<Vec<_>>().join(" | "));
    }
    Ok(())
}

/// MuseScore writes multi-page PNG exports as name-1.png, name-2.png …
pub fn png_pages(output: &Path) -> Vec<PathBuf> {
    let stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let dir = output.parent().unwrap_or(Path::new("."));
    let mut pages = Vec::new();
    if output.exists() {
        pages.push(output.to_path_buf());
    }
    for i in 1..100 {
        let p = dir.join(format!("{stem}-{i}.png"));
        if p.exists() {
            pages.push(p);
        } else if i > 1 {
            break;
        }
    }
    pages
}

/// Open a file in MuseScore (or, without MuseScore, the system default app).
pub fn open_in_musescore(musescore: Option<&Path>, file: &Path) -> Result<()> {
    match musescore {
        Some(ms) => {
            Command::new(ms).arg(file).spawn().with_context(|| format!("starting {}", ms.display()))?;
            Ok(())
        }
        None => open_with_default_app(file),
    }
}

/// Open a file or folder with the operating system's default application.
pub fn open_with_default_app(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        Command::new("cmd").raw_arg(format!("/C start \"\" \"{}\"", path.display())).creation_flags(0x0800_0000).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn().map_err(|e| anyhow!("xdg-open: {e}"))?;
    }
    Ok(())
}

pub const INSTALL_HELP: &str = "MuseScore Studio is free: https://musescore.org/download. \
Without it the app still saves MusicXML (.musicxml), which MuseScore opens directly; \
with it the app can also save .mscz and PDF files and show a notation preview.";
