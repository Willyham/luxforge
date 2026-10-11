# Grafana OSS with the ClickHouse datasource plugin baked in, so nothing is downloaded when the
# container starts. The base image is pinned by digest and the plugin archive by version and SHA-256
# (grafana.com's published checksum for the all-platform package of this version).
FROM grafana/grafana-oss:13.0.2@sha256:5dad0df181cb644a14e13617b913b261a54f7d4fd4510721dba420929f35bea2

USER root
ARG PLUGIN_VERSION=4.22.1
ARG PLUGIN_SHA256=97aad641f62ee4694b54c5bc225e40bb3f6c18cb3d9bfeeef7507a287f2e5a56
RUN mkdir -p /opt/luxforge/plugins \
 && wget -q -O /tmp/plugin.zip "https://grafana.com/api/plugins/grafana-clickhouse-datasource/versions/${PLUGIN_VERSION}/download" \
 && echo "${PLUGIN_SHA256}  /tmp/plugin.zip" | sha256sum -c - \
 && unzip -q /tmp/plugin.zip -d /opt/luxforge/plugins \
 && rm /tmp/plugin.zip \
 && chown -R 472:0 /opt/luxforge/plugins
USER 472
