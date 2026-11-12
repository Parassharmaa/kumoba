FROM debian:trixie-slim

ARG DEBIAN_FRONTEND=noninteractive
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    bash ca-certificates curl wget git jq unzip zip xz-utils procps sudo locales less file \
    openssh-client iproute2 \
    python3 python3-pip python3-venv python3-pil python3-pyatspi gir1.2-atspi-2.0 \
    nodejs npm \
    chromium fonts-dejavu fonts-liberation fonts-noto-cjk \
    xvfb xfce4-session xfwm4 xfdesktop4 xfce4-panel xfce4-terminal xfce4-settings \
    dbus-x11 at-spi2-core x11-utils x11vnc novnc websockify scrot xdotool libnss3-tools \
  && rm -rf /var/lib/apt/lists/* \
  && sed -i 's/^# *\(en_US.UTF-8\)/\1/' /etc/locale.gen \
  && locale-gen

RUN useradd --uid 1000 --create-home --shell /bin/bash kumoba \
  && printf '%s\n' 'kumoba ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/kumoba \
  && printf '%s\n' 'Defaults env_keep += "HTTP_PROXY HTTPS_PROXY NO_PROXY http_proxy https_proxy no_proxy"' > /etc/sudoers.d/proxy \
  && chmod 0440 /etc/sudoers.d/kumoba /etc/sudoers.d/proxy \
  && mkdir -p /workspace \
  && chown kumoba:kumoba /workspace

COPY kumoba-guest /usr/local/bin/kumoba-guest
COPY rootfs/ /
RUN chmod 0755 /usr/local/bin/kumoba-guest /usr/local/bin/kumoba-desktop

ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8
WORKDIR /workspace
