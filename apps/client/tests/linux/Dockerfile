FROM ubuntu:24.04@sha256:224a1869083a311ef3f13648a154ba79832fbef6364d31493642ca03082da254

# Ubuntu 最小镜像尚未安装 CA 包；只取可信镜像的公共 CA，供首轮 HTTPS apt 引导。
COPY --from=nginx:1.28-alpine@sha256:a8b39bd9cf0f83869a2162827a0caf6137ddf759d50a171451b335cecc87d236 /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# 仅用于本机 Docker Desktop 的 Linux/systemd 验收，不是产品分发镜像。
ENV container=docker
ARG DEBIAN_FRONTEND=noninteractive
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    rm -f /etc/apt/apt.conf.d/docker-clean \
    && sed -i 's|http://|https://|g' /etc/apt/sources.list.d/ubuntu.sources \
    && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 update \
    && apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 install -y --no-install-recommends \
    systemd systemd-sysv dbus dbus-user-session sudo util-linux \
    ca-certificates curl xz-utils openssl python3 procps iproute2 \
    build-essential pkg-config libssl-dev git \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 2000 --shell /bin/bash eztest \
    && printf 'eztest ALL=(ALL) NOPASSWD:ALL\n' > /etc/sudoers.d/eztest \
    && chmod 0440 /etc/sudoers.d/eztest \
    && mkdir -p /work /var/log/journal \
    && chown eztest:eztest /work \
    && truncate -s 0 /etc/machine-id

# prepare-node.py 经宿主已配置的代理下载并核验官方包；构建阶段再次校验。
COPY --from=node-runtime /node.tar.xz /node.sha256 /tmp/
RUN cd /tmp && sha256sum --check node.sha256 \
    && tar -xJf node.tar.xz --strip-components=1 -C /usr/local \
    && rm node.tar.xz node.sha256 \
    && test "$(node --version)" = "v24.21.0" && npm --version

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
