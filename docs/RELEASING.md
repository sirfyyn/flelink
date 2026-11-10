# Signing a release

FLE Link checks the signature of a release before it installs an update or an FLE engine. The key is kept off GitHub, so someone who takes over the GitHub account cannot sign. The public key is in `crates/flelink-core/src/signature.rs`.

After the release workflow has published a release, on the machine with the key:

```bash
gh release download v0.1.8 --repo sirfyyn/flelink --pattern SHA256SUMS
minisign -S -s ~/.minisign/flelink.key -m SHA256SUMS
gh release upload v0.1.8 SHA256SUMS.minisig --repo sirfyyn/flelink
```

Use the release's own tag. For an engine release use `--repo sirfyyn/FastLocalExperts`. Remove the two local files before the next release.

An update of FLE Link without `SHA256SUMS.minisig` is refused, and so is an FLE engine newer than v1.2.6. The first install through `install.sh` cannot check the signature: no key is on the machine yet.

Keep the secret key and its password in the password manager. Without them no further release can be signed, and a new key means a new FLE Link release that carries it.
