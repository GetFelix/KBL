# The work tracker and the ingest worker, built from this checkout. Build context: the repository root
# (dev/compose.yml does this). The tracker runs as the unprivileged user kbl;
# tracker/entrypoint.sh explains why the container starts as root. The same
# image carries kbl-ingest; run it by overriding the command (deploy/helm/kbl does).
FROM docker.io/library/rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kbl-tracker -p kbl-ingest

FROM docker.io/library/debian:bookworm-slim
RUN useradd --system --uid 10001 --no-create-home kbl && install -d -o kbl -g kbl -m 0700 /run/kbl
COPY --from=build /src/target/release/kbl-tracker /src/target/release/kbl-ingest /usr/local/bin/
COPY tracker/entrypoint.sh /usr/local/bin/entrypoint.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
