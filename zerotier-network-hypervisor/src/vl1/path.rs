// (c) 2020-2022 ZeroTier, Inc. -- currently propritery pending actual release and licensing. See LICENSE.md.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use lazy_static::lazy_static;
use parking_lot::Mutex;

use crate::vl1::endpoint::Endpoint;
use crate::vl1::fragmentedpacket::FragmentedPacket;
use crate::vl1::node::*;
use crate::vl1::protocol::*;

pub(crate) const SERVICE_INTERVAL_MS: i64 = PATH_KEEPALIVE_INTERVAL;

pub(crate) enum PathServiceResult {
    Ok,
    Dead,
    NeedsKeepalive,
}

lazy_static! {
    static ref INSTANCE_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
}

/// A remote endpoint paired with a local socket and a local interface.
/// These are maintained in Node and canonicalized so that all unique paths have
/// one and only one unique path object. That enables statistics to be tracked
/// for them and uniform application of things like keepalives.
pub struct Path<SI: SystemInterface> {
    pub endpoint: Endpoint,
    pub local_socket: SI::LocalSocket,
    pub local_interface: SI::LocalInterface,
    pub(crate) internal_instance_id: usize, // arbitrary local ID that should be globally unique to a given path object instance
    last_send_time_ticks: AtomicI64,
    last_receive_time_ticks: AtomicI64,
    create_time_ticks: i64,
    fragmented_packets: Mutex<HashMap<PacketId, FragmentedPacket, PacketIdHasher>>,
}

impl<SI: SystemInterface> Path<SI> {
    pub fn new(endpoint: Endpoint, local_socket: SI::LocalSocket, local_interface: SI::LocalInterface, time_ticks: i64) -> Self {
        Self {
            endpoint,
            local_socket,
            local_interface,
            internal_instance_id: INSTANCE_ID_COUNTER.fetch_add(1, Ordering::SeqCst),
            last_send_time_ticks: AtomicI64::new(crate::util::NEVER_HAPPENED_TICKS),
            last_receive_time_ticks: AtomicI64::new(crate::util::NEVER_HAPPENED_TICKS),
            create_time_ticks: time_ticks,
            fragmented_packets: Mutex::new(HashMap::with_capacity_and_hasher(4, PacketIdHasher(zerotier_core_crypto::random::xorshift64_random()))),
        }
    }

    /// Receive a fragment and return a FragmentedPacket if the entire packet was assembled.
    /// This returns None if more fragments are needed to assemble the packet.
    pub(crate) fn receive_fragment(&self, packet_id: PacketId, fragment_no: u8, fragment_expecting_count: u8, packet: PooledPacketBuffer, time_ticks: i64) -> Option<FragmentedPacket> {
        let mut fp = self.fragmented_packets.lock();

        // Discard some old waiting packets if the total incoming fragments for a path exceeds a
        // sanity limit. This is to prevent memory exhaustion DOS attacks.
        let fps = fp.len();
        if fps > packet_constants::FRAGMENT_MAX_INBOUND_PACKETS_PER_PATH {
            let mut entries: Vec<(i64, u64)> = Vec::new();
            entries.reserve(fps);
            for f in fp.iter() {
                entries.push((f.1.ts_ticks, *f.0));
            }
            entries.sort_unstable_by(|a, b| (*a).0.cmp(&(*b).0));
            for i in 0..(fps / 3) {
                let _ = fp.remove(&(*entries.get(i).unwrap()).1);
            }
        }

        if fp.entry(packet_id).or_insert_with(|| FragmentedPacket::new(time_ticks)).add_fragment(packet, fragment_no, fragment_expecting_count) {
            fp.remove(&packet_id)
        } else {
            None
        }
    }

    #[inline(always)]
    pub(crate) fn log_receive_anything(&self, time_ticks: i64) {
        self.last_receive_time_ticks.store(time_ticks, Ordering::Relaxed);
    }

    #[inline(always)]
    pub(crate) fn log_send_anything(&self, time_ticks: i64) {
        self.last_send_time_ticks.store(time_ticks, Ordering::Relaxed);
    }

    pub(crate) fn service(&self, time_ticks: i64) -> PathServiceResult {
        self.fragmented_packets.lock().retain(|_, frag| (time_ticks - frag.ts_ticks) < packet_constants::FRAGMENT_EXPIRATION);
        if (time_ticks - self.last_receive_time_ticks.load(Ordering::Relaxed)) < PATH_EXPIRATION_TIME {
            if (time_ticks - self.last_send_time_ticks.load(Ordering::Relaxed)) >= PATH_KEEPALIVE_INTERVAL {
                self.last_send_time_ticks.store(time_ticks, Ordering::Relaxed);
                PathServiceResult::NeedsKeepalive
            } else {
                PathServiceResult::Ok
            }
        } else if (time_ticks - self.create_time_ticks) < PATH_EXPIRATION_TIME {
            PathServiceResult::Ok
        } else {
            PathServiceResult::Dead
        }
    }
}

#[repr(transparent)]
struct PacketIdHasher(u64);

impl Hasher for PacketIdHasher {
    #[inline(always)]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline(always)]
    fn write(&mut self, _: &[u8]) {
        panic!("u64 only");
    }

    #[inline(always)]
    fn write_u64(&mut self, i: u64) {
        let mut x = self.0.wrapping_add(i);
        x ^= x.wrapping_shl(13);
        x ^= x.wrapping_shr(7);
        x ^= x.wrapping_shl(17);
        self.0 = x;
    }
}

impl BuildHasher for PacketIdHasher {
    type Hasher = Self;

    #[inline(always)]
    fn build_hasher(&self) -> Self::Hasher {
        Self(0)
    }
}