//! `saki probe`: checks a link end to end, with no TUN device and no root.

use crate::ProbeArgs;
use crate::client::open_link;
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use saki_core::checksum;
use saki_core::dns::build_udp;
use saki_core::proto::{RESOLVER_V4, Welcome};
use saki_core::session::{Stats, client_handshake, pump};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::timeout;

const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(args: ProbeArgs) -> Result<()> {
    let started = Instant::now();
    let mut link = open_link(args.dev_tcp, args.server.as_deref(), &args.bluetooth.options()).await?;
    println!("Connected to {} in {:.1} s", link.peer, started.elapsed().as_secs_f32());

    let welcome = client_handshake(&mut link.stream).await.context("the handshake failed")?;
    println!("Address {}, DNS through {}, MTU {}", welcome.client_v4, welcome.upstream_dns, welcome.mtu);

    let (to_link, mut outbound) = mpsc::channel::<Bytes>(16);
    let (delivered, mut inbound) = mpsc::unbounded_channel::<Bytes>();
    let pump_task = tokio::spawn(async move {
        let on_packet = move |packet: Bytes| {
            let _ = delivered.send(packet);
        };
        let result =
            pump(link.stream, &mut outbound, on_packet, Arc::new(Stats::default()), std::future::pending()).await;
        drop(link.keep_alive);
        result
    });

    let mut times = Vec::new();
    for sequence in 0..args.count as u16 {
        let sent = Instant::now();
        to_link.send(gateway_ping(&welcome, sequence)).await?;
        let reply =
            timeout(REPLY_TIMEOUT, next_matching(&mut inbound, |packet| is_ping_reply(packet, &welcome, sequence)))
                .await;
        match reply {
            Ok(Some(())) => {
                let millis = sent.elapsed().as_secs_f32() * 1000.0;
                println!("Ping {}: {millis:.1} ms", sequence + 1);
                times.push(millis);
            }
            Ok(None) => bail!("the link closed"),
            Err(_) => println!("Ping {}: no reply", sequence + 1),
        }
    }
    if times.is_empty() {
        bail!("the server never answered a ping, so the link isn't carrying traffic");
    }
    let average = times.iter().sum::<f32>() / times.len() as f32;
    println!("Answered {} of {} pings, averaging {average:.1} ms", times.len(), args.count);

    // A real lookup, sent the way a client's resolver sends one. Getting an answer shows the
    // server's internet connection works too.
    let query = dns_query(0x5a4b, "one.one.one.one");
    let source_port = 4242;
    let packet = build_udp(RESOLVER_V4, source_port, welcome.upstream_dns, 53, &query);
    to_link.send(Bytes::from(packet)).await?;
    let answer =
        timeout(REPLY_TIMEOUT, next_matching(&mut inbound, |packet| first_a_record(packet, source_port))).await;
    match answer {
        Ok(Some(address)) => println!("DNS through the server works: one.one.one.one is {address}"),
        Ok(None) => bail!("the link closed"),
        Err(_) => bail!("DNS through the server got no answer. Does the server have internet access?"),
    }

    drop(to_link);
    let _ = timeout(Duration::from_secs(2), pump_task).await;
    Ok(())
}

async fn next_matching<T>(
    inbound: &mut mpsc::UnboundedReceiver<Bytes>,
    mut check: impl FnMut(&[u8]) -> Option<T>,
) -> Option<T> {
    while let Some(packet) = inbound.recv().await {
        if let Some(found) = check(&packet) {
            return Some(found);
        }
    }
    None
}

const PING_ID: u16 = 0x5341;

fn gateway_ping(welcome: &Welcome, sequence: u16) -> Bytes {
    let mut icmp = vec![8, 0, 0, 0];
    icmp.extend_from_slice(&PING_ID.to_be_bytes());
    icmp.extend_from_slice(&sequence.to_be_bytes());
    icmp.extend_from_slice(b"saki probe");
    let sum = checksum::of(&icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());

    let total = 20 + icmp.len();
    let mut packet = vec![0; total];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    packet[8] = 64;
    packet[9] = 1;
    packet[12..16].copy_from_slice(&welcome.client_v4.octets());
    packet[16..20].copy_from_slice(&welcome.gateway_v4.octets());
    let sum = checksum::of(&packet[..20]);
    packet[10..12].copy_from_slice(&sum.to_be_bytes());
    packet[20..].copy_from_slice(&icmp);
    Bytes::from(packet)
}

fn is_ping_reply(packet: &[u8], welcome: &Welcome, sequence: u16) -> Option<()> {
    let header_len = usize::from(packet.first()? & 0x0f) * 4;
    let icmp = packet.get(header_len..)?;
    let matches = packet.get(9) == Some(&1)
        && packet.get(12..16)? == welcome.gateway_v4.octets()
        && icmp.first() == Some(&0)
        && icmp.get(4..6)? == PING_ID.to_be_bytes()
        && icmp.get(6..8)? == sequence.to_be_bytes();
    matches.then_some(())
}

fn dns_query(id: u16, name: &str) -> Vec<u8> {
    let mut query = id.to_be_bytes().to_vec();
    query.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    query
}

/// Reads the first IPv4 address out of the DNS reply to our query, if this packet is that reply.
fn first_a_record(packet: &[u8], source_port: u16) -> Option<Ipv4Addr> {
    if packet.first()? >> 4 != 4 || *packet.get(9)? != 17 {
        return None;
    }
    let header_len = usize::from(packet[0] & 0x0f) * 4;
    let udp = packet.get(header_len..)?;
    if u16::from_be_bytes([*udp.get(2)?, *udp.get(3)?]) != source_port {
        return None;
    }
    let message = udp.get(8..)?;
    let answers = u16::from_be_bytes([*message.get(6)?, *message.get(7)?]);

    let mut offset = 12;
    while *message.get(offset)? != 0 {
        offset += 1 + usize::from(message[offset]);
    }
    offset += 5;
    for _ in 0..answers {
        // Names in answers are nearly always a two-byte pointer.
        offset += if message.get(offset)? & 0xc0 == 0xc0 { 2 } else { return None };
        let record = message.get(offset..offset + 10)?;
        let kind = u16::from_be_bytes([record[0], record[1]]);
        let len = usize::from(u16::from_be_bytes([record[8], record[9]]));
        offset += 10;
        if kind == 1 && len == 4 {
            let octets = message.get(offset..offset + 4)?;
            return Some(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]));
        }
        offset += len;
    }
    None
}
