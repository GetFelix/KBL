# The work tracker, built from this checkout. Build context: the repository root
# (dev/compose.yml does this).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kbl-tracker

FROM debian:bookworm-slim
COPY --from=build /src/target/release/kbl-tracker /usr/local/bin/kbl-tracker
ENTRYPOINT ["kbl-tracker"]
