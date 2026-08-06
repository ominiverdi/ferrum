use crate::{
    config::{Config, validate_login_model},
    picker::{self, PickerItem},
    providers::{self, ModelList},
    terminal_text,
};
use anyhow::{Context, Result};
use std::{collections::BTreeSet, io::IsTerminal, path::PathBuf};

const MAX_LOGIN_MODELS: usize = 512;
const MAX_LOGIN_MODELS_TOTAL_BYTES: usize = 64 * 1024;

pub(crate) enum LoginSetupOutcome {
    AuthenticationOnly,
    ProviderUnchanged {
        provider: String,
        model: String,
    },
    Configured {
        config: Box<Config>,
        model: String,
        path: PathBuf,
    },
}

pub(crate) async fn setup_after_login(
    config: &Config,
    auth_only: bool,
    requested_model: Option<&str>,
) -> Result<LoginSetupOutcome> {
    if auth_only {
        return Ok(LoginSetupOutcome::AuthenticationOnly);
    }
    if let Some(model) = requested_model {
        validate_login_model(model)?;
    }
    if !config.should_configure_provider_after_login() {
        return Ok(LoginSetupOutcome::ProviderUnchanged {
            provider: config.provider_name.clone(),
            model: config.model.clone(),
        });
    }

    let model = match requested_model {
        Some(model) => model.to_string(),
        None => {
            if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
                anyhow::bail!(
                    "authentication succeeded, but automatic provider setup needs a terminal; rerun `ferrum login openai --model <MODEL>` or use `--auth-only`"
                );
            }
            let provider = config.openai_codex_provider_for_login()?;
            let ModelList::Live {
                source,
                models,
                notices,
            } = providers::list_models(&provider).await.context(
                "authentication succeeded, but automatic provider setup could not list models; rerun with `--model <MODEL>` to select one explicitly",
            )?;
            for notice in notices {
                println!("{}", terminal_text::sanitize(&notice));
            }
            let models = selectable_models(models);
            if models.is_empty() {
                anyhow::bail!(
                    "authentication succeeded, but no selectable models were returned by {}; rerun with `--model <MODEL>` to select one explicitly",
                    terminal_text::sanitize(&source)
                );
            }
            let items = models
                .iter()
                .map(|model| PickerItem::new(model.clone(), model))
                .collect::<Vec<_>>();
            let Some(model) = picker::pick("Select default model for openai-codex", &items)? else {
                return Ok(LoginSetupOutcome::AuthenticationOnly);
            };
            model
        }
    };

    let mut candidate = config.clone();
    let path = candidate.configure_openai_codex_after_login(&model)?;
    Ok(LoginSetupOutcome::Configured {
        config: Box::new(candidate),
        model,
        path,
    })
}

fn selectable_models(models: Vec<String>) -> Vec<String> {
    let mut accepted = BTreeSet::new();
    let mut total_bytes = 0usize;
    for model in models {
        if accepted.len() >= MAX_LOGIN_MODELS {
            break;
        }
        if validate_login_model(&model).is_err() || accepted.contains(&model) {
            continue;
        }
        let Some(next_total) = total_bytes.checked_add(model.len()) else {
            continue;
        };
        if next_total > MAX_LOGIN_MODELS_TOTAL_BYTES {
            continue;
        }
        total_bytes = next_total;
        accepted.insert(model);
    }
    accepted.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::openai_codex::{
        OpenAiCodexCredential, get_api_key_from_path, save as save_credential,
    };
    use serde_json::{Value, json};
    use std::fs;

    fn synthetic_credential(access: &str) -> OpenAiCodexCredential {
        OpenAiCodexCredential {
            r#type: "oauth".to_string(),
            access: access.to_string(),
            refresh: format!("refresh-{access}"),
            expires: u128::from(u64::MAX),
            account_id: format!("account-{access}"),
        }
    }

    fn seed_synthetic_auth(path: &std::path::Path, access: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "other-provider": {"marker": "preserve-me"}
            }))
            .unwrap(),
        )
        .unwrap();
        save_credential(path.to_path_buf(), &synthetic_credential(access)).unwrap();
    }

    fn assert_unrelated_auth_entry_survives(path: &std::path::Path) {
        let storage = serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            storage["other-provider"]["marker"],
            Value::String("preserve-me".to_string())
        );
    }

    #[tokio::test]
    async fn synthetic_relogin_keeps_explicit_provider_config_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            r#"# keep configured account
provider = "openai-codex"
model = "gpt-existing"

