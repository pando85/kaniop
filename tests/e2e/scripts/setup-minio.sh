#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="${1:-default}"
KIND_CLUSTER_NAME="${2:-chart-testing}"
MINIO_ACCESS_KEY="minioadmin"
MINIO_SECRET_KEY="minioadmin123"
BUCKET_NAME="kaniop-backups"
SILO_IMAGE="docker.io/pgsty/silo:RELEASE.2026-09-16T00-00-00Z"
MINIO_CREDS_SECRET="minio-creds"
MINIO_CREDS_LIMITED_SECRET="minio-creds-limited"

CERT_DIR=$(mktemp -d)
trap 'rm -rf "$CERT_DIR"' EXIT

openssl genrsa -out "$CERT_DIR/ca.key" 2048 2>/dev/null
openssl req -x509 -new -nodes -key "$CERT_DIR/ca.key" -sha256 -days 365 \
    -out "$CERT_DIR/ca.crt" -subj "/CN=MinIO CA" 2>/dev/null

openssl genrsa -out "$CERT_DIR/server.key" 2048 2>/dev/null
openssl req -new -key "$CERT_DIR/server.key" -out "$CERT_DIR/server.csr" \
    -subj "/CN=minio" 2>/dev/null

cat > "$CERT_DIR/san.ext" <<EOF
authorityKeyIdentifier=keyid,issuer
basicConstraints=CA:FALSE
keyUsage = digitalSignature, nonRepudiation, keyEncipherment, dataEncipherment
subjectAltName = @alt_names
[alt_names]
DNS.1 = minio
DNS.2 = minio.${NAMESPACE}
DNS.3 = minio.${NAMESPACE}.svc
DNS.4 = minio.${NAMESPACE}.svc.cluster.local
DNS.5 = localhost
IP.1 = 127.0.0.1
EOF

openssl x509 -req -in "$CERT_DIR/server.csr" \
    -CA "$CERT_DIR/ca.crt" -CAkey "$CERT_DIR/ca.key" -CAcreateserial \
    -out "$CERT_DIR/server.crt" -days 365 -sha256 \
    -extfile "$CERT_DIR/san.ext" 2>/dev/null

kubectl create secret generic minio-tls \
    --from-file=private.key="$CERT_DIR/server.key" \
    --from-file=public.crt="$CERT_DIR/server.crt" \
    -n "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

kubectl create configmap minio-ca \
    --from-file=ca-bundle.pem="$CERT_DIR/ca.crt" \
    -n "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

echo "Pulling Silo image and loading into kind cluster..."
docker pull "$SILO_IMAGE"
kind load --name "$KIND_CLUSTER_NAME" docker-image "$SILO_IMAGE"

kubectl create secret generic "${MINIO_CREDS_SECRET}" \
    --from-literal=AWS_ACCESS_KEY_ID="$MINIO_ACCESS_KEY" \
    --from-literal=AWS_SECRET_ACCESS_KEY="$MINIO_SECRET_KEY" \
    -n "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

kubectl create secret generic minio-creds-invalid \
    --from-literal=AWS_ACCESS_KEY_ID=wrongkey \
    --from-literal=AWS_SECRET_ACCESS_KEY=wrongsecret \
    -n "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

kubectl apply -n "$NAMESPACE" -f - <<YAML
apiVersion: apps/v1
kind: Deployment
metadata:
  name: minio
  namespace: ${NAMESPACE}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: minio
  template:
    metadata:
      labels:
        app: minio
    spec:
      containers:
      - name: minio
        image: ${SILO_IMAGE}
        args: ["server", "/data", "--certs-dir", "/certs"]
        env:
        - name: MINIO_ROOT_USER
          value: ${MINIO_ACCESS_KEY}
        - name: MINIO_ROOT_PASSWORD
          value: ${MINIO_SECRET_KEY}
        ports:
        - containerPort: 9000
        readinessProbe:
          tcpSocket:
            port: 9000
          initialDelaySeconds: 5
          periodSeconds: 3
        volumeMounts:
        - name: tls
          mountPath: /certs
        - name: data
          mountPath: /data
      volumes:
      - name: tls
        secret:
          secretName: minio-tls
      - name: data
        emptyDir: {}
---
apiVersion: v1
kind: Service
metadata:
  name: minio
  namespace: ${NAMESPACE}
spec:
  selector:
    app: minio
  ports:
  - port: 9000
    targetPort: 9000
YAML

