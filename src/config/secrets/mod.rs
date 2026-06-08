// secrets — `${VAR}` 解決 + env.json / env.json.enc の取り扱い（SPEC §5）。
//
// 解決順は SPEC §5 の通り「secrets マップ → プロセス環境変数」。
// secrets マップは起動時に以下のフォールバックチェーンで構築する:
//
//   1. (macOS のみ) `<config_dir>/env.json.enc` を Keychain 鍵で復号
//   2. `<config_dir>/env.json`（平文、ローカル編集用）
//   3. 環境変数のみ
//
// 各段で失敗しても警告にとどめて次のフォールバックへ。起動を止めない（SPEC §5.2 末尾）。
//
// サブモジュール構成（M9 でリファクタ）:
//   crypto.rs    — ChaCha20-Poly1305 AEAD（純関数）
//   keychain.rs  — macOS Keychain 鍵管理（非 macOS は実行時 bail）
//   storage.rs   — env.json / env.json.enc のファイル I/O
//   cli.rs       — `aic env seal/unseal` の本体（main.rs から呼ぶ）
//
// 公開境界は `Secrets` 型 + `seal` / `unseal` の 3 シンボルのみ。
// cli の `seal` / `unseal` は `pub use` で再公開し、外部 import パスを
// `crate::config::secrets::{Secrets, seal, unseal}` のまま保つ。

mod cli;
mod crypto;
mod keychain;
mod storage;

pub use cli::{seal, unseal};

use std::collections::HashMap;
use std::path::Path;

// サブモジュールから参照される定数。`Secrets::load` で `config_dir.join(...)` するため
// mod.rs に置く。cli.rs からは `super::{ENV_JSON, ENV_JSON_ENC}` で引く。
pub(super) const ENV_JSON: &str = "env.json";
pub(super) const ENV_JSON_ENC: &str = "env.json.enc";

/// `${VAR}` 解決の単一窓口。
///
/// 解決順は SPEC §5 の通り「secrets マップ → プロセス環境変数」。
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    map: HashMap<String, String>,
}

impl Secrets {
    /// 環境変数だけを参照する空 Secrets。テストや非常用フォールバック。
    #[allow(dead_code)]
    pub fn from_env_only() -> Self {
        Self::default()
    }

    /// 起動時のロード経路。`config_dir` から env.json(.enc) を解決する。
    ///
    /// どの段階の失敗も `eprintln!` で警告にとどめて次のフォールバックへ。
    /// 戻り値の `Secrets` は最終的に環境変数フォールバックを必ず持つので、
    /// 呼び出し側は失敗を意識する必要が無い（SPEC §5.2 末尾）。
    ///
    /// 非 macOS で `env.json.enc` が存在する場合、`storage::decrypt_env_file` 内の
    /// `keychain::load_key()` が即 bail する。そのエラーがフォールバック警告に
    /// 含まれて出力されるため、ここに cfg gate は不要。
    pub fn load(config_dir: &Path) -> Self {
        let enc_path = config_dir.join(ENV_JSON_ENC);
        let plain_path = config_dir.join(ENV_JSON);

        if enc_path.exists() {
            match storage::decrypt_env_file(&enc_path) {
                Ok(map) => return Self { map },
                Err(e) => {
                    eprintln!(
                        "warning: {} の復号に失敗（{e:#}）。env.json / 環境変数にフォールバックします。",
                        enc_path.display()
                    );
                }
            }
        }

        if plain_path.exists() {
            match storage::read_env_file(&plain_path) {
                Ok(map) => return Self { map },
                Err(e) => {
                    eprintln!(
                        "warning: {} の読み込みに失敗（{e:#}）。環境変数のみで起動します。",
                        plain_path.display()
                    );
                }
            }
        }
        Self::default()
    }

    /// `${VAR}` 1 個分のキー解決。secrets マップ優先、環境変数フォールバック。
    pub fn get(&self, key: &str) -> Option<String> {
        self.map
            .get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
    }

    /// 文字列中の `${VAR}` を全部展開。未解決のものはそのまま残す
    /// （config がプレースホルダのまま動くと API 呼び出し時に分かりやすく死ぬ方が良い）。
    ///
    /// `$$` でリテラル `$` をエスケープする。`$` 単独や `${` の不一致はそのまま残す。
    pub fn expand(&self, input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut chars = input.char_indices().peekable();
        while let Some((_, c)) = chars.next() {
            if c != '$' {
                out.push(c);
                continue;
            }
            match chars.peek().map(|&(_, ch)| ch) {
                Some('$') => {
                    // $$ → $（リテラルエスケープ）
                    chars.next();
                    out.push('$');
                }
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    while let Some(&(_, ch)) = chars.peek() {
                        chars.next();
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        name.push(ch);
                    }
                    if !closed {
                        // `${...` で閉じていない → そのまま戻す
                        out.push('$');
                        out.push('{');
                        out.push_str(&name);
                        continue;
                    }
                    match self.get(&name) {
                        Some(v) => out.push_str(&v),
                        None => {
                            // 未解決はプレースホルダのまま残す
                            out.push_str("${");
                            out.push_str(&name);
                            out.push('}');
                        }
                    }
                }
                _ => out.push('$'),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_replaces_known_vars_from_map() {
        let mut s = Secrets::default();
        s.map.insert("FOO".into(), "bar".into());
        assert_eq!(s.expand("Bearer ${FOO}"), "Bearer bar");
    }

    #[test]
    fn expand_keeps_unknown_placeholder() {
        let s = Secrets::default();
        // 環境変数にも無い前提のキー名
        assert_eq!(
            s.expand("x=${__AIC_TEST_DEFINITELY_UNSET}"),
            "x=${__AIC_TEST_DEFINITELY_UNSET}"
        );
    }

    #[test]
    fn expand_handles_double_dollar_escape() {
        let s = Secrets::default();
        assert_eq!(s.expand("price: $$5"), "price: $5");
    }

    #[test]
    fn expand_passes_through_unclosed_brace() {
        let s = Secrets::default();
        assert_eq!(s.expand("${oops"), "${oops");
    }

    #[test]
    fn map_takes_priority_over_env() {
        let mut s = Secrets::default();
        // 環境変数を実際にいじると並行テストで不安定になるため、map のみ検証
        s.map.insert("HOME".into(), "from-map".into());
        assert_eq!(s.expand("${HOME}"), "from-map");
    }
}
