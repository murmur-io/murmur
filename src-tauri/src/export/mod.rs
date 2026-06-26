//! Obsidian export: atomic `.md` writes into a chosen vault, vault detection
//! from Obsidian's global `obsidian.json`, and note-title listing for `[[links]]`.

pub mod canvas;
pub mod entity_stub;
pub mod obsidian;

pub use obsidian::*;
