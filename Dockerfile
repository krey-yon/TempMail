# TempMail Combined Dockerfile
# Builds both SMTP and HTTP servers

# Build stage
FROM rust:1.86-slim AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy ALL workspace members
COPY Cargo.toml Cargo.lock ./
COPY database/ ./database/
COPY http/ ./http/
COPY smtp/ ./smtp/

# Build binaries
RUN cargo build --release --manifest-path smtp/Cargo.toml && \
    cargo build --release --manifest-path http/Cargo.toml

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies (OpenSSL for rustls, ca-certificates for TLS)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    openssl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1000 appuser

# Copy binaries from builder
COPY --from=builder /app/target/release/smtp /app/smtp
COPY --from=builder /app/target/release/http /app/http

# Change ownership
RUN chown -R appuser:appuser /app

USER appuser

# Expose ports
EXPOSE 25 3000

# Default: run both services
CMD ["sh", "-c", "/app/smtp & /app/http"]
