#!/usr/bin/env bash
set -euo pipefail

# Run this on the VPS with sudo: sudo bash setup-vps.sh
# Sets up phase-server systemd service and nginx reverse proxy

echo "=== phase-server VPS setup ==="

# Create runtime user (runs the service, no login)
if id phase &>/dev/null; then
    echo "User 'phase' already exists"
else
    useradd --system --no-create-home --shell /usr/sbin/nologin phase
    echo "Created system user 'phase'"
fi

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

# Grant deploy user scoped passwordless sudo for phase-server management
cat > /etc/sudoers.d/phase-deploy << 'SUDOERS'
deploy ALL=(ALL) NOPASSWD: /usr/bin/systemctl stop phase-server, /usr/bin/systemctl start phase-server, /usr/bin/systemctl is-active phase-server, /usr/bin/systemctl daemon-reload, /usr/bin/cp, /usr/bin/chmod, /usr/bin/chown
SUDOERS
chmod 440 /etc/sudoers.d/phase-deploy
echo "Configured passwordless sudo for deploy user"

# Create directory structure
mkdir -p /opt/phase-server/data
chown -R phase:phase /opt/phase-server
echo "Created /opt/phase-server/"

# Install systemd service
cp phase-server.service /etc/systemd/system/phase-server.service
systemctl daemon-reload
systemctl enable phase-server
echo "Installed and enabled phase-server.service"

# Install nginx config
cp phase-server.nginx.conf /etc/nginx/sites-available/phase-server
ln -sf /etc/nginx/sites-available/phase-server /etc/nginx/sites-enabled/phase-server
rm -f /etc/nginx/sites-enabled/default
nginx -t && systemctl reload nginx
echo "Configured nginx reverse proxy"

echo ""
echo "=== Setup complete ==="
echo "Add your deploy public key to /home/deploy/.ssh/authorized_keys"
echo "Then run ./deploy.sh to download and start the server"
