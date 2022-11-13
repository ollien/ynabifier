FROM rust:alpine3.16

ARG UID
ARG GID

RUN apk add openssl-dev libc-dev

ENV CARGO_HOME='/.cargo'
# https://users.rust-lang.org/t/sigsegv-with-program-linked-against-openssl-in-an-alpine-container/52172/4
ENV RUSTFLAGS='-C target-feature=-crt-static'
COPY . /app
WORKDIR /app
RUN  \
	--mount=type=cache,target=/app/target \
    --mount=type=cache,target=/.cargo \
	cargo build --release && \
	ls target/release && \
	cp target/release/ynabifier /usr/local/bin && \
	addgroup -S rust -g ${GID:-9990} && \
	adduser -S rust -u ${UID:-9990} -G rust
USER rust

CMD /usr/local/bin/ynabifier
