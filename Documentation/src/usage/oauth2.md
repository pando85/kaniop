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
[`examples/oauth2.yaml`](https://github.com/pando85/kaniop/blob/main/examples/oauth2.yaml):

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
  originLanding: https://myapp.example.com
  originUrl: https://myapp.example.com
```

## Complete OAuth2 Configuration

The `examples/oauth2.yaml` file demonstrates many additional features:

- **Multiple redirect URIs**: For different environments
- **Scope management**: Controlling access permissions
- **Token configuration**: Setting token lifetimes
- **PKCE support**: Enhanced security for public clients

### Full OAuth2 Client Example

```yaml
# Extended example based on examples/oauth2.yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmOAuth2Client
metadata:
  name: production-app
  namespace: default
  labels:
    environment: production
    app: web-frontend
spec:
  kanidmRef:
    name: my-idm
  displayName: Production Web App
  originLanding: https://app.example.com
  originUrl: https://app.example.com
  allowInsecureClientDisablePkce: false # Always use PKCE
  jwtSignAlg: RS256
  scopes:
    - openid
    - profile
    - email
    - groups
  supplementaryScopeMap:
    read_admin: admin_group
    write_admin: super_admin_group
```

## OAuth2 Flow Types

The [`examples/oauth2.yaml`](https://github.com/pando85/kaniop/blob/main/examples/oauth2.yaml) file
shows configuration for different OAuth2 flows:

### Authorization Code Flow (Default)

```yaml
# Standard web application flow from examples/oauth2.yaml
spec:
  allowInsecureClientDisablePkce: false
  jwtSignAlg: RS256
```

### Public Client with PKCE

````yaml
# Public client configuration from examples/oauth2.yaml
spec:
  allowInsecureClientDisablePkce: false  # PKCE required
  enableLocalhostRedirects: true
```Kaniop configures OAuth2/OpenID Connect clients in Kanidm using the `KanidmOAuth2Client` Custom Resource. This enables secure authentication and authorization for your applications.

## Basic OAuth2 Client

Here's a simple OAuth2 client configuration:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmOAuth2Client
metadata:
  name: web-app
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  displayname: Web Application
  origin: https://app.example.com
  redirectUrl:
    - https://app.example.com/oauth2/callback
````

## OpenID Connect Client

For OIDC support, you must include scope mappings with the `openid` scope:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmOAuth2Client
metadata:
  name: oidc-app
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  displayname: OIDC Application
  origin: https://oidc-app.example.com
  redirectUrl:
    - https://oidc-app.example.com/auth/callback
  scopeMap:
    - group: authenticated-users
      scopes:
        - openid
        - profile
        - email
        - groups
```

## Advanced Configuration

### Multiple Redirect URLs

For applications with multiple environments:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmOAuth2Client
metadata:
  name: multi-env-app
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  displayname: Multi-Environment App
  origin: https://app.example.com
  redirectUrl:
    - https://app.example.com/auth/callback
    - https://staging-app.example.com/auth/callback
    - https://dev-app.example.com/auth/callback
```

### Scope Mappings by Group

Configure different access levels based on group membership:

```yaml
apiVersion: kaniop.rs/v1beta1
kind: KanidmOAuth2Client
metadata:
  name: admin-portal
  namespace: default
spec:
  kanidmRef:
    name: my-idm
  displayname: Admin Portal
  origin: https://admin.example.com
  redirectUrl:
    - https://admin.example.com/oauth2/callback
  scopeMap:
    - group: administrators
      scopes:
        - openid
        - profile
        - email
        - admin
    - group: operators
      scopes:
        - openid
        - profile
        - email
        - read-only
```

## Management Operations

### Apply OAuth2 Client

```bash
kubectl apply -f oauth2-client.yaml
```

### Check Client Status

```bash
kubectl get kanidmoauth2clients
kubectl describe kanidmoauth2client web-app
```

## Best Practices

- **Use HTTPS everywhere**: Never use HTTP for OAuth2 redirects in production
- **Implement PKCE**: Always use PKCE for public clients
- **Scope principle of least privilege**: Only grant necessary scopes to each client
- **Monitor authentication logs**: Keep track of authentication attempts and failures
