// /doctor — environment health checks for aic.
//
// Runs a small set of synchronous probes against the current ReplContext and
// system, then prints a per-check report with `✓ / ⚠ / ✗` markers and
// "→ next step" hints when something is off.
//
// Design notes:
//   - Checks are plain `fn(&ReplContext) -> CheckResult` entries in a static
//     slice. If we ever cross ~10 checks, switch to a trait + inventory pattern
//     mirroring the Command system. For now, a flat list is much easier to read.
//   - All checks are sync: every probe is either a struct field read or an
//     immediate syscall (stat, keyring lookup). MCP is *not* re-pinged — we
//     report the connection state captured at startup.
//   - Output is stable, ASCII-aligned, plain-text. No color (rules out
//     surprise breakage with non-TTY pipes; the markers carry enough signal).

use anyhow::Result;
use async_trait::async_trait;

use super::{Command, Outcome};
use crate::repl::context::ReplContext;

// ---------------------------------------------------------------------------
// Result shape
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckLevel {
    Ok,
    Warn,
    Fail,
}

impl CheckLevel {
    fn marker(self) -> &'static str {
        match self {
            CheckLevel::Ok => "✓",
            CheckLevel::Warn => "⚠",
            CheckLevel::Fail => "✗",
        }
    }
}

#[derive(Debug, Clone)]
struct CheckResult {
    level: CheckLevel,
    /// Single-line summary printed right after the marker.
    summary: String,
    /// Extra explanatory lines, indented under the summary.
    details: Vec<String>,
    /// "→ ..." style suggestions, also indented.
    hints: Vec<String>,
}

impl CheckResult {
    fn ok(summary: impl Into<String>) -> Self {
        Self { level: CheckLevel::Ok, summary: summary.into(), details: vec![], hints: vec![] }
    }
    fn warn(summary: impl Into<String>) -> Self {
        Self { level: CheckLevel::Warn, summary: summary.into(), details: vec![], hints: vec![] }
    }
    fn fail(summary: impl Into<String>) -> Self {
        Self { level: CheckLevel::Fail, summary: summary.into(), details: vec![], hints: vec![] }
    }
    fn detail(mut self, line: impl Into<String>) -> Self {
        self.details.push(line.into());
        self
    }
    fn hint(mut self, line: impl Into<String>) -> Self {
        self.hints.push(line.into());
        self
    }
}

type CheckFn = fn(&ReplContext) -> CheckResult;

const CHECKS: &[(&str, CheckFn)] = &[
    ("config", check_config),
    ("default", check_default_model),
    ("secrets", check_secrets),
    ("keyring", check_keyring),
    ("mcp", check_mcp),
];

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

fn check_config(ctx: &ReplContext) -> CheckResult {
    let dir = &ctx.settings.config_dir;
    if dir.as_os_str().is_empty() {
        return CheckResult::warn("no config_dir resolved")
            .hint("Set AIC_CONFIG_DIR or ensure $HOME is set.");
    }
    let yaml_path = dir.join("config.yaml");
    let groups = ctx.settings.model_groups.len();
    let mcp = ctx.settings.mcp_servers.len();
    let mut r = if yaml_path.exists() {
        CheckResult::ok(format!("{}", yaml_path.display()))
    } else {
        CheckResult::warn(format!("{} not found", yaml_path.display()))
            .hint("Run /config setup to generate one.")
    };
    r = r.detail(format!(
        "{} model group{}, {} mcp server{}",
        groups,
        if groups == 1 { "" } else { "s" },
        mcp,
        if mcp == 1 { "" } else { "s" },
    ));
    if groups == 0 {
        r.level = CheckLevel::Warn;
        r.hints.push("Run /config setup to add at least one model group.".into());
    }
    r
}

fn check_default_model(ctx: &ReplContext) -> CheckResult {
    match &ctx.current_model {
        Some(m) => CheckResult::ok(format!("{} → {}", m.label(), m.endpoint_url)),
        None => match &ctx.settings.default_model {
            Some(r) => CheckResult::fail(format!(
                "default_model `{r}` could not be resolved at startup"
            ))
            .hint("Run /model to see configured models.")
            .hint("Then /model use <group>:<model> to pick one."),
            None => CheckResult::warn("no model selected")
                .hint("Set default_model in config.yaml, or run /model use <group>:<model>."),
        },
    }
}

