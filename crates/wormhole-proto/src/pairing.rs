/// Pairing channel protocol — the rendezvous frames exchanged on
/// `pair/<nameplate>` before the main Noise session exists.
///
/// Both sides send `PairingHello` immediately on connect so they can negotiate
/// the SPAKE2 version and identify their role before any PAKE exchange.
/// (Inspired by magic-wormhole's version-negotiation first frame.)
use serde::{Deserialize, Serialize};

/// Current PAKE protocol version. Bump when the SPAKE2 group or KDF changes.
pub const PAIRING_VERSION: u8 = 1;

/// First frame sent on the pairing channel by both sides.
/// Relay delivers it verbatim; it is not encrypted (Noise doesn't exist yet).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairingHello {
    /// PAKE version this side supports. If versions differ, the higher-version
    /// side MUST abort with `PairingError::VersionMismatch` — no downgrade.
    pub version: u8,
    /// Role of the sender.
    pub role: PairingRole,
}

impl PairingHello {
    pub fn node() -> Self {
        Self {
            version: PAIRING_VERSION,
            role: PairingRole::Node,
        }
    }

    pub fn operator() -> Self {
        Self {
            version: PAIRING_VERSION,
            role: PairingRole::Operator,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingRole {
    /// The crabcc node initiating the pairing (generated the code).
    Node,
    /// The operator entering the code.
    Operator,
}

/// Error conditions surfaced during the pairing handshake.
/// Serialized by the relay back to both peers on abort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingError {
    /// Versions are incompatible — no downgrade allowed.
    VersionMismatch { ours: u8, theirs: u8 },
    /// Nameplate already claimed (single-use enforced).
    NameplateAlreadyClaimed,
    /// Nameplate TTL expired (120s window).
    NameplateExpired,
    /// SPAKE2 transcript MAC verification failed — wrong code or active MITM.
    /// The relay inserts a short random delay (100–300ms) before sending this
    /// error so timing does not reveal whether the nameplate exists.
    MacVerificationFailed,
    /// Both sides presented the same role (two nodes or two operators).
    RoleConflict,
}

impl PairingError {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// Result of a successful pairing exchange, held in memory until the Noise
/// session is established. Never written to disk in this form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairingResult {
    /// SPAKE2-derived shared secret (before the channel-binding step).
    /// Input to the channel binding MAC; not used directly.
    pub pake_key: [u8; 32],
    /// Peer's static X25519 public key (for Noise IK).
    pub peer_static_pub: [u8; 32],
    /// Peer's Ed25519 public key (for presence signing / biscuit verification).
    pub peer_ed_pub: [u8; 32],
}
