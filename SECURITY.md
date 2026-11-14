# Security

## Report a problem

Please report a security problem privately, not in a public issue:
[open a private report](https://github.com/XJOC/marsa/security/advisories/new)
on GitHub. Say what you found, how to reproduce it, and which version you use
(`marsa --version`). The answer comes in the same private report.

Only the newest release gets security fixes.

## What runs with administrator rights, and why

Marsa runs as your user. Two things need more, on macOS:

1. **`marsa setup` runs `marsa-helper` once as root**, behind the macOS
   administrator dialog. The helper reads one request in a fixed JSON format,
   with a closed list of operations. It never runs a command it receives, and
   it never opens a file path from the request. It does three things:
   - It writes `/etc/resolver/test`, so that macOS asks Marsa's DNS server on
     `127.0.0.1` for `.test` names. If another tool already owns that file,
     the helper leaves it alone.
   - It adds a `pf` rule in the anchor `com.apple/250.MarsaRedirect`
     (`/Library/Application Support/Marsa/pf-redirect.rules`). The rule sends
     `127.0.0.1:80` and `:443` to the ports Caddy listens on. Caddy itself
     runs as your user, on high ports.
   - It installs a boot job, `/Library/LaunchDaemons/dev.marsa.pf-redirect.plist`.
     At each start of the Mac, the job loads that rule with a root-owned copy
     of the helper, `/Library/PrivilegedHelperTools/dev.marsa.helper`.
2. **Your Mac trusts Marsa's certificate authority**, so that browsers accept
   `https://*.test`. This is done for your user only, in your login keychain,
   after you approve the macOS dialog. The authority is made on your Mac, in
   Marsa's data folder, and never leaves it. Anyone who can read that folder
   can make certificates your Mac trusts, so keep it private.

`marsa uninstall --all` removes all of the above.

## The daemon

`marsad` runs as your user and listens on `127.0.0.1` only. Every request
needs a token that only your user can read (`run/daemon.json`, mode `0600`).
It refuses requests that carry an `Origin` header, so a web page cannot call
it, and requests whose `Host` is not `127.0.0.1:<port>`.

## Downloads

- **PHP, Node.js, Composer, and Caddy** come from a list that the owner
  signs with [minisign](https://jedisct1.github.io/minisign/). The public key
  is built into Marsa. Marsa refuses a list with a bad signature, or one older
  than the last it accepted, and checks the SHA-256 of each file before it
  uses it.
- **Marsa itself**: each release has `SHA256SUMS` and its signature, made with
  the same key (see "Verify a download" in the README). `install.sh` always
  checks the SHA-256. It checks the signature only if `minisign` is installed;
  without it, it says so and prints the command to check by hand.
- The macOS programs are signed ad hoc, not notarized by Apple.
