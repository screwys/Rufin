FROM ghcr.io/servercontainers/samba:smbd-only-latest

ENV NETBIOS_DISABLE=1
ENV AVAHI_DISABLE=1
ENV WSDD2_DISABLE=1

# Enable only 2.0.2 - 3.1.1 dialects.
ENV SAMBA_GLOBAL_CONFIG_server_SPACE_min_SPACE_protocol=SMB2_02
ENV SAMBA_GLOBAL_CONFIG_server_SPACE_max_SPACE_protocol=SMB3_11

RUN mkdir -p /shares/MyShare /shares/PublicShare && \
    chmod -R 777 /shares