echo "Waiting for MinIO deployment to be ready..."
if ! kubectl wait --for=condition=available deployment/minio -n "$NAMESPACE" --timeout=120s; then
    echo "ERROR: MinIO deployment did not become ready. Debugging info:"
    kubectl get pods -n "$NAMESPACE" -l app=minio -o wide
    kubectl describe deployment/minio -n "$NAMESPACE"
    kubectl describe pods -n "$NAMESPACE" -l app=minio
    kubectl logs -n "$NAMESPACE" -l app=minio --all-containers --tail=50 || true
    exit 1
fi

kubectl apply -n "$NAMESPACE" -f - <<YAML
apiVersion: batch/v1
kind: Job
metadata:
  name: minio-setup-bucket
  namespace: ${NAMESPACE}
spec:
  backoffLimit: 6
  template:
    spec:
      containers:
      - name: mc
        image: ${SILO_IMAGE}
        command:
        - /bin/sh
        - -c
        - |
          until mc alias set myminio https://minio:9000 ${MINIO_ACCESS_KEY} ${MINIO_SECRET_KEY} --insecure 2>/dev/null; do
            echo "Waiting for MinIO..."
            sleep 2
          done
          mc mb myminio/${BUCKET_NAME} --insecure --ignore-existing
          echo "Bucket ${BUCKET_NAME} created successfully"
      restartPolicy: OnFailure
YAML

echo "Waiting for bucket creation Job to complete..."
kubectl wait --for=condition=complete job/minio-setup-bucket -n "$NAMESPACE" --timeout=120s
kubectl delete job minio-setup-bucket -n "$NAMESPACE" --ignore-not-found=true

LOCK_BUCKET_NAME="kaniop-backups-lock"
LIMITED_USER="limited-user"
LIMITED_KEY="limitedpass123"

kubectl apply -n "$NAMESPACE" -f - <<YAML
apiVersion: batch/v1
kind: Job
metadata:
  name: minio-setup-lock-bucket
  namespace: ${NAMESPACE}
spec:
  backoffLimit: 6
  template:
    spec:
      containers:
      - name: mc
        image: ${SILO_IMAGE}
        command:
        - /bin/sh
        - -c
        - |
          set -ex

          WAIT=0
          until mc alias set myminio https://minio:9000 ${MINIO_ACCESS_KEY} ${MINIO_SECRET_KEY} --insecure 2>/dev/null; do
            echo "Waiting for MinIO..."
            sleep 2
            WAIT=\$((WAIT + 2))
            [ \$WAIT -ge 60 ] && { echo "Timeout waiting for MinIO"; exit 1; }
          done

          mc mb myminio/${LOCK_BUCKET_NAME} --with-lock --insecure --ignore-existing || \
            mc mb myminio/${LOCK_BUCKET_NAME} --insecure --ignore-existing
          echo "Bucket ${LOCK_BUCKET_NAME} created"

          if mc admin user info myminio ${LIMITED_USER} >/dev/null 2>&1; then
            echo "User ${LIMITED_USER} already exists"
          else
            mc admin user add myminio ${LIMITED_USER} ${LIMITED_KEY} || {
              echo "ERROR: Failed to create user ${LIMITED_USER}"
              mc admin user --help || true
              exit 1
            }
            echo "User ${LIMITED_USER} created"
          fi

          mc admin policy attach myminio readwrite --user ${LIMITED_USER} || true
          echo "Policy readwrite attached to ${LIMITED_USER}"

          WAIT=0
          until mc alias set limitedminio https://minio:9000 ${LIMITED_USER} ${LIMITED_KEY} --insecure 2>/dev/null; do
            echo "Waiting for limited user alias..."
            sleep 2
            WAIT=\$((WAIT + 2))
            [ \$WAIT -ge 30 ] && { echo "Timeout waiting for limited user alias"; exit 1; }
          done
          mc ls limitedminio/${LOCK_BUCKET_NAME} --insecure >/dev/null
          echo "Limited user ${LIMITED_USER} verified: can list bucket ${LOCK_BUCKET_NAME}"
      restartPolicy: OnFailure
YAML

echo "Waiting for lock bucket setup Job to complete..."
kubectl wait --for=condition=complete job/minio-setup-lock-bucket -n "$NAMESPACE" --timeout=120s
kubectl delete job minio-setup-lock-bucket -n "$NAMESPACE" --ignore-not-found=true

kubectl create secret generic "${MINIO_CREDS_LIMITED_SECRET}" \
    --from-literal=AWS_ACCESS_KEY_ID="${LIMITED_USER}" \
    --from-literal=AWS_SECRET_ACCESS_KEY="${LIMITED_KEY}" \
    -n "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -

echo "MinIO setup complete. Endpoint: https://minio.${NAMESPACE}.svc:9000"
