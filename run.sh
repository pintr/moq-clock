#!/usr/bin/env bash
set -euo pipefail

# Podman handover lab for moq-clock.
#
# Scenario:
# - Subscriber stays static on subnet A.
# - Publisher is moved between subnet A and subnet B while running.
# - Relay is attached to both subnets so both clients can reach it.
#
# This script uses Podman networks and containers only (no ip netns).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

NETWORK_A="moq-net-a"
NETWORK_B="moq-net-b"
SUBNET_A="10.41.1.0/24"
SUBNET_B="10.41.2.0/24"

RELAY_CONTAINER="moq-relay"
SUBSCRIBER_CONTAINER="moq-subscriber"
PUBLISHER_CONTAINER="moq-publisher"

RELAY_IMAGE="moq-clock-relay:v1"
SUBSCRIBER_IMAGE="moq-clock-subscriber:v1"
PUBLISHER_IMAGE="moq-clock-publisher:v1"

RELAY_IP_A="10.41.1.2"
RELAY_IP_B="10.41.2.2"
SUBSCRIBER_IP_A="10.41.1.10"
PUBLISHER_IP_A="10.41.1.20"
PUBLISHER_IP_B="10.41.2.20"

# Relay hostname is resolvable inside each Podman network by alias.
RELAY_URL="http://relay:4443/anon"

SIM_WAIT_TIMEOUT_SECONDS="${SIM_WAIT_TIMEOUT_SECONDS:-40}"
USE_GUI_TERMINALS="${USE_GUI_TERMINALS:-1}"

TERM_EMULATOR=""

need_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Missing required command: $cmd" >&2
    exit 1
  fi
}

ensure_prereqs() {
  need_cmd podman
  need_cmd grep
}

detect_terminal_emulator() {
  if command -v gnome-terminal >/dev/null 2>&1; then
    TERM_EMULATOR="gnome-terminal"
    return 0
  fi
  if command -v xfce4-terminal >/dev/null 2>&1; then
    TERM_EMULATOR="xfce4-terminal"
    return 0
  fi
  if command -v konsole >/dev/null 2>&1; then
    TERM_EMULATOR="konsole"
    return 0
  fi
  if command -v xterm >/dev/null 2>&1; then
    TERM_EMULATOR="xterm"
    return 0
  fi
  if command -v kitty >/dev/null 2>&1; then
    TERM_EMULATOR="kitty"
    return 0
  fi
  if command -v alacritty >/dev/null 2>&1; then
    TERM_EMULATOR="alacritty"
    return 0
  fi

  return 1
}

launch_in_terminal() {
  local title="$1"
  local command_str="$2"

  case "$TERM_EMULATOR" in
    gnome-terminal)
      gnome-terminal --title="$title" -- bash -lc "$command_str"
      ;;
    xfce4-terminal)
      xfce4-terminal --title="$title" --hold -e "bash -lc '$command_str'"
      ;;
    konsole)
      konsole --hold -p tabtitle="$title" -e bash -lc "$command_str"
      ;;
    xterm)
      xterm -T "$title" -hold -e bash -lc "$command_str"
      ;;
    kitty)
      kitty --title "$title" bash -lc "$command_str"
      ;;
    alacritty)
      alacritty -t "$title" -e bash -lc "$command_str"
      ;;
    *)
      return 1
      ;;
  esac
}

container_exists() {
  local name="$1"
  podman container exists "$name" >/dev/null 2>&1
}

network_exists() {
  local name="$1"
  podman network exists "$name" >/dev/null 2>&1
}

network_connected() {
  local container="$1"
  local network="$2"

  podman inspect -f '{{range $k, $_ := .NetworkSettings.Networks}}{{$k}} {{end}}' "$container" 2>/dev/null | grep -qw "$network"
}

create_networks() {
  ensure_prereqs

  if ! network_exists "$NETWORK_A"; then
    podman network create --subnet "$SUBNET_A" "$NETWORK_A" >/dev/null
  fi

  if ! network_exists "$NETWORK_B"; then
    podman network create --subnet "$SUBNET_B" "$NETWORK_B" >/dev/null
  fi

  echo "Networks ready: $NETWORK_A, $NETWORK_B"
}

