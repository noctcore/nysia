# Security

Nysia runs shells and Claude agent sessions on your machine with your user's access, behind a
daemon that listens on a local socket. A bug that lets something other than you drive those
sessions, read their output or run a command you did not start is a security bug. Please report it
privately.

## Supported versions

Nysia has no binary release yet. Fixes land on `main`; build from the latest `main` to get them.

| Version | Security fixes |
|---|---|
| `main` | Yes |
| Older commits and tags | No |

## Reporting

Use GitHub's private vulnerability reporting for this repository:
<https://github.com/noctcore/nysia/security/advisories/new>. It opens a draft advisory that only
the maintainer can see.

If that page says reporting is not enabled, open a regular issue whose entire body is "security
report, need a private channel", with no details, and the maintainer will reply with one.

Include the commit you built, your platform, what an attacker needs and what they get. There is no
bounty. You get a reply, a fix with a changelog entry that credits you unless you ask otherwise, and
the details published once the fix is on `main`.

## How a report is handled

1. **Acknowledged** within 3 working days, in the draft advisory.
2. **Triaged**: in scope or not, and how severe. You hear the outcome and the reasoning either way.
3. **Fixed** with a test that fails before the fix. You are invited to review it.
4. **Published**: the advisory goes public once the fix is on `main`, crediting you unless you ask
   otherwise.

## What counts

- Another local user, process or web page reaching the daemon: creating a session, sending input to
  a terminal or reading its screen or scrollback without going through your own client.
- A command run that you did not start: from an issue body, a branch name, a repository file or
  terminal output that the app or the daemon treats as an instruction.
- A worktree, project or session operation that writes outside the folders you registered.
- Secrets from a session (environment, terminal output) written to the store, the logs or the
  status endpoint.

## What does not count

- What an agent session does with the access you give it. Nysia starts Claude in your terminal;
  the agent's own permissions are Claude's settings, not a boundary Nysia enforces.
- Anything that needs an attacker who already runs code as your user.
