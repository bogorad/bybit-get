use serde::Deserialize;
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use url::Url;

const DEFAULT_KEY: &str = "/run/secrets/api_keys/bybit/api_key";
const DEFAULT_SECRET: &str = "/run/secrets/api_keys/bybit/api_secret";
const DEFAULT_BASE: &str = "https://api.bybit.com";

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    api_key: Option<String>,
    api_secret: Option<String>,
    api_key_file: Option<String>,
    api_secret_file: Option<String>,
    base_url: Option<String>,
}

pub struct Settings {
    pub base_url: String,
    pub api_key: Vec<u8>,
    pub api_secret: Vec<u8>,
}

pub fn load_config(explicit: Option<&str>) -> Result<Settings, String> {
    let (path, optional) = match explicit {
        Some(value) => (expand_home(value)?, false),
        None => (default_config_path()?, true),
    };
    let file = match fs::File::open(&path) {
        Ok(file) => {
            let mut content = String::new();
            file.take(65537)
                .read_to_string(&mut content)
                .map_err(|_| "cannot read bounded UTF-8 config file".to_string())?;
            if content.len() > 65536 {
                return Err("config file is too large".into());
            }
            toml::from_str::<FileConfig>(&content)
                .map_err(|_| "config file is malformed".to_string())?
        }
        Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => {
            FileConfig::default()
        }
        Err(_) => return Err("cannot read config file".into()),
    };
    if file.api_key.is_some() && file.api_key_file.is_some() {
        return Err("config cannot set both inline API key and API key file".into());
    }
    if file.api_secret.is_some() && file.api_secret_file.is_some() {
        return Err(
            "config cannot set both inline API signing secret and API signing secret file".into(),
        );
    }
    let base = env::var("BYBIT_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .or(file.base_url)
        .unwrap_or_else(|| DEFAULT_BASE.to_string());
    let base_url = validate_base_url(&base)?;
    let key_path = env::var("BYBIT_API_KEY_FILE")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let secret_path = env::var("BYBIT_API_SECRET_FILE")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let api_key = credential(
        file.api_key,
        file.api_key_file,
        key_path,
        parent,
        DEFAULT_KEY,
        "API key",
    )?;
    let api_secret = credential(
        file.api_secret,
        file.api_secret_file,
        secret_path,
        parent,
        DEFAULT_SECRET,
        "API signing secret",
    )?;
    Ok(Settings {
        base_url,
        api_key,
        api_secret,
    })
}

fn default_config_path() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .ok_or_else(|| "HOME is not set; cannot locate default config".to_string())?;
    Ok(PathBuf::from(home).join(".config/bybit-get/config.toml"))
}

fn expand_home(value: &str) -> Result<PathBuf, String> {
    if value == "~" || value.starts_with("~/") {
        let home = env::var_os("HOME")
            .ok_or_else(|| "HOME is not set; cannot expand config path".to_string())?;
        if value == "~" {
            Ok(PathBuf::from(home))
        } else {
            Ok(PathBuf::from(home).join(&value[2..]))
        }
    } else {
        Ok(PathBuf::from(value))
    }
}

fn credential(
    inline: Option<String>,
    file: Option<String>,
    env_file: Option<PathBuf>,
    parent: &Path,
    default: &str,
    label: &str,
) -> Result<Vec<u8>, String> {
    if inline.is_some() && file.is_some() {
        return Err(format!(
            "config cannot set both inline {label} and {label} file"
        ));
    }
    if env_file.is_none() {
        if let Some(value) = inline {
            return validate_value(value.as_bytes(), label);
        }
    }
    let path = if let Some(value) = env_file {
        expand_home(&value.to_string_lossy())?
    } else if let Some(value) = file {
        join_config_path(parent, PathBuf::from(value))?
    } else {
        PathBuf::from(default)
    };
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|file| file.take(8193).read_to_end(&mut bytes))
        .map_err(|_| format!("cannot read the {label} file"))?;
    validate_value(&bytes, label)
}

fn join_config_path(parent: &Path, path: PathBuf) -> Result<PathBuf, String> {
    let text = path.to_string_lossy();
    if text == "~" || text.starts_with("~/") {
        expand_home(&text)
    } else if path.is_absolute() {
        Ok(path)
    } else {
        Ok(parent.join(path))
    }
}

fn validate_value(bytes: &[u8], label: &str) -> Result<Vec<u8>, String> {
    if bytes.len() > 8192 {
        return Err(format!("{label} value is too large"));
    }
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    let value = &bytes[start..end];
    if value.is_empty() || value.contains(&0) || value.iter().any(|b| *b == b'\r' || *b == b'\n') {
        return Err(format!("the {label} value must contain one nonempty value"));
    }
    Ok(value.to_vec())
}

pub fn validate_base_url(base: &str) -> Result<String, String> {
    let url = Url::parse(base).map_err(|_| "BYBIT_BASE_URL must be an official HTTPS API origin without credentials, a path, or a query".to_string())?;
    let raw_path = base
        .split_once("://")
        .and_then(|(_, authority)| authority.find('/').map(|at| &authority[at..]));
    if url.scheme() != "https"
        || base
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || raw_path.is_some_and(|path| path != "/")
        || url.username() != ""
        || url.password().is_some()
        || url.path() != "" && url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some() && url.port() != Some(443)
    {
        return Err("BYBIT_BASE_URL must be an official HTTPS API origin without credentials, a path, or a query".into());
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    const ALLOWED: &[&str] = &[
        "api.bybit.com",
        "api.bytick.com",
        "api.bybit.eu",
        "api.bybit.nl",
        "api.bybit-tr.com",
        "api.bybit.kz",
        "api.bybitgeorgia.ge",
        "api.bybit.ae",
    ];
    if !ALLOWED.contains(&host.as_str()) {
        return Err("BYBIT_BASE_URL is not a supported official Bybit API host".into());
    }
    Ok(base.trim_end_matches('/').to_string())
}
