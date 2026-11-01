# Security

FLE Link listens on 127.0.0.1 by default. In host mode (`--host 0.0.0.0`) it is reachable from your network and protected by
an access key and TLS with a certificate of its own. Problems there matter most to us: access without the key, ways around the
host-name check, reading files outside the data folder, or running commands through the API.

Please do not open a public issue for a security problem. Report it privately through GitHub's
[security advisories](https://github.com/sirfyyn/flelink/security/advisories/new). You get an answer within a week. Fixes are
released as soon as they are ready, with credit if you want it.

Only the latest release is supported.
