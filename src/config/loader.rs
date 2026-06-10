// config/loader — config.yaml loading and two-layer merge (SPEC §4.1).
//
// Public API:
//   load(explicit_home)     — Shallow-merge home + project and return Settings
//   home_config_path(p)     — Resolve the home config path
//   project_config_path()   — `aic.yaml` in the current directory (fixed)
//
// Merge policy: top-level keys; the overlay (project) wins. We do not merge
// elements within a key (e.g. lists in model_groups) — overlays replace wholesale.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::types::Settings;

/// Resolve the home config path.
///
/// Priority:
/// 1. Explicit `--config <path>` argument
/// 2. `$AIC_CONFIG_DIR/config.yaml`
/// 3. `$HOME/.config/aic/config.yaml`
pub fn home_config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(dir) = std::env::var("AIC_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("config.yaml"));
    }
    let home = std::env::var("HOME").context("$HOME could not be resolved")?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("aic")
        .join("config.yaml"))
}

/// Project config path (always `aic.yaml` in the current directory).
pub fn project_config_path() -> PathBuf {
    PathBuf::from("aic.yaml")
}

/// Build Settings via the two-layer shallow merge.
///
/// Returns default Settings even when neither file exists (DoD: startup works
/// with no home config).
pub fn load(explicit_home: Option<&Path>) -> Result<Settings> {
    let home_path = home_config_path(explicit_home)?;
    let project_path = project_config_path();
    let config_dir = home_path.parent().map(Path::to_path_buf).unwrap_or_default();

    let home_val = read_yaml(&home_path)?;
    let project_val = load_trusted_project(&project_path, &config_dir)?;

    let merged = match (home_val, project_val) {
        (None, None) => serde_yml::Value::Mapping(serde_yml::Mapping::new()),
        (Some(h), None) => h,
        (None, Some(p)) => p,
        (Some(h), Some(p)) => shallow_merge(h, p),
    };

    let mut settings: Settings =
        serde_yml::from_value(merged).context("failed to parse config")?;
    settings.config_dir = home_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Ok(settings)
}

/// Read the project `aic.yaml`, but only return it if the user has approved it.
///
/// A checked-in project config is a credential-exfiltration vector (it can point
/// `base_url`/`headers` at an attacker and have `${SECRETS}` expanded into them),
/// so we gate it behind a one-time approval keyed by path + content hash
/// (`config::trust`). Untrusted or unreviewed configs are dropped — startup
/// continues with the home config alone.
fn load_trusted_project(
    project_path: &Path,
    config_dir: &Path,
) -> Result<Option<serde_yml::Value>> {
    if !project_path.exists() {
        return Ok(None);
    }
    // We need the verbatim text for the trust hash, and the parsed value for the
    // merge — read once, use for both.
    let raw = std::fs::read_to_string(project_path)
        .with_context(|| format!("failed to read config: {}", project_path.display()))?;
    let val: serde_yml::Value = serde_yml::from_str(&raw)
        .with_context(|| format!("failed to parse YAML: {}", project_path.display()))?;
    let val = if matches!(val, serde_yml::Value::Null) {
        serde_yml::Value::Mapping(serde_yml::Mapping::new())
    } else {
        val
    };

    let keys = top_level_keys(&val);
    if crate::config::trust::ensure_project_trusted(project_path, config_dir, &raw, &keys) {
        Ok(Some(val))
    } else {
        Ok(None)
    }
}

/// Top-level mapping keys, for the trust prompt summary. Non-mapping configs
/// yield an empty list.
fn top_level_keys(val: &serde_yml::Value) -> Vec<String> {
    match val {
        serde_yml::Value::Mapping(m) => m
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn read_yaml(path: &Path) -> Result<Option<serde_yml::Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config: {}", path.display()))?;
    let val: serde_yml::Value = serde_yml::from_str(&raw)
        .with_context(|| format!("failed to parse YAML: {}", path.display()))?;
    // Empty file → Null. Merge expects a Mapping, so coerce here.
    let val = if matches!(val, serde_yml::Value::Null) {
        serde_yml::Value::Mapping(serde_yml::Mapping::new())
    } else {
        val
    };
    Ok(Some(val))
}

/// Shallow-merge two top-level Mappings (overlay wins; no per-element merge).
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
// Tests
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
        // `ui` is fully replaced, so history_size is gone.
        assert!(!merged_str.contains("history_size"));
    }
}
