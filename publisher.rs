// ============================================================================
// moq-clock-pub  —  MoQ Learning Project: Publisher
// ============================================================================
//
// WHAT THIS DOES
// --------------
// Connects to a MoQ relay server, creates a "clock" broadcast, and publishes
// the current time once per second. Any subscriber watching the "clock/time"
// track will receive these timestamped frames in real time.
//
// RUN IT
// ------
//   # Against the public test relay (no auth required under /anon/):
//   cargo run --bin moq-clock-pub -- --url https://relay.moq.dev/anon
//
//   # Against a local relay (see README for how to run one):
//   cargo run --bin moq-clock-pub -- --url http://localhost:4443/anon/clock
//
// PROTOCOL STACK REMINDER
//   Frame  ← smallest unit of data (one timestamp string in our case)
//   Group  ← ordered sequence of frames (we use one frame per group)
//   Track  ← out-of-order stream of groups (our "time" track)
//   Broadcast ← named collection of tracks (our "clock" broadcast)
//   Origin    ← server-side container of broadcasts (managed by the relay)
// ============================================================================

use anyhow::Context;
use chrono::Local;
use std::time::Duration;
use url::Url;

fn read_relay_url_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        if arg == "--url" {
            if let Some(url) = args.next() {
                return Some(url);
            }
            break;
        }

        if !arg.starts_with('-') {
            return Some(arg);
        }
    }

    None
}

