# aic — マイルストーン

SPEC.md v0.1 に対する実装計画。SPEC §14 のフェーズを基に、各マイルストーンに **成果物** / **受け入れ条件 (DoD)** / **依存** を明示する。

各マイルストーン末で必ず `cargo check` が通ること（SPEC §14 の原則）。テストは「コンパイラ＋手動 REPL 動作確認」を主、`cargo test` は型・パーサ等の純粋ロジックに限定する。

進捗管理: チェックボックスを更新しながら進める。完了済みは `[x]`、進行中は `[~]`、未着手は `[ ]`。

---

## M0. プロジェクト初期化

下地。SPEC §3 のディレクトリ構成と §13 の依存を物理的に置くだけ。

**成果物**

- [x] `Cargo.toml`（パッケージ名 `aic`, edition 2021, バイナリ 1 本）
- [x] SPEC §13 のクレートを `[dependencies]` に列挙（バージョンは現行安定版）
- [x] `.gitignore`（`/target`, `env.json`, `*.swp` 等）
- [x] `src/main.rs` — `fn main() { println!("aic"); }` だけのスタブ
- [x] SPEC §3 のディレクトリスケルトン（`src/config/`, `src/llm/`, `src/mcp/`, `src/repl/`, `src/commands/`, `src/agent.rs`）を空 `mod.rs` 付きで作成

**DoD**

- [x] `cargo build` が成功する
- [x] `cargo run` で `aic` と表示して終了する

---

## M1. 骨組み — config ロード + 空 REPL

SPEC §14-1。型と config 読み込み、REPL の最外殻まで。

**成果物**

- [x] `config/mod.rs` — `Settings`, `ModelGroup`, `ModelRef`, `McpServerCfg`, `UiConfig` 型（SPEC §4.2, §11）
- [x] `config/mod.rs` — YAML ロード（`serde_yml`）、ホーム + プロジェクトの 2 層浅マージ（SPEC §4.1）
- [x] `config/secrets.rs` — `Secrets` 型のスタブ（プロセス環境変数のみ返す版）と `${VAR}` 展開ヘルパ
- [x] `repl/context.rs` — `Session` / `ReplContext`（SPEC §11）
- [x] `repl/mod.rs` — `rustyline` ベースの最小ループ。`/exit` 入力で終了、それ以外はエコー
- [x] `main.rs` — config ロード → `ReplContext` 構築 → REPL 起動
- [x] `clap` で `aic` / `aic --config <path>` を受ける（`env seal/unseal` はサブコマンド名だけ予約、`unimplemented!()` でよい）

**DoD**

- [x] ホーム config が無くてもデフォルト設定で起動する
- [x] 起動ディレクトリの `aic.yaml` を置くとマージされる（手動確認）
- [x] `/exit` で正常終了する

**依存**: M0

---

## M2. LLM チャット — SSE ストリーミング

SPEC §14-2, §6。ツール無しでチャット往復ができる状態まで。

**成果物**

- [x] `llm/types.rs` — `Message`, `Role`, `ToolCall`, `ChatRequest`, `Tool`（SPEC §6.2）。`serde` 直列化
- [x] `llm/stream.rs` — `eventsource-stream` で SSE をパースし、`delta.content` と `delta.tool_calls`（index ごと連結）を蓄積するイテレータ
  - SPEC §6.1 の注意（id/name は先頭フラグメント、arguments は分割される）を実装コメントに明記
- [x] `llm/mod.rs` — `ChatClient::stream(request) -> impl Stream<Item = StreamEvent>` 相当
- [x] `agent.rs` — `run_turn(ctx, input)` 雛形。tools 空でストリーム→assistant メッセージ push のみ
- [x] REPL: `/` 始まり以外を `run_turn` に流す
- [x] `default_model` を起動時に読み、`current_model` に反映

**DoD**

- [x] 実エンドポイント（OpenAI もしくはローカル ollama）に対して、ユーザ入力→ストリーミング応答が端末に逐次表示される
- [x] 複数ターン続けると履歴 (`session.messages`) が積まれる
- [x] `delta.content` を含まないストリーム（純 tool_call レスポンス）でもパニックしない

**依存**: M1

---

## M3. コマンド基盤

SPEC §14-3, §9。`inventory` ベースの自動収集を最初に入れる（SPEC §14 末尾の指示）。

**成果物**

