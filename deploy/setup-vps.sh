#!/usr/bin/env bash
set -euo pipefail

# Run this on the VPS with sudo: sudo bash setup-vps.sh
# Sets up Docker runtime access and nginx reverse proxy for phase-server.

echo "=== phase-server VPS setup ==="

# Create deploy user (SSH login for CI, scoped sudo)
if id deploy &>/dev/null; then
    echo "User 'deploy' already exists"
else
    useradd --create-home --shell /bin/bash deploy
    echo "Created user 'deploy'"
fi

# Set up deploy user SSH
mkdir -p /home/deploy/.ssh
chmod 700 /home/deploy/.ssh
touch /home/deploy/.ssh/authorized_keys
chmod 600 /home/deploy/.ssh/authorized_keys
chown -R deploy:deploy /home/deploy/.ssh
echo "Configured /home/deploy/.ssh/"

# Install Docker if the host does not already have it.
if command -v docker &>/dev/null; then
    echo "Docker already installed"
else
    apt-get update
    apt-get install -y docker.io
    echo "Installed docker.io"
fi
systemctl enable --now docker
echo "Docker service enabled"

# Install curl if the host does not already have it. Deployment verification
# uses host curl against the loopback health endpoint.
if command -v curl &>/dev/null; then
    echo "curl already installed"
else
    apt-get update
    apt-get install -y curl
    echo "Installed curl"
fi

# Install jq if the host does not already have it. deploy.sh uses jq to resolve
# the latest GitHub Release when no tag is provided.
if command -v jq &>/dev/null; then
    echo "jq already installed"
else
    apt-get update
    apt-get install -y jq
    echo "Installed jq"
fi

# Install nginx if the host does not already have it.
if command -v nginx &>/dev/null; then
    echo "nginx already installed"
else
    apt-get update
    apt-get install -y nginx
    echo "Installed nginx"
fi

# Grant deploy user scoped passwordless sudo for container deployment.
cat > /etc/sudoers.d/phase-deploy << 'SUDOERS'
deploy ALL=(ALL) NOPASSWD: /usr/bin/systemctl stop phase-server, /usr/bin/systemctl disable phase-server, /usr/bin/systemctl mask phase-server, /usr/bin/docker
SUDOERS
chmod 440 /etc/sudoers.d/phase-deploy
echo "Configured passwordless sudo for deploy user"

# Create persistent runtime volume. The container entrypoint owns internal
# file permissions so the host only needs to keep the named volume around.
docker volume create phase-server-data >/dev/null
echo "Created phase-server-data Docker volume"

# Install nginx config
cp phase-server.nginx.conf /etc/nginx/sites-available/phase-server
ln -sf /etc/nginx/sites-available/phase-server /etc/nginx/sites-enabled/phase-server
rm -f /etc/nginx/sites-enabled/default
nginx -t && systemctl reload nginx
echo "Configured nginx reverse proxy"

echo ""
echo "=== Setup complete ==="
echo "Add your deploy public key to /home/deploy/.ssh/authorized_keys"
echo "Then run ./deploy.sh to pull and start the server image"
