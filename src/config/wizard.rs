// config/wizard — interactive Settings construction for `/config setup` (SPEC §10).
//
// Role split:
//   - here              : the "domain logic" that assembles Settings
//   - commands/config.rs: the command entry; decides the write target, confirmation, file write
//   - repl/prompt.rs    : single-line prompt primitives shared with the wizard
//
// IO flows through `BufRead` so tests can drive it with a `Cursor` instead of real stdin.
//
// Style:
//   - api_key / headers are **encouraged to be `${VAR}` placeholders**. We hint
//     in the prompt copy rather than enforcing it — we don't want to block auth-less
//     endpoints like ollama.
//   - default_model: we flatten group × model into a list, then ask for a number,
//     guaranteeing that the resulting Settings always has default_model populated.
//   - MCP servers are optional and can be skipped entirely.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::config::{McpServerCfg, ModelGroup, ModelRef, Settings, UiConfig};
use crate::repl::prompt::{prompt_bool, prompt_optional, prompt_required, prompt_u32};

/// Drive the interactive flow and return the assembled `Settings`. `current` is
/// the existing Settings; we carry forward values the wizard doesn't ask about
/// (e.g. `history_size`).
pub fn run<R: BufRead>(r: &mut R, current: &Settings) -> Result<Settings> {
    // At least one model group is required.
    let mut groups: Vec<ModelGroup> = Vec::new();
    loop {
        println!("\n[model group #{}]", groups.len() + 1);
        let group = read_model_group(r)?;
        groups.push(group);
        if !prompt_bool(r, "Add another model group?", false)? {
            break;
        }
    }

    let default_model = pick_default_model(r, &groups)?;

    // MCP servers are optional.
    let mut servers: Vec<McpServerCfg> = Vec::new();
    if prompt_bool(r, "\nConfigure MCP servers?", false)? {
        loop {
            println!("\n[mcp server #{}]", servers.len() + 1);
            let srv = read_mcp_server(r)?;
            servers.push(srv);
            if !prompt_bool(r, "Add another MCP server?", false)? {
                break;
            }
        }
    }

    let max_iter = prompt_u32(r, "\nui.max_tool_iterations", current.ui.max_tool_iterations)?;

    Ok(build_settings(
        groups,
        default_model,
        servers,
        max_iter,
        current.ui.history_size,
    ))
}

/// Pure builder — no IO.
fn build_settings(
    groups: Vec<ModelGroup>,
    default_model: ModelRef,
    servers: Vec<McpServerCfg>,
    max_iter: u32,
    history_size: usize,
) -> Settings {
    Settings {
        default_model: Some(default_model),
        model_groups: groups,
        mcp_servers: servers,
        ui: UiConfig {
            history_size,
            max_tool_iterations: max_iter,
        },
        config_dir: PathBuf::new(),
        ..Default::default()
    }
}

fn read_model_group<R: BufRead>(r: &mut R) -> Result<ModelGroup> {
    let name = prompt_required(r, "group name (e.g. openai, local)")?;
    let base_url = prompt_required(r, "base_url (e.g. https://api.openai.com/v1)")?;
    let api_key = prompt_optional(
        r,
        "api_key (${VAR} recommended; set VAR in env.json or the environment. Empty = None)",
    )?;
    println!("models (comma-separated, at least one):");
    let models_line = prompt_required(r, "models")?;
    let models: Vec<String> = models_line
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        bail!("at least one model is required");
    }
    Ok(ModelGroup {
        name,
        base_url,
        api_key,
        headers: BTreeMap::new(),
        models,
    })
}

fn read_mcp_server<R: BufRead>(r: &mut R) -> Result<McpServerCfg> {
    let name = prompt_required(r, "name (e.g. tools)")?;
    let url = prompt_required(r, "url (e.g. https://example.com/mcp)")?;
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let auth = prompt_optional(
        r,
        "Authorization header (e.g. Bearer ${MCP_TOKEN}; empty to skip)",
    )?;
    if let Some(v) = auth {
        headers.insert("Authorization".to_string(), v);
    }
    Ok(McpServerCfg {
        name,
        url,
        headers,
        enabled: true,
    })
}

