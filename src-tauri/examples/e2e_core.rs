//! Headless end-to-end check of the MeetNotes core pipeline (no mic, no GUI):
//! transcribe a 16 kHz mono WAV with Whisper, summarize via ClaudeCodeProvider
//! (falling back to a stub note if the `claude` CLI / API is unavailable), and
//! export a `.md` into a vault dir. Driven by `scripts/e2e-core.sh`.
//!
//! Usage: cargo run --example e2e_core -- <wav_16k_mono> <whisper_model.bin> <vault_dir>

use std::path::Path;

use meetnotes_lib::audio;
use meetnotes_lib::export;
use meetnotes_lib::summarize::claude_code::ClaudeCodeProvider;
use meetnotes_lib::summarize::provider::{
    Availability, MeetingMeta, SummarizeRequest, SummarizerProvider,
};
use meetnotes_lib::transcribe::Transcriber;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: e2e_core <wav_16k_mono> <whisper_model.bin> <vault_dir>");
        std::process::exit(2);
    }
    let wav = Path::new(&args[1]);
    let model = Path::new(&args[2]);
    let vault = Path::new(&args[3]);
    let system_wav = args.get(4).map(String::as_str);

    eprintln!("[e2e] loading whisper model: {}", model.display());
    let tx = Transcriber::load(model).expect("load whisper model");

    let transcript = if let Some(sys) = system_wav {
        // Phase 2 mixing path: read mic + system tracks, resample both to 16 kHz, mix,
        // and transcribe the blend. Exercises audio::read_wav_mono / resample_to_16k / mix
        // — i.e. everything in system-audio capture EXCEPT the ScreenCaptureKit syscall.
        eprintln!("[e2e] mixing mic {} + system {}", wav.display(), sys);
        let (mic, mic_rate) = audio::read_wav_mono(wav).expect("read mic wav");
        let (sysm, sys_rate) = audio::read_wav_mono(Path::new(sys)).expect("read system wav");
        let mic16 = audio::resample_to_16k(&mic, mic_rate).expect("resample mic");
        let sys16 = audio::resample_to_16k(&sysm, sys_rate).expect("resample system");
        let mixed = audio::mix(&mic16, &sys16);
        tx.transcribe(&mixed, Some("en")).expect("transcribe mixed")
    } else {
        eprintln!("[e2e] transcribing: {}", wav.display());
        tx.transcribe_wav(wav, Some("en")).expect("transcribe wav")
    };
    println!("=== TRANSCRIPT ===");
    println!("{}", transcript.full_text);
    println!("==================");
    assert!(
        !transcript.full_text.trim().is_empty(),
        "transcript must be non-empty"
    );

    let vault_titles = export::list_vault_titles(vault).unwrap_or_default();
    let duration_s = transcript.segments.last().map(|s| s.end_s as i64).unwrap_or(0);
    let req = SummarizeRequest {
        transcript: transcript.full_text.clone(),
        meta: MeetingMeta {
            date_iso: "2026-06-24T12:00:00Z".to_string(),
            title_hint: Some("E2E core test".to_string()),
            duration_s,
            language: Some("en".to_string()),
        },
        template: String::new(),
        vault_titles,
        related_context: None,
    };

    let provider = ClaudeCodeProvider::new();
    let note = match provider.availability().await {
        Availability::Available => {
            eprintln!("[e2e] claude available — summarizing via ClaudeCodeProvider");
            match provider.summarize(&req).await {
                Ok(md) => {
                    eprintln!("[e2e] provider produced a note ({} bytes)", md.len());
                    md
                }
                Err(e) => {
                    eprintln!("[e2e] provider error: {e} — falling back to stub note");
                    stub_note(&req)
                }
            }
        }
        Availability::Unavailable { reason } => {
            eprintln!("[e2e] claude unavailable ({reason}) — using stub note");
            stub_note(&req)
        }
    };

    assert!(
        note.trim_start().starts_with("---"),
        "note must start with a YAML front-matter line"
    );

    let path = export::write_note(
        vault,
        Some("Meetings"),
        "MeetNotes E2E test",
        &req.meta.date_iso,
        &note,
    )
    .expect("write note to vault");

    println!("=== NOTE WRITTEN ===");
    println!("{}", path.display());
    println!("--- note head ---");
    for line in note.lines().take(16) {
        println!("{line}");
    }
    eprintln!("[e2e] OK");
}

fn stub_note(req: &SummarizeRequest) -> String {
    format!(
        "---\ndate: 2026-06-24\ntype: meeting\nattendees: []\ntags: [meeting, e2e]\n---\n# MeetNotes E2E test\n\n## TL;DR\nHeadless E2E: local transcription + Obsidian export verified. The AI provider step was stubbed (claude/API unavailable at run time).\n\n## Transkrypt (oczyszczony)\n{}\n",
        req.transcript
    )
}
