#!/bin/sh
# Stop and remove the alf-openclaw container and image.

set -x

docker stop alf-openclaw 2>/dev/null || true
docker rm alf-openclaw 2>/dev/null || true
docker rmi alf-installer-openclaw 2>/dev/null || true
