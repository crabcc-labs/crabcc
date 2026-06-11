use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct KeyFile {
    static_secret: [u8; 32],
    static_pub: [u8; 32],
    ed_secret: [u8; 32],
}

pub struct NodeKeys {
    pub static_secret: [u8; 32],
    pub static_pub: [u8; 32],
    pub ed_secret: [u8; 32],
    pub node_id: [u8; 32],
}

impl NodeKeys {
    pub fn load_or_generate(path: &Path) -> Result<Self> {
        if path.exists() {
            Self::load(path)
        } else {
            let keys = Self::generate()?;
            keys.save(path)?;
            Ok(keys)
        }
    }

    fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).context("read key file")?;
        let kf: KeyFile = postcard::from_bytes(&bytes).context("decode key file")?;
        Ok(Self::from_keyfile(kf))
    }

    fn generate() -> Result<Self> {
        let params: snow::params::NoiseParams =
            "Noise_IK_25519_ChaChaPoly_BLAKE2s".parse().context("parse noise params")?;
        let builder = snow::Builder::new(params);
        let kp = builder.generate_keypair().context("generate keypair")?;

        let mut static_secret = [0u8; 32];
        let mut static_pub = [0u8; 32];
        static_secret.copy_from_slice(&kp.private);
        static_pub.copy_from_slice(&kp.public);

        let mut ed_secret = [0u8; 32];
        getrandom::getrandom(&mut ed_secret).context("getrandom ed_secret")?;

        Ok(Self::from_parts(static_secret, static_pub, ed_secret))
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create key dir")?;
        }
        let kf = KeyFile {
            static_secret: self.static_secret,
            static_pub: self.static_pub,
            ed_secret: self.ed_secret,
        };
        let bytes = postcard::to_allocvec(&kf).context("encode key file")?;
        std::fs::write(path, &bytes).context("write key file")?;
        Ok(())
    }

    fn from_keyfile(kf: KeyFile) -> Self {
        Self::from_parts(kf.static_secret, kf.static_pub, kf.ed_secret)
    }

    fn from_parts(static_secret: [u8; 32], static_pub: [u8; 32], ed_secret: [u8; 32]) -> Self {
        let node_id: [u8; 32] = blake3::hash(&static_pub).into();
        Self { static_secret, static_pub, ed_secret, node_id }
    }
}
