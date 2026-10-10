---
title: OAuth2 Client Management
weight: 31000
indent: true
---

# OAuth2 Client Management

Kaniop manages OAuth2 client registrations in your Kanidm cluster using the `KanidmOAuth2Client`
Custom Resource. This enables secure application integration with your identity provider.

## Basic OAuth2 Client

Here's a simple OAuth2 client configuration. You can reference the complete example at
[`examples/oauth2.yaml`](https://github.com/pando85/kaniop/blob/{{KANIOP_VERSION}}/examples/oauth2.yaml):

```yaml
# Basic configuration - see examples/oauth2.yaml for all options
apiVersion: kaniop.rs/v1beta1
kind: KanidmOAuth2Client
metadata:
  name: my-webapp
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  displayName: My Web Application
  origin: https://myapp.example.com
  redirectUrl:
    - https://myapp.example.com/oauth2/callback
```

This will create an Oauth2 client secret named `my-webapp-kanidm-oauth2-credentials` in the same
namespace as the OAuth2 client.

## Refresh token lifetime

Kanidm refresh tokens expire after **16 hours** by default. For clients that stay idle for
longer periods, such as desktop sync applications, set `spec.refreshTokenExpiry` to a
positive number of seconds:

```yaml
spec:
  refreshTokenExpiry: 7776000 # 90 days
```

When configured, Kaniop reconciles this lifetime and restores it if changed directly in
Kanidm. If omitted, Kaniop **does not manage** the attribute: existing values are left
untouched, and new clients use Kanidm's default. Removing the field later also leaves its
last value in Kanidm; it does **not** reset it to the default. You can reset it using the
Kanidm CLI if needed.

Longer refresh-token lifetimes increase exposure if a token is stolen. The OAuth2 session
remains subject to other Kanidm session and account policies.
