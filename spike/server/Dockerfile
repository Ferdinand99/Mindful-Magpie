FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release --jobs 4

FROM alpine:3
COPY --from=build /src/target/release/magpie-spike-server /usr/local/bin/magpie-spike-server
EXPOSE 8099
CMD ["magpie-spike-server"]
