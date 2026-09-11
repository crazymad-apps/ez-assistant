FROM ubuntu:24.04@sha256:224a1869083a311ef3f13648a154ba79832fbef6364d31493642ca03082da254
COPY --from=nginx:1.28-alpine@sha256:a8b39bd9cf0f83869a2162827a0caf6137ddf759d50a171451b335cecc87d236 /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# 包验收环境仅保留 Linux 服务运行依赖；Node 和 Host 必须来自待测归档。
ENV container=docker
ARG DEBIAN_FRONTEND=noninteractive
RUN sed -i 's|http://|https://|g' /etc/apt/sources.list.d/ubuntu.sources \
    && apt-get -o Acquire::Retries=5 update \
    && apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
       systemd systemd-sysv dbus dbus-user-session util-linux \
       ca-certificates libssl3t64 procps \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 2000 --shell /bin/bash eztest \
    && mkdir -p /work /var/log/journal \
    && chown eztest:eztest /work \
    && truncate -s 0 /etc/machine-id
STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
