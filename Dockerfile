FROM rust:1-alpine AS build
WORKDIR /src
COPY . .
RUN apk add --no-cache musl-dev && cargo build --release

FROM alpine:3.20
COPY --from=build /src/target/release/opencode-api-proxy /usr/local/bin/opencode-api-proxy
EXPOSE 6446
ENTRYPOINT ["opencode-api-proxy"]
