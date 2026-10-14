FROM public.ecr.aws/ubuntu/ubuntu:24.04@sha256:f610ab94648195aa356059f5b41d6085c9d4d903c072430cdd1af7bdb646106b
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq && apt-get install -y --no-install-recommends \
    ca-certificates curl git python3 docker.io docker-buildx iproute2 \
    kmod dmsetup util-linux unzip jq lsof openssh-client && rm -rf /var/lib/apt/lists/*
WORKDIR /opt/space-tools
COPY infra/space/kind/scripts/install-prereqs.sh infra/space/kind/scripts/install-prereqs.sh
COPY scripts/lib/space-smoke-prereqs.sh scripts/lib/space-smoke-prereqs.sh
RUN bash infra/space/kind/scripts/install-prereqs.sh
COPY ci/scripts/ensure-uv.sh ci/scripts/ensure-uv.sh
RUN uv_bin="$(bash ci/scripts/ensure-uv.sh)" && ln -s "$uv_bin" /usr/local/bin/uv
ENV PATH=/root/.local/bin:/usr/local/go/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
WORKDIR /workspace
CMD ["sleep", "infinity"]
