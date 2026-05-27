# aic — 仕様書 (v0.1)

aichat にインスパイアされた、Rust 製のミニマルな対話型チャット CLI。

LLM は OpenAI 互換 API、ツールは Streamable HTTP の MCP サーバから取得する。

> `aic` はプロジェクトの仮名。バイナリ名・config ディレクトリ名はあとで自由に変更可。

---

## 1. スコープ

**やること**

- OpenAI 互換 `/chat/completions` への接続（SSE ストリーミングのみ）

- Streamable HTTP トランスポートの MCP サーバ接続（静的ヘッダ認証のみ）

- LLM の tool_call ↔ MCP `tools/call` の橋渡し（エージェントループ）

- 対話 REPL とスラッシュコマンド

- モデルグループによるモデル切り替え

- レイヤード config（ホーム + 起動ディレクトリ）

- macOS でのシークレット暗号化（`env.json.enc` + Keychain）

**やらないこと（非スコープ）**

- MCP の stdio トランスポート（HTTP のみ）

- MCP の OAuth フロー（Tailscale 内部のため静的ヘッダで十分）

- LLM の非ストリーミングパス（ストリーミング一本）

- RAG / serve モード / Web Playground（aichat の該当機能は持ち込まない）

- ネイティブ provider 実装（Claude/Gemini 専用パス。openai-compatible のみ）

---

## 2. 用語

| 用語 | 定義 |

|---|---|

| model group | 1 つの OpenAI 互換エンドポイント設定（base_url + api_key + headers）|

| model | あるグループが提供するモデル ID（例 `gpt-4o`、`qwen2.5-coder:32b`）|

| model ref | `<group>:<model>` 形式の参照。例 `local:qwen2.5-coder:32b` |

| MCP server | Streamable HTTP で接続する自己ホスト型ツールサーバ |

| secrets | `env.json` 由来の機密値マップ。config 内の `${VAR}` に展開される |

---

## 3. プロジェクト構成

各ファイルは 300 行以内を目安に保つ（LLM が 1 ファイルを丸ごと文脈に入れられ、借用エラーもローカルに収まる）。

```

src/

  main.rs              エントリ。引数解析 → REPL 起動 or env サブコマンド

  config/

    mod.rs             Settings、config.yaml のロードとレイヤマージ

    secrets.rs         env.json.enc の復号、Keychain アクセス、${VAR} 展開

  llm/

    mod.rs             ChatClient（リクエスト構築）

    stream.rs          SSE パース、delta と tool_call の蓄積

    types.rs           Message / Role / ToolCall / ChatRequest 等

  mcp/

    mod.rs             McpManager（接続束ね + ツールカタログ）

    transport.rs       Streamable HTTP トランスポート（POST + レスポンス処理）

    protocol.rs        JSON-RPC 型、initialize / tools-list / tools-call

  repl/

    mod.rs             REPL ループ、行編集（rustyline）

    context.rs         ReplContext

    dispatch.rs        コマンド登録の収集とディスパッチ

  commands/

    mod.rs             Command トレイト、Outcome、`mod` 宣言

    model.rs           /model

    config.rs          /config

    clear.rs           /clear

    exit.rs            /exit

  agent.rs             1 ターンの chat ↔ tool ループ

build.rs               （任意）commands/ をスキャンして mod 宣言を生成

```

---

## 4. 設定 (config)

### 4.1 ファイルの場所とレイヤリング

後者が前者を上書きする 2 層構成：

1. **ホーム（グローバル）**: `$AIC_CONFIG_DIR` || `~/.config/aic/config.yaml`

2. **起動ディレクトリ（プロジェクト）**: カレントの `./aic.yaml`（存在すれば）

マージ規則は **トップレベルキー単位の浅い置換**。プロジェクト側に存在するキーはホーム側の値を丸ごと置き換える（リストの要素マージはしない）。予測可能で実装が単純。

`/config setup` の書き込み先はホーム側。プロジェクト側ファイルは手動で作成する。

### 4.2 config.yaml スキーマ

