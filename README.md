# aic

OpenAI 互換 `/chat/completions` と MCP（Model Context Protocol, Streamable HTTP）で
動作する、最小構成の対話型 CLI チャットツール。Rust 製、ストリーミング表示専用。

- **provider 抽象化なし** — openai-compatible エンドポイントだけを直叩きする
- **MCP は Streamable HTTP のみ** — stdio やソケット系のトランスポートは無し
- **secrets は macOS Keychain + ChaCha20-Poly1305** — `env.json` 平文をコミット禁止

詳細仕様は [SPEC.md](SPEC.md)、マイルストーン分解は [MILESTONES.md](MILESTONES.md)。

---

## 5 分でチャットを返すまで

### 1. ビルド

```sh
cargo build --release
# バイナリは target/release/aic
```

### 2. 最小設定

初回は対話的なウィザードが便利:

```sh
aic
aic> /config setup
```

聞かれる順番:

1. **model group**（group 名 / `base_url` / `api_key`（`${VAR}` 推奨）/ models）
2. **default_model** を一覧から番号で選択
3. （任意）MCP サーバ（name / url / Authorization ヘッダ）
4. `ui.max_tool_iterations`

書き出し先は `~/.config/aic/config.yaml`（`AIC_CONFIG_DIR` 環境変数か
`--config <path>` で上書き可能）。

ウィザードを使わず手書きする場合のサンプル:

```yaml
# ~/.config/aic/config.yaml
default_model: openai:gpt-4o-mini

model_groups:
  - name: openai
    base_url: https://api.openai.com/v1
    api_key: ${OPENAI_API_KEY}
    models:
      - gpt-4o-mini
      - gpt-4o

  - name: local
    base_url: http://127.0.0.1:11434/v1
    # ollama は認証不要なので api_key は省略
    models:
      - qwen2.5-coder:32b

mcp_servers:
  - name: tools
    url: https://example.com/mcp
    headers:
      Authorization: Bearer ${MCP_TOKEN}
    enabled: true

ui:
  stream: true
  history_size: 1000
  max_tool_iterations: 10
```

`${VAR}` プレースホルダの解決順は **secrets マップ → プロセス環境変数**。

### 3. secrets

`~/.config/aic/env.json` を作って、`${VAR}` で参照しているキーをここに書く:

```json
{
  "OPENAI_API_KEY": "sk-...",
  "MCP_TOKEN": "tskey-..."
}
```

この平文 `env.json` は **絶対にコミットしない**。`.gitignore` 済みだが念のため。

macOS であれば、`aic env seal` で `env.json.enc` に封印し、平文を削除できる:

```sh
aic env seal
# -> ~/.config/aic/env.json.enc を生成
# -> 32-byte ChaCha20-Poly1305 鍵を macOS Keychain (service=aic, account=env-key) に保存
rm ~/.config/aic/env.json
```

次回起動時は `env.json.enc` を Keychain 鍵で復号して使う。鍵を別マシンに持ち運べば
封印された `env.json.enc` だけコミットしておくことも可能。編集したい場合は
`aic env unseal` で平文を取り出す。

非 macOS（Linux 等）では `env.json` 平文 → 環境変数の順でフォールバック。
Keychain サポートは macOS のみ。

### 4. チャット開始

```sh
aic
aic> こんにちは
assistant> こんにちは。何かお手伝いできることはありますか？
aic> /exit
```

MCP サーバを設定していれば、ツール呼び出しも自動でルートされる:

```
aic> 今日の天気を教えて
· tool call: tools__get_weather({"location":"Tokyo"})
✓ tool ok:   tools__get_weather
assistant> 東京の天気は……
```

---

## REPL コマンド

| コマンド | 説明 |
|---|---|
| `/help` | 登録済みコマンドの一覧 |
| `/exit` | 終了（Ctrl-D でも可） |
| `/clear` | 会話履歴をリセット（モデル選択 / MCP 接続は維持） |
| `/model` | 設定済みグループ／モデル一覧。現在のモデルに `*` |
| `/model use <group>:<model>` | モデル切り替え（例: `/model use local:qwen2.5-coder:32b`） |
| `/config show` | 現在の Settings を YAML 表示（api_key / headers はマスク） |
| `/config setup` | 対話的にホーム `config.yaml` を生成 |

---

## CLI

```sh
aic                       # REPL 起動
aic --config <path>       # 明示的に config.yaml のパス指定
aic env seal              # env.json -> env.json.enc（macOS）
aic env unseal            # env.json.enc -> env.json（macOS）
```

### 環境変数

- `RUST_LOG` — `tracing` のフィルタ。詳細ログを出すには `RUST_LOG=aic=debug aic`
- `AIC_CONFIG_DIR` — `~/.config/aic` の代わりに使うディレクトリ
- `HOME` — 既定の config ディレクトリ解決に使用

---

## ファイル配置

| パス | 役割 |
|---|---|
| `~/.config/aic/config.yaml` | ホーム設定（既定） |
| `~/.config/aic/env.json` | 平文 secrets。**コミット禁止** |
| `~/.config/aic/env.json.enc` | 封印済み secrets。コミット可 |
| `~/.config/aic/history.txt` | rustyline の入力履歴 |
| `./aic.yaml` | 起動ディレクトリ側の上書き設定（トップレベル浅マージ） |

`aic.yaml`（プロジェクト側）はホーム設定にトップレベルキー単位で **完全置換** で
重ねる（要素単位のマージはしない）。

---

## 内部構造

```
src/
├── main.rs          起動・clap・tracing 初期化
├── agent.rs         1 ターンの chat ループ（assistant ↔ tool 再投入）
├── repl/            rustyline ループ・dispatch
├── commands/        /exit /clear /help /model /config（inventory 自動収集）
├── config/          Settings 型・YAML ロード・${VAR} 展開・secrets / Keychain
├── llm/             ChatRequest / SSE パーサ / ChatClient
└── mcp/             JSON-RPC・Streamable HTTP transport・ツールカタログ
```

新コマンドの追加は `src/commands/<name>.rs` を置き、`commands/mod.rs` に `mod`
行を 1 つ足すだけ。ディスパッチ／ヘルプ／登録ロジックには触らない（SPEC §9.3）。

---

## インストール

```sh
cargo install --git https://github.com/niaeashes/aic
```

利用者の `~/.cargo/bin/aic` に入る。Rust toolchain（rustc 1.70+）が必要。
macOS Keychain を使うので、現状 macOS 以外は `aic env seal/unseal` が動かない
（チャットと MCP は動く — 平文 `env.json` または環境変数で `${VAR}` を解決すれば良い）。

アップデート:

```sh
cargo install --git https://github.com/niaeashes/aic --force
```

## ライセンス

MIT License — 詳細は [LICENSE](LICENSE) を参照。
