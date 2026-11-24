//! The config directory: what the user wants devices to get. `distribution.toml` says it, `resources/` holds the files it points to.
//! Mistakes are caught here, before anything is signed: a config that does not
//! pass is not published, and the last good one stays in force.

use std::collections::HashSet;
use std::path::{Component, Path};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::doc::{
    ACTION_ALLOW, ACTION_REVOKE, ChangeDoc, ConfigDoc, DigestDoc, ExecDoc, ForceDoc, LocalDoc, ResourceRefDoc, SCHEMA,
    UpdateDoc,
};
use crate::{Error, Result};

pub const FILE_NAME: &str = "distribution.toml";
const MAX_RESOURCE: u64 = 1024 * 1024;
const MAX_DEVICES: usize = 200;

/// The first part of a key a force request may name. Together these leave out
/// everything that must only be set on the device: the trusted source and its
/// key, the "accept everything" switch, the update verification keys.
const OVERRIDABLE: &[&str] = &[
    "min_client_version",
    "features",
    "poll_interval_secs",
    "update",
    "digest",
    "local",
    "kind_rules",
];

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct File {
    min_client_version: Option<String>,
    features: Vec<String>,
    poll_interval_secs: Option<u32>,
    update: UpdateFile,
    local: LocalFile,
    digest: DigestFile,
    kind_rules: Option<ResourceFile>,
    force: Vec<ForceFile>,
    requests: Vec<ExecFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UpdateFile {
    channel: String,
    urls: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LocalFile {
    copy_form: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DigestFile {
    excerpt: Option<ResourceFile>,
    summary: Option<ResourceFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceFile {
    name: String,
    version: String,
    /// Relative to the config directory, inside `resources/`.
    file: String,
    #[serde(default)]
    executable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForceFile {
    id: String,
    #[serde(default)]
    devices: Vec<String>,
    changes: Vec<ChangeFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeFile {
    key: String,
    value: toml::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecFile {
    id: String,
    action: String,
    #[serde(default)]
    devices: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Resource {
    pub name: String,
    pub version: String,
    pub executable: bool,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

/// A checked config directory.
#[derive(Clone, Debug)]
pub struct Distribution {
    /// `serial` and `issued_at` are filled in when it is published.
    pub doc: ConfigDoc,
    pub resources: Vec<Resource>,
    /// Changes when the file or any resource it points to changes.
    pub source_hash: String,
}

fn bad(text: impl Into<String>) -> Error {
    Error::config(format!("{FILE_NAME}: {}", text.into()))
}

impl Distribution {
    /// What devices get when the directory has no `distribution.toml`: every
    /// default, nothing aimed at anyone. Publishing it anyway means a device
    /// that has just been pointed at the server can already check its
    /// signature.
    pub fn empty() -> Distribution {
        Distribution {
            doc: ConfigDoc {
                schema: SCHEMA.to_string(),
                ..ConfigDoc::default()
            },
            resources: Vec::new(),
            source_hash: "empty".to_string(),
        }
    }

    /// `None` when the directory has no `distribution.toml`: nothing to
    /// distribute is not a mistake.
    pub fn load(dir: &Path) -> Result<Option<Distribution>> {
        let path = dir.join(FILE_NAME);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::config(format!("cannot read {}: {e}", path.display()))),
        };
        let text = String::from_utf8(bytes.clone()).map_err(|_| bad("not UTF-8"))?;
        let file: File = toml::from_str(&text).map_err(|e| bad(e.message().to_string()))?;
        Self::build(dir, file, &bytes).map(Some)
    }

    fn build(dir: &Path, file: File, file_bytes: &[u8]) -> Result<Distribution> {
        let mut hash = Sha256::new();
        hash.update(FILE_NAME.as_bytes());
        hash.update([0]);
        hash.update(file_bytes);

        let mut resources: Vec<Resource> = Vec::new();
        let mut take = |what: &str, r: &Option<ResourceFile>| -> Result<Option<ResourceRefDoc>> {
            let Some(r) = r else { return Ok(None) };
            if r.name.trim().is_empty() || r.version.trim().is_empty() {
                return Err(bad(format!("{what}: name and version cannot be empty")));
            }
            let bytes = read_resource(dir, &r.file).map_err(|e| bad(format!("{what}: {e}")))?;
            hash.update(r.file.as_bytes());
            hash.update([0]);
            hash.update(&bytes);
            let sha256 = hex::encode(Sha256::digest(&bytes));
            if !resources.iter().any(|x| x.sha256 == sha256) {
                resources.push(Resource {
                    name: r.name.clone(),
                    version: r.version.clone(),
                    executable: r.executable,
                    sha256: sha256.clone(),
                    bytes,
                });
            }
            Ok(Some(ResourceRefDoc {
                name: r.name.clone(),
                version: r.version.clone(),
                sha256,
                executable: r.executable,
            }))
        };
        let excerpt = take("digest.excerpt", &file.digest.excerpt)?;
        let summary = take("digest.summary", &file.digest.summary)?;
        let kind_rules = take("kind_rules", &file.kind_rules)?;
        if let Some(r) = &kind_rules
            && r.executable
        {
            return Err(bad("kind_rules is data; it cannot be executable"));
        }
        if let Some(r) = &excerpt
            && r.executable
        {
            return Err(bad("digest.excerpt is a recipe (data); it cannot be executable"));
        }

        if let Some(v) = &file.min_client_version
            && !v.is_empty()
        {
            semver::Version::parse(v)
                .map_err(|_| bad(format!("min_client_version {v:?} is not a version like 0.1.0")))?;
        }
        let poll = file.poll_interval_secs.unwrap_or(0);
        if poll != 0 && !(60..=604_800).contains(&poll) {
            return Err(bad(
                "poll_interval_secs must be 0 (the client default) or between 60 and 604800",
            ));
        }
        if !matches!(file.update.channel.as_str(), "" | "stable" | "beta") {
            return Err(bad("update.channel must be \"stable\" or \"beta\""));
        }
        for url in &file.update.urls {
            let parsed = url::Url::parse(url).map_err(|_| bad(format!("update.urls: {url:?} is not a URL")))?;
            if !matches!(parsed.scheme(), "https" | "http") {
                return Err(bad(format!("update.urls: {url:?} must be http or https")));
            }
        }
        if !matches!(file.local.copy_form.as_str(), "" | "pruned" | "full") {
            return Err(bad("local.copy_form must be \"pruned\" or \"full\""));
        }

        let mut ids: HashSet<String> = HashSet::new();
        let mut force = Vec::new();
        for f in &file.force {
            check_id(&f.id, &mut ids)?;
            check_devices(&f.id, &f.devices)?;
            if f.changes.is_empty() {
                return Err(bad(format!("force {:?} has no changes", f.id)));
            }
            let mut changes = Vec::new();
            for c in &f.changes {
                check_key(&c.key)?;
                let json = toml_to_json(&c.value).map_err(|e| bad(format!("force {:?} key {}: {e}", f.id, c.key)))?;
                changes.push(ChangeDoc {
                    key: c.key.clone(),
                    value_json: json.to_string(),
                });
            }
            force.push(ForceDoc {
                id: f.id.clone(),
                changes,
                devices: f.devices.clone(),
            });
        }
        let mut requests = Vec::new();
        for r in &file.requests {
            check_id(&r.id, &mut ids)?;
            check_devices(&r.id, &r.devices)?;
            let action = match r.action.as_str() {
                "allow" => ACTION_ALLOW,
                "revoke" => ACTION_REVOKE,
                other => {
                    return Err(bad(format!(
                        "request {:?}: action must be \"allow\" or \"revoke\", not {other:?}",
                        r.id
                    )));
                }
            };
            requests.push(ExecDoc {
                id: r.id.clone(),
                action: action.to_string(),
                devices: r.devices.clone(),
            });
        }

        let doc = ConfigDoc {
            schema: SCHEMA.to_string(),
            serial: 0,
            issued_at: String::new(),
            min_client_version: file.min_client_version.unwrap_or_default(),
            features: file.features,
            poll_interval_secs: poll,
            update: UpdateDoc {
                channel: file.update.channel,
                urls: file.update.urls,
            },
            digest: DigestDoc { excerpt, summary },
            local: LocalDoc {
                copy_form: file.local.copy_form,
            },
            force,
            requests,
            kind_rules,
        };
        Ok(Distribution {
            doc,
            resources,
            source_hash: hex::encode(hash.finalize()),
        })
    }
}

fn read_resource(dir: &Path, file: &str) -> std::result::Result<Vec<u8>, String> {
    let rel = Path::new(file);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{file:?} must be a plain relative path"));
    }
    if !rel.starts_with("resources") {
        return Err(format!("{file:?} must be inside resources/"));
    }
    let full = dir.join(rel);
    // A symlink must not lead out of resources/.
    let root = dir
        .join("resources")
        .canonicalize()
        .map_err(|e| format!("resources/: {e}"))?;
    let real = full.canonicalize().map_err(|e| format!("{file}: {e}"))?;
    if !real.starts_with(&root) {
        return Err(format!("{file:?} leads outside resources/"));
    }
    let meta = std::fs::metadata(&real).map_err(|e| format!("{file}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{file:?} is not a file"));
    }
    if meta.len() > MAX_RESOURCE {
        return Err(format!("{file:?} is over {} bytes", MAX_RESOURCE));
    }
    std::fs::read(&real).map_err(|e| format!("{file}: {e}"))
}

fn check_id(id: &str, seen: &mut HashSet<String>) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(bad(format!("id {id:?}: 1 to 64 characters of letters, digits, . _ -")));
    }
    if !seen.insert(id.to_string()) {
        return Err(bad(format!("id {id:?} is used twice")));
    }
    Ok(())
}

fn check_devices(id: &str, devices: &[String]) -> Result<()> {
    if devices.len() > MAX_DEVICES {
        return Err(bad(format!("{id:?}: more than {MAX_DEVICES} devices")));
    }
    for d in devices {
        if d.is_empty()
            || d.len() > 128
            || !d
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(bad(format!("{id:?}: {d:?} is not a device ID")));
        }
    }
    Ok(())
}

/// A dotted path into the config, e.g. `local.copy_form`, whose first part a
/// force request is allowed to touch.
fn check_key(key: &str) -> Result<()> {
    let parts: Vec<&str> = key.split('.').collect();
    let ok = parts.iter().all(|p| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    });
    if !ok {
        return Err(bad(format!("key {key:?} is not a dotted path like local.copy_form")));
    }
    if !OVERRIDABLE.contains(&parts[0]) {
        return Err(bad(format!(
            "key {key:?} cannot be forced: it must start with one of {}",
            OVERRIDABLE.join(", ")
        )));
    }
    Ok(())
}

fn toml_to_json(v: &toml::Value) -> std::result::Result<serde_json::Value, String> {
    use serde_json::Value as J;
    Ok(match v {
        toml::Value::String(s) => J::String(s.clone()),
        toml::Value::Integer(i) => J::from(*i),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .ok_or("not a finite number")?,
        toml::Value::Boolean(b) => J::Bool(*b),
        toml::Value::Array(a) => J::Array(a.iter().map(toml_to_json).collect::<std::result::Result<_, _>>()?),
        toml::Value::Table(t) => J::Object(
            t.iter()
                .map(|(k, v)| Ok((k.clone(), toml_to_json(v)?)))
                .collect::<std::result::Result<_, String>>()?,
        ),
        toml::Value::Datetime(_) => return Err("dates are not supported; write a string".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(toml: &str, resources: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), toml).unwrap();
        std::fs::create_dir_all(dir.path().join("resources")).unwrap();
        for (name, body) in resources {
            std::fs::write(dir.path().join("resources").join(name), body).unwrap();
        }
        dir
    }

    fn load(dir: &tempfile::TempDir) -> Result<Option<Distribution>> {
        Distribution::load(dir.path())
    }

    fn err_of(toml: &str) -> String {
        load(&dir_with(toml, &[("a.json", "{}")])).unwrap_err().to_string()
    }

    const GOOD: &str = r#"
min_client_version = "0.1.0"
features = ["x"]
poll_interval_secs = 1800

[update]
channel = "stable"
urls = ["https://example.com/stable.json"]

[local]
copy_form = "pruned"

[digest.excerpt]
name = "excerpt-default"
version = "2026-11-23.1"
file = "resources/a.json"

[digest.summary]
name = "summary"
version = "1"
file = "resources/b.sh"
executable = true

[[force]]
id = "f-1"
devices = ["dev-1"]
[[force.changes]]
key = "local.copy_form"
value = "full"

[[requests]]
id = "r-1"
action = "allow"
"#;

    #[test]
    fn loads_a_good_directory() {
        let dir = dir_with(GOOD, &[("a.json", "{\"runner\":\"excerpt\"}"), ("b.sh", "echo hi")]);
        let d = load(&dir).unwrap().unwrap();
        assert_eq!(d.doc.schema, "1.0");
        assert_eq!(d.doc.poll_interval_secs, 1800);
        assert_eq!(d.resources.len(), 2);
        let excerpt = d.doc.digest.excerpt.as_ref().unwrap();
        assert_eq!(excerpt.sha256, hex::encode(Sha256::digest(b"{\"runner\":\"excerpt\"}")));
        assert!(!excerpt.executable);
        assert!(d.doc.digest.summary.as_ref().unwrap().executable);
        assert_eq!(d.doc.force[0].changes[0].value_json, "\"full\"");
        assert_eq!(d.doc.requests[0].action, ACTION_ALLOW);
    }

    #[test]
    fn the_source_hash_follows_every_input() {
        let a = load(&dir_with(GOOD, &[("a.json", "1"), ("b.sh", "x")]))
            .unwrap()
            .unwrap();
        let same = load(&dir_with(GOOD, &[("a.json", "1"), ("b.sh", "x")]))
            .unwrap()
            .unwrap();
        let other_resource = load(&dir_with(GOOD, &[("a.json", "2"), ("b.sh", "x")]))
            .unwrap()
            .unwrap();
        let other_file = load(&dir_with(
            &GOOD.replace("1800", "3600"),
            &[("a.json", "1"), ("b.sh", "x")],
        ))
        .unwrap()
        .unwrap();
        assert_eq!(a.source_hash, same.source_hash);
        assert_ne!(a.source_hash, other_resource.source_hash);
        assert_ne!(a.source_hash, other_file.source_hash);
    }

    #[test]
    fn no_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Distribution::load(dir.path()).unwrap().is_none());
        assert!(Distribution::load(&dir.path().join("missing")).unwrap().is_none());
    }

    #[test]
    fn typos_and_bad_values_are_refused() {
        assert!(err_of("min_client_versoin = \"1\"").contains("unknown field"));
        assert!(err_of("min_client_version = \"one\"").contains("not a version"));
        assert!(err_of("poll_interval_secs = 5").contains("poll_interval_secs"));
        assert!(err_of("[update]\nchannel = \"nightly\"").contains("channel"));
        assert!(err_of("[update]\nurls = [\"ftp://x\"]").contains("http"));
        assert!(err_of("[local]\ncopy_form = \"half\"").contains("copy_form"));
        assert!(err_of("[[requests]]\nid = \"r\"\naction = \"enable\"").contains("action"));
        assert!(err_of("not toml at all [").contains(FILE_NAME));
    }

    #[test]
    fn keys_that_must_stay_on_the_device_cannot_be_forced() {
        let force = |key: &str| format!("[[force]]\nid = \"f\"\n[[force.changes]]\nkey = \"{key}\"\nvalue = 1\n");
        for key in [
            "source.url",
            "exec.allowed",
            "accept_all",
            "trusted_source",
            "serial",
            "force.x",
            "Local.x",
            "local..x",
            "",
        ] {
            assert!(err_of(&force(key)).contains("key"), "{key}");
        }
        assert!(load(&dir_with(&force("local.copy_form"), &[])).is_ok());
        assert!(load(&dir_with(&force("digest.excerpt"), &[])).is_ok());
    }

    #[test]
    fn ids_are_unique_and_plain() {
        let two = "[[requests]]\nid = \"x\"\naction = \"allow\"\n[[requests]]\nid = \"x\"\naction = \"revoke\"\n";
        assert!(err_of(two).contains("twice"));
        let shared = "[[force]]\nid = \"x\"\n[[force.changes]]\nkey = \"local.copy_form\"\nvalue = \"full\"\n[[requests]]\nid = \"x\"\naction = \"allow\"\n";
        assert!(err_of(shared).contains("twice"));
        assert!(err_of("[[requests]]\nid = \"a b\"\naction = \"allow\"").contains("id"));
        assert!(err_of("[[requests]]\nid = \"a\"\naction = \"allow\"\ndevices = [\"../x\"]").contains("device"));
    }

    #[test]
    fn resources_stay_inside_resources_dir() {
        let res = |file: &str| format!("[digest.summary]\nname = \"n\"\nversion = \"1\"\nfile = \"{file}\"\n");
        for file in [
            "../x",
            "/etc/passwd",
            "a.json",
            "resources/../x",
            "resources/missing.json",
        ] {
            assert!(err_of(&res(file)).contains("digest.summary"), "{file}");
        }
        // A symlink out of resources/ is refused.
        #[cfg(unix)]
        {
            let dir = dir_with(&res("resources/link"), &[]);
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::os::unix::fs::symlink(outside.path(), dir.path().join("resources/link")).unwrap();
            assert!(load(&dir).unwrap_err().to_string().contains("outside"));
        }
        // Too big.
        let big = "x".repeat(1024 * 1024 + 1);
        let dir = dir_with(&res("resources/big"), &[("big", &big)]);
        assert!(load(&dir).unwrap_err().to_string().contains("bytes"));
    }

    #[test]
    fn data_resources_cannot_be_marked_executable() {
        let toml = "[digest.excerpt]\nname = \"n\"\nversion = \"1\"\nfile = \"resources/a.json\"\nexecutable = true\n";
        assert!(err_of(toml).contains("cannot be executable"));
        let toml = "[kind_rules]\nname = \"n\"\nversion = \"1\"\nfile = \"resources/a.json\"\nexecutable = true\n";
        assert!(err_of(toml).contains("cannot be executable"));
    }

    #[test]
    fn toml_values_become_json_text() {
        let v: toml::Value = toml::from_str("a = [1, \"x\", true]\n[t]\nb = 2.5").unwrap();
        let j = toml_to_json(&v).unwrap();
        assert_eq!(j.to_string(), "{\"a\":[1,\"x\",true],\"t\":{\"b\":2.5}}");
        let d: toml::Value = toml::from_str("d = 1979-05-27").unwrap();
        assert!(toml_to_json(&d).is_err());
    }
}
