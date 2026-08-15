use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const OPENROUTER_API_KEY_ENVIRONMENT_VARIABLE: &str = "OPENROUTER_API_KEY";
const XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE: &str = "XDG_CONFIG_HOME";
const HOME_ENVIRONMENT_VARIABLE: &str = "HOME";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileCredentialPlatform {
    Unix,
    Unsupported,
}

impl FileCredentialPlatform {
    fn current() -> Self {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            Self::Unix
        } else {
            Self::Unsupported
        }
    }
}

#[derive(Deserialize)]
struct AuthFile {
    openrouter: Option<OpenRouterAuth>,
}

#[derive(Deserialize)]
struct OpenRouterAuth {
    #[serde(rename = "type")]
    credential_type: String,
    api_key: Option<String>,
}

pub(super) fn resolve_api_key() -> Result<String, String> {
    let environment_api_key = match env::var_os(OPENROUTER_API_KEY_ENVIRONMENT_VARIABLE) {
        Some(environment_api_key) => Some(
            environment_api_key
                .into_string()
                .map_err(|_| "the OPENROUTER_API_KEY environment value is invalid".to_owned())?,
        ),
        None => None,
    };
    let xdg_config_home = env::var_os(XDG_CONFIG_HOME_ENVIRONMENT_VARIABLE).map(PathBuf::from);
    let home = env::var_os(HOME_ENVIRONMENT_VARIABLE).map(PathBuf::from);

    resolve_api_key_from_values(
        environment_api_key.as_deref(),
        xdg_config_home.as_deref(),
        home.as_deref(),
        FileCredentialPlatform::current(),
        |path| fs::read_to_string(path),
    )
}

pub(crate) fn resolve_api_key_from_values<ReadFile>(
    environment_api_key: Option<&str>,
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
    file_credential_platform: FileCredentialPlatform,
    mut read_file: ReadFile,
) -> Result<String, String>
where
    ReadFile: FnMut(&Path) -> io::Result<String>,
{
    if let Some(environment_api_key) = environment_api_key.filter(|value| !value.is_empty()) {
        return Ok(environment_api_key.to_owned());
    }

    let Some(auth_file_path) = auth_file_path(xdg_config_home, home, file_credential_platform)
    else {
        let error = match file_credential_platform {
            FileCredentialPlatform::Unix => {
                "no OpenRouter API key found; no supported auth file path is configured"
            }
            FileCredentialPlatform::Unsupported => {
                "no OpenRouter API key found; file credentials are unsupported on this platform"
            }
        };
        return Err(error.to_owned());
    };

    let auth_file_contents = match read_file(&auth_file_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err("no OpenRouter API key found in the environment or auth file".to_owned());
        }
        Err(_) => return Err("unable to read the OpenRouter auth file".to_owned()),
    };

    parse_auth_file(&auth_file_contents)
}

fn auth_file_path(
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
    file_credential_platform: FileCredentialPlatform,
) -> Option<PathBuf> {
    if file_credential_platform != FileCredentialPlatform::Unix {
        return None;
    }

    if let Some(xdg_config_home) =
        xdg_config_home.filter(|path| !path.as_os_str().is_empty() && path.is_absolute())
    {
        return Some(xdg_config_home.join("nano/auth.json"));
    }

    home.filter(|path| !path.as_os_str().is_empty())
        .map(|path| path.join(".config/nano/auth.json"))
}

