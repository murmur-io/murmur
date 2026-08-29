//! Canonical plaintext payload for one shared CONTAINER — a Space or a Folder published to an org.
//!
//! The payload is serialized into `OrgEnvelope.markdown` and rides the ordinary OCK-sealed org item,
//! exactly as [`crate::share::task_envelope`] does for Tasks. Keeping container structure inside the
//! existing envelope is what makes this feature need NO server change: the relay stores one more
//! opaque ciphertext blob and never learns that a folder exists, what it is called, or what is in it.
//!
//! STRUCTURE IS A PARENT POINTER, NOT A CHILD LIST. A manifest names its own parent and position;
//! it does not enumerate its children. The alternative — a `children: Vec<docId>` list — turns every
//! add, move and remove into a compare-and-swap against one contended document, so two members
//! filing notes at the same time would race. With a parent pointer, adding a note is one ordinary
//! publish of that note and the manifest changes only when the container itself is renamed,
//! re-tinted or re-ordered.
//!
//! `container_id` is client-generated, opaque to the relay, and is deliberately NEVER the local
//! `folders.id`: a local identifier would hand the relay a stable cross-org correlator for the
//! owner's vault.

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// Wire version of the manifest payload. Bump only for a breaking shape change; readers reject an
/// unknown version fail-closed.
pub const CONTAINER_ENVELOPE_VERSION: u16 = 1;

/// Upper bound on a container name and on a parent-container identifier, in bytes. A name is
/// user-authored text that rides the sealed envelope, so it is bounded for the same reason every
/// other length-prefixed field is: a malformed or hostile payload must fail closed rather than
/// allocate.
pub const MAX_CONTAINER_NAME_BYTES: usize = 512;

/// Which level of the workspace hierarchy this manifest describes.
///
/// Mirrors `folders.level` (`'project' | 'folder'`), but uses the USER-FACING word for the top
/// level: the app calls it a Space everywhere the user can see it, and this string is content that
/// crosses to another member's device, so it says what they will read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerLevel {
    Space,
    Folder,
}

impl ContainerLevel {
    /// The lowercase wire label, also used as the stored `org_containers.level` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Space => "space",
            Self::Folder => "folder",
        }
    }

    /// Parse the stored/wire label. Unknown input fails closed rather than defaulting to a level,
    /// because guessing would silently promote a folder to a top-level Space in another member's
    /// sidebar.
    ///
    /// Named `parse`, not `from_str`, to stay clear of `std::str::FromStr` — the same name
    /// `OrgItemAccess::parse` already uses in this tree.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "space" => Ok(Self::Space),
            "folder" => Ok(Self::Folder),
            _ => Err(AppError::InvalidArg("unknown container level".into())),
        }
    }

    /// Map the LOCAL `folders.level` column (`'project' | 'folder'`) onto the wire level.
    pub fn from_local_level(level: &str) -> Self {
        match level {
            "project" => Self::Space,
            _ => Self::Folder,
        }
    }
}

/// The manifest one shared container publishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerEnvelope {
    pub v: u16,
    /// Stable, client-generated, opaque. Equals the org `docId` this manifest is published under,
    /// so a rename supersedes the same document instead of minting a second container.
    pub container_id: String,
    pub level: ContainerLevel,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub emoji: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tint: Option<String>,
    /// The manifest this container hangs under, or `None` at the share root.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent_container_id: Option<String>,
    #[serde(default)]
    pub position: i64,
}

