FROM rust:1.90-slim-bookworm AS build
COPY . /build/
RUN cd /build && cargo build --release --locked --bin taiga-snake

FROM debian:bookworm-slim
COPY --from=build /build/target/release/taiga-snake /usr/local/bin/taiga-snake
ENV PORT=8080
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/taiga-snake"]
