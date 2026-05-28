// config/loader — config.yaml の読み込みと 2 層マージ（SPEC §4.1）。
//
// 公開 API:
//   load(explicit_home)     — ホーム + プロジェクトを浅マージして Settings を返す
//   home_config_path(p)     — ホーム config パスの解決
//   project_config_path()   — 起動ディレクトリの aic.yaml（固定）
//
// マージ方針: トップレベルキー単位で overlay（プロジェクト側）が優先。
// 要素マージ（model_groups のリストをマージ等）はしない — キー丸ごと置き換え。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::types::Settings;

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
pub fn shallow_merge(base: serde_yml::Value, overlay: serde_yml::Value) -> serde_yml::Value {
    use serde_yml::Value;
    match (base, overlay) {
        (Value::Mapping(mut b), Value::Mapping(o)) => {
            for (k, v) in o {
                b.insert(k, v);
            }
            Value::Mapping(b)
        }
        (_, v) => v,
    }
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shallow_merge_replaces_top_level_keys() {
        let base: serde_yml::Value = serde_yml::from_str(
            r#"
            default_model: a:x
            ui:
              history_size: 100
              max_tool_iterations: 5
            "#,
        )
        .unwrap();
        let overlay: serde_yml::Value = serde_yml::from_str(
            r#"
            ui:
              max_tool_iterations: 20
            "#,
        )
        .unwrap();
        let merged = shallow_merge(base, overlay);
        let merged_str = serde_yml::to_string(&merged).unwrap();
        assert!(merged_str.contains("default_model"));
        assert!(merged_str.contains("max_tool_iterations: 20"));
        // ui は丸ごと置き換えなので history_size は消える
        assert!(!merged_str.contains("history_size"));
    }
}
