---
title: OAuth2 Client Management
weight: 031000
indent: true
---

# OAuth2 Client Management

Kaniop manages OAuth2 client registrations in your Kanidm cluster using the `KanidmOAuth2Client`
Custom Resource. This enables secure application integration with your identity provider.

## Basic OAuth2 Client

Here's a simple OAuth2 client configuration. You can reference the complete example at
[`examples/oauth2.yaml`](https://github.com/pando85/kaniop/blob/master/examples/oauth2.yaml):

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
