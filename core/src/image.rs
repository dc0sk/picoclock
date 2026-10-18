//! Firmware images for the install (ADR-0006): the 16-byte info block every image carries near its start, the rules for accepting
//! an image (right chip, strictly newer, fits the slot) and the Ed25519 signature check.
//!
//! An update file is the raw image followed by a 64-byte signature. The signature is made by the owner's GnuPG agent over the
//! SHA-512 digest of the image (the digest is the Ed25519 message, see `tools/sign_image.py`). The chip and the version are
//! inside the image, so they are covered by the signature; the release tag and the file name are never trusted.

use crate::update::Version;
use ed25519_dalek::{Signature, VerifyingKey};

/// Start of the info block. An image carries it once, in its first 4 KiB.
pub const MAGIC: [u8; 8] = *b"PICOIMG1";
pub const INFO_LEN: usize = 16;
/// The info block is searched for in this many leading bytes of the image.
pub const SCAN: usize = 4096;
/// Length of the signature that follows the image in an update file.
pub const SIGNATURE_LEN: usize = 64;

/// Which chip an image is built for. An RP2040 image on an RP2350 (or the reverse) would not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    Rp2040 = 1,
    Rp2350 = 2,
}

impl Chip {
    pub fn name(self) -> &'static str {
        match self {
            Chip::Rp2040 => "RP2040 (Pico W)",
            Chip::Rp2350 => "RP2350 (Pico 2 W)",
        }
    }
    fn from_u16(v: u16) -> Option<Chip> {
        match v {
            1 => Some(Chip::Rp2040),
            2 => Some(Chip::Rp2350),
            _ => None,
        }
    }
}

/// The version this firmware was built as, as a constant (the info block of its own image is a `static`).
pub const RUNNING: Version = parse_const(crate::VERSION);

const fn parse_const(s: &str) -> Version {
    let b = s.as_bytes();
    let (mut part, mut cur, mut i) = (0usize, 0u32, 0usize);
    let mut nums = [0u16; 3];
    while i <= b.len() {
        if i == b.len() || b[i] == b'.' {
            if part < 3 {
                nums[part] = cur as u16;
            }
            part += 1;
            cur = 0;
        } else {
            assert!(b[i] >= b'0' && b[i] <= b'9', "the package version is major.minor.patch");
            cur = cur * 10 + (b[i] - b'0') as u32;
        }
        i += 1;
    }
    assert!(part == 3, "the package version is major.minor.patch");
    Version { major: nums[0], minor: nums[1], patch: nums[2] }
}

/// Where an install stands, as `update` and the web page show it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Install {
    /// Nothing was uploaded since the start.
    #[default]
    Idle,
    /// An upload is being written.
    Receiving,
    /// A signed, newer image for this chip is waiting in the download slot; `update apply` swaps it in. Index of the signing key.
    Staged(Version, u8),
    /// The last upload was refused.
    Refused(Refusal),
    /// The flash could not be written or read back.
    WriteFailed,
}

/// The line `update` and `status` show about an upload (no line break); nothing for `Idle`.
pub fn describe_install(out: &mut impl core::fmt::Write, i: Install) {
    let _ = match i {
        Install::Idle => return,
        Install::Receiving => write!(out, "image   : receiving an update file..."),
        Install::Staged(v, UNSIGNED) => write!(out, "image   : {v} is staged WITHOUT a signature check (custom build); 'update apply' installs it"),
        Install::Staged(v, key) => write!(out, "image   : {v} is staged (signed by key {}); 'update apply' installs it", key + 1),
        Install::Refused(r) => write!(out, "image   : the last upload was refused: {}", r.message()),
        Install::WriteFailed => write!(out, "image   : the last upload could not be written to flash"),
    };
}

/// What an image says about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub chip: Chip,
    pub version: Version,
}

