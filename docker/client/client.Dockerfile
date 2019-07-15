### Build Image ###
FROM debian:stretch as builder

RUN echo "deb http://deb.debian.org/debian stretch-backports main" > /etc/apt/sources.list.d/backports.list \
    && apt-get update && apt-get install -y protobuf-compiler/stretch-backports cmake golang curl \
    && apt-get clean && rm -r /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
ENV PATH "$PATH:/root/.cargo/bin"

WORKDIR /xpeer
COPY rust-toolchain /xpeer/rust-toolchain
RUN rustup install $(cat rust-toolchain)

COPY . /xpeer
RUN cargo build --release -p client
RUN strip target/release/client

### Production Image ###
FROM debian:stretch

RUN mkdir -p /opt/xpeer/bin /opt/xpeer/etc
COPY --from=builder /xpeer/target/release/client /opt/xpeer/bin/xpeer_client
COPY scripts/cli/trusted_peers.config.toml /opt/xpeer/etc/trusted_peers.config.toml

ENTRYPOINT ["/opt/xpeer/bin/xpeer_client"]
CMD ["--host", "ac.testnet.xpeer.org", "--port", "8000", "-s", "/opt/xpeer/etc/trusted_peers.config.toml"]

ARG BUILD_DATE
ARG GIT_REV
ARG GIT_UPSTREAM

LABEL org.label-schema.schema-version="1.0"
LABEL org.label-schema.build-date=$BUILD_DATE
LABEL org.label-schema.vcs-ref=$GIT_REV
LABEL vcs-upstream=$GIT_UPSTREAM
