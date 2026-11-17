# Build stage: pinned toolchain matching the repo's tested version
FROM rust:1.96-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# Runtime: static binary, no toolchain. The engine binds 127.0.0.1 by design,
# so run the container with --network host to serve the host's loopback port.
FROM debian:bookworm-slim
COPY --from=build /src/target/release/shrinkgate /usr/local/bin/shrinkgate
ENV HOME=/data
EXPOSE 18793
ENTRYPOINT ["shrinkgate"]
CMD ["serve", "--quiet"]
