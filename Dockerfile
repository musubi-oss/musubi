# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p musubi-server && cp target/release/musubi-server /musubi-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates openssh-client && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 musubi
COPY --from=build /musubi-server /usr/local/bin/musubi-server
USER musubi
EXPOSE 7350/tcp 7351/udp
ENV MUSUBI_CONFIG=/etc/musubi/musubi.toml
ENTRYPOINT ["/usr/local/bin/musubi-server"]
