// config/types — 設定型の定義と impl（SPEC §4.2, §11）。
//
// このファイルには「YAML から読む設定スキーマ型」だけを置く:
//   Settings      — アプリ全体の設定
//   ModelGroup    — モデルグループ（base_url / api_key / headers / models）
//   McpServerCfg  — MCP サーバ設定
//   UiConfig      — UI 設定（history_size, max_tool_iterations）
//   ModelRef      — `<group>:<model>` 形式の識別子（SPEC §2, §10）
//
// 「ランタイム解決済みの実体」である `ActiveModel` は `src/active_model.rs` に独立。
// ファイル I/O（load / shallow_merge 等）は loader.rs、シークレット復号は secrets.rs。

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::secrets::Secrets;

// ---------------------------------------------------------------------------
// 設定型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub default_model: Option<ModelRef>,
    #[serde(default)]
    pub model_groups: Vec<ModelGroup>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerCfg>,
    #[serde(default)]
    pub ui: UiConfig,

    // ファイル位置に依存する派生情報。serde では扱わず、load 後に詰める。
    #[serde(skip)]
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelGroup {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerCfg {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UiConfig {
    #[serde(default = "default_history_size")]
    pub history_size: usize,
    #[serde(default = "default_max_tool_iter")]
    pub max_tool_iterations: u32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            history_size: default_history_size(),
            max_tool_iterations: default_max_tool_iter(),
        }
    }
}

fn default_history_size() -> usize {
    1000
}
fn default_max_tool_iter() -> u32 {
    10
}

// ---------------------------------------------------------------------------
// ModelRef — `<group>:<model>` 形式（SPEC §2, §10）
// model 名自体が `:` を含むので、splitn(2, ':') で必ず 1 回だけ分割する。
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub group: String,
    pub model: String,
}

impl ModelRef {
    pub fn parse(s: &str) -> Result<Self> {
        let mut parts = s.splitn(2, ':');
        let group = parts.next().context("model ref が空")?.trim();
        let model = parts
            .next()
            .with_context(|| format!("model ref は '<group>:<model>' 形式が必要: {s:?}"))?
            .trim();
        if group.is_empty() || model.is_empty() {
            anyhow::bail!("group / model はどちらも非空である必要がある: {s:?}");
        }
        Ok(Self {
            group: group.to_string(),
            model: model.to_string(),
        })
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.group, self.model)
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ModelRef::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ModelRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

// ---------------------------------------------------------------------------
// Settings ヘルパ — 検索・展開・機密マスク
// ---------------------------------------------------------------------------

/// 機密値の表示用マスク文字列。`/config show` と `/config setup` のプレビューで使う。
pub const REDACTED: &str = "***";

impl Settings {
    /// グループ名で `ModelGroup` を引く。O(n) だがグループ数は数個想定。
    pub fn group_by_name(&self, name: &str) -> Option<&ModelGroup> {
        self.model_groups.iter().find(|g| g.name == name)
    }

    /// `<group>` が存在し、その `models` に `<model>` が含まれていれば true。
    pub fn model_exists(&self, group: &str, model: &str) -> bool {
        self.group_by_name(group)
            .map(|g| g.models.iter().any(|m| m == model))
            .unwrap_or(false)
    }

    /// config 内の `${VAR}` を全フィールドに対して展開する（M5, SPEC §5）。
    ///
    /// 起動時に 1 回だけ呼ぶ。以降の利用箇所は展開済み前提。
    pub fn expand_secrets(&mut self, secrets: &Secrets) {
        for g in &mut self.model_groups {
            if let Some(k) = g.api_key.as_mut() {
                *k = secrets.expand(k);
            }
            for v in g.headers.values_mut() {
                *v = secrets.expand(v);
            }
        }
        for s in &mut self.mcp_servers {
            for v in s.headers.values_mut() {
                *v = secrets.expand(v);
            }
        }
    }