- [x] `commands/mod.rs` — `Command` トレイト、`Outcome` 列挙、`inventory::collect!`
- [x] `repl/dispatch.rs` — 先頭空白で `name`/`args` 分割、`inventory` 走査でディスパッチ、未知コマンドはエラー表示して継続
- [x] `commands/exit.rs` — `/exit` を `Command` 経由で再実装
- [x] `commands/clear.rs` — `/clear`
- [x] `commands/help.rs` — 登録済み全コマンドの `name()`/`help()` を一覧
- [x] REPL ループは `/` 始まりを `dispatch` に丸投げするだけにリファクタ

**DoD**

- [x] `/help` で `/exit /clear /help` が並ぶ
- [x] `/clear` 後に履歴がリセットされる（モデル選択は維持）
- [x] 新コマンド追加は「ファイル 1 つ + `commands/mod.rs` への `mod` 行 1 つ」で完結する

**依存**: M2

---

## M4. モデル切り替え

SPEC §14-4, §10。複数 group / モデル対応。

**成果物**

- [x] `ModelRef` の `splitn(2, ':')` 分割実装（モデル名に `:` を含むケースに対応、SPEC §10）
- [x] `commands/model.rs` — `/model`（一覧）、`/model use <group>:<model>`
- [x] `commands/config.rs` — `/config show`（API キー・ヘッダはマスク。SPEC §10）
- [x] `ChatClient` リクエスト構築で `current_model` のグループから `base_url` / `api_key` / `headers` を引く
- [x] `Settings` 検索ヘルパ: `group_by_name`, `model_exists(group, model)`

**DoD**

- [x] 複数グループを config に書いて `/model` で一覧表示できる
- [x] `/model use local:qwen2.5-coder:32b` のように `:` を含むモデル名が正しく扱える
- [x] `/config show` で API キーが `***` にリダクトされる
- [x] 未知の group/model は分かりやすいエラーで弾かれる

**依存**: M3

---

## M5. シークレット

SPEC §14-5, §5。macOS Keychain + ChaCha20-Poly1305。

**成果物**

- [x] `config/secrets.rs` — `Secrets` を「secrets マップ → 環境変数」フォールバックで解決（SPEC §5）
- [x] `${VAR}` 展開を config ロード後パスで全フィールドに適用（`api_key`, `headers` 値, `mcp_servers[].headers` 値）
- [x] ChaCha20-Poly1305 シール/アンシール（SPEC §5.3 の `base64(nonce(12) || ciphertext_with_tag)` 形式）
- [x] `keyring` で `service=aic, account=env-key` の 32B 鍵を取得/生成
- [x] `aic env seal` サブコマンド: `env.json` → `env.json.enc`、鍵を Keychain 保存（既存鍵があれば再利用）
- [x] `aic env unseal` サブコマンド: `env.json.enc` → `env.json`
- [x] 起動時に `env.json.enc` を試行 → 失敗で警告し環境変数フォールバック（SPEC §5.2, §5.4）

**DoD**

- [x] macOS で `aic env seal` 実行後、`env.json` を削除しても次回起動で復号が成功し API キーが解決される
- [x] Keychain 鍵を手動削除すると警告が出てフォールバックする
- [x] 非 macOS では `env.json` 平文 → 環境変数の順で解決する

**依存**: M4

---

## M6. MCP — Streamable HTTP

SPEC §14-6, §7。ツールカタログまで。エージェントループはまだ繋がない。

**成果物**

- [x] `mcp/protocol.rs` — JSON-RPC 2.0 型、`initialize` / `notifications/initialized` / `tools/list` / `tools/call` のリクエスト・レスポンス型
- [x] `mcp/transport.rs` — `POST` 1 つにつき `Content-Type` 分岐（`application/json` 単発、`text/event-stream` は薄い SSE データ抽出で再利用、SPEC §7.2）
  - `Accept: application/json, text/event-stream` ヘッダ
  - `MCP-Protocol-Version`, `Mcp-Session-Id` の管理（SPEC §7.1, §7.3）
- [x] `mcp/mod.rs` — `McpManager::connect_all(&Settings)` で initialize → tools/list を全 enabled サーバに対して実行
- [x] ツールカタログ: `HashMap<公開名 "<server>__<tool>", (server_idx, 実ツール名)>`（SPEC §7.4、文字列パースに頼らない）
- [x] `as_openai_tools()` — LLM リクエストの `tools` 配列を生成
- [x] `call(公開名, args)` — 該当サーバに `tools/call` を投げてレスポンスを返す
- [x] 起動時に MCP 接続 → 失敗はログに留め、起動自体は継続

