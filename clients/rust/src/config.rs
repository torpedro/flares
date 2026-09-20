//! Explicit YAML loading and discovery shared by the SDK and CLI.
use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

/// Search XDG_CONFIG_HOME (when absolute), otherwise HOME/.config, then /etc/flares.
/// Files are selected without merging; an existing but invalid file never falls back.
pub fn default_path(filename: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    find_config(
        filename,
        xdg.as_deref(),
        home.as_deref(),
        Path::new("/etc/flares"),
    )
}

fn find_config(
    filename: &str,
    xdg: Option<&Path>,
    home: Option<&Path>,
    system: &Path,
) -> Result<PathBuf> {
    let user = xdg
        .filter(|path| path.is_absolute())
        .map(Path::to_owned)
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|base| base.join("flares").join(filename));
    let paths: Vec<_> = user
        .into_iter()
        .chain(std::iter::once(system.join(filename)))
        .collect();
    for path in &paths {
        // Inspect the directory entry so a broken symlink is an error, not a fallback.
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(path.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => bail!(
                "Cannot access configuration {}; use --config or an explicit SDK path",
                path.display()
            ),
        }
    }
    let searched = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("No {filename} found; searched {searched}; use --config or an explicit SDK path")
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    #[test]
    fn xdg_replaces_home_and_invalid_xdg_uses_home() {
        let root = tempfile::tempdir().unwrap();
        let xdg = root.path().join("xdg");
        let home = root.path().join("home");
        let system = root.path().join("etc/flares");
        for name in ["server.yaml", "client.yaml"] {
            let user = home.join(".config/flares").join(name);
            let preferred = xdg.join("flares").join(name);
            let fallback = system.join(name);
            for path in [&user, &preferred, &fallback] {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, "api_token: token\n").unwrap();
            }
            assert_eq!(
                find_config(name, Some(&xdg), Some(&home), &system).unwrap(),
                preferred
            );
            for invalid in [Path::new(""), Path::new("relative")] {
                assert_eq!(
                    find_config(name, Some(invalid), Some(&home), &system).unwrap(),
                    user
                );
            }
            fs::remove_file(&preferred).unwrap();
            assert_eq!(
                find_config(name, Some(&xdg), Some(&home), &system).unwrap(),
                fallback
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_is_selected_without_fallback() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("flares/client.yaml");
        fs::create_dir(config.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(root.path().join("missing"), &config).unwrap();
        assert_eq!(
            find_config(
                "client.yaml",
                Some(root.path()),
                None,
                &root.path().join("system")
            )
            .unwrap(),
            config
        );
        assert!(ClientConfig::load(&config).is_err());
    }

    #[test]
    fn user_config_takes_precedence_and_missing_files_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let user = home.join(".config/flares");
        let system = root.path().join("etc/flares");
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(&system).unwrap();
        for name in ["server.yaml", "client.yaml"] {
            let path = system.join(name);
            fs::write(&path, "system").unwrap();
            assert_eq!(find_config(name, None, Some(&home), &system).unwrap(), path);
            assert_eq!(find_config(name, None, None, &system).unwrap(), path);
            let path = user.join(name);
            // An invalid user file must be selected, not silently bypassed.
            fs::write(&path, "invalid YAML [").unwrap();
            assert_eq!(find_config(name, None, Some(&home), &system).unwrap(), path);
            assert!(ClientConfig::load(&path).is_err());
        }
    }

    #[test]
    fn missing_config_reports_search_locations_and_does_not_create_directories() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let system = root.path().join("etc/flares");
        let error = find_config("server.yaml", None, Some(&home), &system)
            .unwrap_err()
            .to_string();
        for expected in ["server.yaml", ".config/flares", "etc/flares", "--config"] {
            assert!(error.contains(expected));
        }
        assert!(!home.exists());
        assert!(!system.exists());
    }
}

