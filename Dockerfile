# ---- Builder stage ----
FROM rust:1.85-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/rustqueue

# Cache dependency builds: copy manifests first, then build a dummy to populate the cache.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && \
    echo 'fn main() {}' > src/main.rs && \
    echo '' > src/lib.rs && \
    cargo build --release 2>/dev/null || true && \
    rm -rf src

# Copy the full source tree and build the real binary.
COPY src/ src/
COPY dashboard/ dashboard/
RUN cargo build --release --bin rustqueue

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --shell /bin/bash rustqueue

# Create data and config directories.
RUN mkdir -p /data /etc/rustqueue && \
    chown rustqueue:rustqueue /data

COPY --from=builder /usr/src/rustqueue/target/release/rustqueue /usr/local/bin/rustqueue

USER rustqueue

EXPOSE 6790 6789

VOLUME ["/data"]

CMD ["rustqueue", "serve", "--config", "/etc/rustqueue/rustqueue.toml"]
