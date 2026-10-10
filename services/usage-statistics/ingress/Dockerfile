# The Luxforge usage statistics ingress, built from services/usage-ingest (the compose file sets that
# directory as the build context). Both base images are pinned by digest; the crates come from
# crates.io at the versions and checksums in the committed Cargo.lock (--locked).
FROM rust:1.94.0-bookworm@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock registry.json ./
COPY src ./src
RUN cargo build --release --locked

# No shell, no package manager, no root.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /src/target/release/luxforge-usage-ingest /luxforge-usage-ingest
USER nonroot
EXPOSE 8080
ENTRYPOINT ["/luxforge-usage-ingest"]
