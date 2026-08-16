# The Ferrokey build/unit court image.
#
# Everything Ferrokey needs to build is pure Rust (evdev, x11rb, wayland-rs,
# Slint's software renderer are all Rust + libc), so the builder is a stock
# Rust image plus a tiny bit of tooling. The authoritative build NEVER reuses
# the host's target/, cargo registry or environment — the court scripts mount
# the repository read-only and use container-owned volumes for caches.

FROM rust:1.96-bookworm

# Fonts so that Slint's fontdb finds a font when the UI runs (VM courts).
# libxkbcommon-dev: the `xkb` feature links real libxkbcommon (system layouts
# like de@neo / us(intl)); the dev package ships the .pc + headers.
RUN apt-get update && apt-get install -y --no-install-recommends \
        fonts-dejavu-core \
        ca-certificates \
        git \
        libxkbcommon-dev \
        xkb-data \
        groff \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# cargo-deny: the supply-chain court (SC.SUPPLY.*) gates advisories,
# licenses, duplicate bans and registry sources on EVERY dependency of the
# workspace. Pinned to a specific version so the court is reproducible.
RUN cargo install cargo-deny --locked --version 0.20.2 \
    && rm -rf /usr/local/cargo/registry

# Sanitize the environment inside the court: no host display/session leakage.
ENV DISPLAY="" \
    WAYLAND_DISPLAY="" \
    XDG_RUNTIME_DIR="/tmp" \
    DBUS_SESSION_BUS_ADDRESS="" \
    XAUTHORITY="" \
    SSH_AUTH_SOCK="" \
    RUST_BACKTRACE=1

WORKDIR /repo
CMD ["/bin/bash"]
