//! The resolver a client runs for itself.
//!
//! The operating system is told to use [`GATEWAY_V4`](crate::proto::GATEWAY_V4) for DNS. That
//! address has nothing behind it: queries to it are picked out of the packet stream here, answered
//! from a small cache if possible, and otherwise sent on through the tunnel to the upstream
//! resolver, which is 1.1.1.1 unless the server says otherwise.
//!
//! It only handles UDP. A TCP query, which follows a truncated reply, goes through the tunnel like
//! any other connection, and the server's NAT sends it to the same upstream resolver.

use crate::checksum;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

const IPV4_HEADER_LEN: usize = 20;
const UDP_HEADER_LEN: usize = 8;
const DNS_HEADER_LEN: usize = 12;
const DNS_PORT: u16 = 53;
/// The largest DNS reply to ask for. It's the size the DNS flag day settled on, and it fits inside
/// the tunnel's MTU. A bigger reply couldn't get through, and the query would have to time out.
const MAX_EDNS_UDP_SIZE: u16 = 1232;
const DNS_TYPE_OPT: u16 = 41;

const MAX_CACHE_ENTRIES: usize = 512;
const MAX_CACHE_TTL: Duration = Duration::from_secs(300);
const MAX_PENDING: usize = 512;
const PENDING_TIMEOUT: Duration = Duration::from_secs(15);

/// What to do with a packet the operating system sent.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// It isn't for the resolver. Send it through the tunnel unchanged.
    Pass,
    /// Send this query through the tunnel instead.
    Query(Vec<u8>),
    /// Hand this reply straight back to the operating system.
    Reply(Vec<u8>),
    /// It was for the resolver but can't be answered, so drop it.
    Drop,
}

/// What to do with a packet that arrived through the tunnel.
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    /// It isn't for the resolver. Give it to the operating system unchanged.
    Pass,
    /// Give the operating system this reply instead.
    Reply(Vec<u8>),
    /// It was addressed to the resolver, but nothing is waiting for it.
    Drop,
}

struct Pending {
    client: Ipv4Addr,
    client_port: u16,
    key: Option<Vec<u8>>,
    sent: Instant,
}

struct CacheEntry {
    response: Vec<u8>,
    expires: Instant,
}

pub struct DnsResolver {
    gateway: Ipv4Addr,
    source: Ipv4Addr,
    upstream: Ipv4Addr,
    next_port: u16,
    pending: HashMap<u16, Pending>,
    cache: HashMap<Vec<u8>, CacheEntry>,
}

impl DnsResolver {
    /// `gateway` is the address the OS sends queries to, `source` is where queries to `upstream`
    /// appear to come from. `source` must not be an address the OS itself uses.
    pub fn new(gateway: Ipv4Addr, source: Ipv4Addr, upstream: Ipv4Addr) -> Self {
        DnsResolver { gateway, source, upstream, next_port: 1024, pending: HashMap::new(), cache: HashMap::new() }
    }

    pub fn outgoing(&mut self, packet: &[u8], now: Instant) -> Outgoing {
        let Some(udp) = Udp::parse(packet) else {
            return Outgoing::Pass;
        };
        if udp.dst != self.gateway || udp.dst_port != DNS_PORT {
            return Outgoing::Pass;
        }
        if udp.payload.len() < DNS_HEADER_LEN {
            return Outgoing::Drop;
        }

        let key = cache_key(udp.payload);
        if let Some(entry) = key.as_ref().and_then(|key| self.cache.get(key)) {
            if entry.expires > now {
                let mut response = entry.response.clone();
                // Answer with this query's ID and spelling of the name, in case it mixes case.
                response[..2].copy_from_slice(&udp.payload[..2]);
                if let Some(end) = question_end(udp.payload) {
                    response[DNS_HEADER_LEN..end].copy_from_slice(&udp.payload[DNS_HEADER_LEN..end]);
                }
                return Outgoing::Reply(build_udp(self.gateway, DNS_PORT, udp.src, udp.src_port, &response));
            }
            if let Some(key) = &key {
                self.cache.remove(key);
            }
        }

        self.expire_pending(now);
        if self.pending.len() >= MAX_PENDING {
            return Outgoing::Drop;
        }
        let port = self.allocate_port();
        self.pending.insert(port, Pending { client: udp.src, client_port: udp.src_port, key, sent: now });
        let mut query = udp.payload.to_vec();
        limit_edns_size(&mut query);
        Outgoing::Query(build_udp(self.source, port, self.upstream, DNS_PORT, &query))
    }

