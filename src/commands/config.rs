// /config — 現在の設定の確認 + 初期化ウィザード（SPEC §10, MILESTONES.md M4, M8）。
//
// サブコマンド:
//   - `show`  : 現在の Settings を YAML で表示。api_key / headers の値はマスク。
//   - `setup` : 対話的に model group / MCP サーバを聞き、ホーム config.yaml に書き出す。
//
// `show` の出力方針:
//   - YAML で読みやすい体裁にする（`serde_yml::to_string`）。
//   - api_key と headers の **値** は `***` にリダクトする。secret-bearing で
//     ないヘッダ（Content-Type 等）も区別せず一律マスクする — config に手で
//     書く header は基本 auth 系のみ、という運用前提。
//   - mcp_servers の headers も同様にマスク（M5 で `${VAR}` 展開後の値が
//     入ることがあるため、特に重要）。
//
// `setup` の方針:
//   - 既存ファイルがあれば確認してから上書き
//   - api_key / header の値は **そのまま受け取らず** `${VAR}` プレースホルダで保存させる
//     （ファイルに生秘密を書かせない）
//   - 生成後は次回起動時から有効。現在の session には反映しない（再起動を促す）
//   - MCP は任意。スキップすると tools 無しで動く
//
// rustyline は外側ループが占有しているが、`run` が呼ばれている間は内部状態だけ
// （readline 待機中ではない）なので、wizard 内では `std::io::stdin().read_line` を使う。

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use super::{Command, Outcome};
use crate::config::{home_config_path, McpServerCfg, ModelGroup, ModelRef, Settings};
use crate::repl::context::ReplContext;

const REDACTED: &str = "***";

struct Config;

#[async_trait]
impl Command for Config {
    fn name(&self) -> &'static str {
        "config"
    }

    fn help(&self) -> &'static str {
        "`show` で現在の設定を表示 / `setup` で対話的にホーム config.yaml を生成"
    }

    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let trimmed = args.trim();
        let (sub, _rest) = match trimmed.find(char::is_whitespace) {
            Some(idx) => (&trimmed[..idx], trimmed[idx + 1..].trim()),
            None => (trimmed, ""),
        };
        match sub {
            "" | "show" => show(&ctx.settings),
            "setup" => setup(ctx),
            other => bail!("不明なサブコマンド: {other}（使い方: /config show | /config setup）"),
        }
    }
}

fn show(settings: &Settings) -> Result<Outcome> {
    let redacted = redact(settings);
    let yaml = serde_yml::to_string(&redacted)?;
    if !settings.config_dir.as_os_str().is_empty() {
        println!("# config_dir: {}", settings.config_dir.display());
    }
    print!("{yaml}");
    Ok(Outcome::Continue)
}

