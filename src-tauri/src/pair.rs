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
//! No code is minted unless the panel needs one. On every normal page load
//! of the panel origin, `PROBE_SCRIPT` checks for a stored session token; if
//! there is none it navigates to `/?volt_pair=1`. That marker is the
//! panel-side request for a code: the app then runs the pairing-code command
//! and evaluates `pair_script` in the page, which redeems the code and drops
//! back to `/`. A panel that already has a session never triggers a mint.
//!
//! The panel's origin is `http://127.0.0.1:<local port>`; the tunnel keeps
//! that port stable per profile (see `crate::tunnel::preferred_port`), so the
//! session stored by the panel survives reconnects and app restarts.

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

/// Everything needed to mint a pairing code for the connection currently
/// shown in the webview.
///
/// `password` is the SSH password typed at connect time (Password auth only).
/// It stays in memory for as long as the connection is up because the code is
/// minted on demand — only when the panel reports it has no session, which
/// can happen any time after connect — and minting in `Remote` mode is an
/// `ssh` exec with the same credentials as the tunnel. It never reaches disk:
/// `connect` replaces it, `disconnect` clears it, and `Profile` has no
/// password field by design.
#[derive(Clone)]
pub struct PairContext {
    /// `scheme://host:port` of the panel; page loads elsewhere are ignored.
    pub origin: String,
    pub profile: Profile,
    pub password: Option<String>,
    /// The tunnel runs on the built-in SSH client (no exec path).
    pub embedded: bool,
    pub lang: Lang,
}

/// Tauri-managed state: the context of the active connection, if any.
#[derive(Default)]
pub struct PairState(pub Mutex<Option<PairContext>>);

impl PairState {
    pub fn set(&self, ctx: PairContext) {
        *self.0.lock().expect("PairState mutex poisoned") = Some(ctx);
    }

    pub fn clear(&self) {
        *self.0.lock().expect("PairState mutex poisoned") = None;
    }

    /// The active context if `url` belongs to its origin.
    pub fn for_url(&self, url: &tauri::Url) -> Option<PairContext> {
        self.0
            .lock()
            .expect("PairState mutex poisoned")
            .as_ref()
            .filter(|c| same_origin(url, &c.origin))
            .cloned()
    }
}

/// Does a page URL carry the pairing request marker (`/?volt_pair=1`)?
pub fn is_pair_request(url: &tauri::Url) -> bool {
    url.query_pairs().any(|(k, v)| k == "volt_pair" && v == "1")
}

/// Principal id for the minted code.
pub fn principal(p: &Profile) -> String {
    match p.principal.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => DEFAULT_PRINCIPAL.to_string(),
    }
}

/// The command that prints a fresh pairing code on the machine with voltd.
///
/// The default reaches voltd wherever it runs on that machine: a `voltd`
/// on PATH (native install, running as the SSH user) or, failing that, the
/// pilot kit's container (`docker exec voltd ...`, whose private listener is
/// always 42617 inside). A Windows machine in `Local` mode has no POSIX
/// shell, so there the native form is used as is.
pub fn paircode_command(p: &Profile) -> String {
    if let Some(cmd) = p.paircode_command.as_deref().map(str::trim) {
        if !cmd.is_empty() {
            return cmd
                .replace("{panel_port}", &p.panel_port.to_string())
                .replace("{principal}", &principal(p));
        }
    }
    let principal = principal(p);
    let native = format!(
        "voltd gateway get-paircode --new --port {} --principal {principal}",
        p.panel_port
    );
    let docker = format!(
        "docker exec voltd voltd --config-dir /voltd-data/.voltd gateway get-paircode --new --port 42617 --principal {principal}"
    );
    if p.mode == ConnMode::Local && cfg!(windows) {
        native
    } else {
        format!("if command -v voltd >/dev/null 2>&1; then {native}; else {docker}; fi")
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

/// Evaluated on every normal load of the panel origin: with a stored session
/// (`zeroclaw_token` in localStorage, see the panel's web/src/lib/auth.ts)
/// nothing happens; without one, navigate to `/?volt_pair=1` to ask the app
/// for a code. A page already carrying the marker is left alone so a failed
/// mint cannot loop — the panel's own pairing prompt stays usable there.
pub const PROBE_SCRIPT: &str = "(function(){try{var K='zeroclaw_token';if(localStorage.getItem(K))return;\
if(location.search.indexOf('volt_pair=1')>=0)return;\
location.replace('/?volt_pair=1');}catch(e){}})();";

/// Evaluated on the `/?volt_pair=1` load once a code is minted: if the panel
/// still has no session, redeem the one-time code through the panel's own
/// public pairing endpoint, store the session the way the panel does itself,
/// then go back to `/` (dropping the marker) so the panel picks it up. Any
/// failure leaves the panel's normal pairing prompt in place and does not
/// navigate again.
pub fn pair_script(code: &str) -> String {
    format!(
        "(function(){{try{{var K='zeroclaw_token';if(localStorage.getItem(K))return;\
fetch('/api/pair',{{method:'POST',headers:{{'Content-Type':'application/json'}},\
body:JSON.stringify({{code:'{code}',device_name:'Volt Admin',device_type:'browser'}})}})\
.then(function(r){{return r.ok?r.json():Promise.reject(new Error('pair '+r.status));}})\
.then(function(d){{localStorage.setItem(K,d.token);location.replace('/');}})\
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
        let remote = paircode_command(&profile());
        assert!(
            remote.starts_with("if command -v voltd >/dev/null 2>&1; then voltd gateway get-paircode --new --port 42617 --principal admin; else docker exec voltd voltd --config-dir /voltd-data/.voltd gateway get-paircode --new --port 42617 --principal admin; fi"),
            "got: {remote}"
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
    fn pair_context_matches_its_origin_only_and_clears() {
        let state = PairState::default();
        state.set(PairContext {
            origin: "http://127.0.0.1:1".into(),
            profile: profile(),
            password: Some("pw".into()),
            embedded: false,
            lang: Lang::Ru,
        });
        let other = tauri::Url::parse("http://127.0.0.1:2/").unwrap();
        assert!(state.for_url(&other).is_none());
        let mine = tauri::Url::parse("http://127.0.0.1:1/?volt_pair=1").unwrap();
        let ctx = state.for_url(&mine).expect("same origin");
        assert_eq!(ctx.password.as_deref(), Some("pw"));
        assert_eq!(ctx.profile.id, "a");
        // Not consumed by a lookup: a later page load still sees it.
        assert!(state.for_url(&mine).is_some());
        state.clear();
        assert!(state.for_url(&mine).is_none());
    }

    #[test]
    fn probe_asks_for_a_code_only_without_session_and_marker() {
        assert!(PROBE_SCRIPT.contains("zeroclaw_token"));
        assert!(PROBE_SCRIPT.contains("location.replace('/?volt_pair=1')"));
        assert!(PROBE_SCRIPT.contains("indexOf('volt_pair=1')>=0)return"));
        assert!(pair_script("x").contains("location.replace('/')"));
        assert!(!pair_script("x").contains("reload"));
    }

    #[test]
    fn pair_request_marker_is_read_from_the_query() {
        let yes = tauri::Url::parse("http://127.0.0.1:1/?volt_pair=1").unwrap();
        let no = tauri::Url::parse("http://127.0.0.1:1/sessions?x=1").unwrap();
        assert!(is_pair_request(&yes));
        assert!(!is_pair_request(&no));
    }
}
