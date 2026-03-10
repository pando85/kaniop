---
title: Webhook Validation
weight: 040000
---

# Webhook Validation for Kanidm Custom Resources

The Kaniop validating webhook prevents the creation of Kanidm custom resources (CRs) that would
conflict with existing objects in the same Kanidm cluster. It ensures that no two resources with the
same name point to the same Kanidm cluster (`kanidmRef.name` + `kanidmRef.namespace`), avoiding
duplicate entries managed by different Kubernetes namespaces or CRs that could otherwise collide
inside Kanidm.

## How It Works

- **Scope:** Applies to Kanidm custom resources (e.g., Group, Person, OAuth2).
- **Action:** Rejects `CREATE` requests if another resource with the same name already references
  the same Kanidm cluster.
- **Purpose:** Prevents duplicate or conflicting entries in Kanidm, ensuring data consistency and
  integrity.

## Deployment

The webhook runs as a separate deployment and exposes an HTTPS server backed by a TLS certificate.
You can configure certificate management in two ways:

### 1. Built-in Certificate Generation (Default)

- A Helm job generates a long-lived certificate at install time.
- No external dependencies required.
- Controlled via `webhook.patch` settings in `values.yaml`.

### 2. cert-manager Integration

- Use cert-manager to manage webhook TLS certificates.
- Optionally specify an `issuerRef` or use a self-signed issuer.
- Controlled via `webhook.certManager` settings in `values.yaml`.

## Configuration Options

All options are set in the Helm chart's `values.yaml` under the `webhook:` key. Key settings
include:

- `enabled`: Enable or disable the webhook deployment.
- `port`: Webhook server port (default: 8443).
- `logging.level`: Set the log level (e.g., `info`, `debug`).
- `service.*`: Service configuration for the webhook.
- `patch.*`: Settings for the certificate generation job.
- `certManager.*`: Enable and configure cert-manager integration.
- `image.*`: Webhook container image settings.
- `replicas`: Number of webhook replicas.
- `resources`, `nodeSelector`, `tolerations`, `affinity`: Standard Kubernetes deployment options.

See the [values.yaml](../charts/kaniop/values.yaml) for all available options and documentation.

## Example: Enabling the Webhook

```yaml
webhook:
  enabled: true
  patch:
    enabled: true
  certManager:
    enabled: false
```

## Security

- The webhook server always uses HTTPS.
- TLS certificates are managed automatically (via Helm job or cert-manager).
- RBAC and service account resources are created as needed for secure operation.

## Troubleshooting

- If webhook requests are being rejected unexpectedly, check for existing CRs with the same name and
  Kanidm reference.
- Review webhook logs for detailed error messages (`logging.level: debug`).
- Ensure the webhook service is reachable and the certificate is valid.

---

For more details, see the [Helm chart documentation](../charts/kaniop/README.md) and
[values.yaml](../charts/kaniop/values.yaml).