/// `Settings` を deep-clone し、secret 値を `***` に置換した上で返す。
///
/// `Settings` 自体に `serde(skip)` フィールド (`config_dir`) があり、シリアライズには
/// 含まれない。なので config_dir は呼び出し側でログ風に別途出力している。
fn redact(settings: &Settings) -> Settings {
    let mut s = settings.clone();
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

// ---------------------------------------------------------------------------
// `/config setup` ウィザード
// ---------------------------------------------------------------------------

fn setup(ctx: &mut ReplContext) -> Result<Outcome> {
    // 書き出し先は ctx.settings.config_dir（main.rs でロード済み）配下の config.yaml。
    // `--config <path>` 指定時も config_dir はそのパスの親ディレクトリになっているため、
    // ここで再評価する必要はない。home_config_path(None) を使うと --config 指定が無視される。
    let target = if ctx.settings.config_dir.as_os_str().is_empty() {
        // config_dir が空（デフォルト値のまま）の場合のみフォールバックとして再解決する
        home_config_path(None).context("ホーム config パスの解決に失敗")?
    } else {
        ctx.settings.config_dir.join("config.yaml")
    };

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();

    println!("aic config setup — 対話的にホーム config.yaml を生成します。");
    println!("書き出し先: {}", target.display());

    if target.exists() {
        let overwrite = prompt_bool(&mut stdin, "既存ファイルがあります。上書きしますか？", false)?;
        if !overwrite {
            println!("setup を中断しました。");
            return Ok(Outcome::Continue);
        }
    }

    // モデルグループは最低 1 個必須
    let mut groups: Vec<ModelGroup> = Vec::new();
    loop {
        println!("\n[model group #{}]", groups.len() + 1);
        let group = read_model_group(&mut stdin)?;
        groups.push(group);
        if !prompt_bool(&mut stdin, "もう 1 つ model group を追加しますか？", false)? {
            break;
        }
    }

    // default_model（一覧から番号で選ばせる）
    let default_model = pick_default_model(&mut stdin, &groups)?;

    // MCP サーバは任意
    let mut servers: Vec<McpServerCfg> = Vec::new();
    if prompt_bool(&mut stdin, "\nMCP サーバを設定しますか？", false)? {
        loop {
            println!("\n[mcp server #{}]", servers.len() + 1);
            let srv = read_mcp_server(&mut stdin)?;
            servers.push(srv);
            if !prompt_bool(&mut stdin, "もう 1 つ MCP サーバを追加しますか？", false)? {
                break;
            }
        }
    }

    let max_iter = prompt_u32(
        &mut stdin,
        "\nui.max_tool_iterations",
        ctx.settings.ui.max_tool_iterations,
    )?;

    let new_settings = Settings {
        default_model: Some(default_model),
        model_groups: groups,
        mcp_servers: servers,
        ui: crate::config::UiConfig {
            stream: true,
            history_size: ctx.settings.ui.history_size,
            max_tool_iterations: max_iter,
        },
        config_dir: PathBuf::new(),
    };

    // プレビュー（redact 済み）
    println!("\n=== 生成される config.yaml（秘匿値はマスク表示）===");
    let preview = redact(&new_settings);
    print!("{}", serde_yml::to_string(&preview)?);
    println!("===");

    if !prompt_bool(&mut stdin, "\nこの内容で書き出しますか？", true)? {
        println!("setup を中断しました。");
        return Ok(Outcome::Continue);
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("ディレクトリ作成失敗: {}", parent.display()))?;
    }
    let yaml = serde_yml::to_string(&new_settings)?;
    std::fs::write(&target, yaml)
        .with_context(|| format!("書き出し失敗: {}", target.display()))?;
    println!("wrote: {}", target.display());
    println!("注意: 反映には `aic` の再起動が必要です。secrets は別途 `env.json` を");
    println!(
        "      作成し、必要なら `aic env seal` で `env.json.enc` に封印してください。"
    );

    Ok(Outcome::Continue)
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

// ---------------------------------------------------------------------------
// 入力プリミティブ — `std::io::stdin` をラインで読み、trim して返す
// ---------------------------------------------------------------------------

fn read_line<R: BufRead>(r: &mut R) -> Result<String> {
    let mut buf = String::new();
    let n = r.read_line(&mut buf).context("stdin 読み取りに失敗")?;
    if n == 0 {
        // EOF
        bail!("stdin が閉じられました");
    }
    Ok(buf.trim().to_string())
}

fn print_prompt(label: &str, default: Option<&str>) {
    match default {
        Some(d) => print!("  {label} [{d}]: "),
        None => print!("  {label}: "),
    }
    std::io::stdout().flush().ok();
}

fn prompt_required<R: BufRead>(r: &mut R, label: &str) -> Result<String> {
    loop {
        print_prompt(label, None);
        let s = read_line(r)?;
        if !s.is_empty() {
            return Ok(s);
        }
        println!("error: 空にできません");
    }
}

fn prompt_optional<R: BufRead>(r: &mut R, label: &str) -> Result<Option<String>> {
    print_prompt(label, None);
    let s = read_line(r)?;
    Ok(if s.is_empty() { None } else { Some(s) })
}

fn prompt_bool<R: BufRead>(r: &mut R, label: &str, default: bool) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        print_prompt(label, Some(hint));
        let s = read_line(r)?.to_lowercase();
        if s.is_empty() {
            return Ok(default);
        }
        match s.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("error: y / n で答えてください"),
        }
    }
}

fn prompt_u32<R: BufRead>(r: &mut R, label: &str, default: u32) -> Result<u32> {
    let d = default.to_string();
    loop {
        print_prompt(label, Some(&d));
        let s = read_line(r)?;
        if s.is_empty() {
            return Ok(default);
        }
        match s.parse() {
            Ok(n) => return Ok(n),
            Err(_) => println!("error: 整数で答えてください"),
        }
    }
}

inventory::submit! { &Config as &dyn Command }

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn redact_masks_api_key_and_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer sk-real".to_string());
        let mut mcp_headers = BTreeMap::new();
        mcp_headers.insert("X-Auth".to_string(), "secret-token".to_string());

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

        let r = redact(&s);
        assert_eq!(r.model_groups[0].api_key.as_deref(), Some(REDACTED));
        assert_eq!(r.model_groups[0].headers["Authorization"], REDACTED);
        assert_eq!(r.mcp_servers[0].headers["X-Auth"], REDACTED);
        // 非秘匿フィールドは保持
        assert_eq!(r.model_groups[0].base_url, "https://api.openai.com/v1");
        assert_eq!(r.model_groups[0].models, vec!["gpt-4o-mini".to_string()]);
    }

    #[test]
    fn redact_leaves_none_api_key_as_none() {
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
        let r = redact(&s);
        assert!(r.model_groups[0].api_key.is_none());
    }

    #[test]
    fn read_model_group_parses_basic_input() {
        // 4 行: name, base_url, api_key, models
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
    fn prompt_bool_uses_default_on_empty() {
        let mut cur = Cursor::new(b"\n".as_slice());
        let v = prompt_bool(&mut cur, "ok?", true).unwrap();
        assert!(v);

        let mut cur = Cursor::new(b"\n".as_slice());
        let v = prompt_bool(&mut cur, "ok?", false).unwrap();
        assert!(!v);
    }

    #[test]
    fn prompt_bool_parses_yes_no() {
        let mut cur = Cursor::new(b"y\n".as_slice());
        assert!(prompt_bool(&mut cur, "ok?", false).unwrap());
        let mut cur = Cursor::new(b"no\n".as_slice());
        assert!(!prompt_bool(&mut cur, "ok?", true).unwrap());
    }

    #[test]
    fn prompt_u32_uses_default_on_empty() {
        let mut cur = Cursor::new(b"\n".as_slice());
        assert_eq!(prompt_u32(&mut cur, "n", 7).unwrap(), 7);
        let mut cur = Cursor::new(b"42\n".as_slice());
        assert_eq!(prompt_u32(&mut cur, "n", 7).unwrap(), 42);
    }
}
