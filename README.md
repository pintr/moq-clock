# moq-clock — Media over QUIC in Rust

A heavily-commented starter project for learning [Media over QUIC (MoQ)](https://moq.dev) in Rust.
Implements a "clock broadcaster": the publisher sends the current time every second; the subscriber prints it.

## Protocol Stack

```
┌──────────────────────┐
│   Your Application   │  ← this project
├──────────────────────┤
│       moq-lite       │  ← pub/sub transport (Origin → Broadcast → Track → Group → Frame)
├──────────────────────┤
│   moq-native / WT    │  ← QUIC + TLS plumbing (Quinn + rustls)
├──────────────────────┤
│    QUIC (UDP)        │  ← network
└──────────────────────┘
```

### Key concepts

| Term        | What it is |
|-------------|------------|
| **Origin**  | Root container. Publisher fills it; subscriber reads from it via the relay. |
| **Broadcast** | A named collection of Tracks (like a channel). |
| **Track**   | A named stream of Groups, delivered out-of-order. |
| **Group**   | Ordered sequence of Frames (think: keyframe + delta frames). Relay unit for caching/dropping. |
| **Frame**   | Raw bytes with an upfront size. Streams over the network chunk-by-chunk. |

## Crate choices

| Crate | Role | Why |
|-------|------|-----|
| `moq-lite` | Core protocol | Simpler than IETF moq-transport, works with any moq-transport CDN (Cloudflare etc.) |
| `moq-native` | QUIC/TLS setup | Wraps Quinn + rustls so you don't have to configure certificates manually |

> **moq-native vs raw Quinn**: `moq-native` handles self-signed cert fetching (`http://` URLs), WebSocket fallback, and QUIC transport racing. Use it for native clients. Use raw `moq-lite` + `web_transport_trait` if you need full control (e.g. custom QUIC config, mobile embedded).

## Quick start

### 1. Use the public relay (no setup needed)

```bash
# Terminal 1 — publish
cargo run --bin moq-clock-pub -- --url https://relay.moq.dev/anon

# Terminal 2 — subscribe
cargo run --bin moq-clock-sub -- --url https://relay.moq.dev/anon
```

The `/anon/` namespace is open to everyone — no auth required.

### 2. Run a local relay (recommended for experimentation)

```bash
# Install the relay binary
cargo install moq-relay

# Run it (self-signed cert, public /anon/ path)
moq-relay --server-bind 127.0.0.1:4443 --web-http-listen 127.0.0.1:4443 --web-ws --tls-generate localhost --auth-public anon/

# Terminal 1
cargo run --bin moq-clock-pub -- --url http://localhost:4443/anon

# Terminal 2
cargo run --bin moq-clock-sub -- --url http://localhost:4443/anon
```

> **Note**: `http://` (not `https://`) triggers moq-native's dev mode: it fetches the self-signed certificate fingerprint automatically from `http://localhost:4443/.well-known/moq`. No manual cert config needed.

## Podman setup

This project is containerized with Podman using:

- `Containerfile.publisher` - Publisher binary
- `Containerfile.subscriber` - Subscriber binary
- `Containerfile.relay` - MoQ relay server

### Build images with Podman

```bash
# Build all three images
podman build -f Containerfile.publisher -t moq-clock-publisher:v1 .
podman build -f Containerfile.subscriber -t moq-clock-subscriber:v1 .
podman build -f Containerfile.relay -t moq-clock-relay:v1 .
```

### Run complete local setup with Podman

```bash
# Terminal 1: Start the relay (listening on 4443)
podman run --rm -p 4443:4443/udp -p 4443:4443/tcp moq-clock-relay:v1

# Terminal 2: Start the publisher
podman run --rm moq-clock-publisher:v1 --url http://host.containers.internal:4443/anon

# Terminal 3: Start the subscriber  
podman run --rm moq-clock-subscriber:v1 --url http://host.containers.internal:4443/anon
```

> **Note**: QUIC uses UDP. Publish `4443/udp` on the relay (as above), or clients will fail to connect even if the container is running.
>
> Use `host.containers.internal` to reach the host from inside rootless containers. On some systems, `127.0.0.1:4443` may work instead.

### Alternative: Use the public relay (no local relay needed)

```bash
# Publisher connects to public relay
podman run --rm moq-clock-publisher:v1 --url https://relay.moq.dev/anon

# Subscriber connects to public relay
podman run --rm moq-clock-subscriber:v1 --url https://relay.moq.dev/anon
```

To use a custom relay URL with the public setup:

```bash
podman run --rm moq-clock-publisher:v1 --url http://your-relay.example.com:4443/anon
podman run --rm moq-clock-subscriber:v1 --url http://your-relay.example.com:4443/anon
```

## Debug logging

```bash
RUST_LOG=moq_native=debug,moq_lite=debug cargo run --bin moq-clock-pub -- --url http://localhost:4443/anon
```

## Podman handover script (recommended)

Use the Podman handover script at project root:

- `run.sh`

This script creates two Podman subnets and runs:

- subscriber fixed on subnet A
- publisher moved between subnet A and subnet B at runtime
- relay attached to both subnets

Quick start:

```bash
./run.sh build-images
./run.sh up
./run.sh switch-publisher-b
./run.sh switch-publisher-a
./run.sh down
```

`./run.sh up` starts relay and opens subscriber/publisher in separate terminals (foreground processes).
If GUI terminals are unavailable, it falls back to headless background containers.

Force headless mode explicitly:

```bash
USE_GUI_TERMINALS=0 ./run.sh up
```

Automated timed switch test:

```bash
./run.sh simulate
```

Custom timing (`switch` seconds, `observe` seconds):

```bash
./run.sh simulate 10 6
```

Useful logs:

```bash
./run.sh logs relay
./run.sh logs subscriber
./run.sh logs publisher
```

## What to explore next

1. **Multiple tracks**: Add an `audio` and `video` track to one Broadcast. The relay delivers them independently and subscribers can pick just the tracks they need.

2. **Priority**: Set different `priority` values on tracks. The relay will prefer higher-priority tracks when bandwidth is constrained.

3. **Group expiry / latency control**: In `subscribe_to_clock`, track group sequence numbers. If the received group is more than N seconds old, skip it. This is how MoQ maintains low latency under congestion.

4. **hang crate**: For real media (H.264, AAC, Opus), add the `hang` crate on top of `moq-lite`. It provides a catalog format, container (CMAF/fMP4), and `OrderedConsumer` with built-in latency management.

5. **Broadcasting from FFmpeg**: Use `hang-cli` or `moq-karp` to ingest an FFmpeg stream and publish it via MoQ.

## Resources

- [moq.dev documentation](https://doc.moq.dev)
- [moq-lite docs.rs](https://docs.rs/moq-lite)
- [moq-native docs.rs](https://docs.rs/moq-native)
- [moq-dev/moq on GitHub](https://github.com/moq-dev/moq)