```yaml

default_model: openai:gpt-4o-mini   # 起動時の model ref

model_groups:

  - name: openai

    base_url: https://api.openai.com/v1

    api_key: ${OPENAI_API_KEY}      # ${VAR} は secrets / 環境変数から展開

    headers: {}                      # 任意の追加ヘッダ

    models: [gpt-4o, gpt-4o-mini]

  - name: local

    base_url: http://ollama.ts.net:11434/v1

    models: [qwen2.5-coder:32b]      # api_key 省略可

mcp_servers:

  - name: tools

    url: https://mcp.internal.ts.net/mcp

    headers:

      Authorization: Bearer ${MCP_TOKEN}

    enabled: true

ui:

  stream: true

  history_size: 1000

  max_tool_iterations: 10            # エージェントループの上限

```

YAML のデシリアライズには `serde_yml`（`serde_yaml` はアーカイブ済みのためメンテ版フォークを使用）。

---

## 5. シークレット管理

config 内の `${VAR}` は **secrets マップ → プロセス環境変数** の順で解決する。

### 5.1 ファイル

- `env.json`（平文、`.gitignore` 対象）— 利用者が編集する

  ```json

  { "OPENAI_API_KEY": "sk-...", "MCP_TOKEN": "tskey-..." }

  ```

- `env.json.enc`（暗号化、コミット可）— 実行時に読まれる

両ファイルともホーム config ディレクトリに置く。

### 5.2 macOS フロー

1. `aic env seal` 実行時：

   - 32 byte のランダム鍵を生成し、**Keychain に保存**（`keyring` クレート / service=`aic`, account=`env-key`）。既に鍵があれば再利用。

   - `env.json` を ChaCha20-Poly1305 で暗号化し `env.json.enc` を書き出す。

2. `aic` 起動時：

   - Keychain から鍵を取得 → `env.json.enc` を復号 → secrets マップをメモリに保持。

   - 復号失敗（鍵なし等）は警告して環境変数フォールバックへ。

### 5.3 ファイル形式

`env.json.enc` = `base64( nonce(12B) || ciphertext_with_tag )`。

### 5.4 非 macOS フォールバック

Keychain を使わず、`env.json`（平文）があればそれを、なければプロセス環境変数のみを secrets ソースとする。

---

## 6. LLM 接続（openai-compatible）

- `POST {base_url}/chat/completions`、`stream: true` 固定。

- ヘッダ: `Authorization: Bearer {api_key}`（api_key がある場合）+ グループの `headers`。

- リクエストボディ: `model`, `messages`, `tools`（MCP 由来。空なら省略）, `stream: true`。

### 6.1 SSE パース（`llm/stream.rs`）

`reqwest` のバイトストリームを `eventsource-stream` で行分割し、`data:` 行の JSON を逐次処理：

- `choices[0].delta.content` → 端末へ逐次出力しつつ蓄積。

- `choices[0].delta.tool_calls` → **`index` ごとに蓄積**。`id` と `function.name` は先頭フラグメントに、`function.arguments`（JSON 文字列）は複数フラグメントに分割されて届くので連結する。← よくあるバグ源、明示的に実装すること。

- `data: [DONE]` で終了。

### 6.2 メッセージ型

OpenAI 準拠。`role` は `system` / `user` / `assistant` / `tool`。`assistant` は `tool_calls` を、`tool` は `tool_call_id` と `content` を持つ。

---

## 7. MCP 接続（Streamable HTTP）

JSON-RPC 2.0。MVP では POST のみ使用し、サーバ起点の GET ストリーム・サーバ起点リクエストは扱わない。

### 7.1 リクエスト共通

`POST {server.url}` に JSON-RPC を送る。ヘッダ：

- `Content-Type: application/json`

- `Accept: application/json, text/event-stream`

- config の `headers`（静的トークン等）

- `MCP-Protocol-Version: <実装が対象とする版>`（initialize 後の全リクエスト）

- `Mcp-Session-Id: <値>`（サーバが initialize 応答で発行した場合のみ）

### 7.2 レスポンス処理

`Content-Type` で分岐：

- `application/json` → 単一の JSON-RPC レスポンスとしてパース。

- `text/event-stream` → SSE をパースし、対応 `id` の JSON-RPC レスポンスを取り出す（§6.1 のパーサを再利用）。

### 7.3 ライフサイクル

接続ごとに 1 回：

1. `initialize` → 応答ヘッダの `Mcp-Session-Id` とプロトコル版を保持。

2. `notifications/initialized`（通知、応答なし）。