#[derive(Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Secret {
    Literal(String),
    Environment { env: String },
    File { file: PathBuf },
}
impl Secret {
    pub fn expose(&self) -> &str {
        match self {
            Self::Literal(value) => value,
            _ => panic!("Secret reference must be resolved by configuration loading"),
        }
    }
    pub fn resolve(&mut self, directory: &Path, field: &str) -> Result<()> {
        let value = match self {
            Self::Literal(_) => return Ok(()),
            Self::Environment { env } => std::env::var(env).map_err(|_| {
                anyhow::anyhow!("{field}: environment variable is missing or not UTF-8")
            })?,
            Self::File { file } => fs::read_to_string(directory.join(file))
                .map_err(|_| anyhow::anyhow!("{field}: cannot read secret file as UTF-8"))?
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
        };
        *self = Self::Literal(value);
        Ok(())
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str("[REDACTED]")
    }
}

#[derive(Debug)]
pub struct ClientConfig {
    pub base_url: String,
    pub api_token: Secret,
    pub timeout: f64,
}

#[doc(hidden)]
pub fn directory(path: &Path) -> Result<PathBuf> {
    Ok(path
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("configuration: cannot resolve file directory"))?
        .parent()
        .expect("file has parent")
        .to_owned())
}
fn token(secret: &Secret, field: &str) -> Result<()> {
    if secret.expose().is_empty() || !secret.expose().bytes().all(|b| b.is_ascii_graphic()) {
        bail!("{field}: must be nonempty printable ASCII without whitespace");
    }
    Ok(())
}
fn name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct Seconds(pub u64);
impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let split = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        let (number, unit) = text.split_at(split);
        let multiplier = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            "w" => 604800,
            _ => {
                return Err(serde::de::Error::custom(
                    "expected duration with s, m, h, d, or w suffix",
                ));
            }
        };
        number
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(multiplier))
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("invalid duration"))
    }
}
#[doc(hidden)]
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path)
        .map_err(|_| anyhow::anyhow!("configuration: cannot read UTF-8 YAML file"))?;
    serde_path_to_error::deserialize(serde_yaml_ng::Deserializer::from_str(&text)).map_err(|error| {
        // Never echo the parser's message: it may quote a credential or a full YAML value.
        let mut parts=Vec::new();
        for segment in error.path() {
            match segment {
                serde_path_to_error::Segment::Map {key}=> {
                    parts.push(if name_valid(key) {key.clone()} else {"<field>".into()});
                    if matches!(key.as_str(),"api_token"|"app_token"|"user_key"|"bearer_token"|"url") {break;}
                }
                serde_path_to_error::Segment::Seq {index}=>parts.push(format!("[{index}]")),
                _=>{}
            }
        }
        let field=if parts.is_empty() {"configuration".into()} else {parts.join(".")};
        let location=error.inner().location().map(|l|format!(" at line {}, column {}",l.line(),l.column())).unwrap_or_default();
        anyhow::anyhow!("{field}{location}: invalid configuration YAML; check field names, required fields, and value types")
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Client {
    base_url: Option<String>,
    api_token: Secret,
    timeout: Option<Seconds>,
}
fn client(path: &Path) -> Result<ClientConfig> {
    let raw: Client = read(path)?;
    Ok(ClientConfig {
        base_url: raw
            .base_url
            .unwrap_or_else(|| "http://127.0.0.1:8000".into()),
        api_token: raw.api_token,
        timeout: raw.timeout.map_or(15.0, |value| value.0 as f64),
    })
}
impl ClientConfig {
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_inner(path).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))
    }
    fn load_inner(path: &Path) -> Result<Self> {
        let mut config = client(path)?;
        config.api_token.resolve(&directory(path)?, "api_token")?;
        token(&config.api_token, "api_token")?;
        let url = reqwest::Url::parse(&config.base_url)
            .map_err(|_| anyhow::anyhow!("base_url: invalid HTTP(S) URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("base_url: must be HTTP(S) without credentials, query, or fragment");
        }
        if !config.timeout.is_finite() || config.timeout <= 0.0 || config.timeout > 86400.0 {
            bail!("timeout: must be greater than 0s and at most 1d");
        }
        Ok(config)
    }
    pub fn effective(&self) -> serde_json::Value {
        serde_json::json!({"base_url":self.base_url,"api_token":self.api_token,"timeout":self.timeout})
    }
}
