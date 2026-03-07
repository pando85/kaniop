---
title: Troubleshooting
weight: 050000
---

# Troubleshooting

When something goes wrong, consult the following steps to diagnose and resolve issues with Kaniop:

1. Operator Status

   Check that the operator pod is running and healthy:

   ```bash
   kubectl get pods -n kaniop
   kubectl logs deployment/kaniop -n kaniop
   ```

2. Resource Conditions

   Inspect the status of your Custom Resources (CRs):

   ```bash
   kubectl describe <kind> <name>
   ```

   Look for `Conditions` and `Events` sections to find reconciliation errors.

3. Events and Logs

   Check cluster events for errors:

   ```bash
   kubectl get events --sort-by '.metadata.creationTimestamp'
   ```

   Tail operator logs for insights:

   ```bash
   kubectl logs -f deployment/kaniop -n kaniop
   ```

4. Common Issues

   - **CRD not found**: Ensure CRDs are installed or upgrade chart with CRDs.
   - **RBAC errors**: Verify service account and ClusterRoleBindings.
   - **Missing permissions**: Grant the operator access to necessary resources.

5. Community Support

   If you need help, join the Kaniop discussion or open an issue on GitHub:

   - [GitHub Discussions](https://github.com/pando85/kaniop/discussions)
   - [GitHub Issues](https://github.com/pando85/kaniop/issues)
