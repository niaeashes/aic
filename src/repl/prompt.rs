// repl/prompt — 対話的入力プリミティブ。
//
// `/config setup` 等のウィザード系コマンドで共有する。`BufRead` 抽象化により
// 単体テストでは `Cursor` を渡せる（real stdin にも `stdin().lock()` で噛む）。
//
// 出力は print! / println! で stdout に直書きする：ユーザに表示するプロンプト
// 自体は会話本流と同じ stdout が自然（tracing には流さない）。
//
// 入力末尾の改行は trim 済み。EOF（read_line が 0 を返す）は Err にする — ウィザード
// 中に stdin が閉じられた場合は処理を続行できないため。

use std::io::{BufRead, Write};

use anyhow::{bail, Context, Result};

/// 1 行読んで trim する。EOF は `Err`。
pub fn read_line<R: BufRead>(r: &mut R) -> Result<String> {
    let mut buf = String::new();
    let n = r.read_line(&mut buf).context("stdin 読み取りに失敗")?;
    if n == 0 {
        bail!("stdin が閉じられました");
    }
    Ok(buf.trim().to_string())
}

/// `  label [default]: ` 形式のプロンプトを出す。改行はしない（同じ行で入力を受ける）。
pub fn print_prompt(label: &str, default: Option<&str>) {
    match default {
        Some(d) => print!("  {label} [{d}]: "),
        None => print!("  {label}: "),
    }
    std::io::stdout().flush().ok();
}

/// 必須入力。空文字なら再質問。
pub fn prompt_required<R: BufRead>(r: &mut R, label: &str) -> Result<String> {
    loop {
        print_prompt(label, None);
        let s = read_line(r)?;
        if !s.is_empty() {
            return Ok(s);
        }
        println!("error: 空にできません");
    }
}

/// 任意入力。空文字は `None`。
pub fn prompt_optional<R: BufRead>(r: &mut R, label: &str) -> Result<Option<String>> {
    print_prompt(label, None);
    let s = read_line(r)?;
    Ok(if s.is_empty() { None } else { Some(s) })
}

/// y/n プロンプト。空入力で `default` を返す。
pub fn prompt_bool<R: BufRead>(r: &mut R, label: &str, default: bool) -> Result<bool> {
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

/// 整数入力。空入力で `default` を返す。パース失敗で再質問。
pub fn prompt_u32<R: BufRead>(r: &mut R, label: &str, default: u32) -> Result<u32> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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

    #[test]
    fn read_line_errors_on_eof() {
        let mut cur = Cursor::new(b"".as_slice());
        assert!(read_line(&mut cur).is_err());
    }

    #[test]
    fn prompt_optional_empty_returns_none() {
        let mut cur = Cursor::new(b"\n".as_slice());
        assert!(prompt_optional(&mut cur, "x").unwrap().is_none());
        let mut cur = Cursor::new(b"value\n".as_slice());
        assert_eq!(prompt_optional(&mut cur, "x").unwrap().as_deref(), Some("value"));
    }
}
