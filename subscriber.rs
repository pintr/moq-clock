// ============================================================================
// moq-clock-sub  —  MoQ Learning Project: Subscriber
// ============================================================================
//
// WHAT THIS DOES
// --------------
// Connects to a MoQ relay server, discovers the "clock" broadcast, subscribes
// to its "time" track, and prints each incoming timestamp frame to stdout.
//
// RUN IT (after the publisher is running)
// ----------------------------------------
//   cargo run --bin moq-clock-sub
//
//   # Against a local relay:
//   cargo run --bin moq-clock-sub -- --url http://localhost:4443/anon/clock
//
// HOW MoQ SUBSCRIPTION WORKS (vs plain HTTP/WebRTC)
// ---------------------------------------------------
//  • The subscriber connects to a relay, NOT directly to the publisher.
//    The relay acts as a CDN — it caches groups and fans out to many subs.
//
//  • The relay announces "which broadcasts are available" over the session.
//    We wait for the announcement, then subscribe to individual tracks.
//
//  • Groups arrive out-of-order (the relay may have re-ordered them).
//    Frames within a group arrive in order.
//
//  • If we fall behind, the relay can skip old groups so we're always live.
//    This is the core "low latency at scale" feature of MoQ.
// ============================================================================

use anyhow::Context;
use std::time::Duration;
use url::Url;

const DEFAULT_RELAY: &str = "https://relay.moq.dev/anon";

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

