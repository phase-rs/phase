#!/bin/bash
# Generate a self-signed root CA + wildcard cert for *.local.phase-rs.dev
# and trust the root CA on macOS so the Tilt-managed Caddy reverse proxy
# (see Caddyfile + scripts/run-caddy.sh) serves the local dev frontend
# over HTTPS with no click-through warnings.
#
# Required because WebRTC DTLS (PeerJS P2P hosting) and crypto.randomUUID
# both refuse to operate on insecure origins other than localhost — so a
# bare http://192.168.x.y:5173 cannot host a P2P game for a LAN guest.
#
# Run once per machine:
#   ./scripts/setup-ssl.sh
#
# Then add the hostname to /etc/hosts (host machine):
#   sudo sh -c 'echo "127.0.0.1 local.phase-rs.dev app.local.phase-rs.dev" >> /etc/hosts'
#
# For each LAN guest device:
#   - Add `<HOST-LAN-IP> local.phase-rs.dev app.local.phase-rs.dev` to its /etc/hosts.
#   - Install the exported root CA (certs/local.phase-rs.dev/root-ca.crt) on
#     the guest device's trust store (Settings → General → VPN & Device
#     Management on iOS; double-click on macOS).

set -e

CERT_DIR=certs/local.phase-rs.dev
mkdir -p "$CERT_DIR"

# Root CA — generated once per machine; the trust anchor for all certs below.
if [ ! -f "$CERT_DIR/root-ca.crt" ]; then
  echo -n "Generating phase.rs local root CA... "
  openssl req \
    -x509 -nodes -sha256 -days 824 -newkey rsa:2048 \
    -keyout "$CERT_DIR/root-ca.key" \
    -out "$CERT_DIR/root-ca.crt" \
    -subj "/C=US/O=phase.rs/OU=local-dev/CN=local.phase-rs.dev" >/dev/null 2>&1
  echo "done"
fi

# Server key
if [ ! -f "$CERT_DIR/server.key" ]; then
  echo -n "Generating server private key... "
  openssl genrsa -out "$CERT_DIR/server.key" 2048 >/dev/null 2>&1
  echo "done"
fi

# CSR config — wildcard + apex so both work
cat > "$CERT_DIR/server.csr.conf" <<EOF
[ req ]
default_bits = 2048
prompt = no
default_md = sha256
req_extensions = req_ext
distinguished_name = dn

[ dn ]
C = US
O = phase.rs
OU = local-dev
CN = local.phase-rs.dev

[ req_ext ]
subjectAltName = @alt_names

[ alt_names ]
DNS.1 = local.phase-rs.dev
DNS.2 = *.local.phase-rs.dev
IP.1  = 127.0.0.1
EOF

if [ ! -f "$CERT_DIR/server.csr" ]; then
  openssl req -new -key "$CERT_DIR/server.key" \
    -out "$CERT_DIR/server.csr" \
    -config "$CERT_DIR/server.csr.conf" >/dev/null 2>&1
fi

# X509 extensions for the signed cert (re-state SANs)
cat > "$CERT_DIR/server.crt.conf" <<EOF
authorityKeyIdentifier=keyid,issuer
basicConstraints=CA:FALSE
keyUsage = digitalSignature, nonRepudiation, keyEncipherment, dataEncipherment
subjectAltName = @alt_names

[ alt_names ]
DNS.1 = local.phase-rs.dev
DNS.2 = *.local.phase-rs.dev
IP.1  = 127.0.0.1
EOF

if [ ! -f "$CERT_DIR/server.crt" ]; then
  echo -n "Generating server certificate... "
  openssl x509 -req -sha256 -days 824 \
    -in "$CERT_DIR/server.csr" \
    -CA "$CERT_DIR/root-ca.crt" \
    -CAkey "$CERT_DIR/root-ca.key" \
    -CAcreateserial \
    -out "$CERT_DIR/server.crt" \
    -extfile "$CERT_DIR/server.crt.conf" >/dev/null 2>&1
  echo "done"
fi

# Trust the root CA in the system keychain (macOS) — the one-time sudo prompt
# is the price for skipping all browser security warnings going forward.
if [[ "$OSTYPE" == darwin* ]]; then
  if ! security find-certificate -c "local.phase-rs.dev" /Library/Keychains/System.keychain >/dev/null 2>&1; then
    echo "Installing root CA into System keychain (sudo required)..."
    sudo security add-trusted-cert -d -r trustRoot \
      -k /Library/Keychains/System.keychain \
      "$CERT_DIR/root-ca.crt"
  else
    echo "Root CA already trusted in System keychain — skipping."
  fi
fi

# /etc/hosts hint — don't auto-edit; print what to add so it's deliberate.
if ! grep -q "local.phase-rs.dev" /etc/hosts 2>/dev/null; then
  cat <<EOF

Add this line to /etc/hosts (requires sudo):
  127.0.0.1 local.phase-rs.dev app.local.phase-rs.dev

One-liner:
  sudo sh -c 'echo "127.0.0.1 local.phase-rs.dev app.local.phase-rs.dev" >> /etc/hosts'
EOF
fi

echo
echo "SSL setup complete. Tilt's 'caddy' resource will serve https://local.phase-rs.dev"
