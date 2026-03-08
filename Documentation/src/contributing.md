---
title: Contributing
weight: 060000
---

# Contributing

We welcome contributions to Kaniop! This guide will help you get started with development and
contributing to the project.

## Development Environment Setup

### Prerequisites

- **Rust**: Install the latest stable Rust toolchain
- **Kubernetes**: Access to a Kubernetes cluster (kind, minikube, or remote)
- **kubectl**: Kubernetes command-line tool
- **Helm**: Package manager for Kubernetes
- **Docker**: For building container images

### Getting Started

1. **Fork and Clone**

   ```bash
   git clone https://github.com/<your-username>/kaniop.git
   cd kaniop
   ```

2. **Install Rust Toolchain**

   ```bash
   rustup toolchain install stable
   rustup default stable
   rustup component add rustfmt clippy
   ```

3. **Build the Project**

   ```bash
   make build
   make test
   ```

## Code Generation

Kaniop uses code generation for CRDs and examples:

### Generate CRDs

```bash
make crdgen
```

### Generate Examples

```bash
make examples
```

## Testing

### Unit Tests

```bash
make test
```

### Integration Tests

Integration tests require a running Kubernetes cluster:

```bash
make integration-test
```

### End-to-End Tests

Full E2E tests including Kanidm deployment:

```bash
make e2e-test
```

## Contribution Guidelines

### Pull Request Process

1. **Create a Feature Branch**

   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make Your Changes**

   - Write tests for new functionality
   - Update documentation as needed
   - Follow existing code style

3. **Test Your Changes**

   ```bash
   make test
   make lint
   ```

4. **Commit and Push**

   ```bash
   git commit -m "feat: add new feature description"
   git push origin feature/your-feature-name
   ```

Follow [Conventional Commits](https://www.conventionalcommits.org/) format.

## Documentation

### Book Documentation

Preview the documentation book:

```bash
make book
```

## Coding Standards

### Code Formatting

Use `rustfmt` for all code (automatic via `make lint`). See `rustfmt.yaml`

#### Code Organization

- **Imports**: Always at the top level, grouped by origin:

  ```rust
  // Crate imports
  use crate::crd::KanidmPerson;

  // Internal crates
  use kaniop_operator::reconcile;

  // Standard library
  use std::collections::HashMap;
  use std::sync::Arc;

  // External crates
  use anyhow::Context;
  use kube::Client;
  ```

- **Never inline imports**: Don't add `use` statements inside functions or impl blocks

### Clippy

All clippy warnings must be resolved:

```bash
make lint
```

We use `#![deny(warnings)]` in CI, so your code must be warning-free.

### Dependencies

- **Minimize dependencies**: Only add if no internal equivalent exists
- **Shared dependencies**: Add to `[workspace.dependencies]` in root `Cargo.toml`
- **Version pinning**: Use specific versions for reproducible builds
- **Security**: We regularly update dependencies via Renovate

## Testing

### Test Strategy

1. **Unit Tests**: Test individual functions and modules
2. **Integration Tests**: Test interaction with external services (Tempo tracing)
3. **E2E Tests**: Main way to test, it tests full operator behavior in Kind cluster

## Community

Keep docs in sync with code changes and CRD definitions.

## Community

- **GitHub Issues**: [Report bugs or request features](https://github.com/pando85/kaniop/issues)
- **GitHub Discussions**:
  [Ask questions and discuss ideas](https://github.com/pando85/kaniop/discussions)

Thank you for contributing to Kaniop!