fn parse_auth_file(auth_file_contents: &str) -> Result<String, String> {
    let auth_file: AuthFile = serde_json::from_str(auth_file_contents)
        .map_err(|_| "the OpenRouter auth file is malformed".to_owned())?;
    let Some(openrouter_auth) = auth_file.openrouter else {
        return Err("the OpenRouter auth file contains unsupported credentials".to_owned());
    };

    if openrouter_auth.credential_type != "api" {
        return Err("the OpenRouter auth file contains unsupported credentials".to_owned());
    }

    let Some(api_key) = openrouter_auth.api_key.filter(|value| !value.is_empty()) else {
        return Err("the OpenRouter auth file contains an empty API key".to_owned());
    };

    Ok(api_key)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{FileCredentialPlatform, parse_auth_file, resolve_api_key_from_values};

    fn valid_auth_file(api_key: &str) -> String {
        format!(r#"{{"openrouter":{{"type":"api","api_key":"{api_key}"}}}}"#)
    }

    #[test]
    fn non_empty_environment_key_wins_without_opening_a_file() {
        let file_was_opened = Cell::new(false);

        let result = resolve_api_key_from_values(
            Some("environment-secret"),
            Some(Path::new("/tmp/xdg")),
            Some(Path::new("/tmp/home")),
            FileCredentialPlatform::Unix,
            |_| {
                file_was_opened.set(true);
                Ok(valid_auth_file("file-secret"))
            },
        );

        assert_eq!(result, Ok("environment-secret".to_owned()));
        assert!(!file_was_opened.get());
    }

    #[test]
    fn absolute_xdg_path_is_used_before_home_fallback() {
        let opened_path = Rc::new(std::cell::RefCell::new(None::<PathBuf>));
        let opened_path_for_reader = Rc::clone(&opened_path);

        let result = resolve_api_key_from_values(
            None,
            Some(Path::new("/tmp/xdg-config")),
            Some(Path::new("/tmp/home")),
            FileCredentialPlatform::Unix,
            |path| {
                *opened_path_for_reader.borrow_mut() = Some(path.to_owned());
                Ok(valid_auth_file("xdg-secret"))
            },
        );

        assert_eq!(result, Ok("xdg-secret".to_owned()));
        assert_eq!(
            opened_path.borrow().as_deref(),
            Some(Path::new("/tmp/xdg-config/nano/auth.json"))
        );
    }

    #[test]
    fn relative_or_empty_xdg_path_uses_home_config_fallback() {
        for xdg_config_home in [Some(Path::new("relative")), Some(Path::new("")), None] {
            let opened_path = Rc::new(std::cell::RefCell::new(None::<PathBuf>));
            let opened_path_for_reader = Rc::clone(&opened_path);

            let result = resolve_api_key_from_values(
                None,
                xdg_config_home,
                Some(Path::new("/tmp/home")),
                FileCredentialPlatform::Unix,
                |path| {
                    *opened_path_for_reader.borrow_mut() = Some(path.to_owned());
                    Ok(valid_auth_file("home-secret"))
                },
            );

            assert_eq!(result, Ok("home-secret".to_owned()));
            assert_eq!(
                opened_path.borrow().as_deref(),
                Some(Path::new("/tmp/home/.config/nano/auth.json"))
            );
        }
    }

    #[test]
    fn reads_a_real_temporary_auth_file_without_inspecting_file_modes() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let temporary_directory = std::env::temp_dir().join(format!(
            "nano-openrouter-auth-{}-{unique_suffix}",
            std::process::id()
        ));
        let auth_file_path = temporary_directory.join("nano/auth.json");
        std::fs::create_dir_all(
            auth_file_path
                .parent()
                .expect("auth directory should exist"),
        )
        .expect("temporary auth directory should be created");
        std::fs::write(&auth_file_path, valid_auth_file("temporary-fixture-key"))
            .expect("temporary auth file should be written");

        let result = resolve_api_key_from_values(
            None,
            Some(&temporary_directory),
            None,
            FileCredentialPlatform::Unix,
            |path| std::fs::read_to_string(path),
        );

        std::fs::remove_dir_all(&temporary_directory)
            .expect("temporary auth directory should be removed");
        assert_eq!(result, Ok("temporary-fixture-key".to_owned()));
    }

    #[test]
    fn file_credentials_are_reloaded_for_each_lookup() {
        let contents = Rc::new(std::cell::RefCell::new(valid_auth_file("first-secret")));
        let contents_for_reader = Rc::clone(&contents);
        let read_file = move |_: &Path| Ok::<_, io::Error>(contents_for_reader.borrow().clone());

        assert_eq!(
            resolve_api_key_from_values(
                None,
                Some(Path::new("/tmp/xdg")),
                None,
                FileCredentialPlatform::Unix,
                read_file,
            ),
            Ok("first-secret".to_owned())
        );

        *contents.borrow_mut() = valid_auth_file("second-secret");
        let contents_for_reader = Rc::clone(&contents);
        assert_eq!(
            resolve_api_key_from_values(
                None,
                Some(Path::new("/tmp/xdg")),
                None,
                FileCredentialPlatform::Unix,
                move |_: &Path| Ok::<_, io::Error>(contents_for_reader.borrow().clone()),
            ),
            Ok("second-secret".to_owned())
        );
    }

    #[test]
    fn missing_file_is_a_safe_setup_error() {
        let result = resolve_api_key_from_values(
            None,
            Some(Path::new("/tmp/xdg")),
            None,
            FileCredentialPlatform::Unix,
            |_| Err(io::Error::from(io::ErrorKind::NotFound)),
        );

        assert_eq!(
            result,
            Err("no OpenRouter API key found in the environment or auth file".to_owned())
        );
    }

    #[test]
    fn malformed_empty_and_unsupported_file_records_are_rejected_without_secret_context() {
        let cases = [
            ("not-json", "malformed"),
            (r#"{"openrouter":{"type":"api","api_key":""}}"#, "empty"),
            (
                r#"{"openrouter":{"type":"oauth","api_key":"oauth-secret"}}"#,
                "unsupported",
            ),
            (r#"{"openrouter":{"type":"api"}}"#, "empty"),
        ];

        for (contents, expected_context) in cases {
            let error = parse_auth_file(contents).expect_err("invalid auth should fail");
            assert!(error.contains(expected_context));
            assert!(!error.contains("oauth-secret"));
        }
    }

    #[test]
    fn file_credentials_are_not_considered_on_unsupported_platforms() {
        let file_was_opened = Cell::new(false);

        let result = resolve_api_key_from_values(
            None,
            Some(Path::new("/tmp/xdg")),
            Some(Path::new("/tmp/home")),
            FileCredentialPlatform::Unsupported,
            |_| {
                file_was_opened.set(true);
                Ok(valid_auth_file("file-secret"))
            },
        );

        assert!(result.is_err());
        assert!(!file_was_opened.get());
    }
}