remove_networks() {
  # Ignore failures when networks are in use or already absent.
  podman network rm "$NETWORK_A" >/dev/null 2>&1 || true
  podman network rm "$NETWORK_B" >/dev/null 2>&1 || true
}

remove_container_if_exists() {
  local name="$1"
  if container_exists "$name"; then
    podman rm -f "$name" >/dev/null 2>&1 || true
  fi
}

build_images() {
  ensure_prereqs
  cd "$ROOT_DIR"

  podman build -f Containerfile.publisher -t "$PUBLISHER_IMAGE" .
  podman build -f Containerfile.subscriber -t "$SUBSCRIBER_IMAGE" .
  podman build -f Containerfile.relay -t "$RELAY_IMAGE" .
}

start_relay() {
  ensure_prereqs

  remove_container_if_exists "$RELAY_CONTAINER"

  podman run -d \
    --name "$RELAY_CONTAINER" \
    --network "$NETWORK_A" \
    --ip "$RELAY_IP_A" \
    --network-alias relay \
    -p 4443:4443/udp \
    -p 4443:4443/tcp \
    "$RELAY_IMAGE" >/dev/null

  # Attach relay to subnet B as well so publisher can still reach it after switch.
  podman network connect --ip "$RELAY_IP_B" --alias relay "$NETWORK_B" "$RELAY_CONTAINER" >/dev/null

  echo "Relay started: $RELAY_CONTAINER"
}

start_subscriber() {
  ensure_prereqs

  remove_container_if_exists "$SUBSCRIBER_CONTAINER"

  podman run -d \
    --name "$SUBSCRIBER_CONTAINER" \
    --network "$NETWORK_A" \
    --ip "$SUBSCRIBER_IP_A" \
    "$SUBSCRIBER_IMAGE" \
    --url "$RELAY_URL" >/dev/null

  echo "Subscriber started on subnet A: $SUBSCRIBER_CONTAINER"
}

create_subscriber_container() {
  ensure_prereqs

  remove_container_if_exists "$SUBSCRIBER_CONTAINER"

  podman create \
    --name "$SUBSCRIBER_CONTAINER" \
    --network "$NETWORK_A" \
    --ip "$SUBSCRIBER_IP_A" \
    "$SUBSCRIBER_IMAGE" \
    --url "$RELAY_URL" >/dev/null
}

start_publisher_on_a() {
  ensure_prereqs

  remove_container_if_exists "$PUBLISHER_CONTAINER"

  podman run -d \
    --name "$PUBLISHER_CONTAINER" \
    --network "$NETWORK_A" \
    --ip "$PUBLISHER_IP_A" \
    "$PUBLISHER_IMAGE" \
    --url "$RELAY_URL" >/dev/null

  echo "Publisher started on subnet A: $PUBLISHER_CONTAINER"
}

create_publisher_container_on_a() {
  ensure_prereqs

  remove_container_if_exists "$PUBLISHER_CONTAINER"

  podman create \
    --name "$PUBLISHER_CONTAINER" \
    --network "$NETWORK_A" \
    --ip "$PUBLISHER_IP_A" \
    "$PUBLISHER_IMAGE" \
    --url "$RELAY_URL" >/dev/null
}

start_subscriber_in_terminal() {
  create_subscriber_container

  launch_in_terminal \
    "MoQ Subscriber" \
    "podman start -a ${SUBSCRIBER_CONTAINER}; echo; echo '[subscriber] exited'; read -r _"
}

start_publisher_in_terminal() {
  create_publisher_container_on_a

  launch_in_terminal \
    "MoQ Publisher" \
    "podman start -a ${PUBLISHER_CONTAINER}; echo; echo '[publisher] exited'; read -r _"
}