impl ContainerEnvelope {
    /// Serialize to the canonical JSON payload. `serde_json` emits object keys in declaration
    /// order, so two devices holding the same logical manifest produce identical bytes — which is
    /// what makes the enclosing envelope's `content_sha256` a stable dedup key.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"))
    }

    /// Parse and VALIDATE a manifest payload. Every rejection is content-free: the message names
    /// the field that failed, never the value that failed it.
    pub fn from_json(s: &str) -> Result<Self> {
        let parsed: Self = serde_json::from_str(s)
            .map_err(|_| AppError::InvalidArg("malformed container manifest".into()))?;
        parsed.validate()?;
        Ok(parsed)
    }

    fn validate(&self) -> Result<()> {
        if self.v != CONTAINER_ENVELOPE_VERSION {
            return Err(AppError::InvalidArg(
                "unsupported container manifest version".into(),
            ));
        }
        if self.container_id.trim().is_empty() {
            return Err(AppError::InvalidArg(
                "container manifest is missing its identifier".into(),
            ));
        }
        if self.container_id.len() > MAX_CONTAINER_NAME_BYTES {
            return Err(AppError::InvalidArg(
                "container manifest identifier is too long".into(),
            ));
        }
        if self.name.len() > MAX_CONTAINER_NAME_BYTES {
            return Err(AppError::InvalidArg("container name is too long".into()));
        }
        match self.parent_container_id.as_deref() {
            Some(parent) if parent.trim().is_empty() => {
                return Err(AppError::InvalidArg(
                    "container manifest has an empty parent identifier".into(),
                ));
            }
            Some(parent) if parent.len() > MAX_CONTAINER_NAME_BYTES => {
                return Err(AppError::InvalidArg(
                    "container manifest parent identifier is too long".into(),
                ));
            }
            Some(parent) if parent == self.container_id => {
                // A self-parented manifest is the one cycle a single payload can express, and the
                // forest reader would have to defend against it forever. Refuse it at the door.
                return Err(AppError::InvalidArg(
                    "container manifest cannot be its own parent".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ContainerEnvelope {
        ContainerEnvelope {
            v: CONTAINER_ENVELOPE_VERSION,
            container_id: "c-1".into(),
            level: ContainerLevel::Space,
            name: "Klienci".into(),
            emoji: Some("📁".into()),
            tint: Some("teal".into()),
            parent_container_id: None,
            position: 3,
        }
    }

    #[test]
    fn round_trips_byte_identical() {
        let e = sample();
        let json = e.to_json();
        let back = ContainerEnvelope::from_json(&json).unwrap();
        assert_eq!(back, e);
        assert_eq!(back.to_json(), json);
    }

    #[test]
    fn wire_keys_are_camel_case() {
        let mut e = sample();
        e.parent_container_id = Some("c-parent".into());
        let json = e.to_json();
        assert!(json.contains("\"containerId\""));
        assert!(json.contains("\"parentContainerId\""));
        assert!(!json.contains('_'), "no snake_case key may reach the wire");
    }

    #[test]
    fn absent_optionals_stay_off_the_wire() {
        let e = ContainerEnvelope {
            emoji: None,
            tint: None,
            ..sample()
        };
        let json = e.to_json();
        assert!(!json.contains("emoji"));
        assert!(!json.contains("tint"));
        assert!(!json.contains("parentContainerId"));
    }

    #[test]
    fn unknown_version_fails_closed() {
        let json = r#"{"v":99,"containerId":"c","level":"space","name":"n","position":0}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn unknown_level_fails_closed() {
        let json = r#"{"v":1,"containerId":"c","level":"galaxy","name":"n","position":0}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn unknown_field_fails_closed() {
        let json =
            r#"{"v":1,"containerId":"c","level":"space","name":"n","position":0,"extra":true}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn oversized_name_fails_closed() {
        let e = ContainerEnvelope {
            name: "x".repeat(MAX_CONTAINER_NAME_BYTES + 1),
            ..sample()
        };
        assert!(ContainerEnvelope::from_json(&e.to_json()).is_err());
    }

    #[test]
    fn empty_container_id_fails_closed() {
        let json = r#"{"v":1,"containerId":"","level":"folder","name":"n","position":0}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn empty_parent_id_fails_closed() {
        let json = r#"{"v":1,"containerId":"c","level":"folder","name":"n","position":0,"parentContainerId":"  "}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn self_parented_manifest_fails_closed() {
        let json = r#"{"v":1,"containerId":"c","level":"folder","name":"n","position":0,"parentContainerId":"c"}"#;
        assert!(ContainerEnvelope::from_json(json).is_err());
    }

    #[test]
    fn level_maps_from_the_local_column_and_back() {
        assert_eq!(
            ContainerLevel::from_local_level("project"),
            ContainerLevel::Space
        );
        assert_eq!(
            ContainerLevel::from_local_level("folder"),
            ContainerLevel::Folder
        );
        assert_eq!(ContainerLevel::Space.as_str(), "space");
        assert_eq!(ContainerLevel::Folder.as_str(), "folder");
        assert_eq!(
            ContainerLevel::parse("space").unwrap(),
            ContainerLevel::Space
        );
        assert!(ContainerLevel::parse("project").is_err());
    }

    #[test]
    fn a_missing_position_defaults_to_zero() {
        let json = r#"{"v":1,"containerId":"c","level":"folder","name":"n"}"#;
        assert_eq!(ContainerEnvelope::from_json(json).unwrap().position, 0);
    }
}
