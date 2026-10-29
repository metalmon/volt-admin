//! Things an administrator does to a gateway from the launcher, instead of
//! opening a terminal and remembering where `voltd` lives.
//!
//! Three of them, chosen in `docs/specs/gateway-actions.md`: re-issue the
//! pairing code, restart the gateway, and the emergency stop (engage,
//! inspect, release). They run exactly where pairing runs — this machine
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
    /// Restart the HTTP/WebSocket gateway, not the daemon.
    RestartGateway,
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
            Self::RestartGateway => "gateway restart",
            Self::EmergencyStop => "estop --level kill-all",
            Self::EmergencyStatus => "estop status",
            Self::EmergencyResume => "estop resume",
        }
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
    if action.confirms() && !confirmed {
        return Err(match lang {
            Lang::Ru => "действие требует подтверждения".to_string(),
            Lang::En => "this action needs confirmation".to_string(),
        });
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
    result
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
        }
    }

    #[test]
    fn every_action_names_its_voltd_arguments() {
        assert_eq!(
            GatewayAction::RotatePairing.args(),
            "gateway get-paircode --rotate"
        );
        assert_eq!(GatewayAction::RestartGateway.args(), "gateway restart");
        assert_eq!(
            GatewayAction::EmergencyStop.args(),
            "estop --level kill-all"
        );
        assert_eq!(GatewayAction::EmergencyStatus.args(), "estop status");
        assert_eq!(GatewayAction::EmergencyResume.args(), "estop resume");
    }

    #[test]
    fn only_the_destructive_two_confirm() {
        assert!(
            GatewayAction::RotatePairing.confirms(),
            "revokes every token"
        );
        assert!(GatewayAction::EmergencyStop.confirms(), "stops everything");
        assert!(!GatewayAction::RestartGateway.confirms());
        assert!(!GatewayAction::EmergencyStatus.confirms());
        assert!(!GatewayAction::EmergencyResume.confirms());
    }

    #[tokio::test]
    async fn a_destructive_action_refuses_to_run_unconfirmed() {
        // Drawing the dialog is the UI's job; being impossible to skip is
        // not. Nothing is spawned here: the refusal comes first.
        for action in [GatewayAction::RotatePairing, GatewayAction::EmergencyStop] {
            let err = run(
                &profile(ConnMode::Local),
                action,
                false,
                None,
                false,
                Lang::En,
            )
            .await
            .unwrap_err();
            assert_eq!(err, "this action needs confirmation", "{action:?}");
        }
    }

    #[test]
    fn a_remote_command_line_is_the_bare_binary_and_the_args() {
        let line = command_line(&profile(ConnMode::Remote), GatewayAction::RestartGateway);
        assert_eq!(line, "voltd gateway restart");
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
