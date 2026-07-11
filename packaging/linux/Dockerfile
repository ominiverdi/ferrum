FROM docker.io/library/rust@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f

ARG DEBIAN_SNAPSHOT=20250920T000000Z
RUN rm -f /etc/apt/sources.list.d/debian.sources \
    && printf '%s\n' \
      "deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/${DEBIAN_SNAPSHOT}/ bookworm main" \
      "deb [check-valid-until=no] https://snapshot.debian.org/archive/debian-security/${DEBIAN_SNAPSHOT}/ bookworm-security main" \
      > /etc/apt/sources.list \
    && apt-get -o Acquire::Check-Valid-Until=false update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      binutils \
      ca-certificates \
      dpkg-dev \
      file \
      gzip \
      libssl-dev \
      pkg-config \
      rpm \
      xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-generate-rpm --locked --version 0.21.0

ENV CARGO_HOME=/cargo-home \
    CARGO_TARGET_DIR=/build/target \
    LC_ALL=C.UTF-8 \
    LANG=C.UTF-8
WORKDIR /workspace
ENTRYPOINT ["/workspace/scripts/package-linux-inner.sh"]
