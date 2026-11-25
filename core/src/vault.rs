use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    aead, hkdf,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 4] = b"VLO1";
const HEADER_BYTES: usize = 36;
const TAG_BYTES: usize = 16;
pub const MAX_PLAINTEXT_BYTES: usize = 16 * 1024 * 1024;
const CODE_PREFIX: &str = "vl1_";
const KEY_BYTES: usize = 32;
const CHECK_BYTES: usize = 6;
const RECOVERY_BYTES: usize = 16 + 4 + KEY_BYTES + CHECK_BYTES;
const CODE_BYTES: usize = RECOVERY_BYTES * 4 / 3 + 1;

pub type ObjectId = [u8; 16];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultError {
    Malformed,
    WrongObject,
    Authentication,
    Entropy,
    TooLarge,
    Generation,
}

pub struct Vault {
    id: [u8; 16],
    generation: u32,
    secret: Zeroizing<[u8; KEY_BYTES]>,
}

pub struct SealedObject {
    id: ObjectId,
    envelope: Vec<u8>,
}

impl SealedObject {
    pub fn id(&self) -> &ObjectId {
        &self.id
    }

    pub fn envelope(&self) -> &[u8] {
        &self.envelope
    }
}

impl Vault {
    pub fn create() -> Result<Self, VaultError> {
        Self::generate(random_id()?, 1)
    }

