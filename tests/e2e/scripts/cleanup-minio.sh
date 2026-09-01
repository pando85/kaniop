#!/usr/bin/env bash
set -euo pipefail

NAMESPACE="${1:-default}"

kubectl delete job minio-setup-bucket -n "$NAMESPACE" --ignore-not-found=true
kubectl delete deployment minio -n "$NAMESPACE" --ignore-not-found=true
kubectl delete service minio -n "$NAMESPACE" --ignore-not-found=true
kubectl delete secret minio-tls minio-creds minio-creds-invalid -n "$NAMESPACE" --ignore-not-found=true
kubectl delete configmap minio-ca -n "$NAMESPACE" --ignore-not-found=true

echo "MinIO cleanup complete."
