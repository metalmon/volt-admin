# Spec: gateway actions in the launcher

**Status:** approved in scope, not implemented
**Owner:** launcher (profile card menu, `crate::pair`'s exec path)

## Problem

Everything the launcher can do to a gateway today is "show me its panel".
When the panel is the thing that is not answering — or when the answer is
"stop everything, now" — the administrator opens a terminal, finds the
host, remembers where `voltd` lives, and types. The app already knows all
three of those: it has the profile, it resolves the binary, and it runs
commands there for pairing.

## The actions

Chosen over a general "run a command on the gateway" (see Out of scope):

| action | command | confirm |
|---|---|---|
| Revoke all pairings | `gateway get-paircode --rotate` | yes, naming what breaks |
| Turn the emergency stop on | `config set security.estop.enabled true` | no |
| Emergency stop | `estop --level kill-all` | yes, naming what stops |
| Emergency stop — state | `estop status` | no |
| Emergency stop — release | `estop resume` | no |

`doctor` was considered and dropped: its value is diagnosis, and the
diagnosis an administrator needs first is already in the app's own
Diagnostics section.

**`--rotate` revokes every paired token** and prints a fresh code. It is
named for the revoking, not for the code: "re-issue the pairing code" reads
like a convenience and this is not one. Every
other client — phones, browsers, another admin's laptop — has to pair
again. The confirmation says exactly that, not "are you sure".

**The emergency stop ships disabled**, and until it is enabled all three of
its items answer "Emergency stop is disabled. Enable [security.estop].enabled
= true in config.toml" — which is why turning it on is an item too. Enabling
takes nothing away, so it does not confirm; it writes one key and the gateway
picks it up when it next reads its config.

The menu reads `config list --filter security` when it opens and greys out
what cannot work: the three stop items while it is off, the item that
turns it on while it is on. Only on an ANSWER — a gateway that is down or
unreachable leaves them clickable, the way they were before the question
was asked.

**A stop can be a one-way door.** voltd ships
`security.estop.require_otp_to_resume = true`, and with `security.otp`
disabled it refuses `estop resume` — from here and from the CLI — so the
deployment stays stopped. The same listing answers that too, and the stop's
confirmation says it, naming both keys, before the stop is engaged. (Found
by engaging one: the release answered "require_otp_to_resume=true but
otp.enabled=false".)

**`estop --level kill-all`** stops the deployment. Same rule for its
confirmation. `domain-block` and `tool-freeze` need domain and tool
arguments and belong in the panel, not in a menu.

## Where

The `☰` menu already on each profile card, under a «Шлюз» submenu: these
act on one gateway, named by the profile that reaches it. Not the tray —
the tray has no profile in hand.

## How they run

The same path pairing uses (`crate::pair`): locally for a `Local` profile,
`tunnel::run_ssh_command` for a `Remote` one, with the binary resolved the
way `paircode_command` resolves it — PATH first, then the `bin` directory
of a config directory this user owns.

A profile on the built-in SSH client cannot exec at all; those items are
disabled with that reason, as auto-pairing already reports.

## The output

Each action opens a result window with the command's text output,
selectable and copyable. `doctor` was dropped, but what the rest print still
has to be readable: `--rotate` PRINTS THE NEW CODE, which is the whole
point of running it.

**That output never reaches the log.** The log's rule stands — no codes, no
tokens (see `docs/specs/diagnostics.md`) — so what is logged is which
action ran, on which profile, and whether it succeeded.

## After a rotate

The app's pairing marker (`settings::paired_fingerprint`) says "this
gateway already knows us". A rotate makes that false for every profile
pointing at the same gateway, so each of their markers is forgotten —
otherwise the next connect would skip pairing and land on a pairing form
with nothing to fill it.

## Out of scope

- An arbitrary command box. The profile already carries one executable
  field (`paircodeCommand`), and the import preview shows it precisely
  because running what someone else typed is a decision, not a convenience.
- `gateway restart`. It was in the menu until a live test: it answers
  "config lifecycle is already owned" whenever a daemon or service holds the
  gateway — which is every gateway the launcher is pointed at, since that
  daemon is what serves the panel. The panel's own reload button works
  because the DAEMON reloads itself: it owns the config lifecycle. An
  outside process has to take that lock, and the owner does not give it up.
  Doing what the panel does would mean calling the daemon's own HTTP route,
  i.e. duplicating a button in the window the launcher has just opened.
  (`voltd config set` needs no such lock, so the estop items are unaffected.)
- `service start/stop/restart`. On a `Remote` profile the SSH lands inside
  the deployment that would be stopped — including its sshd. One click,
  no way back.
- `update`. The gateway's own lifecycle, and it restarts things underfoot.
- `status` and `security`: more words than `doctor`, which was already cut.

## Done when

- Rotating from the menu shows the new code, and the next connect to that
  profile pairs again by itself rather than opening a pairing form.
- An emergency stop asks a question that names what stops, and its state
  and release are one item each. On a gateway where `[security.estop]` is
  not enabled, the item says so in voltd's own words — it names the setting
  to change — rather than appearing to do nothing.
- A failed action shows what the command printed, starting with the line
  that says why — not its tail.
- Turning the emergency stop on makes its state item answer with a state
  instead of with "disabled", and the three stop items are greyed out until
  it is on.
- Engaging a stop that could not be released warns about it first, naming
  `security.estop.require_otp_to_resume` and `security.otp.enabled`.
- Nothing in the log contains a pairing code.
- A connect after any rotate — ours or one typed on the gateway — lands on
  the panel signed in, not on its pairing form: the session this app hands
  over REPLACES whatever the panel had, because a rotate leaves a dead token
  in its storage and a panel that boots signed out also boots in its own
  language.
