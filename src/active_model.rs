// active_model — the fully-wired "model in use", resolved at `/model use` time.
//
// Roles split:
//   - `config::Settings` is the blueprint loaded from YAML
//   - `ActiveModel` is the runtime, fully-wired instance:
//     - `endpoint_url`: pre-built `<base_url>/chat/completions`
//     - `api_key` / `headers`: final values after `${VAR}` expansion (which Settings
//       does once at startup, see M5)
//   - The agent and commands only need to see ActiveModel to make an LLM call
//
// This type is the boundary layer that shields the agent from Settings' internal
// structure (model_groups lookup etc.). If Settings ever changes shape, only
// `ActiveModel::resolve` needs updating.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::config::{ModelRef, Settings};

#[derive(Debug, Clone)]
pub struct ActiveModel {
    /// The group name from config (e.g. `"openai"`). Used to mark the active model
    /// with a `*` in the `/model` list.
    pub group: String,
    /// The string that goes into the ChatRequest `model` field (e.g. `"gpt-4o-mini"`).
    pub model: String,
    /// Full URL with `/chat/completions` already appended.
    pub endpoint_url: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl ActiveModel {
    /// Display string for `/model` listing and logs: `<group>:<model>`.
    pub fn label(&self) -> String {
        format!("{}:{}", self.group, self.model)
    }

    /// Resolve `model_ref` into an `ActiveModel`. Called once at `/model use` time
    /// and once at startup for default_model.
    ///
    /// Failure cases:
    ///   - The group is not in settings
    ///   - The model is not in the group's `models` list
    pub fn resolve(settings: &Settings, model_ref: &ModelRef) -> Result<Self> {
        let group = settings
            .group_by_name(&model_ref.group)
            .with_context(|| {
                format!(
                    "model group '{}' is not in the config (`/model` to list)",
                    model_ref.group
                )
            })?;
        if !group.models.iter().any(|m| m == &model_ref.model) {
            anyhow::bail!(
                "model '{}' is not registered under group '{}' (`/model` to list)",
                model_ref.model,
                model_ref.group
            );
        }
        Ok(Self {
            group: model_ref.group.clone(),
            model: model_ref.model.clone(),
            endpoint_url: format!("{}/chat/completions", group.base_url.trim_end_matches('/')),
            api_key: group.api_key.clone(),
            headers: group.headers.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelGroup;

    #[test]
    fn resolve_builds_correct_fields() {
        let mut s = Settings::default();
        s.model_groups.push(ModelGroup {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: Some("sk-xxx".into()),
            headers: BTreeMap::new(),
            models: vec!["gpt-4o-mini".into()],
        });
        let r = ModelRef::parse("openai:gpt-4o-mini").unwrap();
        let a = ActiveModel::resolve(&s, &r).unwrap();
        assert_eq!(a.endpoint_url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(a.api_key.as_deref(), Some("sk-xxx"));
        assert_eq!(a.label(), "openai:gpt-4o-mini");
    }

    #[test]
    fn resolve_strips_trailing_slash() {
        let mut s = Settings::default();
        s.model_groups.push(ModelGroup {
            name: "g".into(),
            base_url: "http://localhost:11434/v1/".into(),
            api_key: None,
            headers: BTreeMap::new(),
            models: vec!["llama3".into()],
        });
        let r = ModelRef::parse("g:llama3").unwrap();
        let a = ActiveModel::resolve(&s, &r).unwrap();
        assert_eq!(a.endpoint_url, "http://localhost:11434/v1/chat/completions");
    }

    #[test]
    fn resolve_unknown_group_errors() {
        let s = Settings::default();
        let r = ModelRef::parse("nonexistent:model").unwrap();
        assert!(ActiveModel::resolve(&s, &r).is_err());
    }

    #[test]
    fn resolve_rejects_model_not_in_group() {
        let mut s = Settings::default();
        s.model_groups.push(ModelGroup {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: None,
            headers: BTreeMap::new(),
            models: vec!["gpt-4o-mini".into()],
        });
        let r = ModelRef::parse("openai:nonexistent").unwrap();
        let err = ActiveModel::resolve(&s, &r).unwrap_err();
        assert!(err.to_string().contains("is not registered"), "{err}");
    }

    #[test]
    fn resolve_accepts_model_in_group() {
        let mut s = Settings::default();
        s.model_groups.push(ModelGroup {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: None,
            headers: BTreeMap::new(),
            models: vec!["gpt-4o-mini".into(), "gpt-4o".into()],
        });
        let r = ModelRef::parse("openai:gpt-4o").unwrap();
        assert!(ActiveModel::resolve(&s, &r).is_ok());
    }
}
