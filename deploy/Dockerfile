FROM rust:1.88.0-alpine3.22@sha256:9dfaae478ecd298b6b5a039e1f2cc4fc040fc818a2de9aa78fa714dea036574d AS builder
# ring and bundled SQLite compile C; the Rust Alpine base omits libc headers.
RUN apk add --no-cache musl=1.2.5-r12 musl-dev=1.2.5-r12
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p listmngr \
    && mkdir -p /runtime/var/lib/listmngr /runtime/tmp \
    && cp /src/target/release/listmngr /runtime/listmngr \
    && chmod 0755 /runtime/listmngr

FROM scratch
COPY --from=builder /runtime/ /
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
WORKDIR /var/lib/listmngr
USER 1000:1000
EXPOSE 8000
HEALTHCHECK --interval=10s --timeout=3s --retries=5 CMD ["/listmngr", "status"]
ENTRYPOINT ["/listmngr"]
CMD ["serve"]