    pub fn incoming(&mut self, packet: &[u8], now: Instant) -> Incoming {
        let Some(udp) = Udp::parse(packet) else {
            return Incoming::Pass;
        };
        if udp.dst != self.source {
            return Incoming::Pass;
        }
        if udp.src != self.upstream || udp.src_port != DNS_PORT {
            return Incoming::Drop;
        }
        let Some(pending) = self.pending.remove(&udp.dst_port) else {
            return Incoming::Drop;
        };
        if let (Some(key), Some(ttl)) = (pending.key, cacheable_ttl(udp.payload)) {
            self.remember(key, udp.payload.to_vec(), now + ttl, now);
        }
        Incoming::Reply(build_udp(self.gateway, DNS_PORT, pending.client, pending.client_port, udp.payload))
    }

    /// Follows the server, in case it changed its mind between connections.
    pub fn set_upstream(&mut self, upstream: Ipv4Addr) {
        if upstream != self.upstream {
            self.upstream = upstream;
            self.cache.clear();
        }
    }

    fn remember(&mut self, key: Vec<u8>, response: Vec<u8>, expires: Instant, now: Instant) {
        if self.cache.len() >= MAX_CACHE_ENTRIES {
            self.cache.retain(|_, entry| entry.expires > now);
            if self.cache.len() >= MAX_CACHE_ENTRIES {
                self.cache.clear();
            }
        }
        self.cache.insert(key, CacheEntry { response, expires });
    }

    fn expire_pending(&mut self, now: Instant) {
        self.pending.retain(|_, pending| now.duration_since(pending.sent) < PENDING_TIMEOUT);
    }

    fn allocate_port(&mut self) -> u16 {
        loop {
            let port = self.next_port;
            self.next_port = if port == u16::MAX { 1024 } else { port + 1 };
            if !self.pending.contains_key(&port) {
                return port;
            }
        }
    }
}

struct Udp<'a> {
    src: Ipv4Addr,
    dst: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &'a [u8],
}

impl<'a> Udp<'a> {
    /// Returns the datagram in an unfragmented IPv4 UDP packet, and `None` for anything else.
    fn parse(packet: &'a [u8]) -> Option<Self> {
        if packet.len() < IPV4_HEADER_LEN + UDP_HEADER_LEN || packet[0] >> 4 != 4 || packet[9] != 17 {
            return None;
        }
        let header_len = usize::from(packet[0] & 0x0f) * 4;
        let fragment = u16::from_be_bytes([packet[6], packet[7]]);
        if header_len < IPV4_HEADER_LEN || fragment & 0x3fff != 0 {
            return None;
        }
        let total_len = usize::from(u16::from_be_bytes([packet[2], packet[3]])).min(packet.len());
        let udp = packet.get(header_len..total_len)?;
        if udp.len() < UDP_HEADER_LEN {
            return None;
        }
        let udp_len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
        Some(Udp {
            src: Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]),
            dst: Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]),
            src_port: u16::from_be_bytes([udp[0], udp[1]]),
            dst_port: u16::from_be_bytes([udp[2], udp[3]]),
            payload: udp.get(UDP_HEADER_LEN..udp_len)?,
        })
    }
}

