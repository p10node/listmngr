# Front MTA for the Compose deployment: internet SMTP in, LMTP to listmngr,
# relay out for listmngr. Build from the repository root:
#   docker build -f deploy/postfix/Dockerfile .
FROM alpine:3.22@sha256:14358309a308569c32bdc37e2e0e9694be33a9d99e68afb0f5ff33cc1f695dce
# Exact package versions fail closed if removed from the Alpine repository and
# must be upgraded through review. Alpine's Postfix has `regexp` and `lmdb`
# tables, not `hash`; listmngr publishes `regexp` maps here.
RUN apk add --no-cache postfix=3.10.13-r0 \
    && addgroup -g 1000 listmngr \
    && adduser postfix listmngr
COPY deploy/postfix/main.cf /etc/postfix/main.cf
COPY deploy/postfix/entrypoint.sh /usr/local/bin/listmngr-postfix
RUN chmod 0755 /usr/local/bin/listmngr-postfix \
    && postconf -n > /dev/null
EXPOSE 25
HEALTHCHECK --interval=10s --timeout=3s --retries=5 CMD ["postfix", "status"]
ENTRYPOINT ["/usr/local/bin/listmngr-postfix"]
