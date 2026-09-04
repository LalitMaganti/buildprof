FROM ubuntu:24.04

ARG RUST_VERSION=1.91
ENV DEBIAN_FRONTEND=noninteractive
ENV UV_PROJECT_ENVIRONMENT=/opt/buildprof-venv
ENV CARGO_HOME=/opt/cargo
ENV RUSTUP_HOME=/opt/rustup
ENV PATH=/opt/uv/bin:/opt/buildprof-venv/bin:/opt/cargo/bin:${PATH}

# Install every build system covered by the conformance suite.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates curl gcc g++ libc6-dev python3 python3-venv git \
       make cmake ninja-build meson golang-go binutils llvm \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rustup component add clippy rustfmt \
    && rustc --version \
    && cargo --version

RUN python3 -m venv /opt/uv \
    && /opt/uv/bin/pip install --no-cache-dir uv

COPY pyproject.toml uv.lock /tmp/buildprof-deps/
RUN cd /tmp/buildprof-deps \
    && uv sync --frozen --no-install-project

WORKDIR /work
CMD ["sleep", "infinity"]
