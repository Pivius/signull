# `netstack`

The networking crate for SIGNULL. It currently supports Ethernet II, IPv4, and TCP packet parsing and serialization, along with the RFC 1071 internet checksum. 
It doesn't implement sockets, connection management, or ARP yet. The kernel re-exports it as `kernel::net` for future networking system calls.

```
addr.rs       MacAddr and Ipv4Addr, with parsing and Display support
checksum.rs   Internet checksum calculation and verification
ethernet.rs   EthernetFrame with a 14-byte header
ipv4.rs       IPv4 headers with automatic checksum calculation
tcp.rs        TCP headers, flags, and pseudo-header checksums
packet.rs     Full packet parsing, serialization, and payload classification
```

## The `Packet` type

Packet represents a complete `Ethernet` frame, including its `IPv4` and `TCP` headers and payload.

```rust
use netstack::{MacAddr, Ipv4Addr, Packet};

let pkt = Packet::tcp(
    MacAddr::BROADCAST, src_mac,
    Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(10, 0, 0, 1),
    4096, 80, seq, ack, flags, payload,
);

let mut buf = [0u8; 1514];
let n = pkt.serialize_into(&mut buf);       // checksums filled in
let parsed = Packet::parse(&buf[..n])?;     // round-trips byte-exactly
```

Serialization fills in the required checksums. Parsing the resulting bytes reconstructs the packet, allowing byte-for-byte round-trip tests.

A `Packet` contains the Ethernet header, IPv4 header, transport protocol, and payload. The transport layer is represented by `L4::Tcp(header)` or `L4::Other(protocol)`.

The `classify()` method categorizes payloads as `Text`, `Binary`, or `Encrypted`. This is a heuristic based on ASCII content and the TLS record prefix `0x16` `0x03`.

## Tests

```sh
cargo test -p netstack
```
