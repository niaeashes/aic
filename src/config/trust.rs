// config/trust — approval gate for project-level `./aic.yaml` (SPEC §4.3).
//
// A checked-in `aic.yaml` can set `base_url` / `headers` / `api_key` and have
// `${VAR}` expanded against the *sealed secrets map*. Without a gate, simply
// running `aic` inside a hostile repo would exfiltrate keyring secrets to an
// attacker-controlled endpoint. We borrow direnv's model: a project config is
// applied only after the user approves it once. Approval is keyed by the file's
// absolute path AND a content hash, so editing the file forces re-approval.
//
// The hash is a non-cryptographic change detector (SipHash via DefaultHasher),
// not an integrity guarantee — the trust file lives in the user's own config dir
// and the real check is the human reviewing the config before saying yes.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::Result;

const TRUST_STORE: &str = "trusted_projects.json";

/// Decide whether a detected project `aic.yaml` may be applied.
///
/// `raw_content` is the file's verbatim text (used for the content hash).
/// `top_level_keys` is shown to the user so they know what the config overrides.
///
/// Returns `true` to apply the project config, `false` to ignore it. Never errors
/// out of the startup path: any failure resolves to "not trusted" (the safe
/// default) with a warning.
pub fn ensure_project_trusted(
    project_path: &Path,
    config_dir: &Path,
    raw_content: &str,
    top_level_keys: &[String],
) -> bool {
    let canonical = std::fs::canonicalize(project_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| project_path.display().to_string());
    let store_path = config_dir.join(TRUST_STORE);
    let hash = content_hash(raw_content);

    if is_trusted(&canonical, &hash, &store_path) {
        return true;
    }

    // Untrusted. In a non-interactive context (CI, piped stdin) we can't prompt,
    // so we fail safe and ignore the project config.
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "warning: untrusted project config {canonical} ignored \
             (run `aic` interactively in this directory once to approve it)"
        );
        return false;
    }

    match prompt_for_approval(&canonical, top_level_keys) {
        Ok(true) => {
            if let Err(e) = record_trust(&canonical, &hash, &store_path) {
                eprintln!("warning: failed to persist trust for {canonical}: {e:#}");
            }
            true
        }
        _ => {
            eprintln!("→ ignoring {canonical}; using home config only");
            false
        }
    }
}

/// 16-hex-digit content fingerprint. Deterministic across runs (DefaultHasher
/// uses fixed keys), so an unedited file stays trusted.
fn content_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Read the trust store and check whether `canonical` is recorded with `hash`.
fn is_trusted(canonical: &str, hash: &str, store_path: &Path) -> bool {
    load_store(store_path)
        .get(canonical)
        .is_some_and(|recorded| recorded == hash)
}

fn load_store(store_path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(store_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn record_trust(canonical: &str, hash: &str, store_path: &Path) -> Result<()> {
    let mut store = load_store(store_path);
    store.insert(canonical.to_string(), hash.to_string());
    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string_pretty(&store)?;
    std::fs::write(store_path, json)?;
    Ok(())
}

/// Show a summary and read a y/N answer from stdin.
fn prompt_for_approval(canonical: &str, top_level_keys: &[String]) -> Result<bool> {
    let sensitive: Vec<&String> = top_level_keys
        .iter()
        .filter(|k| matches!(k.as_str(), "model_groups" | "mcp_servers"))
        .collect();

    eprintln!("⚠ Untrusted project config detected: {canonical}");
    eprintln!("  sets top-level keys: {}", top_level_keys.join(", "));
    if !sensitive.is_empty() {
        eprintln!(
            "  ⚠ {} can redirect requests and expand ${{SECRETS}} into arbitrary \
             URLs/headers — only trust configs you have reviewed.",
            sensitive
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        );
    }
    eprint!("Trust this project config? [y/N]: ");
    std::io::stderr().flush().ok();

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let ans = line.trim().to_ascii_lowercase();
    Ok(ans == "y" || ans == "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_stable_and_sensitive() {
        assert_eq!(content_hash("abc"), content_hash("abc"));
        assert_ne!(content_hash("abc"), content_hash("abd"));
    }

    #[test]
    fn untrusted_until_recorded_then_trusted() {
        let dir = std::env::temp_dir().join(format!("aic-trust-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join(TRUST_STORE);
        let path = "/proj/aic.yaml";
        let hash = content_hash("content-v1");

        assert!(!is_trusted(path, &hash, &store));
        record_trust(path, &hash, &store).unwrap();
        assert!(is_trusted(path, &hash, &store));

        // A different content hash (edited file) is no longer trusted.
        let hash2 = content_hash("content-v2");
        assert!(!is_trusted(path, &hash2, &store));

        std::fs::remove_dir_all(&dir).ok();
    }
}