/// Builds an IPv4 UDP packet with both checksums filled in.
pub fn build_udp(src: Ipv4Addr, src_port: u16, dst: Ipv4Addr, dst_port: u16, payload: &[u8]) -> Vec<u8> {
    let udp_len = UDP_HEADER_LEN + payload.len();
    let total_len = IPV4_HEADER_LEN + udp_len;
    let mut packet = vec![0; total_len];

    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    packet[6] = 0x40; // don't fragment
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&src.octets());
    packet[16..20].copy_from_slice(&dst.octets());
    let header_sum = checksum::of(&packet[..IPV4_HEADER_LEN]);
    packet[10..12].copy_from_slice(&header_sum.to_be_bytes());

    let udp = &mut packet[IPV4_HEADER_LEN..];
    udp[0..2].copy_from_slice(&src_port.to_be_bytes());
    udp[2..4].copy_from_slice(&dst_port.to_be_bytes());
    udp[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    udp[8..].copy_from_slice(payload);
    let udp_sum = checksum::udp_v4(src.octets(), dst.octets(), udp);
    udp[6..8].copy_from_slice(&udp_sum.to_be_bytes());
    packet
}

/// Where the single question in a DNS message ends, or `None` if it doesn't have exactly one.
fn question_end(message: &[u8]) -> Option<usize> {
    if message.len() < DNS_HEADER_LEN || message[4..6] != [0, 1] {
        return None;
    }
    let name_end = skip_name(message, DNS_HEADER_LEN)?;
    let end = name_end + 4;
    (end <= message.len()).then_some(end)
}

/// The cache key for a query: its question in lowercase plus everything after it, so that
/// queries asking for different EDNS options don't share an answer.
fn cache_key(query: &[u8]) -> Option<Vec<u8>> {
    let end = question_end(query)?;
    let mut key = query[DNS_HEADER_LEN..].to_vec();
    key[..end - DNS_HEADER_LEN].make_ascii_lowercase();
    Some(key)
}

/// How long a response may be cached, or `None` if it shouldn't be. Only successful answers are
/// kept. TTLs aren't rewritten when serving from the cache, so the cap also limits how stale a
/// record can get in the operating system's own cache.
fn cacheable_ttl(response: &[u8]) -> Option<Duration> {
    if response.len() < DNS_HEADER_LEN {
        return None;
    }
    let is_response = response[2] & 0x80 != 0;
    let truncated = response[2] & 0x02 != 0;
    let rcode = response[3] & 0x0f;
    let answers = u16::from_be_bytes([response[6], response[7]]);
    let authorities = u16::from_be_bytes([response[8], response[9]]);
    if !is_response || truncated || rcode != 0 || answers == 0 {
        return None;
    }

    let mut offset = question_end(response)?;
    let mut ttl = MAX_CACHE_TTL.as_secs() as u32;
    for _ in 0..u32::from(answers) + u32::from(authorities) {
        offset = skip_name(response, offset)?;
        let fixed = response.get(offset..offset + 10)?;
        ttl = ttl.min(u32::from_be_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]));
        let rdata_len = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
        offset += 10 + rdata_len;
        if offset > response.len() {
            return None;
        }
    }
    (ttl > 0).then(|| Duration::from_secs(u64::from(ttl)))
}

/// Lowers the reply size a query advertises in its EDNS record, if it has one.
fn limit_edns_size(query: &mut [u8]) {
    let Some(mut offset) = question_end(query) else {
        return;
    };
    let count = |at: usize| u32::from(u16::from_be_bytes([query[at], query[at + 1]]));
    let records = count(6) + count(8) + count(10);
    for _ in 0..records {
        let Some(after_name) = skip_name(query, offset) else {
            return;
        };
        let Some(fixed) = query.get_mut(after_name..after_name + 10) else {
            return;
        };
        if u16::from_be_bytes([fixed[0], fixed[1]]) == DNS_TYPE_OPT {
            // In an OPT record, the class field holds the UDP size.
            if u16::from_be_bytes([fixed[2], fixed[3]]) > MAX_EDNS_UDP_SIZE {
                fixed[2..4].copy_from_slice(&MAX_EDNS_UDP_SIZE.to_be_bytes());
            }
            return;
        }
        offset = after_name + 10 + usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
    }
}

