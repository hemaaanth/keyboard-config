#!/usr/bin/env bash
set -euo pipefail

IMAGE="${GO60_BUILD_IMAGE:-go60-zmk-local}"
BRANCH="${1:-main}"
RUNTIME="${CONTAINER_RUNTIME:-}"

if [[ -z "$RUNTIME" ]]; then
  if command -v podman >/dev/null 2>&1; then
    RUNTIME=podman
  elif command -v docker >/dev/null 2>&1; then
    RUNTIME=docker
  else
    echo "podman or docker is required for local Go60 builds" >&2
    exit 1
  fi
fi

RUN_ARGS=()
if [[ "$RUNTIME" == "podman" ]]; then
  RUN_ARGS+=(--privileged --security-opt label=disable)
else
  RUN_ARGS+=(--privileged)
fi

"$RUNTIME" run "${RUN_ARGS[@]}" --rm \
  -v "$PWD:/config" \
  -w /config \
  -e UID="$(id -u)" \
  -e GID="$(id -g)" \
  -e BRANCH="$BRANCH" \
  docker.io/nixpkgs/nix:nixos-23.11 \
  bash -lc '
    set -euo pipefail
    export PATH=/root/.nix-profile/bin:/usr/bin:/bin

    nix-env -iA cachix -f https://cachix.org/api/v1/install
    cachix use moergo-glove80-zmk-dev

    nix-env -iA cacert gnused -f https://github.com/NixOS/nixpkgs/archive/nixos-23.11.tar.gz
    export SSL_CERT_FILE=/root/.nix-profile/etc/ssl/certs/ca-bundle.crt
    export NIX_SSL_CERT_FILE="$SSL_CERT_FILE"
    export GIT_SSL_CAINFO="$SSL_CERT_FILE"
    mkdir -p /etc/ssl/certs
    ln -sf "$SSL_CERT_FILE" /etc/ssl/certs/ca-certificates.crt
    ln -sf "$SSL_CERT_FILE" /etc/ssl/certs/ca-bundle.crt
    printf "\\nssl-cert-file = %s\\nsandbox = false\\n" "$SSL_CERT_FILE" >> /etc/nix/nix.conf

    git clone https://github.com/moergo-sc/zmk /tmp/zmk
    cd /tmp/zmk
    git checkout -q --detach "$BRANCH"
    sed -i "/    pykwalify/a\\    setuptools" /tmp/zmk/nix/zmk.nix
    sed -i "/    setuptools/a\\    ps.protobuf" /tmp/zmk/nix/zmk.nix
    sed -i "s/, cmake, ninja, dtc, gcc-arm-embedded/, cmake, ninja, dtc, gcc-arm-embedded, protobuf/" /tmp/zmk/nix/zmk.nix
    sed -i "s/nativeBuildInputs = \[ cmake ninja python dtc gcc-arm-embedded \]/nativeBuildInputs = [ cmake ninja python dtc gcc-arm-embedded protobuf ]/" /tmp/zmk/nix/zmk.nix

    cd /config
    nix-build ./config --arg firmware "import /tmp/zmk/default.nix {}" -j2 -o /tmp/combined --show-trace --option sandbox false
    install -o "$UID" -g "$GID" /tmp/combined/go60.uf2 ./go60.uf2
  '