    pub fn rotate(&self) -> Result<Self, VaultError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(VaultError::Generation)?;
        Self::generate(self.id, generation)
    }

    fn generate(id: [u8; 16], generation: u32) -> Result<Self, VaultError> {
        let mut secret = Zeroizing::new([0; KEY_BYTES]);
        random_fill(secret.as_mut())?;
        Ok(Self {
            id,
            generation,
            secret,
        })
    }

    pub fn id(&self) -> &[u8; 16] {
        &self.id
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn recovery_code(&self) -> Zeroizing<String> {
        let mut bytes = Zeroizing::new([0_u8; RECOVERY_BYTES]);
        bytes[..16].copy_from_slice(&self.id);
        bytes[16..20].copy_from_slice(&self.generation.to_be_bytes());
        bytes[20..52].copy_from_slice(self.secret.as_ref());
        let checksum = recovery_checksum(&bytes[..52]);
        bytes[52..].copy_from_slice(&checksum);
        let mut output = Zeroizing::new(String::with_capacity(CODE_PREFIX.len() + CODE_BYTES));
        output.push_str(CODE_PREFIX);
        URL_SAFE_NO_PAD.encode_string(bytes.as_ref(), &mut output);
        output
    }

    pub fn recover(code: &str) -> Result<Self, VaultError> {
        let encoded = code
            .strip_prefix(CODE_PREFIX)
            .ok_or(VaultError::Malformed)?;
        if encoded.len() != CODE_BYTES {
            return Err(VaultError::Malformed);
        }
        let mut bytes = Zeroizing::new([0_u8; RECOVERY_BYTES]);
        let decoded_bytes = URL_SAFE_NO_PAD
            .decode_slice(encoded, bytes.as_mut())
            .map_err(|_| VaultError::Malformed)?;
        if decoded_bytes != RECOVERY_BYTES || bytes[52..] != recovery_checksum(&bytes[..52]) {
            return Err(VaultError::Malformed);
        }
        let id = read_array(&bytes[..16])?;
        let generation = u32::from_be_bytes(read_array(&bytes[16..20])?);
        if generation == 0 {
            return Err(VaultError::Generation);
        }
        let mut secret = Zeroizing::new([0; KEY_BYTES]);
        secret.copy_from_slice(&bytes[20..52]);
        Ok(Self {
            id,
            generation,
            secret,
        })
    }

    pub fn seal(&self, plaintext: &[u8]) -> Result<SealedObject, VaultError> {
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(VaultError::TooLarge);
        }
        let id = random_id()?;
        let mut nonce = [0; aead::NONCE_LEN];
        random_fill(&mut nonce)?;
        self.seal_with(id, nonce, plaintext)
    }

    fn seal_with(
        &self,
        id: ObjectId,
        nonce: [u8; aead::NONCE_LEN],
        plaintext: &[u8],
    ) -> Result<SealedObject, VaultError> {
        let key = self.object_key(&id)?;
        let mut envelope = Vec::with_capacity(HEADER_BYTES + plaintext.len() + TAG_BYTES);
        envelope.extend_from_slice(MAGIC);
        envelope.extend_from_slice(&self.generation.to_be_bytes());
        envelope.extend_from_slice(&id);
        envelope.extend_from_slice(&nonce);
        let aad = self.aad(&envelope);
        envelope.extend_from_slice(plaintext);
        let tag = match key.seal_in_place_separate_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(&aad),
            &mut envelope[HEADER_BYTES..],
        ) {
            Ok(tag) => tag,
            Err(_) => {
                envelope.zeroize();
                return Err(VaultError::Authentication);
            }
        };
        envelope.extend_from_slice(tag.as_ref());
        Ok(SealedObject { id, envelope })
    }

    pub fn open(
        &self,
        expected: &ObjectId,
        envelope: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if envelope.len() < HEADER_BYTES + TAG_BYTES || &envelope[..4] != MAGIC {
            return Err(VaultError::Malformed);
        }
        if envelope.len() > HEADER_BYTES + TAG_BYTES + MAX_PLAINTEXT_BYTES {
            return Err(VaultError::TooLarge);
        }
        let header = &envelope[..HEADER_BYTES];
        if header[8..24] != *expected {
            return Err(VaultError::WrongObject);
        }
        if header[4..8] != self.generation.to_be_bytes() {
            return Err(VaultError::Generation);
        }
        let nonce = aead::Nonce::assume_unique_for_key(read_array(&header[24..36])?);
        let aad = self.aad(header);
        let mut plaintext = Zeroizing::new(envelope[HEADER_BYTES..].to_vec());
        let length = self
            .object_key(expected)?
            .open_in_place(nonce, aead::Aad::from(&aad), &mut plaintext)
            .map_err(|_| VaultError::Authentication)?
            .len();
        plaintext[length..].zeroize();
        plaintext.truncate(length);
        Ok(plaintext)
    }

    pub(crate) fn reopen(
        &self,
        envelope: Vec<u8>,
    ) -> Result<(SealedObject, Zeroizing<Vec<u8>>), VaultError> {
        let id: ObjectId = read_array(envelope.get(8..24).ok_or(VaultError::Malformed)?)?;
        let plaintext = self.open(&id, &envelope)?;
        Ok((SealedObject { id, envelope }, plaintext))
    }

    fn aad(&self, header: &[u8]) -> Vec<u8> {
        [
            b"VoiceLedger/object/v1".as_slice(),
            self.id.as_slice(),
            header,
        ]
        .concat()
    }

    fn object_key(&self, id: &ObjectId) -> Result<aead::LessSafeKey, VaultError> {
        struct KeyLength;
        impl hkdf::KeyType for KeyLength {
            fn len(&self) -> usize {
                KEY_BYTES
            }
        }
        let generation = self.generation.to_be_bytes();
        let info: [&[u8]; 3] = [b"VoiceLedger/object-key/v1", &generation, id];
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &self.id).extract(self.secret.as_ref());
        let mut secret = Zeroizing::new([0_u8; KEY_BYTES]);
        prk.expand(&info, KeyLength)
            .and_then(|key| key.fill(secret.as_mut()))
            .map_err(|_| VaultError::Authentication)?;
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, secret.as_ref())
            .map_err(|_| VaultError::Authentication)?;
        Ok(aead::LessSafeKey::new(key))
    }
}

fn recovery_checksum(bytes: &[u8]) -> [u8; CHECK_BYTES] {
    let mut sha = Sha256::new();
    sha.update(b"VoiceLedger/recovery/v1");
    sha.update(bytes);
    let mut digest = sha.finalize();
    let mut out = [0; CHECK_BYTES];
    out.copy_from_slice(&digest[..CHECK_BYTES]);
    digest[..].zeroize();
    out
}

