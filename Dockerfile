# ---- Étape 1 : build sur Alpine (musl) ----
# Le runtime final est Alpine (musl) : il faut donc que le binaire soit
# lui-même lié à musl, sinon "exec: no such file or directory" (chargeur
# glibc /lib64/ld-linux-x86-64.so.2 absent d'Alpine).
FROM rust:1-alpine AS builder
WORKDIR /app

RUN apk add --no-cache musl-dev pkgconfig make

# Cache des dépendances
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && cargo build --release 2>/dev/null || true

COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
RUN cargo build --release --bin eulalie

# ---- Étape 2 : runtime Alpine ----
FROM alpine:3.20
RUN apk add --no-cache ca-certificates sqlite && addgroup -S eulalie && adduser -S eulalie -G eulalie

WORKDIR /app
COPY --from=builder /app/target/release/eulalie /app/eulalie
COPY --from=builder /app/templates /app/templates
COPY --from=builder /app/migrations /app/migrations

# Répertoires de données (montés en volume)
RUN mkdir -p /data/uploads /data/db && chown -R eulalie:eulalie /data /app
ENV EULALIE_DB=/data/db/eulalie.db
ENV EULALIE_UPLOADS=/data/uploads

USER eulalie
EXPOSE 8080
VOLUME ["/data"]

# Durcissement : lecture seule possible, seccomp par défaut de Docker
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/app/eulalie"]
