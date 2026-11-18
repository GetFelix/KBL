# The dashboard built from this checkout and served by nginx, which also forwards
# /v1 to the tracker (the page calls the tracker at its own origin). nginx runs
# as a non-root user and listens on 8080. Build
# context: the repository root (dev/compose.yml does this).
FROM docker.io/library/node:24-bookworm AS build
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --target wasm32-unknown-unknown
ENV PATH=/root/.cargo/bin:$PATH
WORKDIR /src
COPY . .
# The CLI must match the wasm-bindgen crate in Cargo.lock exactly.
RUN cargo install wasm-bindgen-cli --locked --version "$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')"
WORKDIR /src/dashboard
RUN npm ci && npm run build

FROM docker.io/nginxinc/nginx-unprivileged:1-alpine
# The base image renders templates into conf.d at start, after it has read the
# container's resolvers into NGINX_LOCAL_RESOLVERS.
ENV NGINX_ENTRYPOINT_LOCAL_RESOLVERS=1
COPY dashboard/nginx.conf.template /etc/nginx/templates/default.conf.template
COPY --from=build /src/dashboard/dist /usr/share/nginx/html
