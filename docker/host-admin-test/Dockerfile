FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends openssh-server sudo procps iproute2 util-linux python3 && rm -rf /var/lib/apt/lists/* && \
    useradd --create-home admin && printf 'admin:fixture-password\n' | chpasswd && \
    printf 'admin ALL=(root) ALL\n' > /etc/sudoers.d/admin && chmod 440 /etc/sudoers.d/admin && \
    printf '11223344556677889900112233445566\n' > /etc/machine-id
COPY server.sh /server.sh
COPY --chmod=755 update-tools.py /usr/local/bin/update-tools
RUN for tool in skopeo systemd-run systemctl docker podman; do ln -s /usr/local/bin/update-tools /usr/local/bin/$tool; done
ENTRYPOINT ["/bin/sh", "/server.sh"]
