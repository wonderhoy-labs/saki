# The Saki protocol

Version 1. This is what's on the air, for anyone writing another client or server. The Rust version
is in [`proto.rs`](../crates/saki-core/src/proto.rs) and the Kotlin one is in
[`Proto.kt`](../android/app/src/main/kotlin/com/wonderhoylabs/saki/link/Proto.kt). Their tests check
the same bytes.

## Finding a server

A server advertises over Bluetooth Low Energy with this service UUID, and normally its name:

```
53414b49-a776-4be4-9cf4-8ac46746a50f
```

That service has one characteristic, which can only be read over an encrypted link. Reading it is
what starts pairing on devices that haven't paired yet.

```
53414b49-a777-4be4-9cf4-8ac46746a50f
```

| Bytes | Value                 |
| ----- | --------------------- |
| 4     | `SAKI`                |
| 1     | Protocol version      |
| 2     | L2CAP PSM, big-endian |

## The link

The client opens a BLE L2CAP connection-oriented channel to that PSM. The channel needs encryption
at both ends. Everything after that treats it as a reliable, ordered byte stream that runs in both
directions at once, so L2CAP's packet boundaries don't mean anything. The client keeps its GATT
connection open for as long as the channel lasts.

## Frames

Everything on the stream is a frame. Numbers are big-endian.

| Bytes | Value          |
| ----- | -------------- |
| 1     | Type           |
| 2     | Payload length |
| n     | Payload        |

Skip frames with a type you don't know. Later versions may add some.

| Type | Name    | Payload                                                      |
| ---- | ------- | ------------------------------------------------------------ |
| 1    | Hello   | `SAKI`, then the protocol version in 1 byte                  |
| 2    | Welcome | See below                                                    |
| 3    | Packet  | One IP packet, IPv4 or IPv6                                  |
| 4    | Ping    | 8 bytes of anything                                          |
| 5    | Pong    | The same 8 bytes as the ping it answers                      |
| 6    | Bye     | The reason in 1 byte: 0 for normal, 1 for a version mismatch |

Allow for longer payloads than these. A later version may add fields to the end.

### Handshake

The client sends a Hello, and the server answers with a Welcome. If the versions differ, the server
sends a Bye with reason 1 and closes the channel.

| Bytes | Welcome field               | Currently         |
| ----- | --------------------------- | ----------------- |
| 2     | MTU of the tunnel           | 1280              |
| 4     | Client IPv4 address         | 10.233.77.2       |
| 4     | Gateway IPv4 address        | 10.233.77.1       |
| 1     | IPv4 prefix length          | 24                |
| 16    | Client IPv6 address         | fd53:616b:6900::2 |
| 16    | Gateway IPv6 address        | fd53:616b:6900::1 |
| 1     | IPv6 prefix length          | 64                |
| 4     | Upstream DNS resolver, IPv4 | 1.1.1.1           |

Every client gets the same addresses, because each one has a network of its own on the server.

### After the handshake

Either side sends Packet frames whenever it has them. Each side also sends a Ping every 10 seconds,
and treats 35 seconds of silence as a dead link. Answer every Ping with a Pong. It's fine to drop
Packets when there's too much to send, since the protocols inside them handle loss.

## What a server does

A server ends the client's TCP connections and UDP flows and repeats them on sockets of its own, with
these rules:
- It only answers a TCP SYN once it has connected to the real destination. If that fails, it sends a
  reset.
- Addresses inside the tunnel don't go anywhere, with two exceptions. The gateway answers pings, and
  TCP to the gateway's port 53 goes to the upstream resolver.
- Loopback, link-local, multicast and broadcast destinations are refused.
- Pings to anywhere else are relayed with an ICMP socket if the system allows one.

## What a client does

A client sends all of its traffic through the tunnel, and tells its system to use the gateway address
as its DNS server. UDP to the gateway's port 53 never goes on the link as is. The client's own resolver
picks it out and answers from a cache if it can. Otherwise it sends the query on to the upstream
resolver from 10.233.77.3, an address nothing else uses, so that replies are easy to spot. It limits the
query's EDNS reply size to 1232 bytes, since a bigger reply wouldn't fit through the tunnel.
