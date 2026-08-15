---
title: Webhook Validation
weight: 40000
---

# Webhook Validation for Kanidm Custom Resources

The Kaniop validating webhook prevents the creation of Kanidm custom resources (CRs) that would
conflict with existing objects in the same Kanidm cluster. It ensures that no two resources with the
same effective Kanidm entity name point to the same Kanidm cluster (`kanidmRef.name` +
`kanidmRef.namespace`), avoiding duplicate entries managed by different Kubernetes namespaces or
CRs that could otherwise collide inside Kanidm.

When the referenced `Kanidm` resource has external replication configured, the webhook also checks
Kanidm directly before accepting a new CR. This prevents a CR from implicitly adopting an entity
that was created in, or replicated from, another Kanidm cluster.

## How It Works

- **Scope:** Applies to Kanidm Group, Person, OAuth2 Client, and Service Account custom resources.
- **Kubernetes duplicate check:** Rejects `CREATE` requests if another CR with the same effective
  Kanidm entity name already references the same Kanidm cluster.
- **External replication check:** If `spec.externalReplicationNodes` is configured on the referenced
  `Kanidm`, performs an exact live lookup for the entity before allowing creation.
- **Effective entity name:** Uses `spec.kanidmName` when configured, otherwise the Kubernetes object
  name.
- **Failure behavior:** Once external replication is known to be enabled, inability to verify the
  entity against Kanidm rejects the request so a transient failure cannot turn into accidental
  ownership of an externally managed entity.
- **Apply ordering:** If the referenced `Kanidm` resource does not exist yet, the webhook preserves
  the existing behavior and allows the dependent CR to be applied first.

The webhook intentionally does not maintain a periodically refreshed inventory of Kanidm entities.
A cache miss could become stale immediately in a replicated deployment, so external entity checks
are performed against Kanidm at admission time. Authenticated Kanidm clients are reused between
requests to avoid repeating login setup for every validation.

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
- External-replication validation uses the operator-managed Kanidm credentials and TLS trust
  configuration.

## Troubleshooting

- If webhook requests are being rejected unexpectedly, check for existing CRs with the same
  effective `kanidmName` and Kanidm reference.
- For Kanidm clusters using external replication, check whether an entity with that name already
  exists directly in Kanidm.
- If validation reports that it cannot verify an externally replicated entity, check Kanidm
  availability, the admin password Secret, and the configured TLS Secret.
- Review webhook logs for detailed error messages (`logging.level: debug`).
- Ensure the webhook service is reachable and the certificate is valid.

---

For more details, see the [Helm chart documentation](../charts/kaniop/README.md) and
[values.yaml](../charts/kaniop/values.yaml).
