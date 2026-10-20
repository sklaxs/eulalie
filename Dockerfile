# ---- Étape 1 : build (Rust sur Debian slim, gcc nécessaire pour sqlite) ----
FROM rust:1-slim AS builder
WORKDIR /app

# Dépendances système minimales pour compiler (libsqlite3 via sqlx bundled)
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*

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
