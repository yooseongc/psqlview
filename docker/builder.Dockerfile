FROM rust:1.93-alpine AS builder

RUN apk add --no-cache \
    musl-dev \
    pkgconfig \
    ca-certificates \
    git

WORKDIR /src

ENV CARGO_NET_RETRY=5
ENV CARGO_HOME=/usr/local/cargo
ENV RUSTFLAGS="-C target-feature=+crt-static"

RUN rustup target add x86_64-unknown-linux-musl

COPY rust-toolchain.toml ./
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
 && cargo fetch --target x86_64-unknown-linux-musl \
 && rm -rf src

COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl \
 && mkdir -p /out \
 && cp target/x86_64-unknown-linux-musl/release/psqlview /out/psqlview

FROM alpine:3.20 AS runtime
RUN apk add --no-cache ca-certificates tzdata
COPY --from=builder /out/psqlview /usr/local/bin/psqlview
ENTRYPOINT ["/usr/local/bin/psqlview"]
