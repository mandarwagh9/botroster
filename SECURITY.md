# Security

## Reporting

Please report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/mandarwagh9/openbot/security/advisories/new)
rather than a public issue. You will get an acknowledgement within a few days.

## What is in scope

OPENBOT runs model-directed code against a real filesystem, shell and browser, holds third-party
credentials in a broker, and enforces a policy gate in the hub. Anything that lets an agent, a page
it visits, or a connected client do one of these is in scope:

- read a stored credential, or cause one to be sent anywhere other than the connector it belongs to
- act without an approval that the policy says is required, or answer an approval it was not asked
- reach outside the workspace root through `fs.*` or `shell.exec` when confinement is on
- read another session's tool calls, arguments or results
- have `openbot-guest` reach `openbotd` (the isolation boundary)

## What is out of scope, and said plainly

The shipped guest is a process on your machine, not a VM or a container. `README.md` § *Honest
warnings* describes the confinement that exists and the ways it ends. Reports that amount to
"the guest can do what a process running as you can do" describe the documented posture, not a
vulnerability, until the container backend exists.

## Supported versions

Pre-alpha. Only the tip of `main` is supported.
