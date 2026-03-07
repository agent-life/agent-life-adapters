# Installer test image: simulated OpenClaw + run install script + validate, then stay up.
# Build from repo root: docker build -f tests/installer/Dockerfile .

FROM alpine:3.20

RUN apk add --no-cache curl ca-certificates

# Non-root user for running alf and manual testing
RUN adduser -D -h /home/tester tester

# Install script (from repo) — used at container start to download alf
COPY scripts/install.sh /tmp/install.sh

# Test scripts
COPY tests/installer/scripts/setup-openclaw.sh /opt/alf-test/
COPY tests/installer/scripts/run-install-and-validate.sh /opt/alf-test/
RUN chmod +x /opt/alf-test/*.sh

# Create simulated OpenClaw workspace at build time (as root, then chown)
ENV HOME=/home/tester
RUN /opt/alf-test/setup-openclaw.sh && chown -R tester:tester /home/tester/.openclaw

USER tester
ENV HOME=/home/tester
ENV INSTALL_SCRIPT=/tmp/install.sh

# Run install + validation, then keep container alive (even if install failed, e.g. no release yet)
CMD ["/bin/sh", "-c", "/opt/alf-test/run-install-and-validate.sh; exec sleep infinity"]
