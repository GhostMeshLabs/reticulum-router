FROM docker.io/rust:alpine3.23 as BUILD
RUN mkdir -p /tmp/src && apk update && apk add protobuf
COPY . /tmp/src
WORKDIR /tmp/src
RUN cargo build -r


FROM docker.io/alpine:3.23
COPY --from=BUILD /tmp/src/target/release/reticulum-router /usr/local/bin/reticulum-router
ENTRYPOINT "/usr/local/bin/reticulum-router"
EXPOSE 4242/tcp
EXPOSE 4242/udp
