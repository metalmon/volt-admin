# Spec: three gateway actions in the launcher

**Status:** approved in scope, not implemented
**Owner:** launcher (profile card menu, `crate::pair`'s exec path)

## Problem

Everything the launcher can do to a gateway today is "show me its panel".
When the panel is the thing that is not answering — or when the answer is
"stop everything, now" — the administrator opens a terminal, finds the
host, remembers where `voltd` lives, and types. The app already knows all
three of those: it has the profile, it resolves the binary, and it runs
commands there for pairing.

## The three

Chosen over a general "run a command on the gateway" (see Out of scope):

| action | command | confirm |
|---|---|---|
| Re-issue the pairing code | `gateway get-paircode --rotate` | yes, naming what breaks |
| Restart the gateway | `gateway restart` | no |
| Emergency stop | `estop --level kill-all` | yes, naming what stops |
| Emergency stop — state | `estop status` | no |
| Emergency stop — release | `estop resume` | no |

`doctor` was considered and dropped: its value is diagnosis, and the
diagnosis an administrator needs first is already in the app's own
Diagnostics section.

**`--rotate` revokes every paired token** and prints a fresh code. Every
other client — phones, browsers, another admin's laptop — has to pair
again. The confirmation says exactly that, not "are you sure".

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
selectable and copyable. `doctor` was dropped, but the other three still
have to be readable: `--rotate` PRINTS THE NEW CODE, which is the whole
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
- `service start/stop/restart`. On a `Remote` profile the SSH lands inside
  the deployment that would be stopped — including its sshd. One click,
  no way back.
- `update`. The gateway's own lifecycle, and it restarts things underfoot.
- `status` and `security`: more words than `doctor`, which was already cut.

## Done when

- Rotating from the menu shows the new code, and the next connect to that
  profile pairs again by itself rather than opening a pairing form.
- Restarting a gateway whose panel had stopped answering makes it answer,
  without touching the daemon.
- An emergency stop asks a question that names what stops, and its state
  and release are one item each.
- A failed action shows what the command printed, starting with the line
  that says why — not its tail.
- Nothing in the log contains a pairing code.