**DoD**

- [ ] 実 MCP サーバ（Streamable HTTP、静的ヘッダ認証）に対して `initialize` → `tools/list` が成功する（**手動確認待ち** — 実サーバ準備後にチェック）
- [x] 起動ログにツール公開名一覧が出る
- [x] 単体テスト: SSE 形式と単発 JSON の両方で `tools/list` 応答をパースできる
- [x] 名前衝突する 2 サーバを設定しても `<server>__<tool>` で区別される

**依存**: M5（`${MCP_TOKEN}` 展開のため）

---

## M7. エージェントループ

SPEC §14-7, §8。ようやく「ツール呼べるチャット」になる。

**成果物**

- [x] `agent.rs::run_turn` 本実装（SPEC §8 のステップ 1–3）
  - assistant の `tool_calls` を蓄積→ session に push
  - 各 tool_call を `McpManager.call` で実行 → `tool` メッセージ（`tool_call_id` 必須）として push
  - tool_calls 空で完了
- [x] `ui.max_tool_iterations` の上限到達時に警告を出して打ち切り（SPEC §8 末尾、aichat 回避）
- [x] tool 結果のフォーマット: MCP の `content` 配列を `text` 連結して `tool` メッセージ `content` に（連結は M6 で `McpManager.call` 側に実装済み。agent はその文字列をそのまま `content` に詰める）
- [x] tool 実行エラー（ネットワーク・サーバエラー）はその tool 結果に「error: ...」を入れてループ継続（モデルに失敗を伝える）

**DoD**

- [ ] ツール 1 つを呼ばせるプロンプトで、LLM → tool 呼び出し → 結果再投入 → 最終回答 のフルラウンドが回る（**手動確認待ち** — 実 MCP サーバ + LLM 結合）
- [ ] ループ上限を 2 に下げて壊れたツールを使うと、警告付きで打ち切られて REPL に戻る（**手動確認待ち**）
- [x] ツール無効（`enabled: false`）のサーバは `as_openai_tools()` に出ない（M6 `connect_all` で disabled スキップ済み）

**依存**: M6

---

## M8. 仕上げ

SPEC §14-8。実用化のための整形と最後のコマンド。

**成果物**

- [x] `commands/config.rs` に `/config setup` ウィザード追加。model group / MCP サーバを対話的に質問し、**ホーム側** `config.yaml` に書き出す（SPEC §10, §4.1）
- [x] ストリーミング表示の整形（assistant ラベル、tool 呼び出し開始/終了のインジケータ）
- [x] エラーメッセージの体裁統一（ユーザに見せる文面を一通り見直し — `error:` / `warning:` プレフィックスに揃え）
- [x] `tracing-subscriber` の env フィルタ（`RUST_LOG=aic=debug` 等で詳細ログが出る、既定は `warn,aic=info`）
- [x] `rustyline` 履歴ファイルを config ディレクトリに保存（SPEC §9.1 — `config_dir/history.txt`、`history_size` 上限を反映）
- [x] README.md（基本的な起動手順、`aic env seal` の使い方、サンプル config）

**DoD**

- [ ] まっさらな環境で README どおりに 5 分以内で起動でき、最初のチャットが返る（**手動確認待ち**）
- [ ] `/config setup` を初回起動で実行すると config.yaml が生成され、再起動後に有効になる（**手動確認待ち**）
- [x] 非 0 終了は重大エラー時のみ（通常の REPL 終了は 0 — `/exit` / Ctrl-D / Ctrl-C いずれも 0 リターン）

**依存**: M7

---

## 横断的方針

- **ファイル長**: 各ファイル 300 行目安（SPEC §3）。超えそうなら分割する
- **エラー**: 全面的に `anyhow::Result`（SPEC §11）
- **状態**: グローバル `Arc<RwLock<_>>` 禁止。`&mut ReplContext` で受け渡す（SPEC §11）
- **ライフタイム**: 構造体に持たせない。保持は `String`（SPEC §11）
- **ストリーミング**: 非ストリーミングパスを書かない（SPEC §1）
- **provider 抽象化**: openai-compatible のみ。`ChatClient` をトレイト化しない（SPEC §1, §14）
