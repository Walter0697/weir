# A single-shot container: it performs one sync pass and exits.
#
# Deciding *when* it runs stays outside — a cron, a systemd timer, a CI job, a
# Kubernetes CronJob. Shipping a scheduler in here would force one choice on
# everyone and turn a weekly job into a daemon.

FROM rust:1.96-slim-bookworm AS build
WORKDIR /src

# Dependencies first, so editing the source does not rebuild the whole tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
 && echo 'fn main() {}' > src/main.rs \
 && echo '' > src/lib.rs \
 && cargo build --release --locked \
 && rm -rf src

COPY src ./src
# Touch the entry points so cargo does not reuse the stubs above.
RUN touch src/main.rs src/lib.rs \
 && cargo build --release --locked \
 && strip target/release/weir

FROM debian:bookworm-slim

# git because the merge has to be exactly the one a human would get locally,
# ca-certificates because every forge and upstream is reached over HTTPS.
RUN apt-get update \
 && apt-get install --no-install-recommends -y git ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Unprivileged: this clones untrusted upstream code and runs git over it.
RUN useradd --create-home --uid 10001 weir
# Created here and owned by that user, so a volume mounted at /data is writable
# without anyone having to chown it from outside. `serve` keeps its database
# here, and that database holds the forge token.
RUN mkdir -p /data && chown weir:weir /data
VOLUME ["/data"]
USER weir
ENV HOME=/home/weir

COPY --from=build /src/target/release/weir /usr/local/bin/weir

# Mount the fork list here; it holds no secrets, so it is safe to bake into an
# image or a ConfigMap if you prefer. The token comes from the environment.
WORKDIR /etc/weir
ENTRYPOINT ["weir"]
CMD ["run", "--config", "/etc/weir/forks.toml"]
