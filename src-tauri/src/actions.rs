//! Things an administrator does to a gateway from the launcher, instead of
//! opening a terminal and remembering where `voltd` lives.
//!
//! Chosen in `docs/specs/gateway-actions.md`: revoke every pairing, and
//! the emergency stop (enable, engage, inspect, release). They run exactly where pairing runs — this machine
//! for a `Local` profile, the SSH target for a `Remote` one.
//!
//! NOTHING HERE LOGS THE OUTPUT. `get-paircode --rotate` prints a pairing
//! code, and the log must never carry one (`docs/specs/diagnostics.md`).
//! What is logged is which action ran, on which profile, and whether it
//! worked.

use serde::Deserialize;

use crate::connection::{ConnMode, Profile};
use crate::logx;
use crate::pair;
use crate::tunnel::{self, Lang};

/// How long one of these may take. The same bound pairing uses: an ssh
/// handshake plus a command that answers quickly or not at all.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// What the launcher can ask a gateway to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GatewayAction {
    /// Revoke every paired token and issue a fresh code.
    RotatePairing,
    /// The gateway's security settings, read before the menu is drawn:
    /// whether the emergency stop is enabled at all, and whether an engaged
    /// one could be released again.
    SecurityConfig,
    /// The gateway's observability settings, read with the above: without
    /// a recording backend and a persisted log there is nothing for
    /// `GET /api/logs` to serve, and so nothing to watch.
    ObservabilityConfig,
    /// Record events at all (`backend = none` records nothing).
    EnableObservability,
    /// Keep them, so `/api/logs` can serve a window of them.
    EnableLogPersistence,
    /// Keep ENOUGH of them. voltd's own default is 200 entries — under an
    /// hour on a quiet gateway, so a laptop that slept comes back to a
    /// store that has already turned over.
    EnableLogWindow,
    /// Turn the emergency stop ON in the gateway's config — it ships
    /// disabled, and the other three answer "Emergency stop is disabled"
    /// until it is.
    EmergencyEnable,
    /// Stop the deployment.
    EmergencyStop,
    /// Is an emergency stop engaged, and at what level?
    EmergencyStatus,
    /// Release an engaged emergency stop.
    EmergencyResume,
}

impl GatewayAction {
    /// What follows the `voltd` binary on the command line.
    pub fn args(self) -> &'static str {
        match self {
            Self::RotatePairing => "gateway get-paircode --rotate",
            Self::SecurityConfig => "config list --filter security",
            Self::ObservabilityConfig => "config list --filter observability",
            Self::EnableObservability => "config set observability.backend log",
            Self::EnableLogPersistence => "config set observability.log_persistence rolling",
            Self::EnableLogWindow => "config set observability.log_persistence_max_entries 50000",
            Self::EmergencyEnable => "config set security.estop.enabled true",
            Self::EmergencyStop => "estop --level kill-all",
            Self::EmergencyStatus => "estop status",
            Self::EmergencyResume => "estop resume",
        }
    }

    /// Does this action's OUTPUT carry a secret?
    ///
    /// A rotate prints a fresh pairing code. That belongs on the
    /// administrator's screen and nowhere else — not in an error, which the
    /// UI keeps as "the last error" and copies into the diagnostics people
    /// send to support.
    pub fn prints_secret(self) -> bool {
        matches!(self, Self::RotatePairing)
    }

    /// Does this need the user to agree to a consequence first?
    ///
    /// Only the two that take something away: a rotate makes every other
    /// paired client — phones, browsers, another admin's laptop — pair
    /// again, and a stop stops the deployment.
    pub fn confirms(self) -> bool {
        matches!(self, Self::RotatePairing | Self::EmergencyStop)
    }
}

/// Why this action may not run yet, if it may not.
///
/// Separate from [`run`] so the rule can be tested without a test ever
/// calling `run` — one did, with a `Local` profile, and rotated the pairing
/// tokens of the machine the suite was on.
fn confirmation_missing(action: GatewayAction, confirmed: bool, lang: Lang) -> Option<String> {
    (action.confirms() && !confirmed).then(|| match lang {
        Lang::Ru => "действие требует подтверждения".to_string(),
        Lang::En => "this action needs confirmation".to_string(),
    })
}

