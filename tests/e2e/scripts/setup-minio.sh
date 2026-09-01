#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="${1:-default}"
MINIO_ACCESS_KEY="minioadmin"
MINIO_SECRET_KEY="minioadmin123"
BUCKET_NAME="kaniop-backups"

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

kubectl create secret generic minio-creds \
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
        image: minio/minio:latest
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
kubectl wait --for=condition=available deployment/minio -n "$NAMESPACE" --timeout=120s

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
        image: minio/mc:latest
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

echo "MinIO setup complete. Endpoint: https://minio.${NAMESPACE}.svc:9000"
