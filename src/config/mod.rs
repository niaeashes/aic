// config — Settings 型と config.yaml のロード（SPEC §4, §11）。
//
// レイヤリングは「ホーム + 起動ディレクトリ」のトップレベル浅マージ（SPEC §4.1）。
// プロジェクト側に存在するキーは、ホーム側の値を要素マージせず丸ごと置き換える。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub mod secrets;

// ---------------------------------------------------------------------------
// 設定型（SPEC §4.2, §11）
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
    #[serde(default = "default_stream")]
    pub stream: bool,
    #[serde(default = "default_history_size")]
    pub history_size: usize,
    #[serde(default = "default_max_tool_iter")]
    pub max_tool_iterations: u32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            stream: default_stream(),
            history_size: default_history_size(),
            max_tool_iterations: default_max_tool_iter(),
        }
    }
}

fn default_stream() -> bool {
    true
}
fn default_history_size() -> usize {
    1000
}
fn default_max_tool_iter() -> u32 {
    10
}

// ---------------------------------------------------------------------------
// ModelRef — `<group>:<model>` 形式（SPEC §2, §10）
// model 名自体が `:` を含むので、`splitn(2, ':')` で必ず 1 回だけ分割する。
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

// ---------------------------------------------------------------------------
// Settings 検索ヘルパ（M4 — `/model use` の存在検証で使う）
// ---------------------------------------------------------------------------

impl Settings {
    /// グループ名で `ModelGroup` を引く。`O(n)` 線形検索だが、グループ数は数個想定。
    pub fn group_by_name(&self, name: &str) -> Option<&ModelGroup> {
        self.model_groups.iter().find(|g| g.name == name)
    }

    /// `<group>` が存在し、その `models` に `<model>` が含まれていれば true。
    /// `:` の解釈は `ModelRef::parse` 済みの想定で、ここでは文字列比較のみ。
    pub fn model_exists(&self, group: &str, model: &str) -> bool {
        self.group_by_name(group)
            .map(|g| g.models.iter().any(|m| m == model))
            .unwrap_or(false)
    }

    /// config 内の `${VAR}` を全フィールドに対して展開する（M5, SPEC §5）。
    ///
    /// 対象は API キー・グループ headers・MCP サーバ headers の各値。
    /// 起動時に 1 回だけ呼び、以降は展開済みの最終値が利用される（agent / mcp が再展開しない）。
    /// 未解決のプレースホルダはそのまま残るので、実 API 呼び出し時にユーザに分かりやすく失敗する。
    pub fn expand_secrets(&mut self, secrets: &secrets::Secrets) {
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
// ロードとマージ
// ---------------------------------------------------------------------------

/// ホーム config のパスを解決。
///
/// 優先順:
/// 1. `--config <path>` で明示指定
/// 2. `$AIC_CONFIG_DIR/config.yaml`
/// 3. `$HOME/.config/aic/config.yaml`
pub fn home_config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(dir) = std::env::var("AIC_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("config.yaml"));
    }
    let home = std::env::var("HOME").context("$HOME が解決できない")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("aic")
        .join("config.yaml"))
}

/// プロジェクト config のパス（カレントの `aic.yaml`、固定）。
pub fn project_config_path() -> PathBuf {
    PathBuf::from("aic.yaml")
}

/// 2 層浅マージで Settings を構築。
///
/// ホーム/プロジェクトどちらも無くてもエラーにせず、デフォルト Settings を返す
/// （DoD: ホーム config が無くてもデフォルト設定で起動する）。
pub fn load(explicit_home: Option<&Path>) -> Result<Settings> {
    let home_path = home_config_path(explicit_home)?;
    let project_path = project_config_path();

    let home_val = read_yaml(&home_path)?;
    let project_val = read_yaml(&project_path)?;

    let merged = match (home_val, project_val) {
        (None, None) => serde_yml::Value::Mapping(serde_yml::Mapping::new()),
        (Some(h), None) => h,
        (None, Some(p)) => p,
        (Some(h), Some(p)) => shallow_merge(h, p),
    };

    let mut settings: Settings =
        serde_yml::from_value(merged).context("config の解析に失敗")?;
    settings.config_dir = home_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Ok(settings)
}

fn read_yaml(path: &Path) -> Result<Option<serde_yml::Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("config 読み込み失敗: {}", path.display()))?;
    let val: serde_yml::Value = serde_yml::from_str(&raw)
        .with_context(|| format!("YAML パース失敗: {}", path.display()))?;
    // 空ファイル → Null。マージで Mapping を要求するので Mapping 化しておく。
    let val = if matches!(val, serde_yml::Value::Null) {
        serde_yml::Value::Mapping(serde_yml::Mapping::new())
    } else {
        val
    };
    Ok(Some(val))
}

/// トップレベル Mapping 同士を浅くマージ（overlay 側が優先、要素マージはしない）。
fn shallow_merge(base: serde_yml::Value, overlay: serde_yml::Value) -> serde_yml::Value {
    use serde_yml::Value;
    match (base, overlay) {
        (Value::Mapping(mut b), Value::Mapping(o)) => {
            for (k, v) in o {
                b.insert(k, v);
            }
            Value::Mapping(b)
        }
        // どちらかが Mapping でない（壊れた config）場合は overlay を採用。
        (_, v) => v,
    }
}

// ---------------------------------------------------------------------------
// 単体テスト — 純粋ロジックのみ（MILESTONES.md 横断方針）
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
        // SPEC §10: model 名に `:` を含むため splitn(2, ':') で 1 回だけ分割。
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
    fn shallow_merge_replaces_top_level_keys() {
        let base: serde_yml::Value = serde_yml::from_str(
            r#"
            default_model: a:x
            ui:
              stream: true
              history_size: 100
            "#,
        )
        .unwrap();
        let overlay: serde_yml::Value = serde_yml::from_str(
            r#"
            ui:
              stream: false
            "#,
        )
        .unwrap();
        let merged = shallow_merge(base, overlay);
        // ui は丸ごと置き換え。history_size は overlay に無いので消える。
        let merged_str = serde_yml::to_string(&merged).unwrap();
        assert!(merged_str.contains("default_model"));
        assert!(merged_str.contains("stream: false"));
        assert!(!merged_str.contains("history_size"));
    }

    #[test]
    fn expand_secrets_substitutes_into_group_and_mcp_fields() {
        use secrets::Secrets;
        // Secrets::default() の map に直接挿入できるよう、`pub(crate)` 越しの
        // 公開 API で組み立てる。テスト用の値は環境変数を汚染しないよう map のみ使用。
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

        // 同モジュール内なので Secrets::default() を経由してテスト用 map を間接構築。
        // expand() ロジックは secrets 側で網羅済みなので、ここでは「配線」だけ確認する。
        let json = r#"{"TOK":"sk-xyz","MCP":"tskey-aaa"}"#;
        // ダミーの env.json を読ませるため一時ディレクトリに書き出して Secrets::load を使う
        let dir = std::env::temp_dir().join(format!("aic-expand-test-{}", std::process::id()));
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
    fn settings_deserializes_from_empty_mapping() {
        let v = serde_yml::Value::Mapping(serde_yml::Mapping::new());
        let s: Settings = serde_yml::from_value(v).unwrap();
        assert!(s.default_model.is_none());
        assert!(s.model_groups.is_empty());
        assert_eq!(s.ui.max_tool_iterations, 10);
    }
}
