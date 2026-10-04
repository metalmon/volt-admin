//! Automatic panel pairing.
//!
//! The gateway's pairing-code endpoints (`GET /admin/paircode`, `POST
//! /admin/paircode/new`) require the gateway admin token, which exists only
//! on the machine running `voltd` and must stay there. So the code is minted
//! THERE: the profile's pairing-code command runs on that machine (locally in
//! `Local` mode, over the same SSH credentials as the tunnel in `Remote` mode)
//! and prints a fresh one-time code bound to an administrator principal. Only
//! that one-time code travels to the panel, which redeems it through its own
//! public `POST /api/pair` exactly as a person typing it would. The admin
//! token never leaves the host and is never seen by this app.
//!
//! The code is consumed only when the panel holds no session token yet; an
//! unused code is simply replaced by the next mint.

use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use tokio::process::Command;

use crate::connection::{ConnMode, Profile};
use crate::tunnel::{self, Lang};

/// Principal the code is bound to when the profile leaves it empty. The pilot
/// configs declare `[[authz.principals]] id = "admin"` with the operator
/// profile; a code without a principal is useless under enforced authz.
pub const DEFAULT_PRINCIPAL: &str = "admin";

/// How long the pairing-code command may take (ssh handshake included).
const EXEC_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A pairing script waiting for the panel origin to finish loading.
pub struct Pending {
    pub origin: String,
    pub script: String,
}

/// Tauri-managed state: the script to run on the next page load of `origin`.
#[derive(Default)]
pub struct PendingPair(pub Mutex<Option<Pending>>);

impl PendingPair {
    pub fn set(&self, origin: String, script: String) {
        *self.0.lock().expect("PendingPair mutex poisoned") = Some(Pending { origin, script });
    }

    /// Take the pending script if `url` belongs to its origin.
    pub fn take_for(&self, url: &tauri::Url) -> Option<String> {
        let mut guard = self.0.lock().expect("PendingPair mutex poisoned");
        let matches = guard.as_ref().is_some_and(|p| same_origin(url, &p.origin));
        if matches {
            guard.take().map(|p| p.script)
        } else {
            None
        }
    }
}

/// Principal id for the minted code.
pub fn principal(p: &Profile) -> String {
    match p.principal.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => DEFAULT_PRINCIPAL.to_string(),
    }
}

/// The command that prints a fresh pairing code on the machine with voltd.
pub fn paircode_command(p: &Profile) -> String {
    match p.paircode_command.as_deref().map(str::trim) {
        Some(cmd) if !cmd.is_empty() => cmd
            .replace("{panel_port}", &p.panel_port.to_string())
            .replace("{principal}", &principal(p)),
        _ => format!(
            "voltd gateway get-paircode --new --port {} --principal {}",
            p.panel_port,
            principal(p)
        ),
    }
}

/// Mint a pairing code on the gateway machine. `embedded` = the tunnel runs
/// on the built-in SSH client, which has no exec path; the caller then gets a
/// plain explanation instead of a code.
pub async fn mint_code(
    p: &Profile,
    password: Option<&str>,
    embedded: bool,
    lang: Lang,
) -> Result<String, String> {
    let command = paircode_command(p);
    let output = match p.mode {
        ConnMode::Local => run_local(&command).await?,
        ConnMode::Remote if embedded => {
            return Err(match lang {
                Lang::Ru => {
                    "автосопряжение требует системный ssh (встроенный клиент его не поддерживает)"
                        .into()
                }
                Lang::En => {
                    "auto-pairing needs the system ssh (the built-in client cannot run commands)"
                        .into()
                }
            });
        }
        ConnMode::Remote => {
            tunnel::run_ssh_command(p, password, &command, EXEC_TIMEOUT, lang).await?
        }
    };
    extract_pair_code(&output).ok_or_else(|| {
        let tail = tail_of(&output, 160);
        match lang {
            Lang::Ru => format!("команда кода сопряжения не вернула код: {tail}"),
            Lang::En => format!("the pairing-code command printed no code: {tail}"),
        }
    })
}

/// Run `command` through the local shell, windowless, with a timeout; returns
/// stdout+stderr combined (voltd prints the code on stderr).
async fn run_local(command: &str) -> Result<String, String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c.creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    collect_output(child, EXEC_TIMEOUT).await
}

/// Wait for a spawned command (bounded by `timeout`) and return its combined
/// output; a non-zero exit is an error carrying the output tail.
pub async fn collect_output(
    child: tokio::process::Child,
    timeout: Duration,
) -> Result<String, String> {
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("timed out after {} s", timeout.as_secs()))?
        .map_err(|e| format!("wait: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!("exit {}: {}", out.status, tail_of(&text, 200)))
    }
}

fn tail_of(s: &str, max: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= max {
        t
    } else {
        let start = t.chars().count() - max;
        t.chars().skip(start).collect()
    }
}

/// First run of 24..=64 ASCII alphanumerics delimited by non-alphanumerics:
/// voltd prints a 32-character code (on stderr, after a message), and the
/// gateway's JSON reply carries it as `"pairing_code":"..."`.
pub fn extract_pair_code(output: &str) -> Option<String> {
    output
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|run| (24..=64).contains(&run.len()))
        .map(str::to_string)
}