/// The bytes of the info block: magic, chip (u16), major, minor, patch (u16 each), all little endian. The firmware puts this
/// into its image (`firmware/src/main.rs`); `const` so it can be a `static`.
pub const fn info_bytes(chip: Chip, v: Version) -> [u8; INFO_LEN] {
    let mut b = [0u8; INFO_LEN];
    let mut i = 0;
    while i < 8 {
        b[i] = MAGIC[i];
        i += 1;
    }
    let c = (chip as u16).to_le_bytes();
    let (ma, mi, pa) = (v.major.to_le_bytes(), v.minor.to_le_bytes(), v.patch.to_le_bytes());
    b[8] = c[0];
    b[9] = c[1];
    b[10] = ma[0];
    b[11] = ma[1];
    b[12] = mi[0];
    b[13] = mi[1];
    b[14] = pa[0];
    b[15] = pa[1];
    b
}

/// Finds the info block in the first `SCAN` bytes of an image (on a 4-byte boundary, as the linker places it).
pub fn find_info(head: &[u8]) -> Option<Info> {
    let head = &head[..head.len().min(SCAN)];
    let mut at = 0;
    while at + INFO_LEN <= head.len() {
        if head[at..at + 8] == MAGIC {
            let u = |o: usize| u16::from_le_bytes([head[at + o], head[at + o + 1]]);
            return Some(Info { chip: Chip::from_u16(u(8))?, version: Version { major: u(10), minor: u(12), patch: u(14) } });
        }
        at += 4;
    }
    None
}

/// Why an update file is not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Shorter than a signature plus a minimal image, or longer than the slot.
    BadLength,
    /// No info block (not a picoclock image, or too old to have one).
    NoInfo,
    WrongChip,
    /// The image is not newer than the running one (rollback protection).
    NotNewer,
    BadSignature,
}

impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Refusal::BadLength => "the file has the wrong length for an update",
            Refusal::NoInfo => "not a picoclock image (no info block)",
            Refusal::WrongChip => "the image is for the other chip",
            Refusal::NotNewer => "the image is not newer than the version that runs (no downgrades)",
            Refusal::BadSignature => "the signature is not valid for any trusted key",
        }
    }
}

/// Image length inside an update file of `file_len` bytes, if that is plausible for a slot of `slot` bytes.
pub fn image_len(file_len: usize, slot: usize) -> Result<usize, Refusal> {
    let n = file_len.checked_sub(SIGNATURE_LEN).ok_or(Refusal::BadLength)?;
    if n < 4096 || n > slot { Err(Refusal::BadLength) } else { Ok(n) }
}

/// What the clock demands of an update file. Official builds use `SIGNED`; a build made with the cargo feature `custom-images`
/// (README, "Custom builds") uses `CUSTOM`, so anyone can install their own build. The chip and the length are always checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The signature must be valid under a built-in key.
    pub require_signature: bool,
    /// The image must be strictly newer than the running one (rollback protection).
    pub require_newer: bool,
}

impl Policy {
    pub const SIGNED: Policy = Policy { require_signature: true, require_newer: true };
    pub const CUSTOM: Policy = Policy { require_signature: false, require_newer: false };
}

/// The key number reported for an image that was accepted without a signature check (`Policy::CUSTOM`).
pub const UNSIGNED: u8 = 255;

/// The rules for an image that has not been checked cryptographically yet: right chip and, if the policy asks, strictly newer.
pub fn check_info(info: &Info, chip: Chip, running: Version, policy: Policy) -> Result<(), Refusal> {
    if info.chip != chip {
        return Err(Refusal::WrongChip);
    }
    if policy.require_newer && info.version <= running {
        return Err(Refusal::NotNewer);
    }
    Ok(())
}

/// Public keys whose signatures the clock accepts: the signing key, then the backup key kept apart from it (ADR-0006, point 6).
/// Raw 32-byte Ed25519 keys; the OpenPGP fingerprints are in the ADR.
pub const KEYS: [[u8; 32]; 2] = [
    hex32(b"347fe5f208d22b3d4fb3e040ef9760dda05d1fc3f5cfaa8b3a2ce56f77aca933"),
    hex32(b"408f3847c547330335d35394d36c405c872a208a908c6cec5c2e44997c409f8b"),
];

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("not a lower-case hex digit"),
    }
}

const fn hex32(s: &[u8; 64]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nibble(s[2 * i]) << 4 | nibble(s[2 * i + 1]);
        i += 1;
    }
    out
}