fn pick_default_model<R: BufRead>(r: &mut R, groups: &[ModelGroup]) -> Result<ModelRef> {
    // Flatten and number.
    let mut flat: Vec<(String, String)> = Vec::new();
    for g in groups {
        for m in &g.models {
            flat.push((g.name.clone(), m.clone()));
        }
    }
    if flat.is_empty() {
        bail!("no models configured, so default_model can't be chosen");
    }
    println!("\nChoose default_model:");
    for (i, (g, m)) in flat.iter().enumerate() {
        println!("  {}) {}:{}", i + 1, g, m);
    }
    loop {
        let s = prompt_required(r, &format!("number (1-{})", flat.len()))?;
        let n: usize = match s.parse() {
            Ok(n) => n,
            Err(_) => {
                println!("error: please answer with a number");
                continue;
            }
        };
        if n == 0 || n > flat.len() {
            println!("error: out of range");
            continue;
        }
        let (g, m) = &flat[n - 1];
        return Ok(ModelRef {
            group: g.clone(),
            model: m.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_model_group_parses_basic_input() {
        let input = "openai\nhttps://api.openai.com/v1\n${OPENAI_API_KEY}\ngpt-4o-mini, gpt-4o\n";
        let mut cur = Cursor::new(input.as_bytes());
        let g = read_model_group(&mut cur).unwrap();
        assert_eq!(g.name, "openai");
        assert_eq!(g.base_url, "https://api.openai.com/v1");
        assert_eq!(g.api_key.as_deref(), Some("${OPENAI_API_KEY}"));
        assert_eq!(g.models, vec!["gpt-4o-mini", "gpt-4o"]);
    }

    #[test]
    fn read_model_group_allows_empty_api_key() {
        let input = "local\nhttp://127.0.0.1:11434/v1\n\nllama3\n";
        let mut cur = Cursor::new(input.as_bytes());
        let g = read_model_group(&mut cur).unwrap();
        assert!(g.api_key.is_none());
        assert_eq!(g.models, vec!["llama3"]);
    }

    #[test]
    fn pick_default_model_returns_selected_entry() {
        let groups = vec![
            ModelGroup {
                name: "a".into(),
                base_url: "x".into(),
                api_key: None,
                headers: BTreeMap::new(),
                models: vec!["m1".into(), "m2".into()],
            },
            ModelGroup {
                name: "b".into(),
                base_url: "y".into(),
                api_key: None,
                headers: BTreeMap::new(),
                models: vec!["m3".into()],
            },
        ];
        // Number 3 → b:m3
        let mut cur = Cursor::new(b"3\n".as_slice());
        let r = pick_default_model(&mut cur, &groups).unwrap();
        assert_eq!(r.group, "b");
        assert_eq!(r.model, "m3");
    }

    #[test]
    fn run_setup_basic_no_mcp() {
        // 4 group lines → no more groups (n) → default 1 → no MCP (n) → max_iter default
        let input = concat!(
            "openai\n",                    // group name
            "https://api.openai.com/v1\n", // base_url
            "${OPENAI_API_KEY}\n",          // api_key
            "gpt-4o-mini\n",               // models
            "n\n",                         // add another group? → No
            "1\n",                         // default_model number
            "n\n",                         // configure MCP? → No
            "\n",                          // max_tool_iterations default
        );
        let current = Settings::default();
        let mut cur = Cursor::new(input.as_bytes());
        let s = run(&mut cur, &current).unwrap();
        assert_eq!(s.model_groups.len(), 1);
        assert_eq!(s.model_groups[0].name, "openai");
        assert_eq!(s.default_model.as_ref().unwrap().group, "openai");
        assert!(s.mcp_servers.is_empty());
        assert_eq!(s.ui.max_tool_iterations, 10); // UiConfig::default
    }

    #[test]
    fn build_settings_carries_history_size() {
        let groups = vec![ModelGroup {
            name: "g".into(),
            base_url: "http://x".into(),
            api_key: None,
            headers: BTreeMap::new(),
            models: vec!["m".into()],
        }];
        let default_model = ModelRef { group: "g".into(), model: "m".into() };
        let s = build_settings(groups, default_model, vec![], 5, 500);
        assert_eq!(s.ui.history_size, 500);
        assert_eq!(s.ui.max_tool_iterations, 5);
    }
}