[providers.openai-codex]
type = "openai-codex"
default_model = "gpt-existing"
"#,
        )
        .unwrap();
        let config_before = fs::read(&config_path).unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        let auth_path = config.auth_path();
        seed_synthetic_auth(&auth_path, "old-access");

        save_credential(
            auth_path.clone(),
            &synthetic_credential("replacement-access"),
        )
        .unwrap();
        let outcome = setup_after_login(&config, false, None).await.unwrap();

        assert!(matches!(
            outcome,
            LoginSetupOutcome::ProviderUnchanged { provider, model }
                if provider == "openai-codex" && model == "gpt-existing"
        ));
        assert_eq!(fs::read(&config_path).unwrap(), config_before);
        assert_eq!(config.provider_name, "openai-codex");
        assert_eq!(config.model, "gpt-existing");
        assert_eq!(
            get_api_key_from_path(auth_path.clone()).await.unwrap(),
            Some("replacement-access".to_string())
        );
        assert_unrelated_auth_entry_survives(&auth_path);
    }

    #[tokio::test]
    async fn synthetic_first_login_updates_only_setup_fields_and_credential() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            "# preserve setup comment\nthinking = \"high\"\n",
        )
        .unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        let auth_path = config.auth_path();
        seed_synthetic_auth(&auth_path, "old-access");

        save_credential(
            auth_path.clone(),
            &synthetic_credential("replacement-access"),
        )
        .unwrap();
        let outcome = setup_after_login(&config, false, Some("gpt-selected"))
            .await
            .unwrap();

        let LoginSetupOutcome::Configured { config, model, .. } = outcome else {
            panic!("expected configured login outcome");
        };
        assert_eq!(model, "gpt-selected");
        assert_eq!(config.provider_name, "openai-codex");
        assert_eq!(config.model, "gpt-selected");
        let rendered = fs::read_to_string(&config_path).unwrap();
        assert!(rendered.contains("# preserve setup comment"));
        assert!(rendered.contains("thinking = \"high\""));
        assert!(rendered.contains("provider = \"openai-codex\""));
        assert!(rendered.contains("model = \"gpt-selected\""));
        assert_eq!(
            get_api_key_from_path(auth_path.clone()).await.unwrap(),
            Some("replacement-access".to_string())
        );
        assert_unrelated_auth_entry_survives(&auth_path);
    }

    #[tokio::test]
    async fn synthetic_auth_only_replaces_credential_without_creating_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        let auth_path = config.auth_path();
        seed_synthetic_auth(&auth_path, "old-access");

        save_credential(
            auth_path.clone(),
            &synthetic_credential("replacement-access"),
        )
        .unwrap();
        assert!(matches!(
            setup_after_login(&config, true, None).await.unwrap(),
            LoginSetupOutcome::AuthenticationOnly
        ));

        assert!(!dir.path().join("config.toml").exists());
        assert_eq!(
            get_api_key_from_path(auth_path.clone()).await.unwrap(),
            Some("replacement-access".to_string())
        );
        assert_unrelated_auth_entry_survives(&auth_path);
    }

    #[tokio::test]
    async fn explicit_model_configures_implicit_provider_without_catalog_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();

        let outcome = setup_after_login(&config, false, Some("gpt-explicit"))
            .await
            .unwrap();
        let LoginSetupOutcome::Configured {
            config,
            model,
            path,
        } = outcome
        else {
            panic!("expected configured login outcome");
        };
        assert_eq!(model, "gpt-explicit");
        assert_eq!(config.provider_name, "openai-codex");
        assert_eq!(config.model, "gpt-explicit");
        assert_eq!(path, dir.path().join("config.toml"));
    }

    #[tokio::test]
    async fn auth_only_and_explicit_provider_skip_catalog_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert!(matches!(
            setup_after_login(&config, true, None).await.unwrap(),
            LoginSetupOutcome::AuthenticationOnly
        ));

        std::fs::write(dir.path().join("config.toml"), "provider = \"fake\"\n").unwrap();
        let config = Config::load_from_dir(dir.path().to_path_buf()).unwrap();
        assert!(matches!(
            setup_after_login(&config, false, None).await.unwrap(),
            LoginSetupOutcome::ProviderUnchanged { provider, model }
                if provider == "fake" && model == "fake"
        ));
    }

    #[test]
    fn selectable_models_are_safe_deduplicated_and_bounded() {
        let mut models = vec![
            "gpt-z".to_string(),
            "gpt-a".to_string(),
            "gpt-a".to_string(),
            "unsafe model".to_string(),
            "escape\u{1b}".to_string(),
            String::new(),
            "x".repeat(crate::config::MAX_LOGIN_MODEL_BYTES + 1),
        ];
        models.extend((0..MAX_LOGIN_MODELS + 20).map(|index| format!("model-{index:04}")));

        let accepted = selectable_models(models);
        assert_eq!(accepted.len(), MAX_LOGIN_MODELS);
        assert_eq!(accepted[0], "gpt-a");
        assert!(accepted.contains(&"gpt-z".to_string()));
        assert!(!accepted.contains(&"unsafe model".to_string()));
        assert!(accepted.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(accepted.iter().map(String::len).sum::<usize>() <= MAX_LOGIN_MODELS_TOTAL_BYTES);
    }
}