/// Checks the signature over the SHA-512 digest of an image against `keys`; returns the index of the key that signed.
/// Strict verification: small-order keys and non-canonical signatures are refused.
pub fn verify(keys: &[[u8; 32]], digest: &[u8; 64], signature: &[u8; 64]) -> Option<usize> {
    let sig = Signature::from_bytes(signature);
    keys.iter().position(|k| VerifyingKey::from_bytes(k).is_ok_and(|vk| vk.verify_strict(digest, &sig).is_ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha512};

    fn image(chip: Chip, v: Version, len: usize) -> std::vec::Vec<u8> {
        let mut img = std::vec![0xA5u8; len];
        img[192..192 + INFO_LEN].copy_from_slice(&info_bytes(chip, v));
        img
    }
    fn v(a: u16, b: u16, c: u16) -> Version {
        Version { major: a, minor: b, patch: c }
    }
    fn digest(img: &[u8]) -> [u8; 64] {
        Sha512::digest(img).into()
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn the_running_version_is_a_constant_equal_to_the_parsed_one() {
        assert_eq!(RUNNING, Version::running());
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn the_info_block_is_found_and_read_back() {
        let img = image(Chip::Rp2350, v(0, 3, 1), 8192);
        assert_eq!(find_info(&img), Some(Info { chip: Chip::Rp2350, version: v(0, 3, 1) }));
        assert_eq!(info_bytes(Chip::Rp2040, v(1, 2, 3))[..8], MAGIC);
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn an_image_without_a_usable_info_block_has_none() {
        assert_eq!(find_info(&[0u8; 8192]), None);
        // a block placed after the scanned bytes does not count
        let mut late = std::vec![0u8; 8192];
        late[SCAN..SCAN + INFO_LEN].copy_from_slice(&info_bytes(Chip::Rp2040, v(1, 0, 0)));
        assert_eq!(find_info(&late), None);
        // an unknown chip number is not an image for either chip
        let mut odd = image(Chip::Rp2040, v(1, 0, 0), 4096);
        odd[192 + 8] = 9;
        assert_eq!(find_info(&odd), None);
        // not aligned to 4 bytes: the linker never places it so
        let mut unaligned = std::vec![0u8; 4096];
        unaligned[194..194 + INFO_LEN].copy_from_slice(&info_bytes(Chip::Rp2040, v(1, 0, 0)));
        assert_eq!(find_info(&unaligned), None);
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn only_a_newer_image_for_this_chip_is_accepted() {
        let new = Info { chip: Chip::Rp2350, version: v(0, 3, 0) };
        assert_eq!(check_info(&new, Chip::Rp2350, v(0, 2, 0), Policy::SIGNED), Ok(()));
        assert_eq!(check_info(&new, Chip::Rp2040, v(0, 2, 0), Policy::SIGNED), Err(Refusal::WrongChip));
        assert_eq!(check_info(&new, Chip::Rp2350, v(0, 3, 0), Policy::SIGNED), Err(Refusal::NotNewer), "the same version is not an update");
        assert_eq!(check_info(&new, Chip::Rp2350, v(0, 10, 0), Policy::SIGNED), Err(Refusal::NotNewer));
        let older = Info { chip: Chip::Rp2350, version: v(0, 9, 9) };
        assert_eq!(check_info(&older, Chip::Rp2350, v(0, 10, 0), Policy::SIGNED), Err(Refusal::NotNewer), "0.9.9 is older than 0.10.0 by number, not by text");
    }

    // VERIFIES: REQ-UPD-07
    #[test]
    fn a_custom_build_skips_the_signature_and_version_rules_but_never_the_chip() {
        let same = Info { chip: Chip::Rp2350, version: v(0, 2, 0) };
        assert_eq!(check_info(&same, Chip::Rp2350, v(0, 2, 0), Policy::CUSTOM), Ok(()), "a developer reinstalls the same version");
        let older = Info { chip: Chip::Rp2350, version: v(0, 1, 0) };
        assert_eq!(check_info(&older, Chip::Rp2350, v(0, 2, 0), Policy::CUSTOM), Ok(()));
        assert_eq!(check_info(&older, Chip::Rp2040, v(0, 2, 0), Policy::CUSTOM), Err(Refusal::WrongChip));
        assert!(Policy::SIGNED.require_signature && Policy::SIGNED.require_newer, "the official policy demands both");
        assert!(!Policy::CUSTOM.require_signature);
        let mut out = std::string::String::new();
        describe_install(&mut out, Install::Staged(v(0, 3, 0), UNSIGNED));
        assert!(out.contains("WITHOUT a signature check"), "{out}");
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn the_length_of_an_update_file_is_checked() {
        assert_eq!(image_len(SIGNATURE_LEN + 10_000, 20_000), Ok(10_000));
        assert_eq!(image_len(10, 20_000), Err(Refusal::BadLength));
        assert_eq!(image_len(SIGNATURE_LEN + 100, 20_000), Err(Refusal::BadLength), "too small for an image");
        assert_eq!(image_len(SIGNATURE_LEN + 20_001, 20_000), Err(Refusal::BadLength), "does not fit the slot");
        assert_eq!(image_len(0, 20_000), Err(Refusal::BadLength));
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn a_signature_is_accepted_from_a_trusted_key_and_nothing_else() {
        let sk1 = SigningKey::from_bytes(&[7u8; 32]);
        let sk2 = SigningKey::from_bytes(&[9u8; 32]);
        let stranger = SigningKey::from_bytes(&[11u8; 32]);
        let keys = [sk1.verifying_key().to_bytes(), sk2.verifying_key().to_bytes()];
        let img = image(Chip::Rp2350, v(0, 3, 0), 10_000);
        let d = digest(&img);
        assert_eq!(verify(&keys, &d, &sk1.sign(&d).to_bytes()), Some(0));
        assert_eq!(verify(&keys, &d, &sk2.sign(&d).to_bytes()), Some(1), "the backup key works too");
        assert_eq!(verify(&keys, &d, &stranger.sign(&d).to_bytes()), None);
        // another image under a valid signature
        let mut other = img.clone();
        other[5000] ^= 1;
        assert_eq!(verify(&keys, &digest(&other), &sk1.sign(&d).to_bytes()), None);
        // a damaged signature, an all-zero one, and a key list without the signer
        let mut bad = sk1.sign(&d).to_bytes();
        bad[10] ^= 0x40;
        assert_eq!(verify(&keys, &d, &bad), None);
        assert_eq!(verify(&keys, &d, &[0u8; 64]), None);
        assert_eq!(verify(&keys[1..], &d, &sk1.sign(&d).to_bytes()), None);
        assert_eq!(verify(&[], &d, &sk1.sign(&d).to_bytes()), None);
    }

    /// Signatures made on 2026-10-18 by the owner's real keys through gpg-agent () over the message
    /// "picoclock signing test vector 1": the format the release script produces, checked against the embedded keys.
    const REAL_MESSAGE: &[u8] = b"picoclock signing test vector 1";
    const REAL_SIG_PRIMARY: &str = "c6adce37d2514de5fc983717871e8f79f9e1b476746b675c6cf50c01c9b01a8d79e00e58568e97456a0f828b51968a5b1ed1c0fc6856b82fa0612d3262e57009";
    const REAL_SIG_BACKUP: &str = "fd34a5eeb6462fb6c42bfa59109639b7d2b14f3cdf740df90950f6f8e97977c6bd74d0b78dc18c06ab78db6b828647e78b5bbb9db72f17411eb67fadeaf7a902";

    fn unhex64(s: &str) -> [u8; 64] {
        let mut out = [0u8; 64];
        for i in 0..64 {
            out[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn signatures_made_by_the_real_keys_verify_against_the_embedded_keys() {
        let d = digest(REAL_MESSAGE);
        assert_eq!(verify(&KEYS, &d, &unhex64(REAL_SIG_PRIMARY)), Some(0));
        assert_eq!(verify(&KEYS, &d, &unhex64(REAL_SIG_BACKUP)), Some(1));
        let other = digest(b"picoclock signing test vector 2");
        assert_eq!(verify(&KEYS, &other, &unhex64(REAL_SIG_PRIMARY)), None);
    }

    // VERIFIES: REQ-UPD-05
    #[test]
    fn the_embedded_keys_are_valid_distinct_points() {
        assert_ne!(KEYS[0], KEYS[1]);
        for k in KEYS {
            assert!(VerifyingKey::from_bytes(&k).is_ok());
        }
    }
}
