# The `1` tag tracks the latest Rust stable (with rustfmt and clippy), which is
# what CI installs; rebuild the container to pick up a new Rust release.
FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

# The container runs as root without CAP_DAC_OVERRIDE / CAP_DAC_READ_SEARCH
# (see runArgs in devcontainer.json), so root cannot enter the 0700 `_apt`-owned
# download directories apt normally drops privileges into. Have apt fetch as
# root, and hand the base image's `_apt`-owned partial directory to root, so
# `playwright install --with-deps` in postCreateCommand can still use apt.
RUN echo 'APT::Sandbox::User "root";' > /etc/apt/apt.conf.d/99sandbox-root \
    && chown -R root:root /var/cache/apt/archives/partial

# Tauri/WebKitGTK build dependencies, matching the apt list in
# .github/workflows/build.yml. Debian ships the tray library as
# libayatana-appindicator3-dev; Ubuntu's libappindicator3-dev does not exist
# here.
RUN apt-get update \
    && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        patchelf \
        libsecret-1-dev \
    && rm -rf /var/lib/apt/lists/*
