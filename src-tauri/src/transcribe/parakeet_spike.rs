//! T3 SPIKE HARNESS (2026-07-10, transcription research): NVIDIA parakeet-tdt-0.6b-v3 int8
//! through the `sherpa-onnx` crate already in the dependency tree (the diarization crate) —
//! the decision input for the Brain v2 live-ASR seam (whisper stays the batch authority; the
//! question is whether parakeet takes the LIVE path off Metal at ≥ live-quality for Polish).
//!
//! NOT a shipping path: `#[cfg(test)]`-only, `#[ignore]`d, env-driven exactly like the whisper
//! A/B harness (`asr_ab_harness_from_env`) so a human can compare both on the same WAV without
//! recompiling. See docs/research/2026-07-09-transcription-performance.md (Phase T3):
//!
//! ```sh
//! MURMUR_PARAKEET_DIR=~/models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8 \
//! MURMUR_ASR_AB_WAV=~/asr/meeting-16k.wav \
//! cargo test --lib parakeet_spike_from_env -- --ignored --nocapture
//! ```

#[cfg(test)]
mod tests {
    /// Decodes `MURMUR_ASR_AB_WAV` (16 kHz mono) with the parakeet int8 transducer from
    /// `MURMUR_PARAKEET_DIR` (encoder/decoder/joiner `.int8.onnx` + `tokens.txt`), on CPU via
    /// the bundled static onnxruntime — deliberately OFF Metal, that is the whole point of the
    /// spike. Prints model-load + decode wall-clock and writes the transcript to a temp file
    /// for a side-by-side diff with the whisper A/B legs.
    #[test]
    #[ignore = "parakeet spike: needs MURMUR_PARAKEET_DIR + MURMUR_ASR_AB_WAV on a real Mac"]
    fn parakeet_spike_from_env() {
        let Ok(dir) = std::env::var("MURMUR_PARAKEET_DIR") else {
            eprintln!("SKIP: set MURMUR_PARAKEET_DIR to the sherpa parakeet int8 model dir");
            return;
        };
        let Ok(wav) = std::env::var("MURMUR_ASR_AB_WAV") else {
            eprintln!("SKIP: set MURMUR_ASR_AB_WAV to a 16 kHz mono WAV path");
            return;
        };

        let wave = sherpa_onnx::Wave::read(&wav).expect("read wave");
        let audio_secs = wave.samples().len() as f64 / wave.sample_rate() as f64;

        let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
        config.model_config.transducer = sherpa_onnx::OfflineTransducerModelConfig {
            encoder: Some(format!("{dir}/encoder.int8.onnx")),
            decoder: Some(format!("{dir}/decoder.int8.onnx")),
            joiner: Some(format!("{dir}/joiner.int8.onnx")),
        };
        config.model_config.tokens = Some(format!("{dir}/tokens.txt"));
        config.model_config.model_type = Some("nemo_transducer".into());
        // Live-path shape: a handful of CPU threads, leaving the Metal GPU entirely to the
        // brain LLM. 4 matches the E-core budget the live loop's QoS targets.
        config.model_config.num_threads = 4;

        let t_load = std::time::Instant::now();
        let recognizer =
            sherpa_onnx::OfflineRecognizer::create(&config).expect("create parakeet recognizer");
        let load_s = t_load.elapsed().as_secs_f64();

        let t = std::time::Instant::now();
        let stream = recognizer.create_stream();
        stream.accept_waveform(wave.sample_rate(), wave.samples());
        recognizer.decode(&stream);
        let result = stream.get_result().expect("parakeet result");
        let wall = t.elapsed().as_secs_f64();

        let out = std::env::temp_dir().join("murmur-parakeet-spike.txt");
        std::fs::write(&out, &result.text).expect("write transcript");
        println!(
            "parakeet-v3 int8 (CPU x4): load {load_s:.2}s, decode {wall:.2}s for {audio_secs:.1}s \
             audio ({rtf:.1}x realtime), {chars} chars -> {path}",
            rtf = audio_secs / wall.max(1e-9),
            chars = result.text.len(),
            path = out.display()
        );
    }
}