switch_publisher_to_b() {
  ensure_prereqs

  if ! container_exists "$PUBLISHER_CONTAINER"; then
    echo "Publisher container is not running: $PUBLISHER_CONTAINER" >&2
    exit 1
  fi

  if ! network_connected "$PUBLISHER_CONTAINER" "$NETWORK_B"; then
    podman network connect --ip "$PUBLISHER_IP_B" "$NETWORK_B" "$PUBLISHER_CONTAINER" >/dev/null
  fi

  if network_connected "$PUBLISHER_CONTAINER" "$NETWORK_A"; then
    podman network disconnect "$NETWORK_A" "$PUBLISHER_CONTAINER" >/dev/null
  fi

  echo "Publisher switched to subnet B"
}

switch_publisher_to_a() {
  ensure_prereqs

  if ! container_exists "$PUBLISHER_CONTAINER"; then
    echo "Publisher container is not running: $PUBLISHER_CONTAINER" >&2
    exit 1
  fi

  if ! network_connected "$PUBLISHER_CONTAINER" "$NETWORK_A"; then
    podman network connect --ip "$PUBLISHER_IP_A" "$NETWORK_A" "$PUBLISHER_CONTAINER" >/dev/null
  fi

  if network_connected "$PUBLISHER_CONTAINER" "$NETWORK_B"; then
    podman network disconnect "$NETWORK_B" "$PUBLISHER_CONTAINER" >/dev/null
  fi

  echo "Publisher switched to subnet A"
}

wait_for_subscriber_frames() {
  local before_count="$1"
  local need_new="$2"
  local deadline="$((SECONDS + SIM_WAIT_TIMEOUT_SECONDS))"

  while (( SECONDS < deadline )); do
    local now
    now="$(podman logs "$SUBSCRIBER_CONTAINER" 2>/dev/null | grep -c '^\[sub\] group=' || true)"

    if (( now - before_count >= need_new )); then
      return 0
    fi

    sleep 1
  done

  return 1
}

up_headless() {
  create_networks
  start_relay
  start_subscriber
  start_publisher_on_a

  echo "Environment started in headless mode."
  echo "Relay URL (inside containers): $RELAY_URL"
}

up() {
  create_networks
  start_relay

  if [[ "$USE_GUI_TERMINALS" == "1" ]] && [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]] && detect_terminal_emulator; then
    start_subscriber_in_terminal
    start_publisher_in_terminal
    echo "Environment started with GUI terminals for subscriber and publisher."
  else
    echo "GUI terminals unavailable or disabled; falling back to headless containers."
    start_subscriber
    start_publisher_on_a
    echo "Environment started in headless mode."
  fi

  echo "Relay URL (inside containers): $RELAY_URL"
}

down() {
  remove_container_if_exists "$PUBLISHER_CONTAINER"
  remove_container_if_exists "$SUBSCRIBER_CONTAINER"
  remove_container_if_exists "$RELAY_CONTAINER"
  remove_networks
  echo "Environment removed."
}

status() {
  ensure_prereqs

  echo "=== Containers ==="
  podman ps --filter "name=^${RELAY_CONTAINER}$" --filter "name=^${SUBSCRIBER_CONTAINER}$" --filter "name=^${PUBLISHER_CONTAINER}$"

  echo
  echo "=== Publisher networks ==="
  if container_exists "$PUBLISHER_CONTAINER"; then
    podman inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} ip={{$v.IPAddress}} {{end}}' "$PUBLISHER_CONTAINER"
  else
    echo "Publisher not running"
  fi

  echo
  echo "=== Subscriber networks ==="
  if container_exists "$SUBSCRIBER_CONTAINER"; then
    podman inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} ip={{$v.IPAddress}} {{end}}' "$SUBSCRIBER_CONTAINER"
  else
    echo "Subscriber not running"
  fi

  echo
  echo "=== Relay networks ==="
  if container_exists "$RELAY_CONTAINER"; then
    podman inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} ip={{$v.IPAddress}} {{end}}' "$RELAY_CONTAINER"
  else
    echo "Relay not running"
  fi
}

