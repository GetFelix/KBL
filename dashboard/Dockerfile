# The dashboard built from this checkout and served by nginx, which also forwards
# /v1 to the tracker (the page calls the tracker at its own origin). nginx runs
# as a non-root user and listens on 8080. Build
# context: the repository root (dev/compose.yml does this).
# Base images are pinned by digest (node:24-bookworm, nginx-unprivileged:1-alpine); bump them
# on purpose. Only the final stage ships, so the build stage running as root is not in the image.
FROM docker.io/library/node:24-bookworm@sha256:9d266751ca3d7fdbe651c878ffd5ad0a42fed49a18c283509267c97f025b7496 AS build
# rustup-init is fetched by version and checked against its published sha256 per platform,
# not piped from a script.
ARG RUSTUP_VERSION=1.29.3
ARG RUSTUP_SHA256_X86_64=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55
ARG RUSTUP_SHA256_AARCH64=9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282
ARG RUST_TOOLCHAIN=1.100.0
RUN set -eu; \
    case "$(uname -m)" in \
      x86_64) arch=x86_64; sha="$RUSTUP_SHA256_X86_64" ;; \
      aarch64) arch=aarch64; sha="$RUSTUP_SHA256_AARCH64" ;; \
      *) echo "unsupported platform $(uname -m)" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${arch}-unknown-linux-gnu/rustup-init"; \
    echo "${sha}  /tmp/rustup-init" | sha256sum -c -; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --profile minimal --default-toolchain "$RUST_TOOLCHAIN" --target wasm32-unknown-unknown; \
    rm /tmp/rustup-init
ENV PATH=/root/.cargo/bin:$PATH
WORKDIR /src
COPY . .
# The CLI must match the wasm-bindgen crate in Cargo.lock exactly.
RUN cargo install wasm-bindgen-cli --locked --version "$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')"
WORKDIR /src/dashboard
# esbuild's install script only checks its binary; the platform package already carries it.
RUN npm ci --ignore-scripts && npm run build

FROM docker.io/nginxinc/nginx-unprivileged:1-alpine@sha256:b5047b9b76845eac124358373c6852377c52e4b05834a47647e955dd91cb8f1e
# The base image renders templates into conf.d at start, after it has read the
# container's resolvers into NGINX_LOCAL_RESOLVERS.
ENV NGINX_ENTRYPOINT_LOCAL_RESOLVERS=1
COPY dashboard/nginx.conf.template /etc/nginx/templates/default.conf.template
COPY --from=build /src/dashboard/dist /usr/share/nginx/html
USER 101
