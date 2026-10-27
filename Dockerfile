# Build stage: compile a release binary.
FROM rust:1.98-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# Runtime stage: a small image with just the binary and CA certificates
# (needed for HTTPS calls to Jira and Slack).
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/svab-slack-bot /usr/local/bin/svab-slack-bot

# Run as an unprivileged user.
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/svab-slack-bot"]