/// The full command line for `action` on `p`'s gateway.
///
/// `Local` resolves the binary the way the pairing command does — a native
/// Windows install keeps `voltd` in `<config-dir>/bin`, not on PATH.
/// `Remote` uses the bare name: it runs on the server, where the kit puts
/// it on PATH.
pub fn command_line(p: &Profile, action: GatewayAction) -> String {
    format!("{} {}", pair::voltd_binary(p), action.args())
}

/// Run `action` where the gateway lives and return what it printed.
pub async fn run(
    p: &Profile,
    action: GatewayAction,
    confirmed: bool,
    password: Option<&str>,
    embedded: bool,
    lang: Lang,
) -> Result<String, String> {
    // The confirmation is enforced HERE, not only drawn in the UI: a rotate
    // revokes every paired token and a stop stops the deployment, and
    // neither should be one stray call away.
    if let Some(refusal) = confirmation_missing(action, confirmed, lang) {
        return Err(refusal);
    }
    let command = command_line(p, action);
    logx::log(
        "action",
        &format!("{action:?} on {} ({:?})", p.name, p.mode),
    );
    let result = match p.mode {
        ConnMode::Local => pair::run_local(&command).await,
        ConnMode::Remote if embedded => Err(match lang {
            Lang::Ru => {
                "команды шлюза требуют системный ssh (встроенный клиент их не поддерживает)"
                    .to_string()
            }
            Lang::En => {
                "gateway commands need the system ssh (the built-in client cannot run them)"
                    .to_string()
            }
        }),
        ConnMode::Remote => tunnel::run_ssh_command(p, password, &command, TIMEOUT, lang).await,
    };
    // The OUTCOME, never the output: a rotate's output is a pairing code.
    logx::log(
        "action",
        &format!(
            "{action:?} {}",
            if result.is_ok() { "ok" } else { "failed" }
        ),
    );
    // A failing command still prints, and `collect_output` puts that print
    // into the error. For an action whose print is a secret, the error is
    // replaced rather than passed on.
    result.map_err(|e| safe_error(action, e, lang))
}

/// The error a caller may see. A failing command still prints, and
/// `collect_output` builds its error FROM that print — so for an action
/// whose print is a secret, the error is replaced instead of passed on.
/// Errors travel to the UI's "last error", and from there into the
/// diagnostics people send to support.
fn safe_error(action: GatewayAction, e: String, lang: Lang) -> String {
    if !action.prints_secret() {
        return e;
    }
    match lang {
        Lang::Ru => "команда не выполнена; подробности — в выводе на шлюзе".to_string(),
        Lang::En => "the command failed; its output stays on the gateway".to_string(),
    }
}

/// What the menu needs to know before it is drawn.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFlags {
    /// `security.estop.enabled` — until this is true, every stop item just
    /// answers "Emergency stop is disabled".
    pub estop_enabled: bool,
    /// Could an engaged stop be released again? False only when a resume
    /// needs an OTP that is switched off — voltd then refuses the resume,
    /// and a stop engaged in that state has no way back from here.
    pub can_resume: bool,
    /// `observability.backend` records something at all. `none` records
    /// nothing, and then no log exists to watch.
    pub logs_recorded: bool,
    /// `observability.log_persistence` keeps what was recorded. Without it
    /// `GET /api/logs` answers `persistence_enabled: false`.
    pub logs_persisted: bool,
    /// `observability.log_persistence_max_entries` holds a usable window.
    /// voltd defaults to 200, which a watcher outlives in minutes.
    pub logs_window_ok: bool,
}

/// The window a watched gateway needs, and what the deployments this app
/// ships with are configured for.
pub const LOG_WINDOW: u64 = 50_000;