    /// 機密値（api_key / 各 headers の値）を `***` に置換した deep-clone を返す。
    ///
    /// 利用箇所: `/config show`、`/config setup` のプレビュー。
    /// secret-bearing でないヘッダ（Content-Type 等）も区別せず一律マスクする
    /// — config に手で書く header は基本 auth 系のみ、という運用前提。
    pub fn redacted(&self) -> Self {
        let mut s = self.clone();
        for g in &mut s.model_groups {
            if g.api_key.is_some() {
                g.api_key = Some(REDACTED.to_string());
            }
            for v in g.headers.values_mut() {
                *v = REDACTED.to_string();
            }
        }
        for srv in &mut s.mcp_servers {
            for v in srv.headers.values_mut() {
                *v = REDACTED.to_string();
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ref_parses_simple() {
        let r = ModelRef::parse("openai:gpt-4o-mini").unwrap();
        assert_eq!(r.group, "openai");
        assert_eq!(r.model, "gpt-4o-mini");
    }

    #[test]
    fn model_ref_keeps_colon_inside_model() {
        let r = ModelRef::parse("local:qwen2.5-coder:32b").unwrap();
        assert_eq!(r.group, "local");
        assert_eq!(r.model, "qwen2.5-coder:32b");
    }

    #[test]
    fn model_ref_rejects_empty_parts() {
        assert!(ModelRef::parse("openai:").is_err());
        assert!(ModelRef::parse(":gpt").is_err());
        assert!(ModelRef::parse("no-colon").is_err());
    }

    #[test]
    fn model_ref_roundtrips_through_display() {
        let r = ModelRef::parse("local:qwen2.5-coder:32b").unwrap();
        assert_eq!(r.to_string(), "local:qwen2.5-coder:32b");
    }

    #[test]
    fn settings_deserializes_from_empty_mapping() {
        let v = serde_yml::Value::Mapping(serde_yml::Mapping::new());
        let s: Settings = serde_yml::from_value(v).unwrap();
        assert!(s.default_model.is_none());
        assert!(s.model_groups.is_empty());
        assert_eq!(s.ui.max_tool_iterations, 10);
    }

    #[test]
    fn expand_secrets_substitutes_into_group_and_mcp_fields() {
        let mut s = Settings::default();
        s.model_groups.push(ModelGroup {
            name: "g".into(),
            base_url: "http://x".into(),
            api_key: Some("Bearer ${TOK}".into()),
            headers: {
                let mut m = BTreeMap::new();
                m.insert("X".into(), "${TOK}-suffix".into());
                m
            },
            models: vec!["m".into()],
        });
        s.mcp_servers.push(McpServerCfg {
            name: "srv".into(),
            url: "http://srv".into(),
            headers: {
                let mut m = BTreeMap::new();
                m.insert("Authorization".into(), "Bearer ${MCP}".into());
                m
            },
            enabled: true,
        });

        let json = r#"{"TOK":"sk-xyz","MCP":"tskey-aaa"}"#;
        let dir = std::env::temp_dir().join(format!("aic-types-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("env.json"), json).unwrap();
        let secrets = Secrets::load(&dir);
        std::fs::remove_dir_all(&dir).ok();

        s.expand_secrets(&secrets);
        assert_eq!(s.model_groups[0].api_key.as_deref(), Some("Bearer sk-xyz"));
        assert_eq!(s.model_groups[0].headers["X"], "sk-xyz-suffix");
        assert_eq!(s.mcp_servers[0].headers["Authorization"], "Bearer tskey-aaa");
    }

    #[test]
    fn redacted_masks_api_key_and_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".into(), "Bearer sk-real".into());
        let mut mcp_headers = BTreeMap::new();
        mcp_headers.insert("X-Auth".into(), "secret-token".into());

        let s = Settings {
            model_groups: vec![ModelGroup {
                name: "openai".into(),
                base_url: "https://api.openai.com/v1".into(),
                api_key: Some("sk-very-real".into()),
                headers,
                models: vec!["gpt-4o-mini".into()],
            }],
            mcp_servers: vec![McpServerCfg {
                name: "tools".into(),
                url: "http://example/mcp".into(),
                headers: mcp_headers,
                enabled: true,
            }],
            ..Default::default()
        };

        let r = s.redacted();
        assert_eq!(r.model_groups[0].api_key.as_deref(), Some(REDACTED));
        assert_eq!(r.model_groups[0].headers["Authorization"], REDACTED);
        assert_eq!(r.mcp_servers[0].headers["X-Auth"], REDACTED);
        // 非秘匿フィールドは保持
        assert_eq!(r.model_groups[0].base_url, "https://api.openai.com/v1");
        assert_eq!(r.model_groups[0].models, vec!["gpt-4o-mini".to_string()]);
    }

    #[test]
    fn redacted_leaves_none_api_key_as_none() {
        let s = Settings {
            model_groups: vec![ModelGroup {
                name: "local".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                api_key: None,
                headers: BTreeMap::new(),
                models: vec![],
            }],
            ..Default::default()
        };
        let r = s.redacted();
        assert!(r.model_groups[0].api_key.is_none());
    }
}
