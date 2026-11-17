# The work tracker, built from this checkout. Build context: the repository root
# (dev/compose.yml does this). The tracker runs as the unprivileged user kbl;
# tracker/entrypoint.sh explains why the container starts as root.
FROM docker.io/library/rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kbl-tracker

FROM docker.io/library/debian:bookworm-slim
RUN useradd --system --uid 10001 --no-create-home kbl && install -d -o kbl -g kbl -m 0700 /run/kbl
COPY --from=build /src/target/release/kbl-tracker /usr/local/bin/kbl-tracker
COPY tracker/entrypoint.sh /usr/local/bin/entrypoint.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