fn read_relay_url_arg() -> String {
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        if arg == "--url" {
            if let Some(url) = args.next() {
                return url;
            }
            break;
        }

        if !arg.starts_with('-') {
            return arg;
        }
    }

    DEFAULT_RELAY.to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ── 1. LOGGING ───────────────────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,moq_native=warn,moq_lite=warn".into()),
        )
        .init();

    // ── 2. PARSE TARGET URL ──────────────────────────────────────────────────
    let raw_url = read_relay_url_arg();
    let parsed = Url::parse(&raw_url).context("invalid relay URL")?;
    let url = normalize_relay_root(parsed);

    tracing::info!("Subscribing from: {url}");

    // ── 3. QUIC CLIENT ───────────────────────────────────────────────────────
    // Same default client as the publisher — moq-native handles TLS + fallback.
    let client = moq_native::ClientConfig::default()
        .init()
        .context("failed to initialise moq-native client")?;

    // ── 4. CREATE AN ORIGIN (subscriber side) ────────────────────────────────
    //
    // On the subscriber side the Origin works differently:
    //
    //   • `origin` (OriginProducer) is given to the SESSION via `with_consume`.
    //     The session writes incoming broadcast announcements INTO it.
    //
    //   • `consumer` (OriginConsumer from `.consume()`) is what we poll to
    //     learn about newly announced broadcasts.
    //
    // Think of it as a channel:  relay → session → OriginProducer → OriginConsumer → us
    let origin = moq_lite::Origin::produce();
    let mut consumer = origin.consume();

    // ── 5. CONNECT AS A CONSUMER ─────────────────────────────────────────────
    //
    // `.with_consume(origin)` — "I want to receive broadcasts from the relay".
    //   The session will use `origin` (the producer end) to deliver
    //   announcements it receives over the QUIC connection.
    //
    // Note the asymmetry vs publishing:
    //   publish:  with_publish(origin.consume())  → session reads from origin
    //   subscribe: with_consume(origin)            → session writes into origin
    let session = client
        .with_consume(origin)
        .connect(url)
        .await
        .context("failed to connect to relay")?;

    tracing::info!("Connected to relay ✓  — waiting for broadcast…");

    // ── 6. WAIT FOR BROADCAST ANNOUNCEMENTS ──────────────────────────────────
    //
    // `consumer.announced()` is an async stream of (path, Option<BroadcastConsumer>).
    //   • `Some(bc)` → a new broadcast is available at `path`.
    //   • `None`     → the broadcast at `path` has ended / been withdrawn.
    //
    // This is how MoQ handles "live" discovery — you don't need to poll an
    // HTTP endpoint. The relay pushes announcements as they happen.
    while let Some((path, maybe_broadcast)) = consumer.announced().await {
        // Keep the transport session alive while processing announcements.
        let _keep_session_alive = &session;

        let Some(broadcast) = maybe_broadcast else {
            tracing::info!("[sub] broadcast ended: {path}");
            continue;
        };

        tracing::info!("[sub] broadcast available: {path}  — subscribing to track 'time'");

        // Keep re-subscribing on the same broadcast if the track is cancelled.
        // This avoids a "single frame then stop" behavior when the relay
        // transiently cancels a track stream.
        tokio::spawn(async move {
            loop {
                if let Err(e) = subscribe_to_clock(&broadcast).await {
                    tracing::warn!("[sub] error in clock handler: {e:#}");
                }

                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// subscribe_to_clock
//
// Given a BroadcastConsumer (our handle to one live broadcast), subscribe to
// the "time" track and print every incoming frame.
// ─────────────────────────────────────────────────────────────────────────────
async fn subscribe_to_clock(broadcast: &moq_lite::BroadcastConsumer) -> anyhow::Result<()> {
    // ── 7. SUBSCRIBE TO A TRACK ───────────────────────────────────────────────
    //
    // `subscribe_track(name)` registers our interest in this named track.
    // The relay will begin forwarding Groups for this track to our session.
    //
    // Track delivery is *out-of-order* — the relay may deliver group #5 before
    // group #3 if group #3 was delayed. We handle this below.
    let mut track = broadcast
        .subscribe_track(&moq_lite::Track::new("time"))
        .context("failed to subscribe to track 'time'")?;

    tracing::info!("[sub] subscribed to track 'time' ✓");

    // ── 8. READ GROUPS ────────────────────────────────────────────────────────
    //
    // `next_group()` returns the next available Group (may be out-of-order).
    // Returns `None` when the track ends (publisher dropped it / broadcast ended).
    //
    // In a real media application you would:
    //   • Buffer groups by sequence number
    //   • Play the lowest-sequence group available
    //   • Drop old groups if you're too far behind (catch-up / latency control)
    //
    // For our clock we just print each group as it arrives.
    loop {
        let mut group = match track.next_group().await {
            Ok(Some(group)) => group,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!("[sub] next_group cancelled/transient: {err:#}");
                continue;
            }
        };

        let seq = group.info.sequence; // monotonically increasing group number

        // ── 9. READ FRAMES WITHIN THE GROUP ──────────────────────────────────
        //
        // `next_frame()` returns frames in the order the publisher wrote them.
        // Multiple frames per group are common for video (I-frame + P-frames),
        // rare for simple data streams like our clock (1 frame per group).
        loop {
            let mut frame = match group.next_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(err) => {
                    tracing::warn!("[sub] next_frame cancelled/transient in group {seq}: {err:#}");
                    break;
                }
            };
            // ── 10. READ THE FRAME PAYLOAD ───────────────────────────────────
            //
            // A Frame's payload may arrive in multiple chunks over the network.
            // `read_all()` waits until the entire frame is received and
            // concatenates the chunks. For large frames (e.g. video I-frames)
            // you might want to stream chunks instead to start decoding early.
            let payload = match frame.read_all().await {
                Ok(payload) => payload,
                Err(err) => {
                    tracing::warn!("[sub] frame read cancelled/transient in group {seq}: {err:#}");
                    break;
                }
            };

            match std::str::from_utf8(&payload) {
                Ok(timestamp) => println!("[sub] group={seq:04}  →  {timestamp}"),
                Err(_) => tracing::warn!("[sub] non-UTF8 frame in group {seq}"),
            }
        }
    }

    tracing::info!("[sub] track 'time' ended");
    Ok(())
}
