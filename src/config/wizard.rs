// config/wizard — `/config setup` の対話的構築ドメイン（SPEC §10, M8）。
//
// 役割分担:
//   - ここ              : Settings を組み立てる「ドメインロジック」
//   - commands/config.rs: コマンドとして起動し、書き出し先パス決定 / 確認 / ファイル書き出しを行う
//   - repl/prompt.rs    : 1 行プロンプト等の対話 IO プリミティブ
//
// IO は `BufRead` を経由するため、テストでは `Cursor` を差し込める（標準入力に依存しない）。
//
// 方針:
//   - api_key / header は **`${VAR}` プレースホルダ推奨**。プロンプト文で誘導するだけで
//     強制はしない（ollama 等の認証なしエンドポイント運用を妨げないため）。
//   - default_model は収集済みグループ × モデルを 1 列に並べて番号選択させる。
//     これにより `/config setup` の出力 Settings は必ず default_model が埋まる。
//   - MCP サーバは任意。設定したくない場合は丸ごとスキップできる。

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::config::{McpServerCfg, ModelGroup, ModelRef, Settings, UiConfig};
use crate::repl::prompt::{prompt_bool, prompt_optional, prompt_required, prompt_u32};

/// 対話的に設定を収集して `Settings` を返す。`current` から `history_size` 等の
/// 「ウィザードで聞かない値」を引き継ぐ。
pub fn run<R: BufRead>(r: &mut R, current: &Settings) -> Result<Settings> {
    // モデルグループは最低 1 個必須
    let mut groups: Vec<ModelGroup> = Vec::new();
    loop {
        println!("\n[model group #{}]", groups.len() + 1);
        let group = read_model_group(r)?;
        groups.push(group);
        if !prompt_bool(r, "もう 1 つ model group を追加しますか？", false)? {
            break;
        }
    }

    let default_model = pick_default_model(r, &groups)?;

    // MCP サーバは任意
    let mut servers: Vec<McpServerCfg> = Vec::new();
    if prompt_bool(r, "\nMCP サーバを設定しますか？", false)? {
        loop {
            println!("\n[mcp server #{}]", servers.len() + 1);
            let srv = read_mcp_server(r)?;
            servers.push(srv);
            if !prompt_bool(r, "もう 1 つ MCP サーバを追加しますか？", false)? {
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

/// 収集済みの値から `Settings` を組み立てる。IO なし。
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
    }
}

fn read_model_group<R: BufRead>(r: &mut R) -> Result<ModelGroup> {
    let name = prompt_required(r, "group 名（例: openai, local）")?;
    let base_url = prompt_required(r, "base_url（例: https://api.openai.com/v1）")?;
    let api_key = prompt_optional(
        r,
        "api_key（${VAR} 推奨。env.json で VAR=値、または環境変数で設定。空で None）",
    )?;
    println!("models（カンマ区切りで 1 つ以上）:");
    let models_line = prompt_required(r, "models")?;
    let models: Vec<String> = models_line
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if models.is_empty() {
        bail!("models は 1 つ以上必要です");
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
    let name = prompt_required(r, "name（例: tools）")?;
    let url = prompt_required(r, "url（例: https://example.com/mcp）")?;
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let auth = prompt_optional(
        r,
        "Authorization ヘッダ（例: Bearer ${MCP_TOKEN}、空でスキップ）",
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
    // フラット化して番号を振る
    let mut flat: Vec<(String, String)> = Vec::new();
    for g in groups {
        for m in &g.models {
            flat.push((g.name.clone(), m.clone()));
        }
    }
    if flat.is_empty() {
        bail!("models が 1 つも無いので default_model を決定できません");
    }
    println!("\ndefault_model を選んでください:");
    for (i, (g, m)) in flat.iter().enumerate() {
        println!("  {}) {}:{}", i + 1, g, m);
    }
    loop {
        let s = prompt_required(r, &format!("番号 (1-{})", flat.len()))?;
        let n: usize = match s.parse() {
            Ok(n) => n,
            Err(_) => {
                println!("error: 数字で答えてください");
                continue;
            }
        };
        if n == 0 || n > flat.len() {
            println!("error: 範囲外です");
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
        // 番号 3 → b:m3
        let mut cur = Cursor::new(b"3\n".as_slice());
        let r = pick_default_model(&mut cur, &groups).unwrap();
        assert_eq!(r.group, "b");
        assert_eq!(r.model, "m3");
    }

    #[test]
    fn run_setup_basic_no_mcp() {
        // group 名 / base_url / api_key / models の 4 行 → デフォルト選択 (1) →
        // group 追加なし (n) → MCP なし (n) → max_iter デフォルト
        let input = concat!(
            "openai\n",                    // group 名
            "https://api.openai.com/v1\n", // base_url
            "${OPENAI_API_KEY}\n",          // api_key
            "gpt-4o-mini\n",               // models
            "n\n",                         // group 追加？ → No
            "1\n",                         // default_model 番号
            "n\n",                         // MCP サーバ設定？ → No
            "\n",                          // max_tool_iterations デフォルト
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
