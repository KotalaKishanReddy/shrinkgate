FROM rust:1.96-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# Optional corporate build CA; never copied into the runtime image.
RUN --mount=type=secret,id=cargo_ca \
    if [ -f /run/secrets/cargo_ca ]; then export CARGO_HTTP_CAINFO=/run/secrets/cargo_ca; fi; \
    cargo build --release --locked

FROM debian:bookworm-slim
RUN groupadd --gid 10001 shrinkgate \
    && useradd --uid 10001 --gid shrinkgate --home-dir /data --create-home shrinkgate
COPY --from=build /src/target/release/shrinkgate /usr/local/bin/shrinkgate
ENV HOME=/data
USER 10001:10001
VOLUME ["/data"]
EXPOSE 18793
ENTRYPOINT ["shrinkgate"]
CMD ["serve", "--quiet"]
