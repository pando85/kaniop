<img src="https://raw.githubusercontent.com/pando85/kaniop/master/artwork/logo.png" width="20%" height="auto" />

# Kaniop

This repository contains an opinionated example of a Kubernetes Operator built using Rust. It is based on [kube-rs](https://kube.rs/) and demonstrates advanced features and patterns for building effective, scalable Kubernetes operators.

## Overview

This Operator implements **reflectors** (equivalent to informers in Go) to store and share the state between controllers. This design enables a **bidirectional reconciliation** between Custom Resource Definitions (CRDs) and the objects created within the Kubernetes cluster, enhancing the responsiveness and robustness of the Operator.

## Features

- **Bidirectional Reconciliation**: Utilizes reflectors to maintain synchronization between CRDs and Kubernetes objects.
- **State Management**: Stores and shares the state across multiple controllers to ensure consistent behavior and data integrity.
- **Testing**: Comprehensive testing suite that includes:
  - **Unit Tests**: Validate the functionality of individual components.
  - **Integration Tests**: Ensure the interaction between different parts of the system works as intended.
  - **End-to-End (E2E) Tests**: Simulate real-world scenarios to verify the Operator's behavior in a Kubernetes environment.
  - **Tracing and Metrics**: Implements OTLP through `opentelemetry-otlp` for distributed tracing, structured logging, and metrics collection, enabling comprehensive monitoring of the Operator's performance and behavior in production.
