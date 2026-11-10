//! Releases of FLE Link and of the FLE engine are signed offline: the owner signs each release's `SHA256SUMS` with a key
//! that is not on GitHub (docs/RELEASING.md). A checksum from the release alone only says the download arrived whole; the
//! signature says it came from the owner.
use anyhow::{anyhow, bail, Context, Result};
use minisign_verify::{PublicKey, Signature};

const RELEASE_KEY: &str = "RWRoIAMmZ6j7l3LTZhjgP1NTmz8PY9FcYphI3jGsfRce4dcCb+z2eOyL";
/// FLE releases up to this one were published before signing began.
const LAST_UNSIGNED_FLE: &str = "x-v1.2.6";

fn verified_sha(key: &str, sums: &[u8], minisig: &[u8], file: &str) -> Result<String> {
    let key = PublicKey::from_base64(key).map_err(|e| anyhow!("bad release key: {e:?}"))?;
    let sig = Signature::decode(&String::from_utf8_lossy(minisig))
        .map_err(|e| anyhow!("unreadable release signature: {e:?}"))?;
    key.verify(sums, &sig, false)
        .map_err(|_| anyhow!("the release signature does not match"))?;
    sha_in(&String::from_utf8_lossy(sums), file)
        .with_context(|| format!("{file} is not in the signed checksum list"))
}

/// The SHA-256 of `file` in a checksum list (`<hash>  <name>`).
fn sha_in(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file).then(|| hash.to_lowercase())
    })
}

/// The SHA-256 of `file`, from a `SHA256SUMS` signed with the release key.
pub fn signed_sha(sums: &[u8], minisig: &[u8], file: &str) -> Result<String> {
    verified_sha(RELEASE_KEY, sums, minisig, file)
}

/// The SHA-256 of an asset of FLE release `tag`, signed; `None` for a release from before signing that carries no
/// signature. `get` fetches a file of the release. A newer release without a signature is refused.
pub fn fle_sha(
    tag: &str,
    asset: &str,
    get: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<Option<String>> {
    if let (Some(sums), Some(sig)) = (get("SHA256SUMS"), get("SHA256SUMS.minisig")) {
        return signed_sha(&sums, &sig, asset).map(Some);
    }
    use crate::engines::version_key;
    if version_key(&format!("x-{tag}")) <= version_key(LAST_UNSIGNED_FLE) {
        return Ok(None);
    }
    bail!("FLE {tag} has no release signature, so FLE Link does not install it")
}

#[cfg(test)]
mod tests {
    use super::*;

    // made with a throwaway key, only for this test
    const TEST_KEY: &str = "RWQ+i7cpW304z1rEdi43UCBEGEQbanoEOW8w8Bon+UpmkSec5FpVWNWu";
    const SUMS: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  flelink-x86_64-unknown-linux-gnu.tar.gz\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  fle-test-bin-ubuntu.tar.gz\n";
    const SIG: &str = "untrusted comment: signature from minisign secret key\nRUQ+i7cpW304z5LiKUxa1CkP9qC3a6DGpSDiIFX3LQm8wHWZCo1uo4LuPeh+cwColO3nr7aQLydc2d1rtuzRITyEWzzoWBv4OwY=\ntrusted comment: timestamp:1794246318\tfile:SHA256SUMS\thashed\niXfmhDf2/u+ORuwaVGK9F1mhOQRvCn2dhbSUt810EsDC8WIlCk9GOG6aBh1s2R9a2UYIEnawMghTPsxa1zkxAQ==\n";

    #[test]
    fn a_signed_list_gives_the_checksum() {
        let sha = verified_sha(
            TEST_KEY,
            SUMS.as_bytes(),
            SIG.as_bytes(),
            "fle-test-bin-ubuntu.tar.gz",
        )
        .unwrap();
        assert_eq!(sha, "b".repeat(64));
    }

    #[test]
    fn a_changed_list_or_another_key_is_refused() {
        let changed = SUMS.replace('a', "c");
        let file = "fle-test-bin-ubuntu.tar.gz";
        assert!(verified_sha(TEST_KEY, changed.as_bytes(), SIG.as_bytes(), file).is_err());
        assert!(verified_sha(RELEASE_KEY, SUMS.as_bytes(), SIG.as_bytes(), file).is_err());
    }

    #[test]
    fn a_file_outside_the_list_is_refused() {
        assert!(verified_sha(TEST_KEY, SUMS.as_bytes(), SIG.as_bytes(), "other.tar.gz").is_err());
    }

    #[test]
    fn only_old_fle_releases_may_come_unsigned() {
        let none = |_: &str| None;
        assert_eq!(fle_sha("v1.2.6", "x", &none).unwrap(), None);
        assert_eq!(fle_sha("v1.2.5", "x", &none).unwrap(), None);
        assert!(fle_sha("v1.2.7", "x", &none).is_err());
        assert!(fle_sha("v1.10.0", "x", &none).is_err());
    }

    #[test]
    fn a_bad_signature_is_refused_even_on_an_old_release() {
        let bad = |_: &str| Some(b"not a signature".to_vec());
        assert!(fle_sha("v1.2.5", "x", &bad).is_err());
    }
}