fn read_array<const N: usize>(value: &[u8]) -> Result<[u8; N], VaultError> {
    value.try_into().map_err(|_| VaultError::Malformed)
}

fn random_fill(destination: &mut [u8]) -> Result<(), VaultError> {
    SystemRandom::new()
        .fill(destination)
        .map_err(|_| VaultError::Entropy)
}

fn random_id() -> Result<[u8; 16], VaultError> {
    let mut id = [0; 16];
    random_fill(&mut id)?;
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    Ok(id)
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncScopes {
    pub enabled: bool,
    pub text: bool,
    pub raw_audio: bool,
    pub speaker_profiles: bool,
}

impl Default for SyncScopes {
    fn default() -> Self {
        Self {
            enabled: false,
            text: true,
            raw_audio: false,
            speaker_profiles: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(value: &str) -> Vec<u8> {
        assert!(value.len().is_multiple_of(2));
        (0..value.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&value[offset..offset + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn independent_golden_vector_and_clean_process_recovery() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../contracts/fixtures/vault-v1.json")).unwrap();
        let field = |name| fixture[name].as_str().unwrap();
        let restored = Vault::recover(field("recovery_code")).unwrap();
        let object = read_array(&hex(field("object_id"))).unwrap();
        assert_eq!(restored.id().as_slice(), hex(field("vault_id")));
        assert_eq!(restored.generation(), 7);
        assert_eq!(restored.recovery_code().as_str(), field("recovery_code"));
        let nonce = read_array(&hex(field("nonce"))).unwrap();
        let bytes = field("plaintext").as_bytes();
        let expected = hex(field("envelope"));
        assert_eq!(
            restored.seal_with(object, nonce, bytes).unwrap().envelope(),
            expected
        );
        assert_eq!(restored.open(&object, &expected).unwrap().as_slice(), bytes);
    }

    #[test]
    fn rejects_tamper_wrong_vault_object_generation_and_invalid_lengths() {
        let vault = Vault::create().unwrap();
        let wrong_vault = Vault::create().unwrap();
        let rotated = vault.rotate().unwrap();
        let sealed = vault.seal(b"synthetic record").unwrap();
        assert_eq!(rotated.id(), vault.id());
        assert_eq!(rotated.generation(), 2);
        assert!(matches!(
            wrong_vault.open(sealed.id(), sealed.envelope()),
            Err(VaultError::Authentication)
        ));
        assert!(matches!(
            rotated.open(sealed.id(), sealed.envelope()),
            Err(VaultError::Generation)
        ));
        assert!(matches!(
            vault.open(&[0; 16], sealed.envelope()),
            Err(VaultError::WrongObject)
        ));
        assert!(matches!(
            vault.open(sealed.id(), b"VLO1"),
            Err(VaultError::Malformed)
        ));
        for offset in [0, 4, 8, 24, HEADER_BYTES, sealed.envelope().len() - 1] {
            let mut tampered = sealed.envelope().to_vec();
            tampered[offset] ^= 1;
            assert!(vault.open(sealed.id(), &tampered).is_err());
        }
        let mut code = vault.recovery_code();
        code.replace_range(CODE_PREFIX.len()..CODE_PREFIX.len() + 1, "!");
        assert!(matches!(Vault::recover(&code), Err(VaultError::Malformed)));
    }

    #[test]
    fn fresh_object_ids_and_nonce_for_each_encryption() {
        use std::collections::BTreeSet;
        let vault = Vault::create().unwrap();
        let mut headers = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for _ in 0..128 {
            let sealed = vault.seal(b"same synthetic bytes").unwrap();
            assert!(ids.insert(*sealed.id()));
            assert!(headers.insert(sealed.envelope()[24..36].to_vec()));
            assert_eq!(
                vault
                    .open(sealed.id(), sealed.envelope())
                    .unwrap()
                    .as_slice(),
                b"same synthetic bytes"
            );
        }
        let scope = SyncScopes::default();
        assert!(!scope.enabled && scope.text && !scope.raw_audio && !scope.speaker_profiles);
    }
}
