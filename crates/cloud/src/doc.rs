//! The document a device verifies and applies: the JSON inside
//! `SignedConfig.payload`. Its fields are those of
//! `Config` in cloud.proto; a test checks that the two stay in step.

use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "1.0";
pub const ACTION_ALLOW: &str = "EXEC_ACTION_ALLOW";
pub const ACTION_REVOKE: &str = "EXEC_ACTION_REVOKE";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigDoc {
    pub schema: String,
    pub serial: u64,
    pub issued_at: String,
    pub min_client_version: String,
    pub features: Vec<String>,
    pub poll_interval_secs: u32,
    pub update: UpdateDoc,
    pub digest: DigestDoc,
    pub local: LocalDoc,
    pub force: Vec<ForceDoc>,
    pub requests: Vec<ExecDoc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind_rules: Option<ResourceRefDoc>,
    #[serde(default)]
    pub paths: PathsDoc,
}

/// Where sessions from other devices go when restored. Edited in the panel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathsDoc {
    pub map: Vec<PathMapDoc>,
}

/// A working directory that starts with `from` is taken to be `to` plus the
/// rest. The server keeps these as plain text and never reads them as paths.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMapDoc {
    pub from: String,
    /// Empty: the device drops its own rule with this `from`.
    #[serde(default)]
    pub to: String,
    /// Device IDs; empty = every device.
    #[serde(default)]
    pub devices: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateDoc {
    pub channel: String,
    pub urls: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DigestDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<ResourceRefDoc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ResourceRefDoc>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalDoc {
    pub copy_form: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceRefDoc {
    pub name: String,
    pub version: String,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ForceDoc {
    pub id: String,
    pub changes: Vec<ChangeDoc>,
    pub devices: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangeDoc {
    pub key: String,
    pub value_json: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExecDoc {
    pub id: String,
    /// `EXEC_ACTION_ALLOW` or `EXEC_ACTION_REVOKE`, the enum names of cloud.proto.
    pub action: String,
    pub devices: Vec<String>,
}

impl ConfigDoc {
    /// What an unauthenticated device gets: the defaults, with nothing aimed at
    /// particular devices. Path maps are someone's own folders, so they stay
    /// out too.
    pub fn public_view(&self) -> ConfigDoc {
        ConfigDoc {
            force: Vec::new(),
            requests: Vec::new(),
            paths: PathsDoc::default(),
            ..self.clone()
        }
    }

    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a plain struct serializes")
    }

    /// SHA-256 of every resource the document refers to.
    pub fn resource_shas(&self) -> Vec<String> {
        let mut shas: Vec<String> = [
            self.digest.excerpt.as_ref(),
            self.digest.summary.as_ref(),
            self.kind_rules.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|r| r.sha256.clone())
        .collect();
        shas.sort();
        shas.dedup();
        shas
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn full() -> ConfigDoc {
        let r = || ResourceRefDoc {
            name: "n".into(),
            version: "v".into(),
            sha256: "ab".repeat(32),
            executable: false,
        };
        ConfigDoc {
            schema: SCHEMA.into(),
            serial: 3,
            issued_at: "2026-11-23T00:00:00Z".into(),
            min_client_version: "0.1.0".into(),
            features: vec!["x".into()],
            poll_interval_secs: 3600,
            update: UpdateDoc {
                channel: "stable".into(),
                urls: vec!["u".into()],
            },
            digest: DigestDoc {
                excerpt: Some(r()),
                summary: Some(r()),
            },
            local: LocalDoc {
                copy_form: "pruned".into(),
            },
            force: vec![ForceDoc {
                id: "f".into(),
                changes: vec![ChangeDoc {
                    key: "local.copy_form".into(),
                    value_json: "\"full\"".into(),
                }],
                devices: vec!["d".into()],
            }],
            requests: vec![ExecDoc {
                id: "r".into(),
                action: ACTION_ALLOW.into(),
                devices: vec![],
            }],
            kind_rules: Some(r()),
            paths: PathsDoc {
                map: vec![PathMapDoc {
                    from: "D:\\work".into(),
                    to: "/Users/me/work".into(),
                    devices: vec!["d".into()],
                }],
            },
        }
    }

    #[test]
    fn public_view_drops_what_is_aimed_at_devices() {
        let public = full().public_view();
        assert!(public.force.is_empty() && public.requests.is_empty());
        assert!(public.paths.map.is_empty());
        assert_eq!(public.digest, full().digest);
    }

    #[test]
    fn an_older_payload_without_paths_still_reads() {
        let mut v = serde_json::to_value(full()).unwrap();
        v.as_object_mut().unwrap().remove("paths");
        let doc: ConfigDoc = serde_json::from_value(v).unwrap();
        assert!(doc.paths.map.is_empty());
    }

    #[test]
    fn lists_each_resource_once() {
        assert_eq!(full().resource_shas().len(), 1);
    }

    /// The JSON must have exactly the fields cloud.proto's messages have, except
    /// `storages`, which this server does not send.
    #[test]
    fn matches_the_proto_schema() {
        let dir = std::env::var("NEKORISU_PROTO_DIR")
            .unwrap_or_else(|_| format!("{}/../../../nekorisu/proto", env!("CARGO_MANIFEST_DIR")));
        let set = protox::compile(["nekorisu/cloud/v1/cloud.proto"], [dir]).unwrap();
        let file = &set.file[0];
        let fields = |message: &str| -> BTreeSet<String> {
            file.message_type
                .iter()
                .find(|m| m.name() == message)
                .unwrap_or_else(|| panic!("no message {message}"))
                .field
                .iter()
                .map(|f| f.name().to_string())
                .collect()
        };
        let keys = |v: &serde_json::Value| -> BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
        let v = serde_json::to_value(full()).unwrap();
        assert_eq!(keys(&v), fields("Config"));
        assert_eq!(keys(&v["update"]), fields("UpdateConfig"));
        assert_eq!(keys(&v["digest"]), fields("DigestConfig"));
        assert_eq!(keys(&v["local"]), fields("LocalConfig"));
        assert_eq!(keys(&v["force"][0]), fields("ForceRequest"));
        assert_eq!(keys(&v["force"][0]["changes"][0]), fields("ForceChange"));
        assert_eq!(keys(&v["requests"][0]), fields("ExecRequest"));
        assert_eq!(keys(&v["kind_rules"]), fields("ResourceRef"));
        assert_eq!(keys(&v["paths"]), fields("PathsConfig"));
        assert_eq!(keys(&v["paths"]["map"][0]), fields("PathMap"));

        // The action names are the enum's.
        let names: BTreeSet<String> = file
            .enum_type
            .iter()
            .find(|e| e.name() == "ExecAction")
            .unwrap()
            .value
            .iter()
            .map(|v| v.name().to_string())
            .collect();
        assert!(names.contains(ACTION_ALLOW) && names.contains(ACTION_REVOKE));
    }
}
