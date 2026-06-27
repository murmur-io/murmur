//! AI-derived interactive timeline: speaker turns + topic spans from the transcript.
//!
//! Whisper doesn't diarize, so we ask the configured provider to infer speaker turns
//! (named if identifiable in the conversation, else "User N") and topic spans from the
//! timestamped transcript, returning strict JSON we parse into [`MeetingTimeline`].

use crate::error::{AppError, Result};
use crate::storage::models::MeetingTimeline;
use crate::summarize::provider::SummarizerProvider;
use crate::transcribe::types::Segment;

const SYSTEM: &str = "You are an expert meeting analyst. You receive a meeting transcript \
as timestamped lines `[start-end] (speaker) text` (seconds). The `(speaker)` tag comes from \
on-device diarization: `me` is the person recording the meeting; `others`, `others-0`, \
`others-1`, … are the DISTINCT people on the other side of the call. Output STRICT JSON ONLY — \
no prose, no markdown, no code fences — with EXACTLY this shape:\n\
{\"speakers\":[{\"speaker\":\"Speaker 1\",\"startS\":0.0,\"endS\":12.5}],\
\"topics\":[{\"label\":\"Budget\",\"startS\":0.0,\"endS\":60.0}]}\n\
Rules:\n\
- speakers: use the `(speaker)` TAGS as the source of truth for who is talking. Map `me` to \
the recording user (their real name if clearly stated), and each distinct `others-N` to a \
consistent label (a real name if clearly stated in the conversation, else \"Speaker 1\", \
\"Speaker 2\", …). Build consecutive, non-overlapping turns in time order from the tags; do NOT \
invent speaker changes the tags don't support. Cover the whole timeline.\n\
- topics: segment the meeting into 3-8 main topics/threads, each a short 2-4 word label \
with its start/end span; sequential spans covering the discussion.\n\
- Use only timestamps from the transcript; the final endS should be near the meeting end.\n\
- Output ONLY the JSON object, nothing before or after it.";

/// Ask the provider to derive the timeline from `segments`, then parse strict JSON out of
/// the (possibly noisy) reply.
pub async fn generate(
    provider: &dyn SummarizerProvider,
    segments: &[Segment],
    _duration_s: i64,
) -> Result<MeetingTimeline> {
    let transcript: String = segments
        .iter()
        .map(|s| {
            // Feed the canonical diarization tag (me / others / others-N) so the LLM-derived
            // timeline AGREES with the segment speaker labels instead of inventing its own.
            let who = s.speaker.as_deref().unwrap_or("?");
            format!("[{:.1}-{:.1}] ({}) {}", s.start_s, s.end_s, who, s.text.trim())
        })
        .collect::<Vec<_>>()
        .join("\n");

    let reply = provider.complete(SYSTEM, &transcript).await?;
    parse(&reply)
}

/// Extract the outermost JSON object from a reply and parse it into a [`MeetingTimeline`].
fn parse(reply: &str) -> Result<MeetingTimeline> {
    let json = match (reply.find('{'), reply.rfind('}')) {
        (Some(s), Some(e)) if e > s => &reply[s..=e],
        _ => {
            return Err(AppError::Summarize(
                "timeline: model did not return JSON".to_string(),
            ))
        }
    };
    serde_json::from_str::<MeetingTimeline>(json)
        .map_err(|e| AppError::Summarize(format!("timeline: invalid JSON ({e})")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json() {
        let r = r#"{"speakers":[{"speaker":"User 1","startS":0,"endS":5}],"topics":[{"label":"Intro","startS":0,"endS":5}]}"#;
        let t = parse(r).unwrap();
        assert_eq!(t.speakers.len(), 1);
        assert_eq!(t.speakers[0].speaker, "User 1");
        assert_eq!(t.topics[0].label, "Intro");
    }

    #[test]
    fn extracts_json_from_fenced_reply_with_aliases() {
        let r = "Here you go:\n```json\n{\"speakers\":[],\"topics\":[{\"label\":\"X\",\"start\":1,\"end\":2}]}\n```\n";
        let t = parse(r).unwrap();
        assert_eq!(t.topics.len(), 1);
        assert_eq!(t.topics[0].start_s, 1.0); // "start" alias → start_s
        assert_eq!(t.topics[0].end_s, 2.0); // "end" alias → end_s
    }

    #[test]
    fn errors_on_no_json() {
        assert!(parse("no json here").is_err());
    }
}
