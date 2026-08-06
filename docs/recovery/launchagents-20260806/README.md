# Redacted LaunchAgent recovery evidence

This packet preserves evidence for nine malformed on-disk LaunchAgent files
without copying their bytes, arguments, environment values, or logs.

Each plist in `plists/` is canonical XML that records only the label and the
launchctl-observed program path. It is an evidence template, not a deployment
configuration: `unknown` means the original field was not safely
reconstructable and must not be guessed. The original on-disk file is
identified exclusively by its absolute path and SHA-256 in `manifest.json`.

The manifest contains only allowlisted launchctl fields. In particular,
environment variable values, command arguments other than the observed
program path, credentials, and raw logs are intentionally absent.