fn check_secrets(ctx: &ReplContext) -> CheckResult {
    use crate::config::secrets;
    let dir = &ctx.settings.config_dir;
    let has_enc = secrets::enc_path_exists(dir);
    let has_plain = secrets::plain_path_exists(dir);

    match (has_enc, has_plain) {
        (true, _) => CheckResult::ok("env.json.enc present")
            .detail("Decrypted via system keyring at startup."),
        (false, true) => CheckResult::warn("env.json plaintext in use")
            .hint("Consider `aic env seal` to store secrets sealed by the system keyring."),
        (false, false) => CheckResult::warn("no env.json or env.json.enc")
            .detail("`${VAR}` placeholders will resolve via process environment variables only.")
            .hint("Create env.json with your secrets, or set them in the environment."),
    }
}

fn check_keyring(_ctx: &ReplContext) -> CheckResult {
    use crate::config::secrets;
    match secrets::keyring_status() {
        Ok(true) => CheckResult::ok("reachable; key present"),
        Ok(false) => CheckResult::warn("reachable; no key stored yet")
            .hint("Run `aic env seal` after creating env.json to provision the key."),
        Err(e) => {
            // The error string from keychain.rs already includes "→ ..." hint lines
            // (Linux Secret Service setup steps etc.). Split them into the
            // structured CheckResult so the report renders consistently.
            let mut r = CheckResult::fail("backend unreachable");
            for line in e.to_string().lines() {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix("→ ") {
                    r.hints.push(rest.to_string());
                } else if !line.trim().is_empty() {
                    r.details.push(line.to_string());
                }
            }
            r
        }
    }
}

fn check_mcp(ctx: &ReplContext) -> CheckResult {
    let configured = ctx.settings.mcp_servers.iter().filter(|s| s.enabled).count();
    let public = ctx.mcp.public_tool_names().len();
    let connected = ctx.mcp.connected_server_count();

    if configured == 0 {
        return CheckResult::ok("no MCP servers configured");
    }
    if connected == 0 {
        return CheckResult::fail(format!(
            "{} server{} configured but none connected",
            configured,
            if configured == 1 { "" } else { "s" }
        ))
        .hint("Check server URL / auth headers, then restart aic to retry.");
    }
    let mut r = CheckResult::ok(format!(
        "{}/{} server{} connected — {} public tool{}",
        connected,
        configured,
        if configured == 1 { "" } else { "s" },
        public,
        if public == 1 { "" } else { "s" },
    ));
    if connected < configured {
        r.level = CheckLevel::Warn;
        r.hints.push(
            "Some MCP servers failed at startup. See the startup log; restart aic to retry.".into()
        );
    }
    r
}

// ---------------------------------------------------------------------------
// Report rendering
// ---------------------------------------------------------------------------

fn print_report(results: &[(&str, CheckResult)]) {
    println!("aic doctor");
    println!("──────────");

    let width = results.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let indent = " ".repeat(width + 3); // name + 1 space + marker(1) + 1 space

    let (mut ok, mut warn, mut fail) = (0, 0, 0);
    for (name, r) in results {
        println!(
            "{:<width$} {} {}",
            name,
            r.level.marker(),
            r.summary,
            width = width
        );
        for d in &r.details {
            println!("{indent}{d}");
        }
        for h in &r.hints {
            println!("{indent}→ {h}");
        }
        match r.level {
            CheckLevel::Ok => ok += 1,
            CheckLevel::Warn => warn += 1,
            CheckLevel::Fail => fail += 1,
        }
    }
    println!();
    println!("{ok} OK, {warn} warning, {fail} failure");
}

fn run_all_checks(ctx: &ReplContext) -> Vec<(&'static str, CheckResult)> {
    CHECKS.iter().map(|(n, f)| (*n, f(ctx))).collect()
}

// ---------------------------------------------------------------------------
// Command wiring
// ---------------------------------------------------------------------------

struct Doctor;

#[async_trait]
impl Command for Doctor {
    fn name(&self) -> &'static str {
        "doctor"
    }

    fn help(&self) -> &'static str {
        "Run environment checks and show suggestions"
    }

    async fn run(&self, _args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let results = run_all_checks(ctx);
        print_report(&results);
        Ok(Outcome::Continue)
    }
}

inventory::submit! { &Doctor as &dyn Command }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_strings_are_distinct() {
        assert_ne!(CheckLevel::Ok.marker(), CheckLevel::Warn.marker());
        assert_ne!(CheckLevel::Warn.marker(), CheckLevel::Fail.marker());
    }

    #[test]
    fn check_result_builders_chain() {
        let r = CheckResult::warn("hi").detail("d1").hint("h1").hint("h2");
        assert_eq!(r.level, CheckLevel::Warn);
        assert_eq!(r.summary, "hi");
        assert_eq!(r.details, vec!["d1".to_string()]);
        assert_eq!(r.hints, vec!["h1".to_string(), "h2".to_string()]);
    }
}
