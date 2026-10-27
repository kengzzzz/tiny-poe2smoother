ARG TARGET

FROM debian:trixie-slim AS build

ARG TARGET
ENV TARGET=${TARGET}
ENV DEBIAN_FRONTEND=noninteractive

WORKDIR /build

RUN apt-get update && apt-get install -y \
    curl \
    pkg-config \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain none --profile minimal --no-modify-path
ENV CARGO_HOME=/root/.cargo
ENV PATH="${CARGO_HOME}/bin:${PATH}"

COPY rust-toolchain.toml ./
RUN rustup toolchain install

COPY docker-build.sh /docker-build.sh

RUN /docker-build.sh setup

COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ src/

RUN /docker-build.sh fetch

COPY . .

RUN /docker-build.sh build

RUN /docker-build.sh export

FROM build AS test-build

RUN /docker-build.sh tests

FROM scratch AS tests

COPY --from=test-build /tests /

FROM scratch AS output

COPY --from=build /output /