3. `tools/list` → ツール定義をキャッシュ。

ツール実行時：`tools/call`（`name` と `arguments`）。

### 7.4 ツールカタログとネーミング

複数サーバのツール名衝突を避けるため、LLM へ公開する名前は `<server>__<tool>` とする。逆引きは `HashMap<公開名, (server_idx, 実ツール名)>` で持ち、文字列パースに頼らない。

`McpManager` がこのカタログを保持し、`as_openai_tools()` で LLM リクエスト用の `tools` 配列を生成、`call(公開名, args)` でルーティングする。

---

## 8. エージェントループ（`agent.rs`）

`run_turn(ctx, user_input)`：

1. `user` メッセージを session に push。

2. ループ（最大 `ui.max_tool_iterations` 回）：

   1. リクエスト構築（messages + tools + current_model）。

   2. ストリーム実行。assistant テキストを逐次表示し、tool_calls を蓄積。

   3. `assistant` メッセージ（tool_calls 付き）を push。

   4. tool_calls が空 → 完了、ループ脱出。

   5. 各 tool_call を `McpManager.call` で実行 → 結果を `tool` メッセージとして push。

3. 上限到達時は警告を出して打ち切り（aichat の無制限再帰を避ける）。

---

## 9. REPL とコマンド体系

### 9.1 REPL ループ

- 行編集・履歴は `rustyline`。履歴ファイルは config ディレクトリに保存。

- 入力が `/` 始まり → コマンドディスパッチ。それ以外 → `run_turn` でチャット。

### 9.2 Command トレイト（`commands/mod.rs`）

```rust

#[async_trait::async_trait]

pub trait Command: Sync + Send {

    fn name(&self) -> &'static str;   // "model" → "/model" で起動

    fn help(&self) -> &'static str;   // 1 行ヘルプ

    async fn run(&self, args: &str, ctx: &mut ReplContext)

        -> anyhow::Result<Outcome>;   // args は "/<name> " を除いた残り

}

pub enum Outcome { Continue, Exit }

inventory::collect!(&'static dyn Command);

```

- 非同期トレイトをトレイトオブジェクトとして使うため `async-trait` を使用。

- `inventory` でコマンドをコンパイル時に自動収集。中央の `match` は不要。

### 9.3 新コマンドの追加手順

ファイルを 1 つ足すだけ：

```rust

// src/commands/clear.rs

use super::*;

struct Clear;

#[async_trait::async_trait]

impl Command for Clear {

    fn name(&self) -> &'static str { "clear" }

    fn help(&self) -> &'static str { "会話履歴を消去する" }

    async fn run(&self, _args: &str, ctx: &mut ReplContext)

        -> anyhow::Result<Outcome>

    {

        ctx.session.messages.clear();

        println!("会話履歴を消去しました");

        Ok(Outcome::Continue)

    }

}

inventory::submit! { &Clear as &dyn Command }

```

唯一残る中央編集は `commands/mod.rs` への `mod clear;` 1 行。これも嫌なら `build.rs` で `commands/*.rs` を走査して `mod` 宣言を自動生成できる（任意）。ディスパッチ・ヘルプ・登録ロジックは一切触らない。

### 9.4 ディスパッチ（`repl/dispatch.rs`）

入力を最初の空白で `name` と `args` に分割し、`inventory` 収集分から `name()` 一致を検索して `run` を呼ぶ。未知のコマンドはエラー表示して `Continue`。

---

## 10. コマンド仕様

| コマンド | 動作 |

|---|---|

| `/model use <group>:<model>` | カレントモデルを切り替え。`<group>` と `<model>` は **最初の `:` で 1 回だけ分割**（model 名自体が `:` を含むため `splitn(2, ':')`）。config に存在しない group/model はエラー。|

| `/config show` | マージ後の有効な設定を表示。`api_key`・`headers` 内のシークレットは `***` でリダクト。|

| `/config setup` | 対話ウィザード。model group / MCP サーバを質問し、ホーム側 `config.yaml` に書き出す。|

| `/clear` | session のメッセージ履歴を消去（モデル選択・MCP 接続は維持）。|

| `/exit` | `Outcome::Exit` を返して REPL 終了。|

補助として `/model`（引数なし）でグループとモデル一覧、`/help` で全コマンドのヘルプを出すと便利（任意）。

