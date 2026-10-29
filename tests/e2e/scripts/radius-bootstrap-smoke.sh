#!/usr/bin/env bash
# Verify the published Kanidm RADIUS container bootstrap, not real authentication.
# Run: bash tests/e2e/scripts/radius-bootstrap-smoke.sh
set -euo pipefail

IMAGE="kanidm/radius:1.11.2"
WORKDIR="$(mktemp -d)"
NAME="kaniop-radius-smoke-$$"

cleanup() {
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    rm -rf "$WORKDIR"
}
trap cleanup EXIT

# Synthetic credentials; never use real identity/RADIUS credentials for CI.
openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
    -subj "/CN=radius.example.invalid" \
    -keyout "$WORKDIR/key.pem" -out "$WORKDIR/cert.pem" >/dev/null 2>&1
cp "$WORKDIR/cert.pem" "$WORKDIR/ca.pem"
cat > "$WORKDIR/radius.toml" <<'TOML'
uri = "https://idm.example.invalid"
auth_token = "synthetic-testing-token"
verify_hostnames = true
verify_certificate = true
radius_cert_path = "/data/cert.pem"
radius_key_path = "/data/key.pem"
radius_ca_path = "/data/ca.pem"
radius_default_vlan = 10
radius_required_groups = ["radius_access_allowed@idm.example.invalid"]
radius_groups = [
  { spn = "staff@idm.example.invalid", vlan = 20 },
]
radius_clients = [
  { name = "mikrotik", ipaddr = "192.0.2.10/32", secret = "long_testing_radius_secret_1234" },
]
TOML

# A read-only /data mount mirrors K8s Secret projection. Container runs under
# upstream radiusd user, not host UID: allow only these synthetic keys to be read.
chmod 0644 "$WORKDIR/radius.toml" "$WORKDIR/cert.pem" "$WORKDIR/ca.pem" "$WORKDIR/key.pem"

docker pull "$IMAGE" >/dev/null
IMAGE_USER="$(docker image inspect "$IMAGE" --format '{{.Config.User}}')"
if [[ -z "$IMAGE_USER" || "$IMAGE_USER" == "0" || "$IMAGE_USER" == "root" ]]; then
    echo "RADIUS image does not declare a non-root user" >&2
    exit 1
fi

docker run -d --name "$NAME" \
    --network none \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --mount "type=bind,src=$WORKDIR,dst=/data,readonly" \
    "$IMAGE" >/dev/null

for attempt in $(seq 1 20); do
    if [[ "$(docker inspect -f '{{.State.Running}}' "$NAME")" != "true" ]]; then
        echo "RADIUS exited during startup" >&2
        docker logs "$NAME" >&2
        exit 1
    fi
    # Bootstrap writes files inside the writable upstream /etc/raddb layer.
    if docker exec "$NAME" /bin/sh -c \
        'test -s /etc/raddb/clients.conf && test -s /etc/raddb/certs/server.pem && grep -q "require_message_authenticator = yes" /etc/raddb/clients.conf'; then
        echo "RADIUS bootstrap created clients.conf, cert bundle and Message-Authenticator policy"
        exit 0
    fi
    sleep 1
done

echo "RADIUS bootstrap never produced expected FreeRADIUS files" >&2
docker logs "$NAME" >&2
exit 1
