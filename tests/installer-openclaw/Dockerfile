# Installer test image: real OpenClaw (Node.js) + run install script + validate, then stay up.
# Build from repo root: docker build -f tests/installer-openclaw/Dockerfile -t alf-installer-openclaw .

FROM node:18-alpine

RUN apk add --no-cache curl ca-certificates git

# Non-root user for running alf and manual testing
RUN adduser -D -h /home/tester tester

# Install script (from repo) — used at container start to download alf
COPY scripts/install.sh /tmp/install.sh

# Test scripts
COPY tests/installer-openclaw/scripts/setup-openclaw.sh /opt/alf-test/
COPY tests/installer-openclaw/scripts/run-install-and-validate.sh /opt/alf-test/
RUN chmod +x /opt/alf-test/*.sh

# Install OpenClaw and create workspace at build time (as root, then chown)
ENV HOME=/home/tester
RUN /opt/alf-test/setup-openclaw.sh && chown -R tester:tester /home/tester/.openclaw /home/tester/openclaw

USER tester
ENV HOME=/home/tester
ENV INSTALL_SCRIPT=/tmp/install.sh

# Ensure local bin is in PATH for tester user
RUN echo 'export PATH="$HOME/.local/bin:$PATH"' >> /home/tester/.profile && \
    echo 'export PATH="$HOME/.local/bin:$PATH"' >> /home/tester/.bashrc
ENV PATH="/home/tester/.local/bin:${PATH}"

# Run install + validation, then keep container alive (even if install failed, e.g. no release yet)
CMD ["/bin/sh", "-c", "/opt/alf-test/run-install-and-validate.sh; exec sleep infinity"]
