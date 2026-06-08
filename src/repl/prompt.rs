// repl/prompt — interactive input primitives.
//
// Shared by wizard-style commands like `/config setup`. By going through
// `BufRead`, tests can pass a `Cursor` (real stdin is also wrapped via
// `stdin().lock()`).
//
// Output is plain `print!` / `println!` to stdout: the prompts are part of the
// conversational surface and belong on the same stream the user is reading
// (not on tracing).
//
// Trailing newlines on input are trimmed. EOF (read_line returning 0) becomes
// Err — there's no meaningful way to continue a wizard once stdin is closed.

use std::io::{BufRead, Write};

use anyhow::{bail, Context, Result};

/// Read one line and trim. EOF → Err.
pub fn read_line<R: BufRead>(r: &mut R) -> Result<String> {
    let mut buf = String::new();
    let n = r.read_line(&mut buf).context("failed to read stdin")?;
    if n == 0 {
        bail!("stdin closed");
    }
    Ok(buf.trim().to_string())
}

/// Print a `  label [default]: ` prompt without a newline (reads from the same line).
pub fn print_prompt(label: &str, default: Option<&str>) {
    match default {
        Some(d) => print!("  {label} [{d}]: "),
        None => print!("  {label}: "),
    }
    std::io::stdout().flush().ok();
}

/// Required input. Empty answer → re-ask.
pub fn prompt_required<R: BufRead>(r: &mut R, label: &str) -> Result<String> {
    loop {
        print_prompt(label, None);
        let s = read_line(r)?;
        if !s.is_empty() {
            return Ok(s);
        }
        println!("error: cannot be empty");
    }
}

/// Optional input. Empty answer → `None`.
pub fn prompt_optional<R: BufRead>(r: &mut R, label: &str) -> Result<Option<String>> {
    print_prompt(label, None);
    let s = read_line(r)?;
    Ok(if s.is_empty() { None } else { Some(s) })
}

/// y/n prompt. Empty input picks `default`.
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
            _ => println!("error: please answer y / n"),
        }
    }
}

/// Integer input. Empty input picks `default`. Parse error → re-ask.
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
            Err(_) => println!("error: please answer with an integer"),
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