---

## 11. ランタイム状態モデル

グローバルな `Arc<RwLock<…>>` を置かない。単一スレッドの REPL ループで、状態は所有権と `&mut` で受け渡す。

```rust

struct Settings {        // 起動時ロード、実質不変

    default_model: ModelRef,

    model_groups: Vec<ModelGroup>,

    mcp_servers:  Vec<McpServerCfg>,

    ui: UiConfig,

    config_dir: PathBuf,

}

struct Session {         // &mut で更新

    messages: Vec<Message>,

}

struct ReplContext {

    settings: Settings,

    session:  Session,

    http:     reqwest::Client,

    mcp:      McpManager,      // ライブ接続 + ツールカタログ

    secrets:  Secrets,

    current_model: ModelRef,

}

```

コマンドと `agent.rs` は `&mut ReplContext` を受け取る。構造体にライフタイムを持たせない（保持フィールドは `&str` でなく `String`）。エラーは全面的に `anyhow`。

---

## 12. CLI サブコマンド（非 REPL）

`clap` で最小限：

| 呼び出し | 動作 |

|---|---|

| `aic` | REPL 起動 |

| `aic env seal` | `env.json` を暗号化 → `env.json.enc`、鍵を Keychain に保存 |

| `aic env unseal` | `env.json.enc` を復号して `env.json` に書き戻す（編集用、任意）|

| `aic --config <path>` | config パスの明示指定（任意）|

---

## 13. クレート一覧

| クレート | 用途 |

|---|---|

| `tokio` | 非同期ランタイム |

| `reqwest`（`stream`, `json`, `rustls-tls`）| HTTP クライアント |

| `eventsource-stream` | SSE パース（LLM・MCP 両方で再利用）|

| `serde`, `serde_json`, `serde_yml` | シリアライズ（config は YAML）|

| `clap`（`derive`）| 引数解析 |

| `rustyline` | REPL 行編集・履歴 |

| `anyhow` | エラー処理 |

| `async-trait` | Command トレイトのオブジェクト安全化 |

| `inventory` | コマンドのコンパイル時自動登録 |

| `chacha20poly1305` | env.json の AEAD 暗号化 |

| `keyring` | macOS Keychain アクセス |

| `base64`, `rand` | 暗号鍵・nonce・エンコード |

| `directories` または手書き | ホーム/config ディレクトリ解決 |

| `tracing`, `tracing-subscriber` | ロギング |

---

## 14. 実装フェーズ（vibe coding 用の段階分割）

各フェーズ末で `cargo check` が通ること。コンパイラをテストスイートとして使う。

1. **骨組み** — モジュール構成、`Settings`/`Session`/`ReplContext` 型、config.yaml のロードとマージ。`aic` 起動 → 空 REPL（`/exit` のみ動く）。

2. **LLM チャット** — openai-compatible への SSE 接続。ツールなしで `run_turn` が動く。1 モデル決め打ちでよい。

3. **コマンド基盤** — `Command` トレイト + `inventory` ディスパッチ。`/clear`・`/exit`・`/help` を実装。

4. **モデル切り替え** — `model_groups` の複数対応、`/model use`、`/model` 一覧、`/config show`。

5. **シークレット** — `env.json` / `env.json.enc` / Keychain、`aic env seal`、`${VAR}` 展開。

6. **MCP** — Streamable HTTP トランスポート、initialize → tools/list、`McpManager`、ツールカタログ。

7. **エージェントループ** — tool_call ↔ `tools/call` の橋渡し、`max_tool_iterations` 上限。

8. **仕上げ** — `/config setup` ウィザード、ストリーミング表示の整形、エラーメッセージ。

provider が増えないうちは §9 のマクロ的抽象は不要。コマンドは最初から `inventory` でよい（要件のため）。

---

## 15. 将来拡張（非スコープだが設計が許容するもの）

- ネイティブ provider（Claude/Gemini）— `ChatClient` をトレイト化すれば追加可能。

- MCP の stdio トランスポート — `mcp/transport.rs` に実装を 1 つ足す形。

- セッション永続化（JSON ファイル保存・復元）— `Session` を serde 対応にするだけ。

- MCP OAuth — `headers` を動的トークンプロバイダに差し替え。