/// Read the three lines that matter out of `config list --filter security`.
///
/// Absent or unreadable keys claim NOTHING: no greying, no warning. The
/// probe is best-effort (the gateway may be down), and a guess in either
/// direction is worse than silence.
pub fn gateway_flags(listing: &str) -> GatewayFlags {
    let value = |key: &str| -> Option<&str> {
        listing
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with(key) && l[key.len()..].trim_start().starts_with('='))?
            .split('=')
            .nth(1)?
            .split_whitespace()
            .next()
    };
    let flag = |key: &str| value(key).map(|v| v == "true");
    GatewayFlags {
        estop_enabled: flag("security.estop.enabled") == Some(true),
        can_resume: !(flag("security.estop.require_otp_to_resume") == Some(true)
            && flag("security.otp.enabled") == Some(false)),
        // An unreadable setting claims nothing: only the value that says
        // "records nothing" / "keeps nothing" counts as off.
        logs_recorded: value("observability.backend") != Some("none"),
        logs_persisted: value("observability.log_persistence") != Some("off"),
        // Unreadable claims nothing, as everywhere here: only a number
        // that is plainly too small counts as too small.
        logs_window_ok: value("observability.log_persistence_max_entries")
            .and_then(|v| v.parse::<u64>().ok())
            .is_none_or(|n| n >= LOG_WINDOW),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{AuthMethod, ConnMode};

    fn profile(mode: ConnMode) -> Profile {
        Profile {
            id: "a".into(),
            name: "srv".into(),
            mode,
            host: "h".into(),
            user: "u".into(),
            port: 22,
            panel_port: 42617,
            local_port: None,
            auth: AuthMethod::Agent,
            key_path: None,
            principal: None,
            paircode_command: None,
            watch: false,
        }
    }

    #[test]
    fn every_action_names_its_voltd_arguments() {
        assert_eq!(
            GatewayAction::RotatePairing.args(),
            "gateway get-paircode --rotate"
        );
        assert_eq!(
            GatewayAction::EmergencyEnable.args(),
            "config set security.estop.enabled true"
        );
        assert_eq!(
            GatewayAction::EmergencyStop.args(),
            "estop --level kill-all"
        );
        assert_eq!(GatewayAction::EmergencyStatus.args(), "estop status");
        assert_eq!(GatewayAction::EmergencyResume.args(), "estop resume");
    }

    #[test]
    fn the_probe_reads_the_security_section_in_one_command() {
        assert_eq!(
            GatewayAction::SecurityConfig.args(),
            "config list --filter security"
        );
        assert!(!GatewayAction::SecurityConfig.confirms());
    }

    /// What `voltd config list --filter security` prints, trimmed to the
    /// three lines this reads.
    const LISTING: &str = "Security:
  security.otp.enabled                 = false        (bool)
  security.estop.enabled               = true         (bool)
  security.estop.require_otp_to_resume = true         (bool)
Observability:
  observability.backend                = log          (ObservabilityBackend)
  observability.log_persistence        = rolling      (LogPersistence)
  observability.log_persistence_max_entries = 50000   (usize)
";

    #[test]
    fn a_stop_that_could_not_be_released_is_seen_before_it_is_engaged() {
        // The trap this exists for: estop on, resume gated behind an OTP
        // that is off. Engaging then stops the deployment with nothing in
        // the app — or the CLI — able to resume it.
        let flags = gateway_flags(LISTING);
        assert!(flags.estop_enabled);
        assert!(!flags.can_resume);

        let with_otp = LISTING.replace(
            "otp.enabled                 = false",
            "otp.enabled                 = true",
        );
        assert!(gateway_flags(&with_otp).can_resume);
        let no_gate = LISTING.replace(
            "require_otp_to_resume = true",
            "require_otp_to_resume = false",
        );
        assert!(gateway_flags(&no_gate).can_resume);
    }

    #[test]
    fn a_gateway_that_records_nothing_has_nothing_to_watch() {
        // The two settings monitoring needs, and the two values that mean
        // "off" — everything else (log, verbose, prometheus, otel; rolling,
        // single) records and keeps something.
        let on = gateway_flags(LISTING);
        assert!(on.logs_recorded && on.logs_persisted);
        let silent = LISTING.replace(
            "backend                = log",
            "backend                = none",
        );
        assert!(!gateway_flags(&silent).logs_recorded);
        let forgetful = LISTING.replace(
            "log_persistence        = rolling",
            "log_persistence        = off",
        );
        assert!(!gateway_flags(&forgetful).logs_persisted);
        let otel = LISTING.replace(
            "backend                = log",
            "backend                = otel",
        );
        assert!(gateway_flags(&otel).logs_recorded, "otel records too");
    }

    #[test]
    fn a_listing_that_says_nothing_claims_nothing() {
        // An unreadable or empty answer must not grey out the menu or warn
        // about a trap: unknown is not "false".
        let flags = gateway_flags("");
        assert!(!flags.estop_enabled);
        assert!(flags.can_resume, "no evidence of a gate is not a gate");
        assert!(
            flags.logs_recorded && flags.logs_persisted,
            "unknown is not off"
        );
    }

    #[test]
    fn a_failing_rotate_does_not_hand_back_what_it_printed() {
        // Tested on the MAPPING, never by running the command: a rotate
        // revokes every paired token, and a test that runs it revokes them
        // on whatever machine the suite happens to be on.
        let printed = "PAIRCODE abcdefghijklmnopqrstuvwxyz123456".to_string();
        let hidden = safe_error(GatewayAction::RotatePairing, printed.clone(), Lang::En);
        assert!(
            !hidden.contains("abcdefghijklmnopqrstuvwxyz123456"),
            "{hidden}"
        );
        // Everything else keeps its reason — that is what makes it useful.
        let kept = safe_error(GatewayAction::EmergencyStatus, printed.clone(), Lang::En);
        assert_eq!(kept, printed);
    }

    #[test]
    fn only_the_rotate_prints_a_secret() {
        assert!(GatewayAction::RotatePairing.prints_secret());
        for action in [
            GatewayAction::SecurityConfig,
            GatewayAction::EmergencyEnable,
            GatewayAction::EmergencyStop,
            GatewayAction::EmergencyStatus,
            GatewayAction::EmergencyResume,
        ] {
            assert!(!action.prints_secret(), "{action:?}");
        }
    }

    #[test]
    fn only_the_destructive_two_confirm() {
        assert!(
            GatewayAction::RotatePairing.confirms(),
            "revokes every token"
        );
        assert!(GatewayAction::EmergencyStop.confirms(), "stops everything");
        assert!(!GatewayAction::EmergencyStatus.confirms());
        assert!(!GatewayAction::EmergencyStatus.confirms());
        assert!(!GatewayAction::EmergencyResume.confirms());
    }

    #[test]
    fn a_destructive_action_refuses_to_run_unconfirmed() {
        // Drawing the dialog is the UI's job; being impossible to skip is
        // not. Tested on the rule, not by calling `run` — NO test in this
        // file may reach a command line.
        for action in [GatewayAction::RotatePairing, GatewayAction::EmergencyStop] {
            assert_eq!(
                confirmation_missing(action, false, Lang::En).as_deref(),
                Some("this action needs confirmation"),
                "{action:?}"
            );
            assert!(confirmation_missing(action, true, Lang::En).is_none());
        }
        for action in [
            GatewayAction::EmergencyStatus,
            GatewayAction::EmergencyStatus,
            GatewayAction::EmergencyResume,
        ] {
            assert!(
                confirmation_missing(action, false, Lang::En).is_none(),
                "{action:?}"
            );
        }
    }

    #[test]
    fn a_remote_command_line_is_the_bare_binary_and_the_args() {
        let line = command_line(&profile(ConnMode::Remote), GatewayAction::EmergencyResume);
        assert_eq!(line, "voltd estop resume");
    }

    #[test]
    fn a_local_command_line_keeps_the_resolved_binary_usable() {
        // Whatever the resolution found, the line must still be one command:
        // a path with a space in it is quoted, and the arguments follow it.
        let line = command_line(&profile(ConnMode::Local), GatewayAction::EmergencyStatus);
        assert!(line.ends_with(" estop status"), "{line}");
        let binary = line.trim_end_matches(" estop status");
        assert!(!binary.is_empty(), "{line}");
        assert!(
            !binary.contains(' ') || binary.starts_with('"'),
            "a path with a space must be quoted: {line}"
        );
    }
}
