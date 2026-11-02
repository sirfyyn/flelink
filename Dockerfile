# FLE Link in a container: the Studio on port 8642, data (settings, engines, models) in the volume /data.
#   docker run -d -p 127.0.0.1:8642:8642 -v flelink:/data ghcr.io/sirfyyn/flelink
# Requests from outside the container need the access key it prints at the first start (docker logs) or that Settings shows.
# NVIDIA: add --gpus all (needs the NVIDIA Container Toolkit on the host).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p flelink

FROM debian:bookworm-slim
# what the prebuilt llama.cpp and FLE engines for Linux load: OpenMP, curl, TLS roots, the Vulkan loader
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libgomp1 libcurl4 libvulkan1 \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/flelink /usr/local/bin/flelink
ENV FLE_HOME=/data
VOLUME /data
EXPOSE 8642
# the Studio on all interfaces of the container; plain http, since the port is only published where docker run says
ENTRYPOINT ["flelink"]
CMD ["studio", "--host", "0.0.0.0", "--no-tls", "--no-browser", "--home", "/data"]
