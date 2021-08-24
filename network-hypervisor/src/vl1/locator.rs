use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use crate::vl1::{Address, Endpoint, Identity};
use crate::vl1::buffer::Buffer;
use crate::vl1::protocol::PACKET_SIZE_MAX;

/// A signed object generated by nodes to inform the network where they may be found.
///
/// By default this will just enumerate the roots used by this node, but nodes with
/// static IPs can also list physical IP/port addresses where they can be reached with
/// no involvement from a root at all.
#[derive(Clone, PartialEq, Eq)]
pub struct Locator {
    subject: Address,
    signer: Address,
    timestamp: i64,
    endpoints: Vec<Endpoint>,
    signature: Vec<u8>,
}

impl Locator {
    /// Create and sign a new locator.
    ///
    /// If a node is creating its own locator the subject will be the address from the
    /// signer identity. Proxy signing is when these do not match and is only done by
    /// roots to create locators for old versions of ZeroTier that do not create their
    /// own. Proxy locators are always superseded by self-signed locators.
    ///
    /// This returns None if an error occurs, which can only be something indicating a
    /// bug like too many endpoints or the identity lacking its secret keys.
    pub fn create(signer_identity: &Identity, subject: Address, ts: i64, endpoints: &[Endpoint]) -> Option<Locator> {
        let mut loc = Locator {
            subject,
            signer: signer_identity.address(),
            timestamp: ts,
            endpoints: endpoints.to_vec(),
            signature: Vec::new()
        };
        loc.endpoints.sort_unstable();
        loc.endpoints.dedup();

        let mut buf: Buffer<{ PACKET_SIZE_MAX }> = Buffer::new();
        if loc.marshal_internal(&mut buf, true).is_err() {
            return None;
        }
        signer_identity.sign(buf.as_bytes()).map(|sig| {
            loc.signature = sig;
            loc
        })
    }

    /// Check if this locator should replace one that is already known.
    ///
    /// Self-signed locators always replace proxy-signed locators. Otherwise locators
    /// with later timestamps replace locators with earlier timestamps.
    pub fn should_replace(&self, other: &Self) -> bool {
        if self.is_proxy_signed() == other.is_proxy_signed() {
            self.timestamp > other.timestamp
        } else {
            other.is_proxy_signed()
        }
    }

    #[inline(always)]
    pub fn subject(&self) -> Address { self.subject }

    #[inline(always)]
    pub fn signer(&self) -> Address { self.signer }

    #[inline(always)]
    pub fn is_proxy_signed(&self) -> bool { self.subject != self.signer }

    #[inline(always)]
    pub fn timestamp(&self) -> i64 { self.timestamp }

    #[inline(always)]
    pub fn endpoints(&self) -> &[Endpoint] { self.endpoints.as_slice() }

    pub fn verify_signature(&self, signer_identity: &Identity) -> bool {
        let mut buf: Buffer<{ PACKET_SIZE_MAX }> = Buffer::new();
        if self.marshal_internal(&mut buf, true).is_ok() {
            if signer_identity.address() == self.signer {
                signer_identity.verify(buf.as_bytes(), self.signature.as_slice())
            } else {
                false
            }
        } else {
            false
        }
    }

    fn marshal_internal<const BL: usize>(&self, buf: &mut Buffer<BL>, exclude_signature: bool) -> std::io::Result<()> {
        self.subject.marshal(buf)?;
        self.signer.marshal(buf)?;
        buf.append_u64(self.timestamp as u64)?;
        buf.append_varint(self.endpoints.len() as u64)?;
        for e in self.endpoints.iter() {
            e.marshal(buf)?;
        }
        buf.append_varint(0)?; // length of any additional fields
        if !exclude_signature {
            buf.append_varint(self.signature.len() as u64)?;
            buf.append_bytes(self.signature.as_slice())?;
        }
        Ok(())
    }

    #[inline(always)]
    pub(crate) fn marshal<const BL: usize>(&self, buf: &mut Buffer<BL>) -> std::io::Result<()> { self.marshal_internal(buf, false) }

    pub(crate) fn unmarshal<const BL: usize>(buf: &Buffer<BL>, cursor: &mut usize) -> std::io::Result<Self> {
        let subject = Address::unmarshal(buf, cursor)?;
        let signer = Address::unmarshal(buf, cursor)?;
        if subject.is_none() || signer.is_none() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid subject or signer address"));
        }
        let timestamp = buf.read_u64(cursor)? as i64;
        let endpoint_count = buf.read_varint(cursor)? as usize;
        let mut endpoints: Vec<Endpoint> = Vec::new();
        for _ in 0..endpoint_count {
            endpoints.push(Endpoint::unmarshal(buf, cursor)?);
        }
        *cursor += buf.read_varint(cursor)? as usize;
        let signature_len = buf.read_varint(cursor)? as usize;
        let signature = buf.read_bytes(signature_len, cursor)?;
        Ok(Locator {
            subject: subject.unwrap(),
            signer: signer.unwrap(),
            timestamp,
            endpoints,
            signature: signature.to_vec(),
        })
    }
}

impl Ord for Locator {
    /// Natural sort order is in order of subject, then ascending order of timestamp, then signer, then endpoints.
    fn cmp(&self, other: &Self) -> Ordering {
        let a = self.subject.cmp(&other.subject);
        if a == Ordering::Equal {
            let b = self.timestamp.cmp(&other.timestamp);
            if b == Ordering::Equal {
                let c = self.signer.cmp(&other.signer);
                if c == Ordering::Equal {
                    self.endpoints.cmp(&other.endpoints)
                } else {
                    c
                }
            } else {
                b
            }
        } else {
            a
        }
    }
}

impl PartialOrd for Locator {
    #[inline(always)]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Hash for Locator {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if !self.signature.is_empty() {
            state.write(self.signature.as_slice());
        } else {
            state.write_u64(self.signer.to_u64());
            state.write_i64(self.timestamp);
            for e in self.endpoints.iter() {
                e.hash(state);
            }
        }
    }
}