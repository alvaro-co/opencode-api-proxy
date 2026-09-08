FROM rust:1-alpine AS build
WORKDIR /src
COPY . .
RUN apk add --no-cache musl-dev && cargo build --release

FROM alpine:3.20
COPY --from=build /src/target/release/opencode-api-proxy /usr/local/bin/opencode-api-proxy
RUN adduser -D -H -s /sbin/nologin ocproxy \
  && mkdir -p /data && chown ocproxy:ocproxy /data
USER ocproxy
WORKDIR /data
EXPOSE 6446
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD wget -qO- http://127.0.0.1:6446/health || exit 1
ENTRYPOINT ["opencode-api-proxy"]
