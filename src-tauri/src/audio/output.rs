//! Default-output introspection: is the user playing the call through the BUILT-IN SPEAKERS
//! (echo risk) or headphones/external gear? Crash-safe by construction: pure CoreAudio C
//! functions only (rules §7) — `AudioObjectGetPropertyData` returns an error code, it cannot
//! raise an ObjC exception across FFI. Real verification (speakers vs jack vs AirPods) needs a
//! real Mac — headless covers the pure classifier only.

/// 'bltn' — kAudioDeviceTransportTypeBuiltIn.
pub(crate) const TRANSPORT_BUILTIN: u32 = 0x626C_746E;
/// 'ispk' — internal speaker data source.
pub(crate) const SOURCE_INTERNAL_SPEAKER: u32 = 0x6973_706B;
/// 'hdpn' — headphones data source (built-in 3.5 mm jack).
pub(crate) const SOURCE_HEADPHONES: u32 = 0x6864_706E;

/// Pure classifier (unit-tested): transport + optional data source → speakers?.
pub(crate) fn classify_output(transport: Option<u32>, data_source: Option<u32>) -> Option<bool> {
    match transport {
        None => None,
        Some(TRANSPORT_BUILTIN) => match data_source {
            Some(SOURCE_HEADPHONES) => Some(false),
            // 'ispk' or unreadable: the built-in output without headphone routing is speakers.
            _ => Some(true),
        },
        // Bluetooth / USB / HDMI / AirPlay / anything else ⇒ not the built-in speakers.
        Some(_) => Some(false),
    }
}

#[cfg(target_os = "macos")]
fn read_u32(object: u32, selector: u32, scope: u32) -> Option<u32> {
    use objc2_core_audio::{
        kAudioObjectPropertyElementMain, AudioObjectGetPropertyData, AudioObjectPropertyAddress,
    };
    use std::ptr::NonNull;

    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut value: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    // SAFETY: `out_data`/`io_data_size` point at a live `u32` sized exactly `size`; the call
    // returns a non-zero OSStatus on any failure (it does not throw). NonNull-from-ref is valid
    // for the whole call. `in_qualifier` is empty (size 0, null ptr).
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == 0).then_some(value)
}

/// `Some(true)` = default output is the built-in speakers (echo risk); `Some(false)` =
/// headphones / external; `None` = undeterminable.
#[cfg(target_os = "macos")]
pub fn default_output_is_builtin_speakers() -> Option<bool> {
    use objc2_core_audio::{
        kAudioDevicePropertyDataSource, kAudioDevicePropertyTransportType,
        kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    };

    let device = read_u32(
        kAudioObjectSystemObject as u32,
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
    )?;
    let transport = read_u32(
        device,
        kAudioDevicePropertyTransportType,
        kAudioObjectPropertyScopeGlobal,
    );
    let source = read_u32(
        device,
        kAudioDevicePropertyDataSource,
        kAudioObjectPropertyScopeOutput,
    );
    classify_output(transport, source)
}

#[cfg(not(target_os = "macos"))]
pub fn default_output_is_builtin_speakers() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::{classify_output, SOURCE_HEADPHONES, SOURCE_INTERNAL_SPEAKER, TRANSPORT_BUILTIN};

    #[test]
    fn classifies_transport_and_data_source() {
        // Built-in transport + internal-speaker data source ⇒ speakers.
        assert_eq!(
            classify_output(Some(TRANSPORT_BUILTIN), Some(SOURCE_INTERNAL_SPEAKER)),
            Some(true)
        );
        // Built-in transport + headphone data source (3.5 mm jack) ⇒ not speakers.
        assert_eq!(
            classify_output(Some(TRANSPORT_BUILTIN), Some(SOURCE_HEADPHONES)),
            Some(false)
        );
        // Built-in transport, unreadable data source ⇒ conservatively speakers.
        assert_eq!(classify_output(Some(TRANSPORT_BUILTIN), None), Some(true));
        // Bluetooth / USB / anything else ⇒ not the built-in speakers.
        assert_eq!(classify_output(Some(0x626C7565 /* 'blue' */), None), Some(false));
        // Unknown transport ⇒ None.
        assert_eq!(classify_output(None, None), None);
    }
}