fn normalize_relay_root(mut url: Url) -> Url {
    let trimmed = url.path().trim_end_matches('/').to_string();
    if let Some((parent, leaf)) = trimmed.rsplit_once('/') {
        if leaf == "clock" {
            let new_path = if parent.is_empty() { "/" } else { parent };
            url.set_path(new_path);
        }
    }
    url
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. LOGGING ───────────────────────────────────────────────────────────
    // Set RUST_LOG=debug to see moq-native and moq-lite internals.
    // e.g.  RUST_LOG=moq_native=debug,moq_lite=debug cargo run --bin moq-clock-pub -- --url http://localhost:4443/anon
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,moq_native=warn,moq_lite=warn".into()),
        )
        .init();

    // ── 2. PARSE TARGET URL ──────────────────────────────────────────────────
    let raw_url = read_relay_url_arg().ok_or_else(|| {
        anyhow::anyhow!(
            "missing relay URL: pass --url <relay-url> (example: --url http://localhost:4443/anon)"
        )
    })?;
    let parsed = Url::parse(&raw_url).context("invalid relay URL")?;
    let url = normalize_relay_root(parsed);

    tracing::info!("Publishing clock to: {url}");
    println!("[pub] publishing to: {url}");

    // ── 3. CREATE THE moq-native CLIENT ──────────────────────────────────────
    //
    // `moq_native::ClientConfig::default()` gives you sane defaults:
    //   • System TLS roots (so standard HTTPS certs work out of the box)
    //   • QUIC racing: tries raw QUIC first, falls back to WebSocket in 200ms
    //   • Supports https://, http:// (dev self-signed), moql://, moqt:// URLs
    //
    // `.init()` resolves the config and returns a `moq_native::Client`.
    let client = moq_native::ClientConfig::default()
        .init()
        .context("failed to initialise moq-native client")?;

    // ── 4. CREATE AN ORIGIN (publisher side) ─────────────────────────────────
    //
    // An Origin is the root container that holds all your Broadcasts.
    // Think of it as the "source of truth" for what you're publishing.
    //
    // `Origin::produce()` returns an OriginProducer.
    //   • You keep the producer to add/remove broadcasts later.
    //   • You hand `.consume()` to the session so it can forward
    //     announcements and data to the relay.
    let origin = moq_lite::Origin::produce();

    // ── 5. CONNECT TO THE RELAY AND BIND THE ORIGIN ──────────────────────────
    //
    // `.with_publish(origin.consume())` tells the session:
    //   "I am a publisher — please forward anything I announce to the relay."
    //
    // `.connect(url)` performs the QUIC (or WebSocket) handshake and the
    // moq-lite SETUP negotiation in one async step.
    //
    // The returned `session` runs in the background; we don't need to poll it
    // directly — tokio drives it while we do our publishing work below.
    let session = client
        .with_publish(origin.consume())
        .connect(url)
        .await
        .context("failed to connect to relay")?;

    tracing::info!("Connected to relay ✓");
    println!("[pub] connected to relay");

    // ── 6. CREATE A BROADCAST ─────────────────────────────────────────────────
    //
    // A Broadcast is a named collection of Tracks.
    // In a media stream you'd have separate tracks for video, audio, subtitles…
    // Here we have just one: "time".
    //
    // `Broadcast::produce()` returns a BroadcastProducer that lets you create
    // and manage tracks, plus a BroadcastConsumer that the session/relay reads.
    let mut broadcast = moq_lite::Broadcast::produce();

    // ── 7. ANNOUNCE THE BROADCAST TO THE RELAY ────────────────────────────────
    //
    // `publish_broadcast(path, consumer)` registers this broadcast under the
    // given path on the relay. Subscribers watching for broadcasts at this
    // path (or any prefix of it) will be notified immediately.
    //
    // Note: the full subscribe path will be  <relay-url-path>/<broadcast-path>

    origin.publish_broadcast("clock", broadcast.consume());

    tracing::info!("Broadcast announced ✓");
    println!("[pub] broadcast announced: /clock");

    // ── 8. CREATE A TRACK ─────────────────────────────────────────────────────
    //
    // A Track is a named sequence of Groups. Tracks within a Broadcast are
    // delivered independently, allowing out-of-order group delivery.
    // This is key for skipping stale content during congestion.
    //
    // Track priority lets the receiver prefer one track over another when
    // bandwidth is tight (0 = highest priority, higher = lower priority).
    let mut track = broadcast
        .create_track(moq_lite::Track::new("time"))
        .context("failed to create track")?;

    tracing::info!("Track 'time' created ✓  — starting clock loop");

    // ── 9. PUBLISH LOOP ───────────────────────────────────────────────────────
    //
    // We publish one Group per second. Each Group contains one Frame.
    //
    // GROUP vs FRAME:
    //   • A Group is the unit of "seek-ability" / "drop-ability".
    //     Groups can arrive out of order; the receiver sorts them by sequence.
    //     When catching up, the relay drops whole Groups (not partial ones).
    //   • A Frame is the unit of data within a Group — they're in-order
    //     within their Group. Frames cannot span Groups.
    //
    // For a clock, each second is independent so we use one frame per group.
    // For video: keyframe + following delta frames would all be in ONE group.

    loop {
        // Keep the transport session alive for the entire publish loop.
        let _keep_session_alive = &session;

        let now = Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        let payload = now.as_bytes();

        // ── 9a. Open a new Group ─────────────────────────────────────────────
        //
        // `append_group()` creates the next sequential group on this track.
        // The returned `GroupProducer` lets us write frames into it.
        // When the GroupProducer is dropped, the group is considered "done".
        let mut group = track.append_group().context("failed to append group")?;

        // ── 9b. Write a Frame ────────────────────────────────────────────────
        //
        // `write_frame(payload)` creates a Frame with the given bytes.
        // Frames must be written in order within their Group.
        // The relay forwards frames to subscribers as they are written —
        // sub-second delivery even for large frames.
        group
            .write_frame(moq_lite::bytes::Bytes::copy_from_slice(payload))
            .context("failed to write frame")?;

        // Mark the group complete so subscribers can advance to the next group.
        group.finish().context("failed to finish group")?;

        tracing::info!("[pub] sent: {now}");
        println!("[pub] sent: {now}");

        // Group is explicitly finished above.
        drop(group);

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
