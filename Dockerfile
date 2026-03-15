FROM rust:alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/
COPY migrations/ migrations/

RUN cargo build --release

FROM alpine:3.21

RUN apk add --no-cache ca-certificates

COPY --from=builder /build/target/release/fugue /usr/local/bin/fugue
COPY fugue.toml.example /etc/fugue/fugue.toml.example

EXPOSE 4533

ENTRYPOINT ["fugue"]
CMD ["--config", "/etc/fugue/fugue.toml", "serve"]