logs() {
  ensure_prereqs

  local target="${1:-}"
  case "$target" in
    relay)
      exec podman logs -f "$RELAY_CONTAINER"
      ;;
    subscriber)
      exec podman logs -f "$SUBSCRIBER_CONTAINER"
      ;;
    publisher)
      exec podman logs -f "$PUBLISHER_CONTAINER"
      ;;
    *)
      echo "Usage: $0 logs <relay|subscriber|publisher>" >&2
      exit 1
      ;;
  esac
}

simulate() {
  ensure_prereqs

  local switch_after="${1:-12}"
  local observe_after="${2:-8}"

  down >/dev/null 2>&1 || true
  up_headless

  echo "[sim] waiting for initial subscriber frames"
  if ! wait_for_subscriber_frames 0 3; then
    echo "[sim] FAIL: subscriber did not receive initial frames"
    exit 1
  fi

  local baseline
  baseline="$(podman logs "$SUBSCRIBER_CONTAINER" 2>/dev/null | grep -c '^\[sub\] group=' || true)"

  echo "[sim] waiting ${switch_after}s, then switching publisher to subnet B"
  sleep "$switch_after"
  switch_publisher_to_b

  sleep "$observe_after"
  if ! wait_for_subscriber_frames "$baseline" 3; then
    echo "[sim] FAIL: subscriber stopped after publisher switched to subnet B"
    exit 1
  fi

  local after_b
  after_b="$(podman logs "$SUBSCRIBER_CONTAINER" 2>/dev/null | grep -c '^\[sub\] group=' || true)"

  echo "[sim] waiting ${switch_after}s, then switching publisher back to subnet A"
  sleep "$switch_after"
  switch_publisher_to_a

  sleep "$observe_after"
  if ! wait_for_subscriber_frames "$after_b" 3; then
    echo "[sim] FAIL: subscriber stopped after publisher switched back to subnet A"
    exit 1
  fi

  local final
  final="$(podman logs "$SUBSCRIBER_CONTAINER" 2>/dev/null | grep -c '^\[sub\] group=' || true)"

  echo "[sim] PASS: subscriber stayed connected while publisher changed subnet"
  echo "[sim] total subscriber frames: $final"
  echo "[sim] tail logs:"
  echo "  $0 logs relay"
  echo "  $0 logs subscriber"
  echo "  $0 logs publisher"
}

usage() {
  cat <<EOF
Usage: ./run.sh <command>

Commands:
  build-images                 Build publisher/subscriber/relay Podman images.
  up                           Create networks, start relay, and open publisher/subscriber in separate terminals.
  up-headless                  Create networks and start relay/subscriber/publisher in background containers.
  down                         Stop/remove containers and delete networks.
  status                       Show running containers and network attachments.
  switch-publisher-b           Move publisher from subnet A to subnet B.
  switch-publisher-a           Move publisher from subnet B to subnet A.
  simulate [switch] [observe]  Auto A->B->A switch with continuity checks.
  logs <relay|subscriber|publisher>
                               Follow logs of one container.

Defaults:
  switch=12 seconds, observe=8 seconds

Example:
  ./run.sh build-images
  ./run.sh up
  ./run.sh switch-publisher-b
  ./run.sh switch-publisher-a
  ./run.sh down
EOF
}

main() {
  local cmd="${1:-}"

  case "$cmd" in
    build-images)
      build_images
      ;;
    up)
      up
      ;;
    up-headless)
      up_headless
      ;;
    down)
      down
      ;;
    status)
      status
      ;;
    switch-publisher-b)
      switch_publisher_to_b
      ;;
    switch-publisher-a)
      switch_publisher_to_a
      ;;
    simulate)
      shift
      simulate "${1:-12}" "${2:-8}"
      ;;
    logs)
      shift
      logs "${1:-}"
      ;;
    ""|-h|--help|help)
      usage
      ;;
    *)
      echo "Unknown command: $cmd" >&2
      usage
      exit 1
      ;;
  esac
}

main "$@"
