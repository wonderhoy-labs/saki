//! The one's-complement checksum used by IPv4, UDP and ICMP.

use std::net::Ipv6Addr;

/// Adds `data` to a running one's-complement sum.
pub fn add(mut sum: u32, data: &[u8]) -> u32 {
    let (pairs, remainder) = data.as_chunks::<2>();
    for pair in pairs {
        sum += u32::from(u16::from_be_bytes(*pair));
    }
    if let [last] = remainder {
        sum += u32::from(*last) << 8;
    }
    sum
}

/// Folds a running sum into its final 16-bit checksum.
pub fn finish(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// The checksum of a single buffer, as IPv4 headers and ICMPv4 use it.
pub fn of(data: &[u8]) -> u16 {
    finish(add(0, data))
}

/// The checksum of an ICMPv6 message, which covers an IPv6 pseudo-header.
pub fn icmpv6(src: Ipv6Addr, dst: Ipv6Addr, message: &[u8]) -> u16 {
    let mut sum = add(0, &src.octets());
    sum = add(sum, &dst.octets());
    sum = add(sum, &(message.len() as u32).to_be_bytes());
    sum = add(sum, &[0, 0, 0, 58]);
    finish(add(sum, message))
}

/// The checksum of a TCP segment over IPv4.
pub fn tcp_v4(src: [u8; 4], dst: [u8; 4], segment: &[u8]) -> u16 {
    let mut sum = add(0, &src);
    sum = add(sum, &dst);
    sum = add(sum, &[0, 6]);
    sum = add(sum, &(segment.len() as u16).to_be_bytes());
    finish(add(sum, segment))
}

/// The checksum of a TCP segment over IPv6.
pub fn tcp_v6(src: Ipv6Addr, dst: Ipv6Addr, segment: &[u8]) -> u16 {
    let mut sum = add(0, &src.octets());
    sum = add(sum, &dst.octets());
    sum = add(sum, &(segment.len() as u32).to_be_bytes());
    sum = add(sum, &[0, 0, 0, 6]);
    finish(add(sum, segment))
}

/// The checksum of a UDP datagram over IPv4. A result of zero is sent as `0xffff`.
pub fn udp_v4(src: [u8; 4], dst: [u8; 4], datagram: &[u8]) -> u16 {
    let mut sum = add(0, &src);
    sum = add(sum, &dst);
    sum = add(sum, &[0, 17]);
    sum = add(sum, &(datagram.len() as u16).to_be_bytes());
    match finish(add(sum, datagram)) {
        0 => 0xffff,
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_rfc_1071_example() {
        // RFC 1071 section 3 sums these to 0xddf2, so the checksum is its complement.
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(of(&data), !0xddf2);
    }

    #[test]
    fn a_header_with_its_checksum_verifies_to_zero() {
        let mut header =
            [0x45, 0x00, 0x00, 0x1c, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 10, 233, 77, 3, 1, 1, 1, 1];
        let sum = of(&header);
        header[10..12].copy_from_slice(&sum.to_be_bytes());
        assert_eq!(of(&header), 0);
    }
}
