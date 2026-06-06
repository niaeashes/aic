// active_model — `/model use` 時に解決される「使用中モデル」の完全な状態。
//
// 役割の住み分け:
//   - `config::Settings` は YAML から読んだ「設計図」
//   - `ActiveModel` はランタイムに配線済みの「実体」
//     - `endpoint_url`: `<base_url>/chat/completions` を結合済み
//     - `api_key` / `headers`: `${VAR}` 展開済みの最終値（M5 で起動時に Settings 側で展開済）
//   - agent / コマンド群は **ActiveModel だけ見れば LLM を呼べる**
//
// この型は Settings の内部構造（model_groups の検索など）への依存を agent から切り離す
// バウンダリ層。Settings 形が変わっても、`ActiveModel::resolve` を直すだけで済む。

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::config::{ModelRef, Settings};

#[derive(Debug, Clone)]
pub struct ActiveModel {
    /// config 上のグループ名（例: `"openai"`）。`/model` 一覧で `*` を付ける基準に使う。
    pub group: String,
    /// ChatRequest の `model` フィールドに入る文字列（例: `"gpt-4o-mini"`）。
    pub model: String,
    /// `/chat/completions` を付加済みの完全 URL。
    pub endpoint_url: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl ActiveModel {
    /// `/model` 一覧・ログ用の `<group>:<model>` 表示文字列。
    pub fn label(&self) -> String {
        format!("{}:{}", self.group, self.model)
    }

    /// `model_ref` を `ActiveModel` に解決する。`/model use` 時 / 起動時の default_model
    /// 解決時に 1 度ずつ呼ぶ。
    ///
    /// 失敗パターン:
    ///   - group が settings に無い
    ///   - model が group の `models` リストに登録されていない
    pub fn resolve(settings: &Settings, model_ref: &ModelRef) -> Result<Self> {
        let group = settings
            .group_by_name(&model_ref.group)
            .with_context(|| {
                format!(
                    "model group '{}' が config に存在しません（`/model` で一覧）",
                    model_ref.group
                )
            })?;
        if !group.models.iter().any(|m| m == &model_ref.model) {
            anyhow::bail!(
                "モデル '{}' は group '{}' に登録されていません（`/model` で一覧）",
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
        assert!(err.to_string().contains("登録されていません"), "{err}");
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