/// Escape `s` for use inside a single-quoted JavaScript string literal.
pub fn js_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '<' => out.push_str("\\x3c"),
            '>' => out.push_str("\\x3e"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The script evaluated in the panel once it has loaded: if the panel has no
/// session yet, redeem the one-time code through the panel's own public
/// pairing endpoint and store the session the way the panel does itself
/// (`zeroclaw_token` in localStorage, see the panel's web/src/lib/auth.ts),
/// then reload so the panel picks it up. Any failure leaves the panel's
/// normal pairing prompt in place.
pub fn pair_script(code: &str) -> String {
    format!(
        "(function(){{try{{var K='zeroclaw_token';if(localStorage.getItem(K))return;\
fetch('/api/pair',{{method:'POST',headers:{{'Content-Type':'application/json'}},\
body:JSON.stringify({{code:'{code}',device_name:'Volt Admin',device_type:'browser'}})}})\
.then(function(r){{return r.ok?r.json():Promise.reject(new Error('pair '+r.status));}})\
.then(function(d){{localStorage.setItem(K,d.token);location.reload();}})\
.catch(function(e){{console.warn('[volt-admin] auto-pair skipped:',e&&e.message);}});}}catch(e){{}}}})();",
        code = js_str(code)
    )
}

/// A small dismissable banner explaining why auto-pairing did not happen;
/// the panel's own pairing prompt stays usable underneath.
pub fn warning_script(text: &str) -> String {
    format!(
        "(function(){{try{{var b=document.createElement('div');b.textContent='{text}';\
b.style.cssText='position:fixed;left:0;right:0;top:0;z-index:99999;padding:8px 40px 8px 12px;\
background:#b45309;color:#fff;font:13px system-ui,sans-serif;';\
var x=document.createElement('button');x.textContent='\\u00d7';\
x.style.cssText='position:absolute;right:8px;top:4px;background:none;border:0;color:#fff;font-size:18px;cursor:pointer;';\
x.onclick=function(){{b.remove();}};b.appendChild(x);document.body.appendChild(b);}}catch(e){{}}}})();",
        text = js_str(text)
    )
}

/// Scheme, host and port equal; path and query ignored.
pub fn same_origin(url: &tauri::Url, origin: &str) -> bool {
    tauri::Url::parse(origin).is_ok_and(|o| {
        o.scheme() == url.scheme()
            && o.host_str() == url.host_str()
            && o.port_or_known_default() == url.port_or_known_default()
    })
}

/// `scheme://host:port` of `base_url` (what `same_origin` compares against).
pub fn origin_of(base_url: &str) -> Option<String> {
    let u = tauri::Url::parse(base_url).ok()?;
    Some(format!(
        "{}://{}:{}",
        u.scheme(),
        u.host_str()?,
        u.port_or_known_default()?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{AuthMethod, ConnMode};

    fn profile() -> Profile {
        Profile {
            id: "a".into(),
            name: "srv".into(),
            mode: ConnMode::Remote,
            host: "h".into(),
            user: "u".into(),
            port: 22,
            panel_port: 42617,
            auth: AuthMethod::Agent,
            key_path: None,
            principal: None,
            paircode_command: None,
        }
    }

    #[test]
    fn default_command_targets_the_panel_port_and_admin_principal() {
        assert_eq!(
            paircode_command(&profile()),
            "voltd gateway get-paircode --new --port 42617 --principal admin"
        );
        let mut p = profile();
        p.principal = Some(" ops ".into());
        p.paircode_command = Some(
            "docker exec voltd voltd gateway get-paircode --new --port {panel_port} --principal {principal}"
                .into(),
        );
        assert_eq!(
            paircode_command(&p),
            "docker exec voltd voltd gateway get-paircode --new --port 42617 --principal ops"
        );
    }

    #[test]
    fn extracts_the_code_from_cli_output_and_from_json() {
        let cli = "✅ New pairing code (principal admin): lW6VuAuY8362LL0Udf9CmJsZXZN6YFkE\nExpires in 10 minutes";
        assert_eq!(
            extract_pair_code(cli).as_deref(),
            Some("lW6VuAuY8362LL0Udf9CmJsZXZN6YFkE")
        );
        let json = r#"{"message":"New pairing code generated","pairing_code":"FKJSbE2XDO8miL15mx3Y8EhDQq5Baiw0","pairing_required":true}"#;
        assert_eq!(
            extract_pair_code(json).as_deref(),
            Some("FKJSbE2XDO8miL15mx3Y8EhDQq5Baiw0")
        );
        assert!(extract_pair_code("error: no gateway admin token at /x/y").is_none());
    }

    #[test]
    fn js_string_escaping_neutralises_quotes_and_tags() {
        assert_eq!(js_str("a'b\"c\\d"), "a\\'b\\\"c\\\\d");
        assert_eq!(js_str("</script>"), "\\x3c/script\\x3e");
        assert!(pair_script("AB'CD").contains("code:'AB\\'CD'"));
    }

    #[test]
    fn origin_comparison_ignores_the_path() {
        let a = tauri::Url::parse("http://127.0.0.1:52610/sessions?x=1").unwrap();
        assert!(same_origin(&a, "http://127.0.0.1:52610"));
        assert!(!same_origin(&a, "http://127.0.0.1:52611"));
        assert!(!same_origin(&a, "https://127.0.0.1:52610"));
        assert_eq!(
            origin_of("http://127.0.0.1:52610/").as_deref(),
            Some("http://127.0.0.1:52610")
        );
    }

    #[test]
    fn pending_pair_is_taken_once_for_its_origin_only() {
        let state = PendingPair::default();
        state.set("http://127.0.0.1:1".into(), "x".into());
        let other = tauri::Url::parse("http://127.0.0.1:2/").unwrap();
        assert!(state.take_for(&other).is_none());
        let mine = tauri::Url::parse("http://127.0.0.1:1/").unwrap();
        assert_eq!(state.take_for(&mine).as_deref(), Some("x"));
        assert!(state.take_for(&mine).is_none());
    }
}