fn skip_name(message: &[u8], mut offset: usize) -> Option<usize> {
    loop {
        let len = *message.get(offset)?;
        match len {
            0 => return Some(offset + 1),
            1..=63 => offset += 1 + usize::from(len),
            // A compression pointer ends the name.
            0xc0..=0xff => return message.get(offset + 1).map(|_| offset + 2),
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{CLIENT_V4, DEFAULT_DNS, GATEWAY_V4, RESOLVER_V4};

    fn resolver() -> DnsResolver {
        DnsResolver::new(GATEWAY_V4, RESOLVER_V4, DEFAULT_DNS)
    }

    /// A query for `example.com` A with the given ID.
    fn query(id: u16, name: &[u8]) -> Vec<u8> {
        let mut message = id.to_be_bytes().to_vec();
        message.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
        message.push(7);
        message.extend_from_slice(name);
        message.extend_from_slice(&[3, b'c', b'o', b'm', 0, 0, 1, 0, 1]);
        message
    }

    /// An answer to `query` with one A record, using a compression pointer for its name.
    fn answer(query: &[u8], ttl: u32) -> Vec<u8> {
        let mut message = query.to_vec();
        message[2] = 0x81;
        message[3] = 0x80;
        message[6..8].copy_from_slice(&[0, 1]);
        message.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1]);
        message.extend_from_slice(&ttl.to_be_bytes());
        message.extend_from_slice(&[0, 4, 93, 184, 216, 34]);
        message
    }

    fn from_os(payload: &[u8]) -> Vec<u8> {
        build_udp(CLIENT_V4, 5000, GATEWAY_V4, 53, payload)
    }

    fn parts(packet: &[u8]) -> (Ipv4Addr, u16, Ipv4Addr, u16, Vec<u8>) {
        assert_eq!(checksum::of(&packet[..IPV4_HEADER_LEN]), 0, "bad IPv4 checksum");
        let udp = Udp::parse(packet).unwrap();
        (udp.src, udp.src_port, udp.dst, udp.dst_port, udp.payload.to_vec())
    }

    #[test]
    fn unrelated_packets_pass_through() {
        let mut resolver = resolver();
        let now = Instant::now();
        let elsewhere = build_udp(CLIENT_V4, 5000, Ipv4Addr::new(8, 8, 8, 8), 53, &query(1, b"example"));
        let other_port = build_udp(CLIENT_V4, 5000, GATEWAY_V4, 54, &query(1, b"example"));
        assert_eq!(resolver.outgoing(&elsewhere, now), Outgoing::Pass);
        assert_eq!(resolver.outgoing(&other_port, now), Outgoing::Pass);
        assert_eq!(resolver.outgoing(&[0x60, 0, 0, 0], now), Outgoing::Pass);
        assert_eq!(resolver.incoming(&elsewhere, now), Incoming::Pass);
    }

    #[test]
    fn a_query_is_forwarded_and_its_reply_comes_back_from_the_gateway() {
        let mut resolver = resolver();
        let now = Instant::now();
        let question = query(0x1234, b"example");

        let Outgoing::Query(forwarded) = resolver.outgoing(&from_os(&question), now) else {
            panic!("expected a forwarded query");
        };
        let (src, port, dst, dst_port, payload) = parts(&forwarded);
        assert_eq!((src, dst, dst_port), (RESOLVER_V4, DEFAULT_DNS, 53));
        assert_eq!(payload, question);

        let reply = build_udp(DEFAULT_DNS, 53, RESOLVER_V4, port, &answer(&question, 60));
        let Incoming::Reply(delivered) = resolver.incoming(&reply, now) else {
            panic!("expected a reply");
        };
        let (src, src_port, dst, dst_port, payload) = parts(&delivered);
        assert_eq!((src, src_port, dst, dst_port), (GATEWAY_V4, 53, CLIENT_V4, 5000));
        assert_eq!(payload, answer(&question, 60));
    }

    #[test]
    fn a_cached_answer_uses_the_new_id_and_spelling_and_then_expires() {
        let mut resolver = resolver();
        let now = Instant::now();
        let first = query(1, b"example");
        let Outgoing::Query(forwarded) = resolver.outgoing(&from_os(&first), now) else { panic!() };
        let (_, port, ..) = parts(&forwarded);
        resolver.incoming(&build_udp(DEFAULT_DNS, 53, RESOLVER_V4, port, &answer(&first, 60)), now);

        let second = query(2, b"ExAmPlE");
        let Outgoing::Reply(reply) = resolver.outgoing(&from_os(&second), now) else {
            panic!("expected a cache hit");
        };
        let (.., payload) = parts(&reply);
        assert_eq!(payload[..2], [0, 2]);
        assert_eq!(&payload[13..20], b"ExAmPlE");

        let later = now + Duration::from_secs(61);
        assert!(matches!(resolver.outgoing(&from_os(&second), later), Outgoing::Query(_)));
    }

    #[test]
    fn failures_and_truncated_answers_are_not_cached() {
        let question = query(1, b"example");

        let mut failure = answer(&question, 60);
        failure[3] = 0x83; // NXDOMAIN
        assert_eq!(cacheable_ttl(&failure), None);

        let mut truncated = answer(&question, 60);
        truncated[2] |= 0x02;
        assert_eq!(cacheable_ttl(&truncated), None);

        assert_eq!(cacheable_ttl(&answer(&question, 0)), None);
        assert_eq!(cacheable_ttl(&answer(&question, 30)), Some(Duration::from_secs(30)));
        assert_eq!(cacheable_ttl(&answer(&question, 86400)), Some(MAX_CACHE_TTL));
    }

    #[test]
    fn spoofed_and_unexpected_replies_are_dropped() {
        let mut resolver = resolver();
        let now = Instant::now();
        let question = query(1, b"example");
        let Outgoing::Query(forwarded) = resolver.outgoing(&from_os(&question), now) else { panic!() };
        let (_, port, ..) = parts(&forwarded);

        let wrong_source = build_udp(Ipv4Addr::new(6, 6, 6, 6), 53, RESOLVER_V4, port, &answer(&question, 60));
        assert_eq!(resolver.incoming(&wrong_source, now), Incoming::Drop);
        let wrong_port = build_udp(DEFAULT_DNS, 53, RESOLVER_V4, port + 1, &answer(&question, 60));
        assert_eq!(resolver.incoming(&wrong_port, now), Incoming::Drop);
    }

    #[test]
    fn malformed_queries_do_not_panic() {
        let mut resolver = resolver();
        let now = Instant::now();
        assert_eq!(resolver.outgoing(&from_os(&[1, 2, 3]), now), Outgoing::Drop);
        // A header claiming one question, with a name that runs off the end.
        let bad = [0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 200];
        assert!(matches!(resolver.outgoing(&from_os(&bad), now), Outgoing::Query(_)));
    }

    #[test]
    fn large_edns_sizes_are_lowered_on_the_way_out() {
        let mut resolver = resolver();
        let now = Instant::now();
        let mut question = query(1, b"example");
        question[11] = 1; // one additional record
        question.extend_from_slice(&[0, 0, 41, 0x10, 0x00, 0, 0, 0, 0, 0, 0]); // OPT, 4096 bytes

        let Outgoing::Query(forwarded) = resolver.outgoing(&from_os(&question), now) else { panic!() };
        let (.., payload) = parts(&forwarded);
        let at = payload.len() - 8;
        assert_eq!(u16::from_be_bytes([payload[at], payload[at + 1]]), 1232);

        // A smaller size is left alone.
        let mut small = question.clone();
        let at = small.len() - 8;
        small[at..at + 2].copy_from_slice(&512u16.to_be_bytes());
        limit_edns_size(&mut small);
        assert_eq!(u16::from_be_bytes([small[at], small[at + 1]]), 512);

        // A query cut off partway through its records doesn't cause trouble.
        let mut cut = question.clone();
        cut.truncate(cut.len() - 4);
        limit_edns_size(&mut cut);
    }

    #[test]
    fn built_packets_have_valid_udp_checksums() {
        let packet = build_udp(CLIENT_V4, 1000, DEFAULT_DNS, 53, b"odd");
        let datagram = &packet[IPV4_HEADER_LEN..];
        let mut sum = checksum::add(0, &CLIENT_V4.octets());
        sum = checksum::add(sum, &DEFAULT_DNS.octets());
        sum = checksum::add(sum, &[0, 17]);
        sum = checksum::add(sum, &(datagram.len() as u16).to_be_bytes());
        assert_eq!(checksum::finish(checksum::add(sum, datagram)), 0);
    }
}
