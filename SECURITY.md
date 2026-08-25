# Security

## Reporting

Please report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/mandarwagh9/botroster/security/advisories/new)
rather than a public issue. You will get an acknowledgement within a few days.

## What is in scope

BOTROSTER runs model-directed code against a real filesystem, shell and browser, holds third-party
credentials in a broker, and enforces a policy gate in the hub. Anything that lets an agent, a page
it visits, or a connected client do one of these is in scope:

- read a stored credential, or cause one to be sent anywhere other than the connector it belongs to
- act without an approval that the policy says is required, or answer an approval it was not asked
- reach outside the workspace root through `fs.*` or `shell.exec` when confinement is on
- read another session's tool calls, arguments or results
- have `botroster-guest` reach `botrosterd` (the isolation boundary)

## The model key in an official build

Installers published on the releases page carry an API key for the model they ship with, compiled
in. **It is recoverable from any installer you download** — `grep -a` on the binary finds it, which
was measured rather than assumed. Treat it as public.

That is a deliberate trade and worth being exact about, because it is not the usual advice:

- The key is **not** in this repository. It is injected at build time from a CI secret, so the source
  stays publishable and rotating the key does not require rewriting history.
- It buys a download that works with no account, no signup and nothing to paste. That was judged
  worth more than the key staying secret, given the model it reaches is free of token charges.
- It is **not** your key. It belongs to whoever cut the release, and it is rate-limited and rotated
  by them. If a build stops working, that is what happened.
- **A build from source has no key at all**, and nothing about this applies to it: `option_env!`
  yields nothing and BOTROSTER asks you to name a model, exactly as it did before.

If you would rather not use a shared credential — and there are good reasons not to, including that
the shipped provider retains prompts and completions — name your own model and BOTROSTER will use it
instead:

```sh
botroster config set --model qwen3:1.7b --dialect openai --base-url http://localhost:11434/v1 --api-key-env ''
```

Nothing leaves your machine on that arrangement.

Please do not report the recoverable key as a vulnerability. It is documented here because it is
intended, and the report we would want instead is a way for it to reach something it should not.

## What is out of scope, and said plainly

The shipped guest is a process on your machine, not a VM or a container. `README.md` § *Honest
warnings* describes the confinement that exists and the ways it ends. Reports that amount to
"the guest can do what a process running as you can do" describe the documented posture, not a
vulnerability, until the container backend exists.

## Supported versions

Pre-alpha. Only the tip of `main` is supported.
