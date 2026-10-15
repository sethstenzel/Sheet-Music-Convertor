//! Sheet Music Convertor: photos of hymnals and sheet music → MuseScore,
//! MusicXML, MIDI and lyrics, using Claude through Claude Code or the API.

pub mod claude;
pub mod config;
pub mod export;
pub mod imaging;
pub mod jobs;
pub mod model;
pub mod pipeline;
pub mod secrets;
pub mod ui;
