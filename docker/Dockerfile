# herdr-board sandbox image.
#
# Everything is pinned: the base image by digest (Rust 1.97.0 on Debian
# bookworm slim), the Herdr 0.8.0 release asset by SHA-256 per architecture,
# and the socket protocol by a build-time check (must be exactly 19).
#
# Build: docker build --platform linux/<amd64|arm64> -t <tag> docker/
# The sandbox wrapper (scripts/sandbox.sh) is the supported entry point.

FROM rust:1.97.0-slim-bookworm@sha256:6d220bf85c74e842a79da63997af8d2e74455c0b8847d8bb3a5888572334991d

# TARGETARCH is provided by BuildKit (amd64/arm64). The fallback covers
# classic builders: derive it from the kernel so cross-platform safety never
# depends on builder features.
ARG TARGETARCH

# Tool dependencies from the pinned Debian base. node+npm come from a pinned
# Node 22 LTS install (see below); they are only used by the explicit
# real-provider agent mode (installs the pinned provider CLIs at prepare
# time into the state volume, never into the image's config).
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    build-essential git curl ca-certificates python3 jq procps \
  && rm -rf /var/lib/apt/lists/*

# fmt/clippy components for the pinned toolchain (no-op when present).
RUN rustup component add clippy rustfmt

# Herdr 0.8.0 — one pinned release asset per architecture, verified by
# SHA-256, exact version string, and socket protocol 19 before the image can
# be produced. Mirrors the pins enforced by e2e/ci.sh (x86_64) and extends
# them to aarch64 with the upstream-published digest.
ENV HERDR_VERSION=0.8.0
ENV HERDR_PROTOCOL=19
RUN set -eu; \
  arch="${TARGETARCH:-$(uname -m)}"; \
  case "$arch" in \
    x86_64|amd64) asset=herdr-linux-x86_64; \
      sha=b872ea7e40fa2cb17e857ac9b62b1bf26db7b403c622f5d2f3f5b35f6e9acd28 ;; \
    aarch64|arm64) asset=herdr-linux-aarch64; \
      sha=f647ac66468d9efbc642fe534fb284468f0aea60641606fc008dfc0d82a3ca87 ;; \
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
  esac; \
  curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 300 --retry 3 --retry-all-errors \
    --output /usr/local/bin/herdr \
    "https://github.com/herdrdev/herdr/releases/download/v${HERDR_VERSION}/${asset}"; \
  chmod 755 /usr/local/bin/herdr; \
  printf '%s  %s\n' "$sha" /usr/local/bin/herdr | sha256sum --check; \
  [ "$(/usr/local/bin/herdr --version)" = "herdr ${HERDR_VERSION}" ]; \
  /usr/local/bin/herdr api schema --json | jq -e ".protocol == ${HERDR_PROTOCOL}" >/dev/null; \
  echo "sandbox image: herdr ${HERDR_VERSION} protocol ${HERDR_PROTOCOL} (${arch}) verified"

# Node 22 LTS — one pinned tarball per architecture, verified by SHA-256 before
# the image can be produced. pi 0.84.x requires Node >= 22.19.0 (the Debian
# bookworm node is 18, too old); the node dist tarball provides node+npm with
# its own build (npm 10.x). Provider CLIs install into the state volume at
# runtime, so node exists only to install/run them and never holds config.
ENV NODE_VERSION=22.23.2
RUN set -eu; \
  arch="${TARGETARCH:-$(uname -m)}"; \
  case "$arch" in \
    x86_64|amd64) node_asset=node-v${NODE_VERSION}-linux-x64.tar.xz; \
      node_sha=d60acfe00a2932254bb0ad20e01b0d74397a0875595de719654b214f4b03f307 ;; \
    aarch64|arm64) node_asset=node-v${NODE_VERSION}-linux-arm64.tar.xz; \
      node_sha=fff4078c5def658577f92c88db7db3bc0072924bfb93fe52c1e744a54e94abb8 ;; \
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;; \
  esac; \
  curl --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 600 --retry 3 --retry-all-errors \
    --output /tmp/node.tar.xz "https://nodejs.org/dist/v${NODE_VERSION}/${node_asset}"; \
  printf '%s  %s\n' "$node_sha" /tmp/node.tar.xz | sha256sum --check; \
  tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1; \
  rm /tmp/node.tar.xz; \
  node --version | grep -qx "v${NODE_VERSION}"; \
  npm --version >/dev/null; \
  echo "sandbox image: node ${NODE_VERSION} ($arch) verified"

# Non-root runtime user. HOME and all mutable state live in named volumes
# mounted by the wrapper; the image itself stays stateless.
RUN useradd --uid 1000 --create-home --shell /bin/bash board \
  && mkdir -p /opt/cargo /opt/target /artifacts \
  && chown -R 1000:1000 /opt/cargo /opt/target /artifacts

ENV RUSTUP_HOME=/usr/local/rustup
WORKDIR /repo
USER board
